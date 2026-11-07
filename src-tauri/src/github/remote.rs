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

// assetsの直下の通常のファイル(共有の画像)。シンボリックリンクとサブモジュールは除く
fn shared_rows(tree: &Tree) -> BTreeMap<String, String> {
    tree.entries
        .iter()
        .filter(|entry| !entry.path.contains('/') && is_regular_blob(entry))
        .map(|entry| (entry.path.clone(), entry.sha.clone()))
        .collect()
}

#[derive(Default)]
struct BaseEntries {
    articles: BTreeMap<String, ArticleFiles>,
    shared_assets: BTreeMap<String, String>,
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

    // 読んだassetsツリーから、直下の共有の画像を組み立てる。recursiveがtruncatedなら直下のツリーを取り直す
    fn shared_assets(
        &mut self,
        assets_sha: Option<&str>,
    ) -> Result<BTreeMap<String, String>, GithubError> {
        let Some(sha) = assets_sha else {
            return Ok(BTreeMap::new());
        };
        let mut tree = self.tree(sha, true)?;
        if tree.truncated {
            tree = self.tree(sha, false)?;
        }
        Ok(shared_rows(&tree))
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

    // ベースの記事と共有の画像の一覧。読めないものはskippedに入れる
    fn read_base(
        &mut self,
        basis: &Basis,
        commit: &str,
        skipped: &mut Vec<Skipped>,
    ) -> Result<BaseEntries, GithubError> {
        let Some((posts_tree, assets_sha)) = self.read_positions(commit, basis)? else {
            return Ok(BaseEntries::default());
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
        Ok(BaseEntries {
            articles,
            shared_assets: self.shared_assets(assets_sha.as_deref())?,
        })
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

// 記事用のブランチが移した記事。Unreadableは、そのブランチの記事を読めない記事にする
enum Moved {
    None,
    Article(String),
    Unreadable,
}

// ベースとの分岐点から消した・名前を変えた記事のmdを調べる
fn moved_article(
    reader: &mut Reader<'_>,
    basis: &Basis,
    base: &BaseSnapshot,
    commit: &str,
) -> Result<Moved, GithubError> {
    let compared = match git::compare(
        reader.t,
        reader.token,
        reader.now,
        reader.repo,
        &base.commit,
        commit,
    ) {
        Ok(compared) => compared,
        // 共通の祖先が無い
        Err(GithubError::Status { status: 404 }) => return Ok(Moved::Unreadable),
        Err(error) => return Err(error),
    };
    // 300件までしか返らず、続きを取る方法が無い
    if compared.len() >= 300 {
        return Ok(Moved::Unreadable);
    }
    let prefix = format!("{}/", basis.posts_path);
    let mut moved = std::collections::BTreeSet::new();
    for file in &compared {
        let path = match (file.status.as_str(), &file.previous_filename) {
            ("removed", _) => &file.filename,
            ("renamed", Some(path)) => path,
            _ => continue,
        };
        let Some(name) = path.strip_prefix(&prefix) else {
            continue;
        };
        let Some(slug) = name.strip_suffix(".md") else {
            continue;
        };
        if name.contains('/') || crate::store::validate_slug(slug).is_err() {
            continue;
        }
        if base.articles.contains_key(slug) {
            moved.insert(slug.to_string());
        }
    }
    match moved.len() {
        0 => Ok(Moved::None),
        1 => Ok(Moved::Article(moved.into_iter().next().unwrap())),
        _ => Ok(Moved::Unreadable),
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
    // 前のバージョンのstate.jsonにはsharedAssetsが無いので、先頭が同じでもツリーを取り直す
    let base = match previous_base
        .filter(|base| base.commit == base_commit && base.shared_assets.is_some())
    {
        Some(kept) => kept.clone(),
        None => {
            let BaseEntries {
                mut articles,
                shared_assets,
            } = reader.read_base(basis, &base_commit, &mut skipped)?;
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
                shared_assets: Some(shared_assets),
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
                let moved = if base.articles.contains_key(&slug) {
                    Moved::None
                } else {
                    moved_article(&mut reader, basis, &base, &reference.commit)?
                };
                let moved_from = match moved {
                    Moved::None => None,
                    Moved::Article(old) => Some(old),
                    Moved::Unreadable => {
                        skipped.push(unreadable(&slug, &reference.name));
                        if let Some(kept) = kept {
                            branches.insert(reference.name, kept.clone());
                        }
                        continue;
                    }
                };
                branches.insert(
                    reference.name,
                    BranchSnapshot {
                        commit: reference.commit,
                        slug,
                        article: Some(article),
                        moved_from,
                    },
                );
            }
            // 記事の無い記事用のブランチ(空のブランチ)
            Ok(None) => {
                branches.insert(
                    reference.name,
                    BranchSnapshot {
                        commit: reference.commit,
                        slug,
                        article: None,
                        moved_from: None,
                    },
                );
            }
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

    // 先頭が同じで前回の様子を残したブランチも、移した記事が今のベースで成り立つか確かめ直す
    // (マージした後、ベースに新しいslugが入り元のslugが消える)
    for branch in branches.values_mut() {
        if branch.moved_from.as_ref().is_some_and(|old| {
            !base.articles.contains_key(old) || base.articles.contains_key(&branch.slug)
        }) {
            branch.moved_from = None;
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
    use crate::github::fake::{blob_sha, compare_url, repo_ref, url_of, FakeRepo, RouteTransport};
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
            // Windowsの予約名
            ("blog/{slug}", "blog/con"),
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
                ("src/content/blog/con.md", OTHER_MD),
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
        let article = branch.article.as_ref().expect("記事はあるはず");
        assert_eq!(branch.slug, "post");
        assert_eq!(article.md, blob_sha(MD));
        // 別の記事の画像は一覧に入らない
        assert_eq!(keys_of(&article.assets), ["01.png"]);
        assert_eq!(
            article.assets.get("01.png").map(String::as_str),
            Some(blob_sha(PNG).as_str())
        );
        assert_eq!(keys_of(&read.snapshot.branches), ["blog/post"]);
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
                ("src/assets/shared.png", PNG),
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
                shared_assets: Some(BTreeMap::from([("shared.png".to_string(), blob_sha(PNG))])),
            }),
            branches: BTreeMap::from([(
                "blog/post".to_string(),
                BranchSnapshot {
                    commit: branch_commit.clone(),
                    slug: "post".to_string(),
                    article: Some(article),
                    moved_from: None,
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
                shared_assets: None,
            }),
            branches: BTreeMap::from([(
                "blog/post".to_string(),
                BranchSnapshot {
                    commit: "commit-old-branch".to_string(),
                    slug: "post".to_string(),
                    article: Some(article),
                    moved_from: None,
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

    #[test]
    fn ベースのassetsの直下のファイルをsharedAssetsに入れる() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(
            &transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/shared.png", PNG),
                ("src/assets/post/01.png", PNG),
            ],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let base = read.snapshot.base.as_ref().expect("ベースの様子はあるはず");
        assert_eq!(
            base.shared_assets,
            Some(BTreeMap::from([("shared.png".to_string(), blob_sha(PNG),)]))
        );
        // 共有の画像は記事のassetsには入らない
        assert_eq!(keys_of(&base.articles["post"].assets), ["01.png"]);
    }

    #[test]
    fn sharedAssetsに通常のファイルでないものは入れず記事は読める() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch_with_entries(
            &transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/shared.png", PNG),
            ],
            &[
                // 実行できるファイル(中身は空)と、シンボリックリンクとサブモジュール
                ("src/assets/run.png", "100755", "blob"),
                ("src/assets/link.png", "120000", "blob"),
                ("src/assets/locked.png", "160000", "commit"),
            ],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let base = read.snapshot.base.as_ref().expect("ベースの様子はあるはず");
        assert_eq!(
            base.shared_assets,
            Some(BTreeMap::from([
                ("run.png".to_string(), blob_sha(b"")),
                ("shared.png".to_string(), blob_sha(PNG)),
            ]))
        );
        assert!(read.skipped.is_empty());
        assert_eq!(base_keys(&read), ["post"]);
    }

    #[test]
    fn 記事用のブランチのsharedAssetsは持たない() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(&transport, "main", &[("src/content/blog/post.md", MD)]);
        // 記事用のブランチにだけある共有の画像
        repo.branch(
            &transport,
            "blog/post",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/branch-only.png", PNG),
            ],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let base = read.snapshot.base.as_ref().expect("ベースの様子はあるはず");
        assert_eq!(base.shared_assets, Some(BTreeMap::new()));
        assert!(read.snapshot.branches.contains_key("blog/post"));
    }

    #[test]
    fn assetsが無ければsharedAssetsは空() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(&transport, "main", &[("src/content/blog/post.md", MD)]);

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let base = read.snapshot.base.as_ref().expect("ベースの様子はあるはず");
        assert_eq!(base.shared_assets, Some(BTreeMap::new()));
    }

    #[test]
    fn 再帰のassetsがtruncatedでも直下のツリーからsharedAssetsを取る() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(
            &transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/content/blog/second.md", OTHER_MD),
                ("src/assets/shared.png", PNG),
                ("src/assets/post/01.png", PNG),
                ("src/assets/second/01.png", PNG),
            ],
        );

        let _first = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
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
                // 切り詰められて共有の画像が入っていない
                "tree": [
                    {"path": "post/01.png", "mode": "100644", "type": "blob", "sha": blob_sha(PNG)},
                    {"path": "second/01.png", "mode": "100644", "type": "blob", "sha": blob_sha(PNG)},
                ],
                "truncated": true,
            }),
        );
        let before = transport.requests().len();

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let base = read.snapshot.base.as_ref().expect("ベースの様子はあるはず");
        assert_eq!(
            base.shared_assets,
            Some(BTreeMap::from([("shared.png".to_string(), blob_sha(PNG),)]))
        );
        let assets_immediate = assets_url[0].split('?').next().unwrap().to_string();
        let urls = request_urls(&transport);
        assert!(urls[before..].contains(&assets_immediate), "{urls:?}");
    }

    #[test]
    fn assetsの直下のツリーもtruncatedなら読み込みを止める() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        // 記事が無いので、assetsの直下のツリーを読むのは共有の画像の一覧だけ
        repo.branch(
            &transport,
            "main",
            &[
                ("src/content/blog/README.txt", MD),
                ("src/assets/shared.png", PNG),
                ("src/assets/post/01.png", PNG),
            ],
        );

        let _first = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        // assetsのrecursiveの応答をtruncatedにする
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
                    {"path": "shared.png", "mode": "100644", "type": "blob", "sha": blob_sha(PNG)},
                    {"path": "post/01.png", "mode": "100644", "type": "blob", "sha": blob_sha(PNG)},
                ],
                "truncated": true,
            }),
        );
        // 直下のツリーの応答もtruncatedにする
        let assets_immediate = assets_url[0]
            .split('?')
            .next()
            .expect("assetsのツリーのURLはあるはず")
            .to_string();
        transport.json(
            Method::Get,
            &assets_immediate,
            200,
            serde_json::json!({
                "sha": "sha-ignored",
                "tree": [
                    {"path": "shared.png", "mode": "100644", "type": "blob", "sha": blob_sha(PNG)},
                ],
                "truncated": true,
            }),
        );

        let before = transport.requests().len();

        assert_stopped(read(&transport, &basis(), &RemoteSnapshot::default()));
        let urls = request_urls(&transport);
        assert!(urls[before..].contains(&assets_immediate), "{urls:?}");
    }

    #[test]
    fn 前回のsharedAssetsが無ければ先頭が同じでもツリーを取り直す() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        let base_commit = repo.branch(
            &transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/shared.png", PNG),
            ],
        );
        let previous = RemoteSnapshot {
            base: Some(BaseSnapshot {
                commit: base_commit.clone(),
                articles: BTreeMap::new(),
                shared_assets: None,
            }),
            branches: BTreeMap::new(),
        };

        let read = read(&transport, &basis(), &previous).unwrap();
        let base = read.snapshot.base.as_ref().expect("ベースの様子はあるはず");
        assert_eq!(base.commit, base_commit);
        assert_eq!(
            base.shared_assets,
            Some(BTreeMap::from([("shared.png".to_string(), blob_sha(PNG),)]))
        );
        let urls = request_urls(&transport);
        assert!(
            urls.iter().any(|url| url.contains("/git/trees/")),
            "{urls:?}"
        );
    }

    #[test]
    fn 記事のディレクトリがcontent_blogならassetsの直下を読む() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(
            &transport,
            "main",
            &[("content/blog/post.md", MD), ("assets/shared.png", PNG)],
        );
        let basis = Basis {
            base_branch: "main".to_string(),
            branch_format: "blog/{slug}".to_string(),
            posts_path: "content/blog".to_string(),
        };

        let read = read(&transport, &basis, &RemoteSnapshot::default()).unwrap();
        let base = read.snapshot.base.as_ref().expect("ベースの様子はあるはず");
        assert_eq!(
            base.shared_assets,
            Some(BTreeMap::from([("shared.png".to_string(), blob_sha(PNG),)]))
        );
    }

    fn compare_files_json(count: usize) -> serde_json::Value {
        let files: Vec<serde_json::Value> = (0..count)
            .map(|index| {
                serde_json::json!({
                    "filename": format!("src/content/blog/other{index}.md"),
                    "status": "modified",
                })
            })
            .collect();
        serde_json::json!({ "status": "diverged", "files": files })
    }

    #[test]
    fn 記事の無い記事用のブランチも空のブランチとして様子に入る() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(&transport, "main", &[("src/content/blog/post.md", MD)]);
        let commit = repo.branch(
            &transport,
            "blog/empty",
            &[("src/content/blog/other.md", OTHER_MD)],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let branch = read
            .snapshot
            .branches
            .get("blog/empty")
            .expect("空のブランチも様子に入るはず");
        assert_eq!(branch.commit, commit);
        assert_eq!(branch.slug, "empty");
        assert!(branch.article.is_none());
        assert!(branch.moved_from.is_none());
        assert!(read.skipped.is_empty());
    }

    #[test]
    fn 分岐点からベースの記事のmdを消していれば移した記事として持つ() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        let base = repo.branch(&transport, "main", &[("src/content/blog/old.md", MD)]);
        // ベースから切って、old.mdを消しnew.mdを足したブランチ
        repo.branch_from(
            &transport,
            "blog/new",
            &base,
            &[("src/content/blog/new.md", BRANCH_MD)],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let branch = &read.snapshot.branches["blog/new"];
        assert_eq!(branch.slug, "new");
        assert_eq!(branch.moved_from.as_deref(), Some("old"));
        assert_eq!(
            branch.article.as_ref().map(|article| article.md.as_str()),
            Some(blob_sha(BRANCH_MD).as_str())
        );
        assert!(read.skipped.is_empty());
    }

    #[test]
    fn 名前を変えていれば移した記事として持つ() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        let base = repo.branch(&transport, "main", &[("src/content/blog/old.md", MD)]);
        let branch_commit = repo.branch_from(
            &transport,
            "blog/new",
            &base,
            &[("src/content/blog/new.md", BRANCH_MD)],
        );
        transport.json(
            Method::Get,
            &compare_url(&base, &branch_commit),
            200,
            serde_json::json!({
                "files": [{
                    "filename": "src/content/blog/new.md",
                    "previous_filename": "src/content/blog/old.md",
                    "status": "renamed",
                }],
            }),
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let branch = &read.snapshot.branches["blog/new"];
        assert_eq!(branch.moved_from.as_deref(), Some("old"));
        assert!(read.skipped.is_empty());
    }

    #[test]
    fn ベースに記事が増える前に切ったブランチは移した記事を持たない() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        let first = repo.branch(
            &transport,
            "main",
            &[("src/content/blog/other.md", OTHER_MD)],
        );
        // other.mdだけがあるベースから切ったブランチ
        repo.branch_from(
            &transport,
            "blog/new",
            &first,
            &[
                ("src/content/blog/other.md", OTHER_MD),
                ("src/content/blog/new.md", BRANCH_MD),
            ],
        );
        // その後にベースへ記事が増える
        repo.branch_from(
            &transport,
            "main",
            &first,
            &[
                ("src/content/blog/other.md", OTHER_MD),
                ("src/content/blog/old.md", MD),
            ],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let branch = &read.snapshot.branches["blog/new"];
        assert_eq!(branch.moved_from, None);
        assert!(read.skipped.is_empty());
    }

    #[test]
    fn 直下でないmdやslugでないmdやベースに無いmdは移した記事にしない() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        let base = repo.branch(&transport, "main", &[("src/content/blog/old.md", MD)]);
        let branch_commit = repo.branch_from(
            &transport,
            "blog/new",
            &base,
            &[("src/content/blog/new.md", BRANCH_MD)],
        );
        transport.json(
            Method::Get,
            &compare_url(&base, &branch_commit),
            200,
            serde_json::json!({
                "files": [
                    // 記事のディレクトリの下のディレクトリ
                    {"filename": "src/content/blog/sub/old.md", "status": "removed"},
                    // slugとして正しくない
                    {"filename": "src/content/blog/Bad.md", "status": "removed"},
                    // ベースに無い
                    {"filename": "src/content/blog/unknown.md", "status": "removed"},
                    // mdでない
                    {"filename": "src/assets/old/01.png", "status": "removed"},
                ],
            }),
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        let branch = &read.snapshot.branches["blog/new"];
        assert_eq!(branch.moved_from, None);
        assert!(read.skipped.is_empty());
    }

    #[test]
    fn slugがベースにあるブランチと空のブランチはcompareを呼ばない() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        repo.branch(&transport, "main", &[("src/content/blog/post.md", MD)]);
        // slugがベースにある
        repo.branch(&transport, "blog/post", &[("src/content/blog/post.md", MD)]);
        // 記事の無いブランチで、slugがベースに無い
        repo.branch(
            &transport,
            "blog/draft",
            &[("src/content/blog/other.md", OTHER_MD)],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        assert!(read.snapshot.branches.contains_key("blog/post"));
        assert!(
            !request_urls(&transport)
                .iter()
                .any(|url| url.contains("/compare/")),
            "{:?}",
            request_urls(&transport)
        );
    }

    #[test]
    fn 先頭が前回と同じならcompareを呼ばず前回の移した記事を残す() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        let base = repo.branch(&transport, "main", &[("src/content/blog/old.md", MD)]);
        let branch_commit = repo.branch_from(
            &transport,
            "blog/new",
            &base,
            &[("src/content/blog/new.md", BRANCH_MD)],
        );
        // ベースだけ進む
        repo.branch_from(
            &transport,
            "main",
            &base,
            &[("src/content/blog/old.md", MD), ("README.md", OTHER_MD)],
        );
        let article = ArticleFiles {
            md: blob_sha(BRANCH_MD),
            assets: BTreeMap::new(),
        };
        let previous = RemoteSnapshot {
            base: Some(BaseSnapshot {
                commit: base.clone(),
                articles: BTreeMap::new(),
                shared_assets: Some(BTreeMap::new()),
            }),
            branches: BTreeMap::from([(
                "blog/new".to_string(),
                BranchSnapshot {
                    commit: branch_commit.clone(),
                    slug: "new".to_string(),
                    article: Some(article),
                    moved_from: Some("old".to_string()),
                },
            )]),
        };

        let read = read(&transport, &basis(), &previous).unwrap();
        let branch = &read.snapshot.branches["blog/new"];
        assert_eq!(branch.moved_from.as_deref(), Some("old"));
        assert!(
            !request_urls(&transport)
                .iter()
                .any(|url| url.contains("/compare/")),
            "{:?}",
            request_urls(&transport)
        );
    }

    #[test]
    fn compareのfilesが300件なら読めない記事にし299件なら読める() {
        for (count, readable) in [(299, true), (300, false)] {
            let transport = RouteTransport::new();
            let repo = new_repo(&transport);
            let base = repo.branch(
                &transport,
                "main",
                &[("src/content/blog/other.md", OTHER_MD)],
            );
            let branch_commit = repo.branch_from(
                &transport,
                "blog/new",
                &base,
                &[("src/content/blog/new.md", BRANCH_MD)],
            );
            transport.json(
                Method::Get,
                &compare_url(&base, &branch_commit),
                200,
                compare_files_json(count),
            );

            let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
            if readable {
                assert!(read.skipped.is_empty(), "{count}件: {:?}", read.skipped);
                assert!(read.snapshot.branches.contains_key("blog/new"), "{count}件");
            } else {
                assert_eq!(
                    skipped_pairs(&read.skipped),
                    vec![("new".to_string(), "blog/new".to_string())],
                    "{count}件"
                );
                assert!(
                    !read.snapshot.branches.contains_key("blog/new"),
                    "{count}件"
                );
            }
        }
    }

    #[test]
    fn 移した記事が2つ以上あれば読めない記事にする() {
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        let base = repo.branch(
            &transport,
            "main",
            &[
                ("src/content/blog/old-a.md", MD),
                ("src/content/blog/old-b.md", OTHER_MD),
            ],
        );
        // 2つの記事を消して1つ足したブランチ
        repo.branch_from(
            &transport,
            "blog/new",
            &base,
            &[("src/content/blog/new.md", BRANCH_MD)],
        );

        let read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        assert_eq!(
            skipped_pairs(&read.skipped),
            vec![("new".to_string(), "blog/new".to_string())]
        );
        assert!(!read.snapshot.branches.contains_key("blog/new"));
    }

    #[test]
    fn compareが404なら読めない記事にし500なら読み込みを止める() {
        // 404は、そのブランチの記事だけ読めない記事にする
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        let base = repo.branch(
            &transport,
            "main",
            &[("src/content/blog/other.md", OTHER_MD)],
        );
        let branch_commit = repo.branch_from(
            &transport,
            "blog/new",
            &base,
            &[("src/content/blog/new.md", BRANCH_MD)],
        );
        transport.json(
            Method::Get,
            &compare_url(&base, &branch_commit),
            404,
            serde_json::json!({"message": "Not Found"}),
        );

        let first_read = read(&transport, &basis(), &RemoteSnapshot::default()).unwrap();
        assert_eq!(
            skipped_pairs(&first_read.skipped),
            vec![("new".to_string(), "blog/new".to_string())]
        );
        assert!(!first_read.snapshot.branches.contains_key("blog/new"));

        // 500は読み込みを止める
        let transport = RouteTransport::new();
        let repo = new_repo(&transport);
        let base = repo.branch(
            &transport,
            "main",
            &[("src/content/blog/other.md", OTHER_MD)],
        );
        let branch_commit = repo.branch_from(
            &transport,
            "blog/new",
            &base,
            &[("src/content/blog/new.md", BRANCH_MD)],
        );
        transport.json(
            Method::Get,
            &compare_url(&base, &branch_commit),
            500,
            serde_json::json!({"message": "Server Error"}),
        );

        assert_stopped(read(&transport, &basis(), &RemoteSnapshot::default()));
    }
}
