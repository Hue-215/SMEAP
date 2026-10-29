use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::config::{self, RepoRef};
use super::git::{self, Tree, TreeEntry};
use super::http::Transport;
use super::state::{ArticleFiles, BaseSnapshot, Basis, BranchSnapshot, RemoteSnapshot, Skipped};
use super::GithubError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRead {
    pub snapshot: RemoteSnapshot,
    pub skipped: Vec<Skipped>,
}

// 形式に合うブランチ名からslugを取り出す
pub fn slug_of_branch(format: &str, branch: &str) -> Option<String> {
    let (before, after) = format.split_once("{slug}")?;
    let slug = branch.strip_prefix(before)?.strip_suffix(after)?;
    if slug.is_empty() {
        return None;
    }
    crate::store::validate_slug(slug)
        .ok()
        .map(|()| slug.to_string())
}

// 形式の{slug}より前の部分
pub fn branch_prefix(format: &str) -> &str {
    format
        .split_once("{slug}")
        .map_or(format, |(before, _)| before)
}

const UNREADABLE: &str = "unreadable";

fn unreadable(slug: &str, branch: &str) -> Skipped {
    Skipped {
        slug: slug.to_string(),
        branch: branch.to_string(),
        reason: UNREADABLE.to_string(),
    }
}

fn truncated() -> GithubError {
    GithubError::InvalidResponse {
        reason: "the tree is truncated".to_string(),
    }
}

// 通常のファイル(blobのmode 100644・100755)か。シンボリックリンク(120000)とサブモジュール(160000)は除く
fn is_regular_blob(entry: &TreeEntry) -> bool {
    entry.kind == "blob" && (entry.mode == "100644" || entry.mode == "100755")
}

// postsツリーから、mdのslug -> blobのsha。通常のファイルでないmdは読めない記事にする
fn md_entries(tree: &Tree, branch: &str, skipped: &mut Vec<Skipped>) -> BTreeMap<String, String> {
    let mut shas = BTreeMap::new();
    for entry in &tree.entries {
        let Some(stem) = entry.path.strip_suffix(".md") else {
            continue;
        };
        if crate::store::validate_slug(stem).is_err() {
            continue;
        }
        if !is_regular_blob(entry) {
            skipped.push(unreadable(stem, branch));
            continue;
        }
        shas.insert(stem.to_string(), entry.sha.clone());
    }
    shas
}

struct FileRow {
    name: String,
    sha: String,
    mode: String,
}

// treeの一覧を、slugのディレクトリの下のファイルにする。slugはrecursiveの
// 応答(パスはそのツリーからの相対)での取り出しの鍵。Noneなら読んだツリーの直下(subの応答)。
// slugの位置がtreeでない・下のファイルが通常のファイルでなければNone
fn file_rows(tree: &Tree, slug: Option<&str>) -> Option<Vec<FileRow>> {
    let mut rows = Vec::new();
    for entry in &tree.entries {
        let rest = match slug {
            Some(slug) => {
                if entry.path == slug {
                    // assets/{slug}の位置がtreeでない
                    if entry.kind != "tree" {
                        return None;
                    }
                    continue;
                }
                entry
                    .path
                    .strip_prefix(slug)
                    .and_then(|rest| rest.strip_prefix('/'))
            }
            None => Some(entry.path.as_str()),
        };
        let Some(name) = rest else {
            continue;
        };
        // 下のディレクトリ。ファイルはそれぞれのentryで来る
        if entry.kind == "tree" {
            continue;
        }
        if !is_regular_blob(entry) {
            return None;
        }
        rows.push(FileRow {
            name: name.to_string(),
            sha: entry.sha.clone(),
            mode: entry.mode.clone(),
        });
    }
    Some(rows)
}

// 1回の読み込みの通信。同じコミット・ツリーは取り直さない
struct Reader<'a> {
    t: &'a dyn Transport,
    token: &'a str,
    now: i64,
    repo: &'a RepoRef,
    // コミットsha -> ルートのツリーのsha
    root_trees: HashMap<String, String>,
    // (ツリーのsha, recursive) -> 取ったツリー
    trees: HashMap<(String, bool), Tree>,
}

impl Reader<'_> {
    fn root_tree(&mut self, commit: &str) -> Result<String, GithubError> {
        if let Some(sha) = self.root_trees.get(commit) {
            return Ok(sha.clone());
        }
        let sha = git::commit_tree(self.t, self.token, self.now, self.repo, commit)?;
        self.root_trees.insert(commit.to_string(), sha.clone());
        Ok(sha)
    }

    fn tree(&mut self, sha: &str, recursive: bool) -> Result<Tree, GithubError> {
        let key = (sha.to_string(), recursive);
        if let Some(tree) = self.trees.get(&key) {
            return Ok(tree.clone());
        }
        let tree = git::tree(self.t, self.token, self.now, self.repo, sha, recursive)?;
        if !recursive && tree.truncated {
            // 要るエントリがあるか決められないので、この回の読み込みは止める
            return Err(truncated());
        }
        self.trees.insert(key, tree.clone());
        Ok(tree)
    }

    // ルートのツリーからpathの名前で下りる。途中が無ければNone、treeでなければInvalidLayout
    fn walk(&mut self, root: &str, path: &str) -> Result<Option<String>, GithubError> {
        let mut sha = root.to_string();
        let mut position = String::new();
        for part in path.split('/') {
            if !position.is_empty() {
                position.push('/');
            }
            position.push_str(part);
            let tree = self.tree(&sha, false)?;
            sha = match tree.entries.iter().find(|entry| entry.path == part) {
                None => return Ok(None),
                Some(entry) if entry.kind == "tree" => entry.sha.clone(),
                Some(_) => return Err(GithubError::InvalidLayout { path: position }),
            };
        }
        Ok(Some(sha))
    }

    // assetsディレクトリから、そのslugの画像を組み立てる。Noneは読めない記事。recursiveがtruncatedなら、
    // そのslugのディレクトリを取り直す。取り直しもtruncatedなら、この回の読み込みは止める
    fn assets_of_slug(
        &mut self,
        assets_sha: Option<&str>,
        slug: &str,
    ) -> Result<Option<BTreeMap<String, String>>, GithubError> {
        Ok(self
            .asset_rows(assets_sha, slug)?
            .map(|rows| rows.into_iter().map(|row| (row.name, row.sha)).collect()))
    }

    // 上のfnと同じ読み取りで、modeも返す(read_articleのmodes用)
    fn asset_rows(
        &mut self,
        assets_sha: Option<&str>,
        slug: &str,
    ) -> Result<Option<Vec<FileRow>>, GithubError> {
        let Some(sha) = assets_sha else {
            return Ok(Some(Vec::new()));
        };
        let tree = self.tree(sha, true)?;
        if !tree.truncated {
            return Ok(file_rows(&tree, Some(slug)));
        }
        let immediate = self.tree(sha, false)?;
        let Some(entry) = immediate.entries.iter().find(|entry| entry.path == slug) else {
            return Ok(Some(Vec::new()));
        };
        if entry.kind != "tree" {
            return Ok(None);
        }
        let sub = self.tree(&entry.sha, true)?;
        if sub.truncated {
            return Err(truncated());
        }
        Ok(file_rows(&sub, None))
    }

    // コミットのpostsツリーとassetsの位置。記事のディレクトリが無ければNone
    fn read_positions(
        &mut self,
        commit: &str,
        basis: &Basis,
    ) -> Result<Option<(Tree, Option<String>)>, GithubError> {
        let root = self.root_tree(commit)?;
        let Some(posts_sha) = self.walk(&root, &basis.posts_path)? else {
            return Ok(None);
        };
        let posts_tree = self.tree(&posts_sha, false)?;
        let assets_sha = self.walk(&root, &config::assets_path(&basis.posts_path))?;
        Ok(Some((posts_tree, assets_sha)))
    }

    // ベースの記事の一覧。読めないものはskippedに入れる
    fn read_base(
        &mut self,
        basis: &Basis,
        commit: &str,
        skipped: &mut Vec<Skipped>,
    ) -> Result<BTreeMap<String, ArticleFiles>, GithubError> {
        let Some((posts_tree, assets_sha)) = self.read_positions(commit, basis)? else {
            return Ok(BTreeMap::new());
        };
        let mut articles = BTreeMap::new();
        for (slug, md) in md_entries(&posts_tree, &basis.base_branch, skipped) {
            match self.assets_of_slug(assets_sha.as_deref(), &slug)? {
                Some(assets) => {
                    articles.insert(slug, ArticleFiles { md, assets });
                }
                None => skipped.push(unreadable(&slug, &basis.base_branch)),
            }
        }
        Ok(articles)
    }

    // 記事のディレクトリやslugのmdが無ければ、このブランチに記事は無い
    fn read_branch(
        &mut self,
        basis: &Basis,
        slug: &str,
        commit: &str,
    ) -> Result<Option<ArticleFiles>, GithubError> {
        let Some((posts_tree, assets_sha)) = self.read_positions(commit, basis)? else {
            return Ok(None);
        };
        let name = format!("{slug}.md");
        let Some(entry) = posts_tree.entries.iter().find(|entry| entry.path == name) else {
            return Ok(None);
        };
        if !is_regular_blob(entry) {
            return Err(GithubError::InvalidLayout { path: name });
        }
        let Some(assets) = self.assets_of_slug(assets_sha.as_deref(), slug)? else {
            return Err(GithubError::InvalidLayout { path: name });
        };
        Ok(Some(ArticleFiles {
            md: entry.sha.clone(),
            assets,
        }))
    }
}

// 先頭がpreviousと同じブランチはツリーを取らない。読めない記事はpreviousの様子を残し、skippedに入れる
pub fn read_remote(
    t: &dyn Transport,
    token: &str,
    now: i64,
    repo: &RepoRef,
    basis: &Basis,
    previous: &RemoteSnapshot,
) -> Result<RemoteRead, GithubError> {
    let mut reader = Reader {
        t,
        token,
        now,
        repo,
        root_trees: HashMap::new(),
        trees: HashMap::new(),
    };
    let mut skipped = Vec::new();

    let base_commit = git::branch_head(t, token, now, repo, &basis.base_branch)?
        .ok_or(GithubError::BaseBranchNotFound)?;
    let previous_base = previous.base.as_ref();
    let base = match previous_base.filter(|base| base.commit == base_commit) {
        Some(kept) => kept.clone(),
        None => {
            let mut articles = reader.read_base(basis, &base_commit, &mut skipped)?;
            // 読めない記事は前回の様子のままにする
            for unreadable in &skipped {
                if let Some(files) =
                    previous_base.and_then(|base| base.articles.get(&unreadable.slug))
                {
                    articles.insert(unreadable.slug.clone(), files.clone());
                }
            }
            BaseSnapshot {
                commit: base_commit,
                articles,
            }
        }
    };

    let prefix = branch_prefix(&basis.branch_format).trim_end_matches('/');
    let mut branches = BTreeMap::new();
    for reference in git::matching_branches(t, token, now, repo, prefix)? {
        // ベースの名前は記事用にしない
        if reference.name == basis.base_branch {
            continue;
        }
        let Some(slug) = slug_of_branch(&basis.branch_format, &reference.name) else {
            continue;
        };
        let kept = previous.branches.get(&reference.name);
        if let Some(kept) = kept.filter(|branch| branch.commit == reference.commit) {
            branches.insert(reference.name, kept.clone());
            continue;
        }
        match reader.read_branch(basis, &slug, &reference.commit) {
            Ok(Some(article)) => {
                branches.insert(
                    reference.name,
                    BranchSnapshot {
                        commit: reference.commit,
                        slug,
                        article,
                    },
                );
            }
            Ok(None) => {}
            Err(GithubError::InvalidLayout { .. }) => {
                // 記事は読めない。様子があれば前回のまま
                skipped.push(unreadable(&slug, &reference.name));
                if let Some(kept) = kept {
                    branches.insert(reference.name, kept.clone());
                }
            }
            // 通信の失敗(404も含む)は、この回の読み込みを止める
            Err(error) => return Err(error),
        }
    }

    Ok(RemoteRead {
        snapshot: RemoteSnapshot {
            base: Some(base),
            branches,
        },
        skipped,
    })
}

// そのコミットでの記事。modesはリポジトリのルートからのパス -> mode(記事のmdとassets/{slug}/の下のファイル)
pub struct RemoteArticle {
    pub files: ArticleFiles,
    pub modes: BTreeMap<String, String>,
}

pub fn read_article(
    t: &dyn Transport,
    token: &str,
    now: i64,
    repo: &RepoRef,
    basis: &Basis,
    slug: &str,
    commit: &str,
) -> Result<Option<RemoteArticle>, GithubError> {
    let mut reader = Reader {
        t,
        token,
        now,
        repo,
        root_trees: HashMap::new(),
        trees: HashMap::new(),
    };
    let Some((posts_tree, assets_sha)) = reader.read_positions(commit, basis)? else {
        return Ok(None);
    };
    let name = format!("{slug}.md");
    let Some(entry) = posts_tree.entries.iter().find(|entry| entry.path == name) else {
        return Ok(None);
    };
    if !is_regular_blob(entry) {
        return Err(GithubError::InvalidLayout { path: name });
    }
    let rows = reader
        .asset_rows(assets_sha.as_deref(), slug)?
        .ok_or(GithubError::InvalidLayout { path: name.clone() })?;
    let assets_dir = format!("{}/{slug}", config::assets_path(&basis.posts_path));
    let mut modes = BTreeMap::from([(format!("{}/{name}", basis.posts_path), entry.mode.clone())]);
    let assets = rows
        .into_iter()
        .map(|row| {
            modes.insert(format!("{assets_dir}/{}", row.name), row.mode);
            (row.name, row.sha)
        })
        .collect();
    Ok(Some(RemoteArticle {
        files: ArticleFiles {
            md: entry.sha.clone(),
            assets,
        },
        modes,
    }))
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::github::fake::{blob_sha, repo_ref, url_of, FakeRepo, RouteTransport};
    use crate::github::http::Method;
    use crate::github::state::{ArticleFiles, BaseSnapshot, BranchSnapshot};
    const TOKEN: &str = "TOKEN";
    const NOW: i64 = 1_000_000;
    const MD: &[u8] = b"post\n";
    const OTHER_MD: &[u8] = b"other\n";
    const BRANCH_MD: &[u8] = b"branch\n";
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

    fn basis_of(format: &str) -> Basis {
        Basis {
            base_branch: "main".to_string(),
            branch_format: format.to_string(),
            posts_path: "src/content/blog".to_string(),
        }
    }

    fn basis() -> Basis {
        basis_of("blog/{slug}")
    }

    fn new_repo(transport: &RouteTransport) -> FakeRepo {
        FakeRepo::new(transport, "owner", "blog")
    }

    fn read(
        t: &RouteTransport,
        basis: &Basis,
        previous: &RemoteSnapshot,
    ) -> Result<RemoteRead, GithubError> {
        read_remote(t, TOKEN, NOW, &repo_ref(), basis, previous)
    }

    // skippedは、slugとbranchの組で比べる(理由の文言は縛らない)
    fn skipped_pairs(skipped: &[Skipped]) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = skipped
            .iter()
            .map(|s| (s.slug.clone(), s.branch.clone()))
            .collect();
        pairs.sort();
        pairs
    }

    fn keys_of(map: &BTreeMap<String, impl Sized>) -> Vec<&str> {
        map.keys().map(String::as_str).collect()
    }

    fn base_keys(read: &RemoteRead) -> Vec<&str> {
        keys_of(
            &read
                .snapshot
                .base
                .as_ref()
                .expect("ベースの様子はあるはず")
                .articles,
        )
    }

    fn request_urls(transport: &RouteTransport) -> Vec<String> {
        transport
            .requests()
            .iter()
            .map(|request| request.url.clone())
            .collect()
    }

    // 止まるのは、BaseBranchNotFound(ベースのrefが404のとき)とは別のエラーで
    fn assert_stopped(result: Result<RemoteRead, GithubError>) {
        match result {
            Err(error) => assert!(
                !matches!(error, GithubError::BaseBranchNotFound),
                "ベースが404のときとは別のエラーで止めるはず: {error}"
            ),
            Ok(_) => panic!("読み込みは止まるはず"),
        }
    }

    // /git/trees/へのリクエストのURL。recursiveなら?recursive=1が付いているもの
    fn tree_urls(urls: &[String], recursive: bool) -> Vec<String> {
        urls.iter()
            .filter(|url| url.contains("/git/trees/") && url.contains("?recursive=1") == recursive)
            .cloned()
            .collect()
    }

    #[test]
    fn ブランチ名からslugを取り出す() {
        assert_eq!(
            slug_of_branch("blog/{slug}", "blog/first-post"),
            Some("first-post".to_string())
        );
        assert_eq!(
            slug_of_branch("{slug}", "first-post"),
            Some("first-post".to_string())
        );
        assert_eq!(
            slug_of_branch("pre-{slug}-post", "pre-first-post"),
            Some("first".to_string())
        );

        for (format, branch) in [
            // 前が合わない・後ろが合わない
            ("blog/{slug}", "first-post"),
            ("blog/{slug}", "blog/first-post/extra"),
            ("pre-{slug}-post", "pre-first"),
            ("pre-{slug}-post", "pre-first-post/extra"),
            // 間が空
            ("blog/{slug}", "blog/"),
            // slugとして正しくない
            ("blog/{slug}", "blog/First-Post"),
            ("{slug}", "a_b"),
        ] {
            assert_eq!(
                slug_of_branch(format, branch),
                None,
                "{format} の {branch} は読まないはず"
            );
        }
    }

    #[test]
    fn ベースのブランチは記事用のブランチにしない() {
        // prefixは形式の{slug}より前の部分。末尾の/が残る
        assert_eq!(branch_prefix("blog/{slug}"), "blog/");
        assert_eq!(branch_prefix("{slug}"), "");
        assert_eq!(branch_prefix("a/b-{slug}"), "a/b-");

        // slug_of_branchそのものは、ベースの名前も記事用の形として読める。読み込みで除く
        assert_eq!(slug_of_branch("{slug}", "main"), Some("main".to_string()));

        let basis = basis_of("{slug}");
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(&transport, "main", &[("src/content/blog/post.md", MD)]);
        repo.branch(&transport, "post", &[("src/content/blog/post.md", MD)]);

        let read = read(&transport, &basis, &RemoteSnapshot::default()).unwrap();
        assert_eq!(base_keys(&read), ["post"]);
        // mainは記事用のブランチの一覧に入らない
        assert_eq!(keys_of(&read.snapshot.branches), ["post"]);
        assert_eq!(read.snapshot.branches["post"].slug, "post");
    }

    #[test]
    fn ベースのブランチのrefが404なら読み込みを止める() {
        let transport = RouteTransport::new();
        new_repo(&transport);
        // ベースのrefだけ404にする
        transport.json(
            Method::Get,
            &url_of(
                &["repos", "owner", "blog", "git", "ref", "heads", "main"],
                &[],
            ),
            404,
            serde_json::json!({"message": "Not Found"}),
        );

        assert!(matches!(
            read(
                &transport,
                &basis_of("blog/{slug}"),
                &RemoteSnapshot::default()
            ),
            Err(GithubError::BaseBranchNotFound)
        ));
    }

    #[test]
    fn ベース以外の404は読み込みを止める() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(&transport, "main", &[("src/content/blog/post.md", MD)]);
        let branch_commit = repo.branch(
            &transport,
            "blog/post",
            &[("src/content/blog/post.md", BRANCH_MD)],
        );
        // 一覧の後に記事用のブランチが消えた(commitが404)
        transport.on(
            Method::Get,
            &url_of(
                &[
                    "repos",
                    "owner",
                    "blog",
                    "git",
                    "commits",
                    branch_commit.as_str(),
                ],
                &[],
            ),
            Box::new(|_| {
                Ok(crate::github::fake::json_response(
                    404,
                    serde_json::json!({"message": "Not Found"}),
                ))
            }),
        );

        assert_stopped(read(&transport, &basis(), &RemoteSnapshot::default()));
    }

    #[test]
    fn 記事のディレクトリまで1段ずつ下りる() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        let commit = repo.branch(
            &transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/content/blog/second.md", OTHER_MD),
                ("src/assets/post/01.png", PNG),
                ("src/assets/second/01.png", PNG),
            ],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let base = read.snapshot.base.as_ref().expect("ベースの様子はあるはず");
        assert_eq!(base.commit, commit);
        let post = &base.articles["post"];
        assert_eq!(post.md, blob_sha(MD));
        assert_eq!(
            post.assets.get("01.png").map(String::as_str),
            Some(blob_sha(PNG).as_str())
        );
        assert_eq!(
            base.articles["second"]
                .assets
                .get("01.png")
                .map(String::as_str),
            Some(blob_sha(PNG).as_str())
        );

        // 再帰でないツリーはルート・src・content・blogの4つで、srcは1回だけ
        let urls = request_urls(&transport);
        let without = tree_urls(&urls, false);
        assert_eq!(without.len(), 4, "{urls:?}");
        let mut once = without.clone();
        once.sort();
        once.dedup();
        assert_eq!(once.len(), 4, "同じツリーを取り直している: {without:?}");
        // assetsはrecursive=1で読む
        let with = tree_urls(&urls, true);
        assert_eq!(with.len(), 1, "{urls:?}");
    }

    #[test]
    fn 記事のディレクトリが無ければ記事は無い() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        let commit = repo.branch(&transport, "main", &[("src/content/other.md", OTHER_MD)]);

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let base = read.snapshot.base.as_ref().expect("ベースの様子はあるはず");
        assert_eq!(base.commit, commit);
        assert!(base.articles.is_empty());
        assert!(read.skipped.is_empty());
    }

    #[test]
    fn 途中の部分がtreeでなければベースの読み込みを止める() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch_with_entries(
            &transport,
            "main",
            &[("src/content/other.md", OTHER_MD)],
            // 記事のディレクトリの位置がblobになっている
            &[("src/content/blog", "100644", "blob")],
        );

        assert!(matches!(
            read(&transport, &basis(), &RemoteSnapshot::default()),
            Err(GithubError::InvalidLayout { .. })
        ));
    }

    #[test]
    fn 途中の部分がtreeでなければその記事は読めない記事になる() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        // ベースは正しく、記事用のブランチだけが使えない形
        repo.branch(&transport, "main", &[("src/content/blog/post.md", MD)]);
        repo.branch_with_entries(
            &transport,
            "blog/post",
            &[("src/content/blog/other.md", BRANCH_MD)],
            &[("src/content/blog", "100644", "blob")],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        assert_eq!(
            skipped_pairs(&read.skipped),
            vec![("post".to_string(), "blog/post".to_string())]
        );
        assert!(read.snapshot.branches.is_empty());
        // ベースの記事に触れない
        assert_eq!(base_keys(&read), ["post"]);
    }

    #[test]
    fn mdはslugの正しい名前だけ入る() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(
            &transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/content/blog/README.md", OTHER_MD),
                ("src/content/blog/Foo.md", OTHER_MD),
                ("src/content/blog/a.txt", OTHER_MD),
            ],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        assert!(read.skipped.is_empty());
        assert!(read.snapshot.branches.is_empty());
        let base = read.snapshot.base.as_ref().expect("ベースの様子はあるはず");
        assert_eq!(keys_of(&base.articles), ["post"]);
        let post = &base.articles["post"];
        assert_eq!(post.md, blob_sha(MD));
        assert!(post.assets.is_empty());
    }

    #[test]
    fn mdがblobでない記事は読めない記事になる() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        // ベースの記事は正しく
        repo.branch(&transport, "main", &[("src/content/blog/post.md", MD)]);
        // サブモジュール・シンボリックリンク・ディレクトリ
        repo.branch_with_entries(
            &transport,
            "blog/sub",
            &[],
            &[("src/content/blog/sub.md", "160000", "commit")],
        );
        repo.branch_with_entries(
            &transport,
            "blog/link",
            &[],
            &[("src/content/blog/link.md", "120000", "blob")],
        );
        repo.branch_with_entries(
            &transport,
            "blog/dir",
            &[],
            &[("src/content/blog/dir.md", "040000", "tree")],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        assert_eq!(
            skipped_pairs(&read.skipped),
            vec![
                ("dir".to_string(), "blog/dir".to_string()),
                ("link".to_string(), "blog/link".to_string()),
                ("sub".to_string(), "blog/sub".to_string()),
            ]
        );
        assert!(read.snapshot.branches.is_empty());
        assert_eq!(base_keys(&read), ["post"]);
    }

    #[test]
    fn assetsの下にblobでないものがあれば読めない記事になる() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(
            &transport,
            "main",
            &[("src/content/blog/other.md", OTHER_MD)],
        );
        repo.branch_with_entries(
            &transport,
            "blog/post",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
            // assetsの下にサブモジュール
            &[("src/assets/post/locked.png", "160000", "commit")],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        assert_eq!(
            skipped_pairs(&read.skipped),
            vec![("post".to_string(), "blog/post".to_string())]
        );
        assert!(read.snapshot.branches.is_empty());
    }

    #[test]
    fn assetsのslugのディレクトリがtreeでなければ読めない記事になる() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(
            &transport,
            "main",
            &[("src/content/blog/other.md", OTHER_MD)],
        );
        // assets/{slug}の位置がblobになっている
        repo.branch_with_entries(
            &transport,
            "blog/post",
            &[("src/content/blog/post.md", MD)],
            &[("src/assets/post", "100644", "blob")],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        assert_eq!(
            skipped_pairs(&read.skipped),
            vec![("post".to_string(), "blog/post".to_string())]
        );
        assert!(read.snapshot.branches.is_empty());
    }

    #[test]
    fn 再帰のassetsがtruncatedなら記事ごとに取り直す() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(
            &transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/content/blog/second.md", OTHER_MD),
                ("src/assets/post/01.png", PNG),
                ("src/assets/second/01.png", PNG),
            ],
        );

        let first = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        // assetsのrecursiveの応答をtruncatedに上書きする
        let urls = request_urls(&transport);
        let assets_url = tree_urls(&urls, true);
        assert_eq!(assets_url.len(), 1, "{urls:?}");
        transport.json(
            Method::Get,
            &assets_url[0],
            200,
            serde_json::json!({
                "sha": "sha-ignored",
                "tree": [
                    {"path": "post/01.png", "mode": "100644", "type": "blob", "sha": blob_sha(PNG)},
                    {"path": "second/01.png", "mode": "100644", "type": "blob", "sha": blob_sha(PNG)},
                ],
                "truncated": true,
            }),
        );

        // 記事ごとにassets/{slug}を再帰で取り直している
        let before = transport.requests().len();
        let second = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let urls: Vec<String> = transport.requests()[before..]
            .iter()
            .map(|r| r.url.clone())
            .collect();
        // assetsの1回と、記事ごとの2回
        let mut refetched = tree_urls(&urls, true);
        assert_eq!(refetched.len(), 3, "{urls:?}");
        refetched.sort();
        refetched.dedup();
        assert_eq!(refetched.len(), 3, "{urls:?}");
        // 一覧は、取り直しで同じになる
        assert_eq!(second.snapshot, first.snapshot);
        assert!(second.skipped.is_empty());
    }

    #[test]
    fn 記事ごとの取り直しもtruncatedなら読み込みを止める() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(
            &transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );

        let first = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        assert!(first.skipped.is_empty());
        // assetsのrecursiveの応答をtruncatedにして、記事ごとの取り直しもtruncatedにする
        let urls = request_urls(&transport);
        let assets_url = tree_urls(&urls, true);
        assert_eq!(assets_url.len(), 1, "{urls:?}");
        transport.json(
            Method::Get,
            &assets_url[0],
            200,
            serde_json::json!({
                "sha": "sha-ignored",
                "tree": [
                    {"path": "post/01.png", "mode": "100644", "type": "blob", "sha": blob_sha(PNG)},
                ],
                "truncated": true,
            }),
        );
        let before = transport.requests().len();
        read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let urls: Vec<String> = transport.requests()[before..]
            .iter()
            .map(|r| r.url.clone())
            .collect();
        let refetched = tree_urls(&urls, true);
        // 1つはassetsのrecursive、もう1つは記事ごとの取り直し
        assert_eq!(refetched.len(), 2, "{urls:?}");
        transport.json(
            Method::Get,
            &refetched[1],
            200,
            serde_json::json!({
                "sha": "sha-ignored",
                "tree": [
                    {"path": "01.png", "mode": "100644", "type": "blob", "sha": blob_sha(PNG)},
                ],
                "truncated": true,
            }),
        );

        assert_stopped(read(&transport, &basis(), &RemoteSnapshot::default()));
    }

    #[test]
    fn 再帰なしのツリーがtruncatedなら読み込みを止める() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(
            &transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        // ルートのツリー(下りてくる最初のツリー)をtruncatedに上書きする
        let urls = request_urls(&transport);
        let root_url = tree_urls(&urls, false);
        assert_eq!(root_url.len(), 4, "{urls:?}");
        transport.json(
            Method::Get,
            &root_url[0],
            200,
            serde_json::json!({
                "sha": "sha-ignored",
                "tree": [],
                "truncated": true,
            }),
        );

        assert_stopped(read(&transport, &basis(), &RemoteSnapshot::default()));
    }

    #[test]
    fn 記事用のブランチはそのslugの記事だけを見る() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(&transport, "main", &[("src/content/blog/post.md", MD)]);
        repo.branch(
            &transport,
            "blog/post",
            &[
                ("src/content/blog/post.md", MD),
                // 同じブランチにある別の記事
                ("src/content/blog/other.md", OTHER_MD),
                ("src/assets/other/01.png", PNG),
                ("src/assets/post/01.png", PNG),
            ],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let branch = &read.snapshot.branches["blog/post"];
        assert_eq!(branch.slug, "post");
        assert_eq!(branch.article.md, blob_sha(MD));
        // 別の記事の画像は一覧に入らない
        assert_eq!(keys_of(&branch.article.assets), ["01.png"]);
        assert_eq!(
            branch.article.assets.get("01.png").map(String::as_str),
            Some(blob_sha(PNG).as_str())
        );
        assert_eq!(keys_of(&read.snapshot.branches), ["blog/post"]);
        assert_eq!(base_keys(&read), ["post"]);
    }

    #[test]
    fn 記事用のブランチにそのslugのmdが無ければそのブランチは無いもの() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(&transport, "main", &[("src/content/blog/post.md", MD)]);
        // blog/postには、別の記事のmdと画像だけがある
        repo.branch(
            &transport,
            "blog/post",
            &[
                ("src/content/blog/other.md", OTHER_MD),
                ("src/assets/other/01.png", PNG),
            ],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        assert!(read.snapshot.branches.is_empty());
        assert!(read.skipped.is_empty());
        assert_eq!(base_keys(&read), ["post"]);
    }

    #[test]
    fn 先頭のコミットが覚えたものと同じならツリーを取らない() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        let base_commit = repo.branch(
            &transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        let branch_commit = repo.branch(
            &transport,
            "blog/post",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        // 覚えておく一覧を手で組み立てる(ツリーの応答と同じ値)
        let article = ArticleFiles {
            md: blob_sha(MD),
            assets: BTreeMap::from([("01.png".to_string(), blob_sha(PNG))]),
        };
        let previous = RemoteSnapshot {
            base: Some(BaseSnapshot {
                commit: base_commit.clone(),
                articles: BTreeMap::from([("post".to_string(), article.clone())]),
            }),
            branches: BTreeMap::from([(
                "blog/post".to_string(),
                BranchSnapshot {
                    commit: branch_commit.clone(),
                    slug: "post".to_string(),
                    article,
                },
            )]),
        };

        let read = read(&transport, &basis(), &previous).unwrap();
        assert_eq!(read.snapshot, previous);
        assert!(read.skipped.is_empty());
        // headを確かめる通信(一覧とref)のほかに、commitsとtreesの読み込みは無い
        let urls = request_urls(&transport);
        assert!(
            !urls
                .iter()
                .any(|url| url.contains("/git/commits/") || url.contains("/git/trees/")),
            "{urls:?}"
        );
    }

    #[test]
    fn 読めない記事は前回の様子を残す() {
        let article = ArticleFiles {
            md: blob_sha(MD),
            assets: BTreeMap::from([("01.png".to_string(), blob_sha(PNG))]),
        };
        let previous = RemoteSnapshot {
            base: Some(BaseSnapshot {
                commit: "commit-old".to_string(),
                articles: BTreeMap::from([("post".to_string(), article.clone())]),
            }),
            branches: BTreeMap::from([(
                "blog/post".to_string(),
                BranchSnapshot {
                    commit: "commit-old-branch".to_string(),
                    slug: "post".to_string(),
                    article,
                },
            )]),
        };

        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        // どちらでもmdがサブモジュールになっていて読めない
        let base_commit = repo.branch_with_entries(
            &transport,
            "main",
            &[],
            &[("src/content/blog/post.md", "160000", "commit")],
        );
        repo.branch_with_entries(
            &transport,
            "blog/post",
            &[],
            &[("src/content/blog/post.md", "160000", "commit")],
        );

        let read = read(&transport, &basis(), &previous).unwrap();
        assert_eq!(
            skipped_pairs(&read.skipped),
            vec![
                ("post".to_string(), "blog/post".to_string()),
                ("post".to_string(), "main".to_string()),
            ]
        );
        // 読めない記事の一覧は前回のまま
        let base = read.snapshot.base.as_ref().expect("ベースの様子はあるはず");
        let previous_base = previous
            .base
            .as_ref()
            .expect("前回のベースの様子はあるはず");
        assert_eq!(base.articles, previous_base.articles);
        assert_eq!(read.snapshot.branches, previous.branches);
        assert_eq!(base.commit, base_commit);
    }
}
