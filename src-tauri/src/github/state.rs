use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::{fs, io};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::config::{GithubSettings, RepoRef};
use super::GithubError;

pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Basis {
    pub base_branch: String,
    pub branch_format: String,
    pub posts_path: String,
}

impl Basis {
    // Repositoryを選んでいなければNone。base_branchがNoneならrepoのdefault_branch
    pub fn of(settings: &GithubSettings) -> Option<Basis> {
        let repo = settings.repo.as_ref()?;
        Some(Basis {
            base_branch: settings
                .base_branch
                .clone()
                .unwrap_or_else(|| repo.default_branch.clone()),
            branch_format: settings.branch_format.clone(),
            posts_path: settings.posts_path.clone(),
        })
    }
}

// リモートの1記事。assetsの鍵はassets/{slug}/からの相対パス
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleFiles {
    pub md: String,
    pub assets: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseSnapshot {
    pub commit: String,
    pub articles: BTreeMap<String, ArticleFiles>,
    // assets/の直下のファイル名 -> SHA。前のバージョンのstate.jsonではNoneで、次の読み込みで取り直す
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_assets: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSnapshot {
    pub commit: String,
    pub slug: String,
    // Noneなら空のブランチ
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub article: Option<ArticleFiles>,
    // 移した記事の元のslug
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSnapshot {
    pub base: Option<BaseSnapshot>,
    pub branches: BTreeMap<String, BranchSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetRecord {
    pub sha: String,
    pub fetched: bool,
}

// 置き換えの途中の印。mdがNoneなら削除
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Applying {
    pub md: Option<String>,
    pub assets: BTreeMap<String, AssetRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Deleting {
    Delete,
    Unpublish,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub branch: Option<String>,
    pub commit: String,
    pub md: String,
    pub assets: BTreeMap<String, AssetRecord>,
    pub applying: Option<Applying>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleting: Option<Deleting>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    pub slug: String,
    pub branch: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub version: u32,
    pub basis: Basis,
    pub fetched_at: Option<i64>,
    pub remote: RemoteSnapshot,
    pub articles: BTreeMap<String, Record>,
    pub conflicts: BTreeSet<String>,
    pub skipped: Vec<Skipped>,
}

impl State {
    pub fn empty(basis: Basis) -> State {
        State {
            version: STATE_VERSION,
            basis,
            fetched_at: None,
            remote: RemoteSnapshot::default(),
            articles: BTreeMap::new(),
            conflicts: BTreeSet::new(),
            skipped: Vec::new(),
        }
    }
}

// 公開済みの記事から移した記事の、ベースにある元のslug(renamedFromか、記録のブランチが移した記事)
pub fn original_slug(state: &State, slug: &str) -> Option<String> {
    let base = state.remote.base.as_ref()?;
    if let Some(old) = state
        .articles
        .get(slug)
        .and_then(|record| record.renamed_from.as_ref())
    {
        return base.articles.contains_key(old).then(|| old.clone());
    }
    let (branch, _) = article_branch(state, slug)?;
    state.remote.branches.get(&branch)?.moved_from.clone()
}

// 記事用のブランチと、消してよいか確かめる先頭のコミット。記録にブランチが無くても、前回読み込んだ
// リモートの様子にあればそれを使う(手元と食い違って記録を作らなかった記事)
pub fn article_branch(state: &State, slug: &str) -> Option<(String, String)> {
    if let Some(record) = state.articles.get(slug) {
        if let Some(branch) = &record.branch {
            return Some((branch.clone(), record.commit.clone()));
        }
    }
    state
        .remote
        .branches
        .iter()
        .find(|(_, branch)| branch.slug == slug)
        .map(|(name, branch)| (name.clone(), branch.commit.clone()))
}

pub(crate) fn repo_dir(data_dir: &Path, repo: &RepoRef) -> PathBuf {
    data_dir.join("github").join(&repo.owner).join(&repo.name)
}

pub fn state_path(data_dir: &Path, repo: &RepoRef) -> PathBuf {
    repo_dir(data_dir, repo).join("state.json")
}

pub fn synced_path(data_dir: &Path, repo: &RepoRef, slug: &str) -> PathBuf {
    repo_dir(data_dir, repo)
        .join("synced")
        .join(format!("{slug}.md"))
}

// Pullで手元のファイルを写し置く場所の根(backup/{日時}/…)
pub(crate) fn backup_root(data_dir: &Path, repo: &RepoRef) -> PathBuf {
    repo_dir(data_dir, repo).join("backup")
}

// 無ければState::empty。versionが違う・読めなければStateUnreadable。basisが違えばState::empty(basis)
pub fn read_state(path: &Path, basis: &Basis) -> Result<State, GithubError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(State::empty(basis.clone()))
        }
        Err(error) => return Err(GithubError::io(path, error)),
    };
    let mut state: State =
        serde_json::from_slice(&bytes).map_err(|_| GithubError::StateUnreadable)?;
    if state.version != STATE_VERSION {
        return Err(GithubError::StateUnreadable);
    }
    // 記録とリモートの様子はbasisごとに結び付くので、替わったら引き継がない
    if state.basis != *basis {
        state = State::empty(basis.clone());
    }
    Ok(state)
}

pub fn write_state(path: &Path, state: &State) -> Result<(), GithubError> {
    write_replacing(
        path,
        &serde_json::to_vec(state).expect("State should serialize"),
    )
}

// 一時ファイルに書いてから置き換える。ディレクトリが無ければ作る
pub(crate) fn write_replacing(path: &Path, json: &[u8]) -> Result<(), GithubError> {
    let dir = path
        .parent()
        .ok_or_else(|| GithubError::io(path, "no parent directory"))?;
    fs::create_dir_all(dir).map_err(|error| GithubError::io(dir, error))?;
    let mut tmp =
        tempfile::NamedTempFile::new_in(dir).map_err(|error| GithubError::io(dir, error))?;
    tmp.write_all(json)
        .map_err(|error| GithubError::io(tmp.path(), error))?;
    tmp.as_file()
        .sync_all()
        .map_err(|error| GithubError::io(tmp.path(), error))?;
    tmp.persist(path)
        .map_err(|error| GithubError::io(path, error.error))?;
    Ok(())
}

// "blob {len}\0{内容}"のSHA-1を16進の小文字で
pub fn blob_sha(content: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(b"blob ");
    hasher.update(content.len().to_string().as_bytes());
    hasher.update([0]);
    hasher.update(content);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// 手元の1記事のSHAと中身。中身はCommitでそのまま送る(push.rs)
#[derive(Debug, Clone, Default)]
pub(crate) struct LocalBodies {
    // (SHA, 中身)
    pub md: Option<(String, Vec<u8>)>,
    // assets/{slug}/の直下のファイル
    pub assets: BTreeMap<String, (String, Vec<u8>)>,
}

impl LocalBodies {
    pub(crate) fn shas(&self) -> LocalFiles {
        LocalFiles {
            md: self.md.as_ref().map(|(sha, _)| sha.clone()),
            assets: self
                .assets
                .iter()
                .map(|(name, (sha, _))| (name.clone(), sha.clone()))
                .collect(),
        }
    }
}

pub(crate) fn local_bodies(
    posts_dir: &Path,
    assets_base: &Path,
    slug: &str,
) -> Result<LocalBodies, GithubError> {
    let md_path = posts_dir.join(format!("{slug}.md"));
    let md = match fs::read(&md_path) {
        Ok(content) => Some((blob_sha(&content), content)),
        // mdが無い記事もある。ほかの読み取り失敗は止める
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(GithubError::io(&md_path, error)),
    };

    let dir = assets_base.join(slug);
    let mut assets = BTreeMap::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LocalBodies { md, assets })
        }
        Err(error) => return Err(GithubError::io(&dir, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| GithubError::io(&dir, error))?;
        // 下のディレクトリのファイルは、保存層も画面も扱わないので見ない
        if !entry
            .file_type()
            .map_err(|error| GithubError::io(&dir, error))?
            .is_file()
        {
            continue;
        }
        let path = entry.path();
        let content = fs::read(&path).map_err(|error| GithubError::io(&path, error))?;
        assets.insert(
            entry.file_name().to_string_lossy().to_string(),
            (blob_sha(&content), content),
        );
    }
    Ok(LocalBodies { md, assets })
}

// 手元の1記事のSHA。mdが無ければNone。assetsはassets_base/{slug}/の直下のファイルだけ
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalFiles {
    pub md: Option<String>,
    pub assets: BTreeMap<String, String>,
}

impl LocalFiles {
    pub fn is_empty(&self) -> bool {
        self.md.is_none() && self.assets.is_empty()
    }
}

pub fn local_files(
    posts_dir: &Path,
    assets_base: &Path,
    slug: &str,
) -> Result<LocalFiles, GithubError> {
    Ok(local_bodies(posts_dir, assets_base, slug)?.shas())
}

// 手元に変更があればtrue。記録が無い記事は常にtrue
pub fn is_dirty(record: Option<&Record>, local: &LocalFiles) -> bool {
    let Some(record) = record else {
        return true;
    };
    if !md_matches(record, local) {
        return true;
    }
    // 置き換えている最中は、古い記録かapplyingのどちらかに合うなら混ざっていても変更ではない
    let applying = record.applying.as_ref();
    for (name, sha) in &local.assets {
        let has_sha = |entry: Option<&AssetRecord>| entry.is_some_and(|asset| asset.sha == *sha);
        if !has_sha(record.assets.get(name))
            && !has_sha(applying.and_then(|applying| applying.assets.get(name)))
        {
            return true;
        }
    }
    if applying.is_some() {
        // 置き換え・削除の途中の足りないファイルは、消す予定のものなので変更ではない
        return false;
    }
    for (name, asset) in &record.assets {
        // 取ってきた画像が手元に無い
        if asset.fetched && !local.assets.contains_key(name) {
            return true;
        }
    }
    false
}

// mdは、記録のmdと同じか、applyingの印と同じなら一致にする(置き換えの途中の混ざった状態を変更と見なさない)
fn md_matches(record: &Record, local: &LocalFiles) -> bool {
    if local.md.as_deref() == Some(record.md.as_str()) {
        return true;
    }
    match (&record.applying, &local.md) {
        (
            Some(Applying {
                md: Some(applied), ..
            }),
            Some(sha),
        ) => sha == applied,
        // 削除の途中(mdはapplyingで無い扱い)
        (Some(Applying { md: None, .. }), None) => true,
        _ => false,
    }
}

// 手元にある画像がすべてリモートと同じSHA。リモートにあって手元に無い画像は、
// 記録が取ってきていないものだけ許す(記録が無ければ、手元に無い画像は同じと見なさない)
pub fn same_as_remote(record: Option<&Record>, local: &LocalFiles, remote: &ArticleFiles) -> bool {
    if local.md.as_deref() != Some(remote.md.as_str()) {
        return false;
    }
    for (name, sha) in &local.assets {
        if remote.assets.get(name) != Some(sha) {
            return false;
        }
    }
    let not_fetched =
        |record: &Record, name: &str| record.assets.get(name).is_none_or(|asset| !asset.fetched);
    let mut wanted =
        crate::store::fetchable_names(remote.assets.keys().map(String::as_str)).into_iter();
    match record {
        None => wanted.all(|name| local.assets.contains_key(name)),
        Some(record) => {
            wanted.all(|name| local.assets.contains_key(name) || not_fetched(record, name))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    Drafting,
    Editing,
    Published,
}

pub fn category(remote: &RemoteSnapshot, slug: &str, dirty: bool) -> Category {
    let Some(base) = remote.base.as_ref() else {
        return Category::Drafting;
    };
    let Some(base_files) = base.articles.get(slug) else {
        // ベースに無い記事は、ほかの記事を移した記事用のブランチの記事か、dirtyなら編集中。
        // 呼び出し側は、公開済みの記事からslugを変えた記事だけdirtyにする
        let moved = remote
            .branches
            .values()
            .any(|branch| branch.slug == slug && branch.moved_from.is_some());
        return if moved || dirty {
            Category::Editing
        } else {
            Category::Drafting
        };
    };
    // 記事用のブランチの内容がベースと違えば編集中。コミットは比べない
    let branch_differs = remote
        .branches
        .values()
        .any(|branch| branch.slug == slug && branch.article.as_ref() != Some(base_files));
    if dirty || branch_differs {
        Category::Editing
    } else {
        Category::Published
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    #[cfg(unix)]
    use crate::github::fake::read_only_dir;

    fn basis() -> Basis {
        Basis {
            base_branch: "main".to_string(),
            branch_format: "blog/{slug}".to_string(),
            posts_path: "src/content/blog".to_string(),
        }
    }

    fn sha_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, sha)| (name.to_string(), sha.to_string()))
            .collect()
    }

    fn asset_map(pairs: &[(&str, AssetRecord)]) -> BTreeMap<String, AssetRecord> {
        pairs
            .iter()
            .map(|(name, record)| (name.to_string(), record.clone()))
            .collect()
    }

    fn files_of(md: &str, assets: &[(&str, &str)]) -> ArticleFiles {
        ArticleFiles {
            md: md.to_string(),
            assets: sha_map(assets),
        }
    }

    fn asset(sha: &str, fetched: bool) -> AssetRecord {
        AssetRecord {
            sha: sha.to_string(),
            fetched,
        }
    }

    fn record_of(branch: Option<&str>, assets: &[(&str, AssetRecord)]) -> Record {
        Record {
            branch: branch.map(str::to_string),
            commit: "commit-1".to_string(),
            md: "sha-md-a".to_string(),
            assets: asset_map(assets),
            applying: None,
            renamed_from: None,
            deleting: None,
        }
    }

    fn local_of(md: Option<&str>, assets: &[(&str, &str)]) -> LocalFiles {
        LocalFiles {
            md: md.map(str::to_string),
            assets: sha_map(assets),
        }
    }

    fn applying_of(md: &str, assets: &[(&str, AssetRecord)]) -> Applying {
        Applying {
            md: Some(md.to_string()),
            assets: asset_map(assets),
        }
    }

    #[test]
    fn blob_shaはgitのblobのSHAになる() {
        assert_eq!(
            blob_sha(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        assert_eq!(blob_sha(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn state_jsonが無ければ空の状態になる() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("github/owner/blog/state.json");
        let state = read_state(&path, &basis()).unwrap();
        assert_eq!(state, State::empty(basis()));
        assert_eq!(state.version, STATE_VERSION);
    }

    #[test]
    fn 読み書きの往復() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("github/owner/blog/state.json");
        let mut state = State::empty(basis());
        state.fetched_at = Some(1_700_000_000);
        let article = files_of("sha-md-a", &[("01.png", "sha-png-a")]);
        state.remote.base = Some(BaseSnapshot {
            commit: "commit-base".to_string(),
            articles: BTreeMap::from([("post".to_string(), article.clone())]),
            shared_assets: Some(BTreeMap::from([(
                "shared.png".to_string(),
                "sha-shared".to_string(),
            )])),
        });
        state.remote.branches = BTreeMap::from([(
            "blog/post".to_string(),
            BranchSnapshot {
                commit: "commit-branch".to_string(),
                slug: "post".to_string(),
                article: Some(article.clone()),
                moved_from: None,
            },
        )]);
        let mut record = record_of(Some("blog/post"), &[("01.png", asset("sha-png-a", true))]);
        record.applying = Some(applying_of(
            "sha-md-b",
            &[("01.png", asset("sha-png-a", true))],
        ));
        state.articles = BTreeMap::from([("post".to_string(), record)]);
        state.conflicts = BTreeSet::from(["other".to_string()]);
        state.skipped = vec![Skipped {
            slug: "skip".to_string(),
            branch: "blog/skip".to_string(),
            reason: "unreadable".to_string(),
        }];

        write_state(&path, &state).unwrap();
        assert_eq!(read_state(&path, &basis()).unwrap(), state);

        // 項目名はcamelCaseのJSONにする
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(value["basis"]["baseBranch"], "main");
        assert_eq!(value["fetchedAt"], 1_700_000_000);
        assert_eq!(value["articles"]["post"]["applying"]["md"], "sha-md-b");
        assert_eq!(
            value["articles"]["post"]["assets"]["01.png"]["fetched"],
            true
        );
        // 共有の画像の一覧も鍵はsharedAssetsで、版番号は1のまま
        assert_eq!(
            value["remote"]["base"]["sharedAssets"]["shared.png"],
            "sha-shared"
        );
        assert_eq!(value["version"], 1);
    }

    #[test]
    fn versionが1でなければStateUnreadable() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        fs::write(
            &path,
            r#"{"version":2,"basis":{"baseBranch":"main","branchFormat":"blog/{slug}","postsPath":"src/content/blog"},"fetchedAt":null,"remote":{"base":null,"branches":{}},"articles":{},"conflicts":[],"skipped":[]}"#,
        )
        .unwrap();
        assert!(matches!(
            read_state(&path, &basis()),
            Err(GithubError::StateUnreadable)
        ));
    }

    #[test]
    fn jsonとして読めなければStateUnreadable() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        fs::write(&path, "{").unwrap();
        assert!(matches!(
            read_state(&path, &basis()),
            Err(GithubError::StateUnreadable)
        ));
    }

    #[test]
    fn basisが今の設定と違えば記録もリモートの様子も無い扱い() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        let mut state = State::empty(basis());
        state.remote.base = Some(BaseSnapshot {
            commit: "commit-base".to_string(),
            articles: BTreeMap::from([("post".to_string(), files_of("sha-md-a", &[]))]),
            shared_assets: None,
        });
        state
            .articles
            .insert("post".to_string(), record_of(Some("blog/post"), &[]));
        write_state(&path, &state).unwrap();

        for changed in [
            Basis {
                base_branch: "dev".to_string(),
                ..basis()
            },
            Basis {
                branch_format: "{slug}".to_string(),
                ..basis()
            },
            Basis {
                posts_path: "src/content/posts".to_string(),
                ..basis()
            },
        ] {
            let read = read_state(&path, &changed).unwrap();
            assert_eq!(read.basis, changed);
            // 記録とリモートの様子は引き継がない
            assert!(read.articles.is_empty());
            assert!(read.remote.base.is_none());
            assert!(read.remote.branches.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn 書くときに一時ファイルから置き換える() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        fs::write(&path, "old").unwrap();
        let Some(_read_only) = read_only_dir(tmp.path()) else {
            return;
        };
        let result = write_state(&path, &State::empty(basis()));
        // 読み出しだけならread-onlyでもできる
        let original = fs::read_to_string(&path).unwrap();
        drop(_read_only);
        assert!(result.is_err());
        assert_eq!(original, "old");
    }

    #[test]
    fn 記録と同じなら変更無し() {
        let record = record_of(None, &[("01.png", asset("sha-png-a", true))]);
        let local = local_of(Some("sha-md-a"), &[("01.png", "sha-png-a")]);
        assert!(!is_dirty(Some(&record), &local));
    }

    #[test]
    fn mdのSHAが違えば変更あり() {
        let record = record_of(None, &[("01.png", asset("sha-png-a", true))]);
        let local = local_of(Some("sha-md-b"), &[("01.png", "sha-png-a")]);
        assert!(is_dirty(Some(&record), &local));
    }

    #[test]
    fn mdが無ければ変更あり() {
        let record = record_of(None, &[("01.png", asset("sha-png-a", true))]);
        let local = local_of(None, &[("01.png", "sha-png-a")]);
        assert!(is_dirty(Some(&record), &local));
    }

    #[test]
    fn 記録に無い画像があれば変更あり() {
        let record = record_of(None, &[("01.png", asset("sha-png-a", true))]);
        let local = local_of(
            Some("sha-md-a"),
            &[("01.png", "sha-png-a"), ("02.png", "sha-png-b")],
        );
        assert!(is_dirty(Some(&record), &local));
    }

    #[test]
    fn 画像のSHAが違えば変更あり() {
        let record = record_of(None, &[("01.png", asset("sha-png-a", true))]);
        let local = local_of(Some("sha-md-a"), &[("01.png", "sha-png-b")]);
        assert!(is_dirty(Some(&record), &local));
    }

    #[test]
    fn 取ってきた画像が無ければ変更あり() {
        let record = record_of(None, &[("01.png", asset("sha-png-a", true))]);
        let local = local_of(Some("sha-md-a"), &[]);
        assert!(is_dirty(Some(&record), &local));
    }

    #[test]
    fn 取ってきていない画像が無いのは変更無し() {
        let record = record_of(None, &[("01.png", asset("sha-png-a", false))]);
        let local = local_of(Some("sha-md-a"), &[]);
        assert!(!is_dirty(Some(&record), &local));
    }

    #[test]
    fn 取ってきていない画像が手元にありSHAが記録と違えば変更あり() {
        let record = record_of(None, &[("01.png", asset("sha-png-a", false))]);
        let local = local_of(Some("sha-md-a"), &[("01.png", "sha-png-b")]);
        assert!(is_dirty(Some(&record), &local));
    }

    #[test]
    fn 記録が無ければ変更あり() {
        assert!(is_dirty(None, &local_of(None, &[])));
        assert!(is_dirty(
            None,
            &local_of(Some("sha-md-a"), &[("01.png", "sha-png-a")])
        ));
    }

    #[test]
    fn assetsの下のディレクトリは見ない() {
        let tmp = TempDir::new().unwrap();
        let posts_dir = tmp.path().join("src/content/blog");
        let assets_base = tmp.path().join("src/assets");
        fs::create_dir_all(&posts_dir).unwrap();
        fs::create_dir_all(assets_base.join("post/sub")).unwrap();
        fs::write(posts_dir.join("post.md"), b"hello\n").unwrap();
        fs::write(assets_base.join("post/01.png"), b"old").unwrap();
        fs::write(assets_base.join("post/sub/02.png"), b"old").unwrap();

        let local = local_files(&posts_dir, &assets_base, "post").unwrap();
        assert!(local.md.is_some());
        // 直下のファイルだけを見る
        assert!(local.assets.contains_key("01.png"));
        assert!(!local.assets.contains_key("sub/02.png"));
    }

    #[test]
    fn 手元がリモートと同じかの判定() {
        let remote = files_of(
            "sha-md-r",
            &[("01.png", "sha-png-r"), ("02.png", "sha-png-r2")],
        );
        // mdと手元にある画像が同じで、足りない画像が取ってきていないものだけなら同じ
        let record = record_of(
            None,
            &[
                ("01.png", asset("sha-png-r", true)),
                ("02.png", asset("sha-png-r2", false)),
            ],
        );
        let local = local_of(Some("sha-md-r"), &[("01.png", "sha-png-r")]);
        assert!(same_as_remote(Some(&record), &local, &remote));
        // 記録が無ければ、手元に無い画像は同じと見なさない
        let local = local_of(Some("sha-md-r"), &[("01.png", "sha-png-r")]);
        assert!(!same_as_remote(None, &local, &remote));
        // 画像を全部持っていれば記録が無くても同じ
        let full = local_of(
            Some("sha-md-r"),
            &[("01.png", "sha-png-r"), ("02.png", "sha-png-r2")],
        );
        assert!(same_as_remote(None, &full, &remote));
        // mdが違えば同じではない
        let other_md = local_of(Some("sha-md-x"), &[]);
        assert!(!same_as_remote(None, &other_md, &remote));
        // 画像のSHAが違えば同じではない
        let other_png = local_of(Some("sha-md-r"), &[("01.png", "sha-png-x")]);
        assert!(!same_as_remote(None, &other_png, &remote));
        // リモートに無い画像が手元にあれば同じではない
        let unknown = record_of(None, &[("01.png", asset("sha-png-r", true))]);
        assert!(!same_as_remote(
            Some(&unknown),
            &local_of(
                Some("sha-md-r"),
                &[("01.png", "sha-png-r"), ("03.png", "sha-png-x")]
            ),
            &remote
        ));
    }

    #[test]
    fn 大文字小文字だけが違う名前の画像は手元に無くてもリモートと同じで変更でもない() {
        let remote = files_of(
            "sha-md-a",
            &[
                ("A.png", "sha-png-1"),
                ("a.png", "sha-png-2"),
                ("01.png", "sha-png-r"),
            ],
        );
        let local = local_of(Some("sha-md-a"), &[("01.png", "sha-png-r")]);
        assert!(same_as_remote(None, &local, &remote));
        let record = record_of(
            None,
            &[
                ("A.png", asset("sha-png-1", false)),
                ("a.png", asset("sha-png-2", false)),
                ("01.png", asset("sha-png-r", true)),
            ],
        );
        assert!(same_as_remote(Some(&record), &local, &remote));
        // 取ってこなかった画像はfetchedがfalseのままなので、Commitで消すものにならない
        assert!(!is_dirty(Some(&record), &local));
    }

    #[test]
    fn applyingのある記録は古い記録とapplyingの混ざった手元を変更無しにする() {
        // mdだけ新しい
        let mut record = record_of(None, &[("01.png", asset("sha-png-a", true))]);
        record.applying = Some(Applying {
            md: Some("sha-md-b".to_string()),
            assets: BTreeMap::from([("01.png".to_string(), asset("sha-png-a", true))]),
        });
        let local = local_of(Some("sha-md-b"), &[("01.png", "sha-png-a")]);
        assert!(!is_dirty(Some(&record), &local));

        // mdが古い記録のままでもよい
        let local = local_of(Some("sha-md-a"), &[("01.png", "sha-png-a")]);
        assert!(!is_dirty(Some(&record), &local));

        // 画像だけ新しい。取ってきていない画像がapplyingにあれば、手元に無くてもよい
        let mut record = record_of(
            None,
            &[
                ("01.png", asset("sha-png-a", true)),
                ("02.png", asset("sha-png-b", true)),
            ],
        );
        record.applying = Some(applying_of(
            "sha-md-a",
            &[
                ("01.png", asset("sha-png-c", true)),
                ("03.png", asset("sha-png-d", false)),
            ],
        ));
        let local = local_of(
            Some("sha-md-a"),
            &[("01.png", "sha-png-c"), ("02.png", "sha-png-b")],
        );
        assert!(!is_dirty(Some(&record), &local));
        // 削除した画像が消えていても、applyingに無ければよい
        let local = local_of(
            Some("sha-md-a"),
            &[("01.png", "sha-png-c"), ("03.png", "sha-png-d")],
        );
        assert!(!is_dirty(Some(&record), &local));
        // applyingがある間は、足りないファイルを変更と見なさない
        let local = local_of(Some("sha-md-a"), &[("02.png", "sha-png-b")]);
        assert!(!is_dirty(Some(&record), &local));
    }

    #[test]
    fn applyingのある記録でもどちらとも違うファイルがあれば変更あり() {
        let mut record = record_of(None, &[("01.png", asset("sha-png-a", true))]);
        record.applying = Some(applying_of(
            "sha-md-b",
            &[("01.png", asset("sha-png-c", true))],
        ));
        // mdもapplyingとも記録とも違う
        let local = local_of(Some("sha-md-x"), &[("01.png", "sha-png-a")]);
        assert!(is_dirty(Some(&record), &local));
        // 画像がどちらとも違う
        let local = local_of(Some("sha-md-b"), &[("01.png", "sha-png-x")]);
        assert!(is_dirty(Some(&record), &local));
        // 置き換え(applyingのmdがSome)の途中でmdが無ければ変更あり
        let local = local_of(None, &[("01.png", "sha-png-c")]);
        assert!(is_dirty(Some(&record), &local));
    }

    #[test]
    fn sharedAssetsの無いstate_jsonを読める() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        fs::write(
            &path,
            r#"{"version":1,"basis":{"baseBranch":"main","branchFormat":"blog/{slug}","postsPath":"src/content/blog"},"fetchedAt":null,"remote":{"base":{"commit":"commit-base","articles":{}},"branches":{}},"articles":{},"conflicts":[],"skipped":[]}"#,
        )
        .unwrap();

        let state = read_state(&path, &basis()).unwrap();
        let base = state.remote.base.as_ref().expect("ベースの様子はあるはず");
        assert_eq!(base.shared_assets, None);
    }

    #[test]
    fn renamedFromの無い古いstate_jsonを読める() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        fs::write(
            &path,
            r#"{"version":1,"basis":{"baseBranch":"main","branchFormat":"blog/{slug}","postsPath":"src/content/blog"},"fetchedAt":null,"remote":{"base":null,"branches":{}},"articles":{"post":{"branch":null,"commit":"commit-1","md":"sha-md-a","assets":{},"applying":null}},"conflicts":[],"skipped":[]}"#,
        )
        .unwrap();

        let state = read_state(&path, &basis()).unwrap();
        let record = &state.articles["post"];
        assert_eq!(record.commit, "commit-1");
        assert_eq!(record.renamed_from, None);
    }

    #[test]
    fn renamedFromは読み書きの往復で残り鍵はrenamedFrom() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        let mut state = State::empty(basis());
        let mut record = record_of(Some("blog/new"), &[]);
        record.renamed_from = Some("old".to_string());
        state.articles = BTreeMap::from([("new".to_string(), record.clone())]);

        write_state(&path, &state).unwrap();
        assert_eq!(read_state(&path, &basis()).unwrap(), state);

        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(value["articles"]["new"]["renamedFrom"], "old");
        // 無いときは鍵を出さない
        let mut plain = record;
        plain.renamed_from = None;
        assert!(
            serde_json::to_value(&plain).unwrap()["renamedFrom"]
                .as_str()
                .is_none(),
            "renamedFromの鍵は無いはず"
        );
    }

    #[test]
    fn 記事を三つの分類に振り分ける() {
        let article = files_of("sha-md-a", &[]);
        let remote = RemoteSnapshot {
            base: Some(BaseSnapshot {
                commit: "commit-base".to_string(),
                articles: BTreeMap::from([
                    ("post".to_string(), article.clone()),
                    ("only-base".to_string(), article.clone()),
                    ("other".to_string(), article.clone()),
                ]),
                shared_assets: None,
            }),
            branches: BTreeMap::from([
                (
                    "blog/post".to_string(),
                    BranchSnapshot {
                        commit: "commit-branch".to_string(),
                        slug: "post".to_string(),
                        article: Some(article.clone()),
                        moved_from: None,
                    },
                ),
                (
                    "blog/other".to_string(),
                    BranchSnapshot {
                        commit: "commit-branch".to_string(),
                        slug: "other".to_string(),
                        // ブランチの内容がベースと違う
                        article: Some(files_of("sha-md-b", &[])),
                        moved_from: None,
                    },
                ),
                (
                    "blog/draft".to_string(),
                    BranchSnapshot {
                        commit: "commit-branch".to_string(),
                        slug: "draft".to_string(),
                        article: Some(article.clone()),
                        moved_from: None,
                    },
                ),
            ]),
        };

        // ブランチがあっても内容がベースと同じなら公開済み
        assert_eq!(category(&remote, "post", false), Category::Published);
        // ブランチが無くても変更が無ければ公開済み
        assert_eq!(category(&remote, "only-base", false), Category::Published);
        // ブランチの内容がベースと違えば編集中
        assert_eq!(category(&remote, "other", false), Category::Editing);
        // 手元に変更があればベースにあっても編集中
        assert_eq!(category(&remote, "post", true), Category::Editing);
        assert_eq!(category(&remote, "only-base", true), Category::Editing);
        // ベースに無い記事は執筆中
        assert_eq!(category(&remote, "draft", false), Category::Drafting);
        assert_eq!(category(&remote, "unknown", false), Category::Drafting);
        // 連携の記録が無ければ全部執筆中
        let empty = RemoteSnapshot::default();
        assert_eq!(category(&empty, "post", false), Category::Drafting);
    }

    #[test]
    fn 空のブランチがありベースに記事があれば編集中() {
        let remote = RemoteSnapshot {
            base: Some(BaseSnapshot {
                commit: "commit-base".to_string(),
                articles: BTreeMap::from([("post".to_string(), files_of("sha-md-a", &[]))]),
                shared_assets: None,
            }),
            branches: BTreeMap::from([(
                "blog/post".to_string(),
                BranchSnapshot {
                    commit: "commit-branch".to_string(),
                    slug: "post".to_string(),
                    article: None,
                    moved_from: None,
                },
            )]),
        };

        assert_eq!(category(&remote, "post", false), Category::Editing);
    }

    #[test]
    fn 記事用のブランチが移した記事を持てばベースに無くても編集中() {
        let remote = RemoteSnapshot {
            base: Some(BaseSnapshot {
                commit: "commit-base".to_string(),
                articles: BTreeMap::from([("old".to_string(), files_of("sha-md-a", &[]))]),
                shared_assets: None,
            }),
            branches: BTreeMap::from([(
                "blog/new".to_string(),
                BranchSnapshot {
                    commit: "commit-branch".to_string(),
                    slug: "new".to_string(),
                    article: Some(files_of("sha-md-b", &[])),
                    moved_from: Some("old".to_string()),
                },
            )]),
        };

        assert_eq!(category(&remote, "new", false), Category::Editing);
    }

    #[test]
    fn renamedFromの元のslugがベースにあり記録のブランチが無い記事は編集中() {
        let remote = RemoteSnapshot {
            base: Some(BaseSnapshot {
                commit: "commit-base".to_string(),
                articles: BTreeMap::from([("old".to_string(), files_of("sha-md-a", &[]))]),
                shared_assets: None,
            }),
            branches: BTreeMap::new(),
        };

        // renamedFromのある記事は、手元に変更があるものとして分類する
        assert_eq!(category(&remote, "new", true), Category::Editing);
    }

    #[test]
    fn 前のバージョンのstate_jsonのarticleをオブジェクトとして読める() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        fs::write(
            &path,
            r#"{"version":1,"basis":{"baseBranch":"main","branchFormat":"blog/{slug}","postsPath":"src/content/blog"},"fetchedAt":null,"remote":{"base":null,"branches":{"blog/post":{"commit":"commit-1","slug":"post","article":{"md":"sha-md-a","assets":{}}}}},"articles":{"post":{"branch":"blog/post","commit":"commit-1","md":"sha-md-a","assets":{},"applying":null}},"conflicts":[],"skipped":[]}"#,
        )
        .unwrap();

        let state = read_state(&path, &basis()).unwrap();
        let branch = &state.remote.branches["blog/post"];
        assert_eq!(branch.article, Some(files_of("sha-md-a", &[])));
        assert_eq!(branch.moved_from, None);
        assert_eq!(state.articles["post"].deleting, None);
    }

    #[test]
    fn 空のブランチと移した記事と削除予定の印は読み書きで残る() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        let mut state = State::empty(basis());
        state.remote.branches = BTreeMap::from([
            (
                "blog/empty".to_string(),
                BranchSnapshot {
                    commit: "commit-empty".to_string(),
                    slug: "empty".to_string(),
                    article: None,
                    moved_from: None,
                },
            ),
            (
                "blog/new".to_string(),
                BranchSnapshot {
                    commit: "commit-new".to_string(),
                    slug: "new".to_string(),
                    article: Some(files_of("sha-md-b", &[])),
                    moved_from: Some("old".to_string()),
                },
            ),
        ]);
        let mut record = record_of(Some("blog/new"), &[]);
        record.deleting = Some(Deleting::Unpublish);
        state.articles = BTreeMap::from([("new".to_string(), record)]);

        write_state(&path, &state).unwrap();
        assert_eq!(read_state(&path, &basis()).unwrap(), state);

        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(value["remote"]["branches"]["blog/new"]["movedFrom"], "old");
        assert_eq!(value["articles"]["new"]["deleting"], "unpublish");
        assert!(
            value["remote"]["branches"]["blog/empty"]
                .get("article")
                .is_none(),
            "空のブランチのarticleの鍵は無いはず"
        );
    }

    #[test]
    fn articleとmovedFromとdeletingがNoneならstate_jsonに鍵を出さない() {
        let branch = BranchSnapshot {
            commit: "commit-1".to_string(),
            slug: "post".to_string(),
            article: None,
            moved_from: None,
        };
        let value = serde_json::to_value(&branch).unwrap();
        assert!(value.get("article").is_none(), "{value}");
        assert!(value.get("movedFrom").is_none(), "{value}");

        let record = record_of(Some("blog/post"), &[]);
        assert_eq!(record.deleting, None);
        let value = serde_json::to_value(&record).unwrap();
        assert!(value.get("deleting").is_none(), "{value}");
    }
}
