use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

use super::config;
use super::dates;
use super::git::{self, NewTreeEntry};
use super::remote;
use super::state::{self, ArticleFiles, AssetRecord, Basis, BranchSnapshot, LocalBodies, Record};
use super::sync::{self, SyncInput};
use super::GithubError;

// PATCHは4回まで(作り直しは3回まで。超えたらPushRetryExceeded)
const RETRY_LIMIT: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommitKind {
    Add,
    Fix,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitPreview {
    pub title: String,
    pub md_name: String,
    pub old_md_name: Option<String>,
    pub old_branch: Option<String>,
    pub added: usize,
    pub removed: usize,
    pub images_added: usize,
    pub images_removed: usize,
    pub kind: CommitKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResult {
    pub branch: String,
    pub commit: String,
    pub kept_branch: Option<String>,
}

// 最初のCheckingは画面が出す。ここから送るのは作り直すときだけ
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "step", rename_all = "camelCase")]
pub enum PushProgress {
    Checking,
    Uploading { current: usize, total: usize },
    Committing,
    Pushing,
}

// SHAと、送る中身(SHAを求めるときに読んだもの。読んでから送るまでに替わっても、記録と送った中身を合わせる)
type Content = (String, Vec<u8>);

// 手元と記録の違い
#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalDiff {
    // 送るmd。Noneはmdが手元に無いか、記録と中身が同じ
    md: Option<Content>,
    // 替えた・足した画像
    added: BTreeMap<String, Content>,
    // 手元から消した画像(取ってきてあったもの)
    removed: BTreeSet<String>,
}

impl LocalDiff {
    fn empty(&self) -> bool {
        self.md.is_none() && self.added.is_empty() && self.removed.is_empty()
    }
}

// 記録(無ければ空の記録)と手元のファイルを比べて、替えた・足した・消したファイルを集める
fn local_diff(record: Option<&Record>, local: &LocalBodies) -> LocalDiff {
    let md = match (&local.md, record) {
        (None, _) => None,
        // 記録の無い記事は、mdの中身をすべて送る
        (Some(content), None) => Some(content.clone()),
        (Some(content), Some(record)) => (record.md != content.0).then(|| content.clone()),
    };
    let Some(record) = record else {
        // 記録の無い記事は、手元の画像をすべて送る
        return LocalDiff {
            md,
            added: local.assets.clone(),
            removed: BTreeSet::new(),
        };
    };
    let mut added = BTreeMap::new();
    for (name, content) in &local.assets {
        let same = record
            .assets
            .get(name)
            .is_some_and(|asset| asset.sha == content.0);
        if !same {
            added.insert(name.clone(), content.clone());
        }
    }
    let mut removed = BTreeSet::new();
    for (name, asset) in &record.assets {
        if asset.fetched && !local.assets.contains_key(name) {
            removed.insert(name.clone());
        }
    }
    LocalDiff { md, added, removed }
}

// 手元のmdのフロントマターのtitle(読めなければ空)
fn title_of(content: &[u8]) -> String {
    crate::store::parse_md("", &String::from_utf8_lossy(content))
        .fields
        .title
        .unwrap_or_default()
}

// synced(無ければ空)と手元のmdの行単位の差分の行数。(追加, 削除)
fn count_lines(synced: &[u8], local: &[u8]) -> (usize, usize) {
    let (Ok(local), Ok(synced)) = (std::str::from_utf8(local), std::str::from_utf8(synced)) else {
        // UTF-8でないmdは行に分けられないので、全体を1行として数える
        return (
            usize::from(!local.is_empty()),
            usize::from(!synced.is_empty()),
        );
    };
    let diff = TextDiff::from_lines(synced, local);
    let mut counts = (0, 0);
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => counts.0 += 1,
            ChangeTag::Delete => counts.1 += 1,
            ChangeTag::Equal => {}
        }
    }
    counts
}

// preview: 手元と記録の違いの概略。通信しない。手元のmdのSHAを求めるときの読みで済ませる
pub fn preview(input: &SyncInput<'_>, slug: &str) -> Result<CommitPreview, GithubError> {
    let record = state::read_state(&state::state_path(input.data_dir, input.repo), input.basis)?
        .articles
        .get(slug)
        .cloned();
    let local = input
        .store
        .with_lock(|posts_dir, assets_base| state::local_bodies(posts_dir, assets_base, slug))?;
    let Some(md) = &local.md else {
        // 手元にmdが無い記事はPushできない
        return Err(GithubError::NothingToCommit);
    };
    let diff = local_diff(record.as_ref(), &local);
    let renamed_from = record
        .as_ref()
        .and_then(|record| record.renamed_from.clone());
    if diff.empty() && renamed_from.is_none() {
        return Err(GithubError::NothingToCommit);
    }
    let kind = match &record {
        None => CommitKind::Add,
        Some(record) if record.branch.is_none() => CommitKind::Fix,
        Some(_) => CommitKind::Update,
    };
    let synced_path = state::synced_path(input.data_dir, input.repo, slug);
    let synced = match std::fs::read(&synced_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(GithubError::io(&synced_path, error)),
    };
    let (added, removed) = count_lines(&synced, &md.1);
    let replaced = diff
        .added
        .keys()
        .filter(|name| {
            record
                .as_ref()
                .is_some_and(|record| record.assets.contains_key(*name))
        })
        .count();
    Ok(CommitPreview {
        title: title_of(&md.1),
        md_name: format!("{slug}.md"),
        old_branch: renamed_from
            .as_ref()
            .and(record.as_ref().and_then(|record| record.branch.clone())),
        old_md_name: renamed_from.map(|old| format!("{old}.md")),
        added,
        removed,
        images_added: diff.added.len(),
        // 差し替えは追加と削除に1件ずつ数える
        images_removed: diff.removed.len() + replaced,
        kind,
    })
}

fn md_path(basis: &Basis, slug: &str) -> String {
    format!("{}/{slug}.md", basis.posts_path)
}

fn assets_dir(basis: &Basis, slug: &str) -> String {
    format!("{}/{slug}", config::assets_path(&basis.posts_path))
}

// 形式の{slug}にslugを入れたbranch名
pub(crate) fn branch_name(basis: &Basis, slug: &str) -> String {
    match basis.branch_format.split_once("{slug}") {
        Some((before, after)) => format!("{before}{slug}{after}"),
        // {slug}を含まない形式は、validate_branch_formatで断られているので起きない
        None => slug.to_string(),
    }
}

// Pushするbranch。記録のbranchがあればそれ、無ければ形式の{slug}にslugを入れた名前
fn target_branch(record: Option<&Record>, basis: &Basis, slug: &str) -> String {
    record
        .and_then(|record| record.branch.as_deref())
        .map(str::to_string)
        .unwrap_or_else(|| branch_name(basis, slug))
}

// blobの送り直しを避けるための中身->SHAと、送っている間の進み具合
struct BlobCache<'a> {
    shas: HashMap<Vec<u8>, String>,
    progress: &'a dyn Fn(PushProgress),
    sent: usize,
    total: usize,
}

impl<'a> BlobCache<'a> {
    fn new(progress: &'a dyn Fn(PushProgress)) -> Self {
        BlobCache {
            shas: HashMap::new(),
            progress,
            sent: 0,
            total: 0,
        }
    }

    // このツリーで送る中身のうち、まだ送っていないものを、中身が同じものは1つにして数える
    fn begin(&mut self, diff: &LocalDiff) {
        let md = diff.md.iter().map(|(_, body)| body);
        let assets = diff.added.values().map(|(_, body)| body);
        self.sent = 0;
        self.total = md
            .chain(assets)
            .filter(|body| !self.shas.contains_key(*body))
            .collect::<BTreeSet<_>>()
            .len();
    }
}

fn upload_blob(
    input: &SyncInput<'_>,
    blobs: &mut BlobCache,
    content: &[u8],
) -> Result<String, GithubError> {
    if let Some(sha) = blobs.shas.get(content) {
        return Ok(sha.clone());
    }
    blobs.sent += 1;
    (blobs.progress)(PushProgress::Uploading {
        current: blobs.sent,
        total: blobs.total,
    });
    let sha = git::create_blob(input.transport, input.token, input.now, input.repo, content)?;
    blobs.shas.insert(content.to_vec(), sha.clone());
    Ok(sha)
}

// 変えたファイルをblobにして、ツリーに重ねるエントリを作る。親にあるファイルはそのmode、
// 新しいものは100644。消すものはshaを送らない
fn tree_entries(
    input: &SyncInput<'_>,
    slug: &str,
    diff: &LocalDiff,
    modes: &BTreeMap<String, String>,
    blobs: &mut BlobCache,
) -> Result<Vec<NewTreeEntry>, GithubError> {
    blobs.begin(diff);
    let slug_assets = assets_dir(input.basis, slug);
    let md = diff
        .md
        .iter()
        .map(|(_, body)| (md_path(input.basis, slug), body));
    let assets = diff
        .added
        .iter()
        .map(|(name, (_, body))| (format!("{slug_assets}/{name}"), body));
    let mut entries = Vec::new();
    for (path, body) in md.chain(assets) {
        let mode = modes.get(&path).map(String::as_str).unwrap_or("100644");
        let sha = upload_blob(input, blobs, body)?;
        entries.push(NewTreeEntry {
            path,
            mode: mode.to_string(),
            sha: Some(sha),
        });
    }
    for name in &diff.removed {
        let path = format!("{slug_assets}/{name}");
        // 親に無いパスを消そうとするとGitHubに断られる(リモートでも消えていたとき)
        if !modes.contains_key(&path) {
            continue;
        }
        entries.push(NewTreeEntry {
            path,
            mode: "100644".to_string(),
            sha: None,
        });
    }
    Ok(entries)
}

// 改名した記事のツリー。親の元のslugのパスを外し、mergedの内容を新しいslugのパスに置く。
// 送ったblob以外は親のSHAをそのまま使い、modeは元のパスのものを引き継ぐ
fn renamed_tree_entries(
    input: &SyncInput<'_>,
    old_slug: &str,
    slug: &str,
    parent: &ParentArticle,
    merged: &ArticleFiles,
    diff: &LocalDiff,
    blobs: &mut BlobCache,
) -> Result<Vec<NewTreeEntry>, GithubError> {
    blobs.begin(diff);
    let mode_of = |path: &str| {
        parent
            .modes
            .get(path)
            .cloned()
            .unwrap_or_else(|| "100644".to_string())
    };
    let old_md = md_path(input.basis, old_slug);
    let old_assets = assets_dir(input.basis, old_slug);
    let new_assets = assets_dir(input.basis, slug);
    let old_paths = (!parent.files.md.is_empty())
        .then(|| old_md.clone())
        .into_iter()
        .chain(
            parent
                .files
                .assets
                .keys()
                .map(|name| format!("{old_assets}/{name}")),
        );
    let mut entries: Vec<NewTreeEntry> = old_paths
        .map(|path| NewTreeEntry {
            mode: mode_of(&path),
            path,
            sha: None,
        })
        .collect();
    // mdは手元に必ずあるので、変えていなくても新しいパスに置く
    let md_sha = match &diff.md {
        Some((_, body)) => upload_blob(input, blobs, body)?,
        None => merged.md.clone(),
    };
    entries.push(NewTreeEntry {
        path: md_path(input.basis, slug),
        mode: mode_of(&old_md),
        sha: Some(md_sha),
    });
    for (name, sha) in &merged.assets {
        let sha = match diff.added.get(name) {
            Some((_, body)) => upload_blob(input, blobs, body)?,
            None => sha.clone(),
        };
        entries.push(NewTreeEntry {
            path: format!("{new_assets}/{name}"),
            mode: mode_of(&format!("{old_assets}/{name}")),
            sha: Some(sha),
        });
    }
    Ok(entries)
}

// 親のコミットでの記事のファイルとmode、親のルートのツリー
struct ParentArticle {
    files: ArticleFiles,
    modes: BTreeMap<String, String>,
    base_tree: String,
}

// parentでの記事。mdがGitHub上に無ければ、mdは空文字列にする
fn parent_article(
    input: &SyncInput<'_>,
    slug: &str,
    parent: &str,
) -> Result<ParentArticle, GithubError> {
    let base_tree = git::commit_tree(input.transport, input.token, input.now, input.repo, parent)?;
    let article = remote::read_article(
        input.transport,
        input.token,
        input.now,
        input.repo,
        input.basis,
        slug,
        parent,
    )?;
    match article {
        Some(article) => Ok(ParentArticle {
            files: article.files,
            modes: article.modes,
            base_tree,
        }),
        None => Ok(ParentArticle {
            files: ArticleFiles {
                md: String::new(),
                assets: BTreeMap::new(),
            },
            modes: BTreeMap::new(),
            base_tree,
        }),
    }
}

// 親のコミットでのファイルを記録と比べ、リモートで変わったファイルと手元で変えたファイルの
// 重なりを求める。(md, 画像の名前)。中身が同じなら重なりにしない(同じように消した・同じ内容に替えた)
fn conflict_of(
    record: Option<&Record>,
    remote: &ArticleFiles,
    diff: &LocalDiff,
) -> (bool, Vec<String>) {
    // md: 親のコミットのmdが記録と違い(記録が無ければ親にmdがあり)、手元のmdの中身が親と違えば重なり。
    // 記録があって親にmdが無い(空)のは、GitHub上で消されたということ
    let md = diff.md.as_ref().is_some_and(|ours| {
        let remote_changed = match record {
            None => !remote.md.is_empty(),
            Some(record) => record.md != remote.md,
        };
        remote_changed && ours.0 != remote.md
    });
    // 手元で替えた・消した画像と、リモートで替わった・消えた画像の名前
    // 消したものはNone
    let mut ours: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    for name in &diff.removed {
        ours.insert(name, None);
    }
    for (name, (sha, _)) in &diff.added {
        ours.insert(name, Some(sha));
    }
    let mut images = Vec::new();
    for (name, ours_sha) in ours {
        let remote_sha = remote.assets.get(name).map(String::as_str);
        // リモートで替わった・消えたか
        let remote_changed = match (record, remote_sha) {
            // 記録が無い記事は、親に画像があれば別の内容として重なり得る
            (None, _) => remote_sha.is_some(),
            (Some(record), Some(sha)) => {
                record.assets.get(name).is_none_or(|asset| asset.sha != sha)
            }
            (Some(record), None) => record.assets.contains_key(name),
        };
        // 中身が同じなら重なりにしない
        if !remote_changed || remote_sha == ours_sha {
            continue;
        }
        images.push(name.to_string());
    }
    (md, images)
}

// conflict_ofで比べる元にする、親のコミットの記事の内容。fetchedは使わない
fn compared_record(commit: &str, files: &ArticleFiles) -> Record {
    Record {
        branch: None,
        commit: commit.to_string(),
        md: files.md.clone(),
        assets: files
            .assets
            .iter()
            .map(|(name, sha)| {
                (
                    name.clone(),
                    AssetRecord {
                        sha: sha.clone(),
                        fetched: false,
                    },
                )
            })
            .collect(),
        applying: None,
        renamed_from: None,
        deleting: None,
    }
}

// リモートの様子を作る。親の内容に、手元で替えたファイルを重ねたもの
fn merged_files(remote: &ArticleFiles, diff: &LocalDiff) -> ArticleFiles {
    let mut merged = remote.clone();
    if let Some((sha, _)) = &diff.md {
        merged.md = sha.clone();
    }
    for (name, (sha, _)) in &diff.added {
        merged.assets.insert(name.clone(), sha.clone());
    }
    for name in &diff.removed {
        merged.assets.remove(name);
    }
    merged
}

// Push後の保存層の書き込み。ロックを取り、世代と向き先を確かめてからstate.jsonを読み直し、
// この記事の記録とリモートの様子だけを替える(画像の取得が通信の間に書いた記録を消さないため)。
// moved_fromがNoneなら、同じブランチの様子の移した記事を残す
#[allow(clippy::too_many_arguments)]
fn save_pushed(
    input: &SyncInput<'_>,
    slug: &str,
    result: &PushResult,
    merged: ArticleFiles,
    diff: &LocalDiff,
    md: &Content,
    moved_from: Option<&str>,
    state_path: &std::path::Path,
) -> Result<(), GithubError> {
    input.store.with_lock(|posts_dir, _| {
        sync::check_unchanged(input, posts_dir)?;
        let mut state = state::read_state(state_path, input.basis)?;
        let record = state.articles.entry(slug.to_string()).or_default();
        record.renamed_from = None;
        record.branch = Some(result.branch.clone());
        record.commit = result.commit.clone();
        record.md = md.0.clone();
        // 送った画像は取ってきたものにする。取ってきていない画像は前の記録のまま
        for (name, (sha, _)) in &diff.added {
            record.assets.insert(
                name.clone(),
                AssetRecord {
                    sha: sha.clone(),
                    fetched: true,
                },
            );
        }
        for name in &diff.removed {
            record.assets.remove(name);
        }
        // 送った記録がこの記事の内容なので、置き換えの途中の印は要らない
        record.applying = None;
        let moved_from = moved_from.map(str::to_string).or_else(|| {
            state
                .remote
                .branches
                .get(&result.branch)
                .and_then(|branch| branch.moved_from.clone())
        });
        state.remote.branches.insert(
            result.branch.clone(),
            BranchSnapshot {
                commit: result.commit.clone(),
                slug: slug.to_string(),
                article: Some(merged),
                moved_from,
            },
        );
        // 食い違いの印を外す
        state.conflicts.remove(slug);
        // syncedは記録に合わせて写すので、記録を書けた後にする
        state::write_state(state_path, &state)?;
        sync::write_synced(input, slug, &md.1)
    })
}

// 消したブランチをリモートの様子から外す。書けなくても次の読み込みで直るので、失敗は知らせない
fn forget_branch(input: &SyncInput<'_>, state_path: &std::path::Path, branch: &str) {
    let forgot = input.store.with_lock(|posts_dir, _| {
        sync::check_unchanged(input, posts_dir)?;
        let mut state = state::read_state(state_path, input.basis)?;
        state.remote.branches.remove(branch);
        state::write_state(state_path, &state)
    });
    if let Err(error) = forgot {
        eprintln!("Failed to forget the deleted branch {branch}: {error}");
    }
}

struct RenamedPush<'a> {
    slug: &'a str,
    message: &'a str,
    old_slug: &'a str,
    record: &'a Record,
    diff: &'a LocalDiff,
    md: &'a Content,
    overwrite: Option<&'a str>,
}

// 公開済みの記事のslugを変える(記録のブランチが無い)。ベースの先頭を親に新しいブランチを作る
fn push_renamed_from_base(
    input: &SyncInput<'_>,
    push: RenamedPush<'_>,
    progress: &dyn Fn(PushProgress),
) -> Result<PushResult, GithubError> {
    let RenamedPush {
        slug,
        message,
        old_slug,
        record,
        diff,
        md,
        overwrite,
    } = push;
    // Conflictで「Push」を選んだときは、確かめてもらった先頭を親にし、重なりを確かめ直さない
    let base_head = match overwrite {
        Some(head) => head.to_string(),
        None => git::branch_head(
            input.transport,
            input.token,
            input.now,
            input.repo,
            &input.basis.base_branch,
        )?
        .ok_or(GithubError::BaseBranchNotFound)?,
    };
    let parent = parent_article(input, old_slug, &base_head)?;
    // ベースから元のslugの記事が消えていれば何も作らない
    if parent.files.md.is_empty() {
        return Err(GithubError::BranchGone);
    }
    let (md_conflict, images) = conflict_of(Some(record), &parent.files, diff);
    if overwrite.is_none() && (md_conflict || !images.is_empty()) {
        return Err(GithubError::Conflict {
            head: base_head,
            md: md_conflict,
            images,
        });
    }
    let mut merged = merged_files(&parent.files, diff);
    if merged.md.is_empty() {
        merged.md = md.0.clone();
    }
    let mut blobs = BlobCache::new(progress);
    let entries = renamed_tree_entries(input, old_slug, slug, &parent, &merged, diff, &mut blobs)?;
    progress(PushProgress::Committing);
    let tree = git::create_tree(
        input.transport,
        input.token,
        input.now,
        input.repo,
        &parent.base_tree,
        &entries,
    )?;
    let commit = git::create_commit(
        input.transport,
        input.token,
        input.now,
        input.repo,
        message,
        &tree,
        &[&base_head],
    )?;
    progress(PushProgress::Pushing);
    let branch = branch_name(input.basis, slug);
    match git::create_ref(
        input.transport,
        input.token,
        input.now,
        input.repo,
        &branch,
        &commit,
    ) {
        Ok(()) => {}
        Err(error @ GithubError::Status { status: 422 }) => {
            // 空のブランチでも進めない(親が元の記事を含まないことがある)
            if git::branch_head(input.transport, input.token, input.now, input.repo, &branch)?
                .is_some()
            {
                return Err(GithubError::BranchExists { branch });
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    }
    let result = PushResult {
        branch: branch.clone(),
        commit,
        kept_branch: None,
    };
    let state_path = state::state_path(input.data_dir, input.repo);
    let saved = save_pushed(
        input,
        slug,
        &result,
        merged,
        diff,
        md,
        Some(old_slug),
        &state_path,
    );
    // 記録を書けていれば、syncedを書けなかっただけなのでPushは成功にする(差分の行数がずれるだけ)
    if let Err(error) = saved {
        let recorded = state::read_state(&state_path, input.basis).is_ok_and(|state| {
            state
                .articles
                .get(slug)
                .is_some_and(|record| record.branch.as_deref() == Some(branch.as_str()))
        });
        if !recorded {
            return Err(error);
        }
        eprintln!("Failed to write the synced md of {slug}: {error}");
    }
    Ok(result)
}

// overwriteは、Conflictで返したheadのうち「Push」で確かめてもらったもの
pub fn commit_push(
    input: &SyncInput<'_>,
    slug: &str,
    message: &str,
    overwrite: Option<&str>,
    progress: &dyn Fn(PushProgress),
) -> Result<PushResult, GithubError> {
    let state_path = state::state_path(input.data_dir, input.repo);
    // state.jsonは通信の前に読む。読めなければ通信もしない
    let header = state::read_state(&state_path, input.basis)?;
    let record = header.articles.get(slug);
    // 削除予定の記事はPushできない
    if record.is_some_and(|record| record.deleting.is_some()) {
        return Err(GithubError::NothingToCommit);
    }
    let renamed_from = record.and_then(|record| record.renamed_from.clone());
    // 改名した記事は新しいslugのブランチを作る
    let branch = match &renamed_from {
        Some(_) => branch_name(input.basis, slug),
        None => target_branch(record, input.basis, slug),
    };

    // 手元の中身を、記録と比べるときにstoreのロックの中で読む
    let local = input
        .store
        .with_lock(|posts_dir, assets_base| state::local_bodies(posts_dir, assets_base, slug))?;
    // 手元にmdが無い記事はPushできない
    let md = local.md.clone().ok_or(GithubError::NothingToCommit)?;
    let diff = local_diff(record, &local);
    if diff.empty() && renamed_from.is_none() {
        return Err(GithubError::NothingToCommit);
    }

    // まだcommitしていない記事で、送り先の空のブランチが様子にあれば、その先頭を親にする
    let mut empty_branch_head = None;
    if record.is_none()
        && header
            .remote
            .branches
            .get(&branch)
            .is_some_and(|branch| branch.article.is_none())
    {
        // 先頭を読み直して、記事が足されていれば作らない
        if let Some(head) =
            git::branch_head(input.transport, input.token, input.now, input.repo, &branch)?
        {
            if remote::read_article(
                input.transport,
                input.token,
                input.now,
                input.repo,
                input.basis,
                slug,
                &head,
            )?
            .is_some()
            {
                return Err(GithubError::BranchExists {
                    branch: branch.clone(),
                });
            }
            empty_branch_head = Some(head);
        }
    }
    // 記録のbranchがあるか。PATCHとPOST /git/refsで分ける
    let recorded_branch =
        record.and_then(|record| record.branch.as_deref()).is_some() || empty_branch_head.is_some();

    if let Some(old_slug) = renamed_from {
        let record = record.expect("renamed_from is only on a record");
        if record.branch.is_none() {
            let result = push_renamed_from_base(
                input,
                RenamedPush {
                    slug,
                    message,
                    old_slug: &old_slug,
                    record,
                    diff: &diff,
                    md: &md,
                    overwrite,
                },
                progress,
            )?;
            clear_sent_updated_mark(input, slug, &md.1);
            return Ok(result);
        }
        let old_branch = record.branch.as_deref().ok_or(GithubError::BranchGone)?;
        // 元の記事を移したブランチなら、その元のslugを新しいブランチにも書く
        let moved_from = header
            .remote
            .branches
            .get(old_branch)
            .and_then(|branch| branch.moved_from.clone());
        // 古いブランチの先頭を親にする。消えていれば何も作らない
        let old_head = git::branch_head(
            input.transport,
            input.token,
            input.now,
            input.repo,
            old_branch,
        )?
        .ok_or(GithubError::BranchGone)?;
        let parent = overwrite.map(str::to_string).unwrap_or(old_head);
        // overwriteの初回は、確かめてもらった先頭から作り直すので重なりを確かめ直さない
        let confirming = overwrite.is_none();
        let parent_article = parent_article(input, &old_slug, &parent)?;
        let (md_conflict, images) = conflict_of(Some(record), &parent_article.files, &diff);
        if confirming && (md_conflict || !images.is_empty()) {
            return Err(GithubError::Conflict {
                head: parent,
                md: md_conflict,
                images,
            });
        }
        let mut merged = merged_files(&parent_article.files, &diff);
        // 古いブランチでmdが消されていても、改名した記事なので手元のmdを置く
        if merged.md.is_empty() {
            merged.md = md.0.clone();
        }
        let mut blobs = BlobCache::new(progress);
        let entries = renamed_tree_entries(
            input,
            &old_slug,
            slug,
            &parent_article,
            &merged,
            &diff,
            &mut blobs,
        )?;
        progress(PushProgress::Committing);
        let tree = git::create_tree(
            input.transport,
            input.token,
            input.now,
            input.repo,
            &parent_article.base_tree,
            &entries,
        )?;
        let commit = git::create_commit(
            input.transport,
            input.token,
            input.now,
            input.repo,
            message,
            &tree,
            &[&parent],
        )?;
        progress(PushProgress::Pushing);
        match git::create_ref(
            input.transport,
            input.token,
            input.now,
            input.repo,
            &branch,
            &commit,
        ) {
            Ok(()) => {}
            Err(error @ GithubError::Status { status: 422 }) => {
                // 同じ名前のbranchがあった。古いbranchも記録も替えない
                if git::branch_head(input.transport, input.token, input.now, input.repo, &branch)?
                    .is_some()
                {
                    return Err(GithubError::BranchExists { branch });
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        }
        let mut result = PushResult {
            branch: branch.clone(),
            commit,
            kept_branch: None,
        };
        // 記録を新しいブランチにできていれば(その後のsyncedの書き込みで失敗していても)、古いブランチを
        // 片付ける。記録が古いままなら、古いブランチはBranchExistsからのPullで要る
        let recorded = || {
            state::read_state(&state_path, input.basis).is_ok_and(|state| {
                state
                    .articles
                    .get(slug)
                    .is_some_and(|record| record.branch.as_deref() == Some(branch.as_str()))
            })
        };
        let saved = match save_pushed(
            input,
            slug,
            &result,
            merged,
            &diff,
            &md,
            moved_from.as_deref(),
            &state_path,
        ) {
            Err(error) if !recorded() => return Err(error),
            saved => saved,
        };
        // 記録は書けたので、syncedを書けなかっただけならPushは成功にする(差分の行数がずれるだけ)
        if let Err(error) = saved {
            eprintln!("Failed to write the synced md of {slug}: {error}");
        }
        match super::article::remove_branch(input, old_branch, &parent) {
            Ok(()) => forget_branch(input, &state_path, old_branch),
            Err(error) => {
                eprintln!("Failed to delete the old branch {old_branch}: {error}");
                result.kept_branch = Some(old_branch.to_string());
            }
        }
        clear_sent_updated_mark(input, slug, &md.1);
        return Ok(result);
    }

    // 記録したbranchが消えていたら、何も作らずに読み込み直させる
    if recorded_branch {
        git::branch_head(input.transport, input.token, input.now, input.repo, &branch)?
            .ok_or(GithubError::BranchGone)?;
    }

    // 親を決める。overwriteがあれば、記録の有る無いにかかわらずそのコミットを親にする
    let mut parent = match (overwrite, record) {
        (Some(commit), _) => commit.to_string(),
        (None, Some(record)) if recorded_branch => record.commit.clone(),
        (None, _) => git::branch_head(
            input.transport,
            input.token,
            input.now,
            input.repo,
            &input.basis.base_branch,
        )?
        .ok_or(GithubError::BaseBranchNotFound)?,
    };
    if let (None, Some(head)) = (overwrite, &empty_branch_head) {
        parent = head.clone();
    }
    // overwriteの初回は、確かめてもらった先頭から作り直すので重なりを確かめ直さない
    let mut confirming = overwrite.is_none();
    let mut blobs = BlobCache::new(progress);
    // PATCHを送った回数
    let mut attempts = 0;
    // リモートで変わったファイルを求めるときに比べる元。重なりを確かめた(または確かめてもらった)親に進める
    let mut compared = record.cloned();

    loop {
        // 親のコミットでの記事のファイルを確かめる。mdやmodeの読みは作り直しのたびに要る
        let parent_article = parent_article(input, slug, &parent)?;
        let (md_conflict, images) = conflict_of(compared.as_ref(), &parent_article.files, &diff);
        if confirming && (md_conflict || !images.is_empty()) {
            return Err(GithubError::Conflict {
                head: parent,
                md: md_conflict,
                images,
            });
        }
        compared = Some(compared_record(&parent, &parent_article.files));
        let entries = tree_entries(input, slug, &diff, &parent_article.modes, &mut blobs)?;
        progress(PushProgress::Committing);
        let tree = git::create_tree(
            input.transport,
            input.token,
            input.now,
            input.repo,
            &parent_article.base_tree,
            &entries,
        )?;
        let commit = git::create_commit(
            input.transport,
            input.token,
            input.now,
            input.repo,
            message,
            &tree,
            &[&parent],
        )?;
        let merged = merged_files(&parent_article.files, &diff);
        progress(PushProgress::Pushing);
        let pushed = if recorded_branch {
            attempts += 1;
            git::update_ref(
                input.transport,
                input.token,
                input.now,
                input.repo,
                &branch,
                &commit,
            )
        } else {
            git::create_ref(
                input.transport,
                input.token,
                input.now,
                input.repo,
                &branch,
                &commit,
            )
        };
        match pushed {
            Ok(()) => {
                let result = PushResult {
                    branch: branch.clone(),
                    commit,
                    kept_branch: None,
                };
                save_pushed(input, slug, &result, merged, &diff, &md, None, &state_path)?;
                clear_sent_updated_mark(input, slug, &md.1);
                return Ok(result);
            }
            Err(error @ GithubError::Status { status: 422 | 404 }) if recorded_branch => {
                progress(PushProgress::Checking);
                let head =
                    git::branch_head(input.transport, input.token, input.now, input.repo, &branch)?
                        .ok_or(GithubError::BranchGone)?;
                if head == parent {
                    // 先頭が親と同じまま進められなかったので、元のエラーを返す
                    return Err(error);
                }
                if attempts >= RETRY_LIMIT {
                    return Err(GithubError::PushRetryExceeded);
                }
                // 親と違えば、そのコミットを親にして重なりの確かめから作り直す
                parent = head;
                confirming = true;
            }
            Err(error @ GithubError::Status { status: 422 }) if !recorded_branch => {
                // 同じ名前のbranchがあった。記録は替えない。消えていれば元のエラー
                if git::branch_head(input.transport, input.token, input.now, input.repo, &branch)?
                    .is_some()
                {
                    return Err(GithubError::BranchExists { branch });
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    }
}

// 送ったmdがまだ手元にあるときだけ、updatedDateの印を外す。送っている間に保存されていれば残す
fn clear_sent_updated_mark(input: &SyncInput<'_>, slug: &str, sent: &[u8]) {
    let path = dates::marks_path(input.data_dir, input.repo);
    if let Err(error) = dates::clear_updated_mark_if(&path, slug, || {
        input.store.with_lock(|posts_dir, _| {
            let md = posts_dir.join(format!("{slug}.md"));
            match fs::read(&md) {
                Ok(current) => current == sent,
                Err(error) => {
                    eprintln!("Failed to read {} after pushing it: {error}", md.display());
                    false
                }
            }
        })
    }) {
        eprintln!("Failed to clear the updatedDate mark of {slug}: {error}");
    }
}

// branchがNoneなら記録のbranch、記録にbranchが無ければベース。backup_nameは日時(YYYYMMDD-HHMMSS)。
// 同じ名前があれば-2、-3…を付ける
pub fn pull(
    input: &SyncInput<'_>,
    slug: &str,
    branch: Option<&str>,
    backup_name: &str,
) -> Result<(), GithubError> {
    let state_path = state::state_path(input.data_dir, input.repo);
    let header = state::read_state(&state_path, input.basis)?;
    let record = header.articles.get(slug);
    let target = match branch {
        Some(branch) => branch.to_string(),
        None => record
            .and_then(|record| record.branch.clone())
            .unwrap_or_else(|| input.basis.base_branch.clone()),
    };
    // branchを指定したら改名をやめる。指定が無く改名していれば、記録のブランチにある元のslugの記事を読む
    let renamed_from = record.and_then(|record| record.renamed_from.clone());
    let read_slug = match (&renamed_from, branch) {
        (Some(old), None) => old.clone(),
        _ => slug.to_string(),
    };
    // 先頭と、そこでの記事のファイルを読む
    let Some(head) =
        git::branch_head(input.transport, input.token, input.now, input.repo, &target)?
    else {
        return Err(if target == input.basis.base_branch {
            GithubError::BaseBranchNotFound
        } else {
            GithubError::BranchGone
        });
    };
    if remote::read_article(
        input.transport,
        input.token,
        input.now,
        input.repo,
        input.basis,
        &read_slug,
        &head,
    )?
    .is_none()
    {
        return Err(GithubError::ArticleNotFound);
    }

    // 手元の記事のファイルをbackup/{日時}/に写す(mdとassets/{slug}/の直下の画像)。
    // 書けなければ記録を替えない
    let backup_dir = backup_dir(input, backup_name)?;
    input.store.with_lock(|posts_dir, assets_base| {
        sync::check_unchanged(input, posts_dir)?;
        let local = state::local_bodies(posts_dir, assets_base, slug)?;
        let Some(md) = &local.md else {
            // 手元にmdが無い記事は、Pullで記録に書ける内容が無い
            return Err(GithubError::io(
                &posts_dir.join(format!("{slug}.md")),
                "the article is not found",
            ));
        };
        sync::write_file(&backup_dir.join(md_path(input.basis, slug)), &md.1)?;
        for (name, (_, body)) in &local.assets {
            sync::write_file(
                &backup_dir.join(assets_dir(input.basis, slug)).join(name),
                body,
            )?;
        }
        // 記録を、branch・読んだ先頭・手元の内容(取ってきたものとして)にして書く
        let mut state = state::read_state(&state_path, input.basis)?;
        let record = state.articles.entry(slug.to_string()).or_default();
        record.md = md.0.clone();
        record.assets = local
            .assets
            .iter()
            .map(|(name, (sha, _))| {
                (
                    name.clone(),
                    AssetRecord {
                        sha: sha.clone(),
                        fetched: true,
                    },
                )
            })
            .collect();
        record.commit = head.clone();
        // branchはConflictなら記録のbranch(無ければベース)、BranchExistsならそのbranch
        if let Some(branch) = branch {
            record.branch = Some(branch.to_string());
            record.renamed_from = None;
        }
        record.applying = None;
        state::write_state(&state_path, &state)?;
        Ok(())
    })?;
    if let Err(error) =
        dates::clear_updated_mark(&dates::marks_path(input.data_dir, input.repo), slug)
    {
        eprintln!("Failed to clear the updatedDate mark of {slug}: {error}");
    }
    Ok(())
}

// まだ無いバックアップのディレクトリ。同じ名前があれば{backup_name}-2、-3…を探す
fn backup_dir(input: &SyncInput<'_>, backup_name: &str) -> Result<PathBuf, GithubError> {
    let root = state::backup_root(input.data_dir, input.repo);
    for count in 1.. {
        let name = if count == 1 {
            backup_name.to_string()
        } else {
            format!("{backup_name}-{count}")
        };
        let path = root.join(&name);
        if !path.exists() {
            return Ok(path);
        }
    }
    unreachable!("バックアップのディレクトリは必ず見つかる")
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::github::config::{github_posts_dir, RepoRef};
    use crate::github::dates;
    use crate::github::fake::{
        b64_decode, blob_sha, json_response, read_only_dir, repo_ref, url_of, FakeRepo,
        RouteTransport,
    };
    use crate::github::http::{Method, Request};
    use crate::github::state::{Basis, Category};
    use crate::github::sync::{self, SyncInput};
    use crate::store::Store;

    const TOKEN: &str = "TOKEN";
    const NOW: i64 = 1_000_000;

    const README: &[u8] = b"readme\n";
    const OTHER_MD: &[u8] = b"other\n";
    const OTHER2_MD: &[u8] = b"other2\n";

    // 記事aのGitHub上のmd。本文5行
    const A_SYNCED: &[u8] = b"---\ntitle: \"A\"\n---\nline-a\nline-b\nline-c\nline-d\nline-e\n";
    // GitHub上のmdをほかの端末が替えたもの(手元・記録のどちらとも違う中身)。2つ目の変え方
    const A_REMOTE: &[u8] = b"---\ntitle: \"A\"\n---\nchanged-a\n";
    const A_REMOTE2: &[u8] = b"---\ntitle: \"A\"\n---\nchanged2-a\n";
    // 手元で2行替えて1行足したmd(A_SYNCEDとの差分が+3 −2)
    const A_LOCAL: &[u8] = b"---\ntitle: \"A\"\n---\nX-a\nX-b\nline-c\nline-d\nline-e\nZ-f\n";
    // 記録の無い新しい記事のmd(行の数をはっきりさせるため、改行で終えない)
    fn md_bytes(title: &str, body: &str) -> Vec<u8> {
        format!("---\ntitle: \"{title}\"\n---\n{body}").into_bytes()
    }

    fn a_new() -> Vec<u8> {
        md_bytes("新しい記事", "h-a\nh-b\nh-c")
    }
    const PNG: &[u8] = b"png-1\n";
    const PNG2: &[u8] = b"png-2\n";
    const PNG3: &[u8] = b"png-3\n";
    // assetsの直下の共有の画像
    const SHARED_PNG: &[u8] = b"shared-1\n";

    // リポジトリのルートからの場所 -> 中身のファイル一覧
    type Files = &'static [(&'static str, &'static [u8])];

    // mainの下ごしらえ。ほかの記事のファイルと、記事aのファイルと、外のファイル
    const MAIN_FILES: Files = &[
        ("README.md", README),
        ("src/content/blog/other.md", OTHER_MD),
        ("src/assets/other/02.png", PNG2),
        ("src/content/blog/a.md", A_SYNCED),
        ("src/assets/a/01.png", PNG),
    ];
    const SHARED_MAIN_FILES: Files = &[
        ("README.md", README),
        ("src/content/blog/other.md", OTHER_MD),
        ("src/assets/other/02.png", PNG2),
        ("src/content/blog/a.md", A_SYNCED),
        ("src/assets/a/01.png", PNG),
        ("src/assets/shared.png", SHARED_PNG),
    ];
    // 記事aが無いmain
    const NO_A_MAIN: Files = &[
        ("README.md", README),
        ("src/content/blog/other.md", OTHER_MD),
        ("src/assets/other/02.png", PNG2),
    ];
    // 記事aだけがGitHubにあるmain(同じslugの記事が既にあるときの下ごしらえ)
    const ONLY_A_MAIN: Files = &[("src/content/blog/a.md", A_SYNCED)];

    // ベースの内容をほかの端末が替えた: 記事aのmdが替わった・ほかの記事が替わった
    const BASE_A_CHANGED: Files = &[
        ("README.md", README),
        ("src/content/blog/other.md", OTHER_MD),
        ("src/assets/other/02.png", PNG2),
        ("src/content/blog/a.md", A_REMOTE),
        ("src/assets/a/01.png", PNG),
    ];
    const BASE_OTHER_CHANGED: Files = &[
        ("README.md", README),
        ("src/content/blog/other.md", OTHER2_MD),
        ("src/assets/other/02.png", PNG2),
        ("src/content/blog/a.md", A_SYNCED),
        ("src/assets/a/01.png", PNG),
    ];

    // ほかの端末がbranchを替えたときのファイル一覧(PATCHの応答の前にhookで作り替える)
    // 記事aと重ならない画像02.pngを足す
    const HEAD_ADVANCED: Files = &[
        ("README.md", README),
        ("src/content/blog/other.md", OTHER_MD),
        ("src/assets/other/02.png", PNG2),
        ("src/content/blog/a.md", A_SYNCED),
        ("src/assets/a/01.png", PNG),
        ("src/assets/a/02.png", PNG2),
    ];
    // 画像を足した
    const OTHER_ADDS_IMAGE: Files = HEAD_ADVANCED;
    // 画像を消した
    const OTHER_REMOVES_IMAGE: Files = &[
        ("README.md", README),
        ("src/content/blog/other.md", OTHER_MD),
        ("src/assets/other/02.png", PNG2),
        ("src/content/blog/a.md", A_SYNCED),
    ];
    // 画像を差し替えた
    const OTHER_REPLACES_IMAGE: Files = &[
        ("README.md", README),
        ("src/content/blog/other.md", OTHER_MD),
        ("src/assets/other/02.png", PNG2),
        ("src/content/blog/a.md", A_SYNCED),
        ("src/assets/a/01.png", PNG2),
    ];
    // mdを替えた
    const OTHER_CHANGES_MD: Files = &[
        ("README.md", README),
        ("src/content/blog/other.md", OTHER_MD),
        ("src/assets/other/02.png", PNG2),
        ("src/content/blog/a.md", A_REMOTE),
        ("src/assets/a/01.png", PNG),
    ];
    const OTHER_CHANGES_MD2: Files = &[
        ("README.md", README),
        ("src/content/blog/other.md", OTHER_MD),
        ("src/assets/other/02.png", PNG2),
        ("src/content/blog/a.md", A_REMOTE2),
        ("src/assets/a/01.png", PNG),
    ];
    // mdを替え、さらに画像を足した
    const OTHER_CHANGES_MD_ADDS: Files = &[
        ("README.md", README),
        ("src/content/blog/other.md", OTHER_MD),
        ("src/assets/other/02.png", PNG2),
        ("src/content/blog/a.md", A_REMOTE),
        ("src/assets/a/01.png", PNG),
        ("src/assets/a/02.png", PNG2),
    ];
    // mdを、手元と同じ中身に替えた
    const OTHER_MAKES_SAME_MD: Files = &[
        ("README.md", README),
        ("src/content/blog/other.md", OTHER_MD),
        ("src/assets/other/02.png", PNG2),
        ("src/content/blog/a.md", A_LOCAL),
        ("src/assets/a/01.png", PNG),
    ];

    fn basis() -> Basis {
        Basis {
            base_branch: "main".to_string(),
            branch_format: "blog/{slug}".to_string(),
            posts_path: "src/content/blog".to_string(),
        }
    }

    const BRANCH: &str = "blog/a";
    const SLUG: &str = "a";
    const BACKUP_NAME: &str = "20260101-120000";

    struct PushTest {
        _tmp: TempDir,
        data_dir: PathBuf,
        store: Arc<Store>,
        transport: Arc<RouteTransport>,
        repo_of: RepoRef,
        basis_of: Basis,
        posts_dir: PathBuf,
        assets_base: PathBuf,
        state_path: PathBuf,
        synced_dir: PathBuf,
    }

    impl PushTest {
        fn new() -> Self {
            let tmp = TempDir::new().unwrap();
            let data_dir = tmp.path().join("data");
            let repo_of = repo_ref();
            let basis_of = basis();
            let posts_dir = github_posts_dir(&data_dir, &repo_of, "src/content/blog");
            let store = Arc::new(Store::new(posts_dir.clone(), tmp.path().join("backup")).unwrap());
            let repo_part = "github/owner/blog";
            Self {
                _tmp: tmp,
                posts_dir: posts_dir.clone(),
                assets_base: posts_dir.parent().unwrap().parent().unwrap().join("assets"),
                state_path: data_dir.join(repo_part).join("state.json"),
                synced_dir: data_dir.join(repo_part).join("synced"),
                data_dir,
                store,
                transport: Arc::new(RouteTransport::new()),
                repo_of,
                basis_of,
            }
        }

        fn new_repo(&self) -> FakeRepo {
            FakeRepo::new(&self.transport, "owner", "blog")
        }

        // syncとcommit_pushに渡す入力
        fn input(&self) -> SyncInput<'_> {
            SyncInput {
                transport: self.transport.as_ref(),
                token: TOKEN,
                now: NOW,
                data_dir: &self.data_dir,
                repo: &self.repo_of,
                basis: &self.basis_of,
                store: self.store.as_ref(),
                generation: self.store.generation(),
                posts_dir: &self.posts_dir,
                protect: None,
            }
        }

        fn sync(&self) {
            sync::sync(&self.input()).unwrap();
        }

        fn preview(&self, slug: &str) -> Result<CommitPreview, GithubError> {
            preview(&self.input(), slug)
        }

        fn commit_push(
            &self,
            slug: &str,
            message: &str,
            overwrite: Option<&str>,
        ) -> Result<PushResult, GithubError> {
            commit_push(&self.input(), slug, message, overwrite, &|_| {})
        }

        // 知らせと、知らせを受けた時点で送り終えていたリクエストの数を組にして返す
        fn commit_push_progress(
            &self,
            slug: &str,
            message: &str,
            overwrite: Option<&str>,
        ) -> (Result<PushResult, GithubError>, Vec<(PushProgress, usize)>) {
            let received = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&received);
            let transport = Arc::clone(&self.transport);
            let result = commit_push(&self.input(), slug, message, overwrite, &move |progress| {
                let at = transport.requests().len();
                recorded.lock().unwrap().push((progress, at));
            });
            let received = received.lock().unwrap().clone();
            (result, received)
        }

        fn pull(
            &self,
            slug: &str,
            branch: Option<&str>,
            backup_name: &str,
        ) -> Result<(), GithubError> {
            pull(&self.input(), slug, branch, backup_name)
        }

        fn article_state(&self, slug: &str) -> sync::ArticleState {
            sync::article_state(
                &self.data_dir,
                &self.repo_of,
                &self.basis_of,
                self.store.as_ref(),
                slug,
            )
            .unwrap()
        }

        fn md_path(&self, slug: &str) -> PathBuf {
            self.posts_dir.join(format!("{slug}.md"))
        }

        fn synced_path(&self, slug: &str) -> PathBuf {
            self.synced_dir.join(format!("{slug}.md"))
        }

        fn image_path(&self, rel: &str) -> PathBuf {
            self.assets_base.join(rel)
        }

        fn backup_path(&self, name: &str, rel: &str) -> PathBuf {
            self.backup_root().join(name).join(rel)
        }

        // 手元を直接書く
        fn write_md(&self, content: &[u8]) {
            let path = self.md_path(SLUG);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn write_image(&self, rel: &str, content: &[u8]) {
            let path = self.assets_base.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        // 画像のfetchedを付け替える
        fn mark_fetched(&self, slug: &str, names: &[(&str, bool)]) {
            self.edit_state(|state| {
                for (name, fetched) in names {
                    state["articles"][slug]["assets"][name]["fetched"] = Value::Bool(*fetched);
                }
            });
        }

        fn state(&self) -> Value {
            serde_json::from_slice(&fs::read(&self.state_path).unwrap()).unwrap()
        }

        fn edit_state(&self, change: impl FnOnce(&mut Value)) {
            let mut state = self.state();
            change(&mut state);
            fs::write(&self.state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        }

        // Pullのバックアップの場所
        fn backup_root(&self) -> PathBuf {
            self.data_dir.join("github/owner/blog/backup")
        }

        // PATCH /git/refs/heads/{branch}のURL
        fn patch_url(&self, branch: &str) -> String {
            let mut segments = vec!["repos", "owner", "blog", "git", "refs", "heads"];
            segments.extend(branch.split('/'));
            url_of(&segments, &[])
        }

        fn trees_url(&self) -> String {
            url_of(&["repos", "owner", "blog", "git", "trees"], &[])
        }

        fn refs_url(&self) -> String {
            url_of(&["repos", "owner", "blog", "git", "refs"], &[])
        }

        // URLに一致する書き込みの通信の回数
        fn post_count(&self, url: &str) -> usize {
            self.transport
                .requests()
                .iter()
                .map(|r| (r.method == Method::Post && r.url == url) as usize)
                .sum()
        }

        fn patch_count(&self, url: &str) -> usize {
            self.transport
                .requests()
                .iter()
                .map(|r| (r.method == Method::Patch && r.url == url) as usize)
                .sum()
        }

        // 書き込みの通信の本文。POSTとPATCHで分ける
        fn post_bodies(&self, url_part: &str) -> Vec<Value> {
            bodies(&self.transport.requests(), Method::Post, url_part)
        }

        fn patch_bodies(&self, url_part: &str) -> Vec<Value> {
            bodies(&self.transport.requests(), Method::Patch, url_part)
        }

        // POST /git/blobsで送った中身
        fn blob_contents(&self) -> Vec<Vec<u8>> {
            self.post_bodies("/git/blobs")
                .iter()
                .map(|body| {
                    b64_decode(body["content"].as_str().expect("contentはあるはず"))
                        .expect("base64は解けるはず")
                })
                .collect()
        }

        // atは知らせを受けた時点のリクエストの数。知らせの直後に送ったリクエストを返す
        fn request_after(&self, at: usize) -> String {
            let requests = self.transport.requests();
            let request = requests
                .get(at)
                .unwrap_or_else(|| panic!("{at}番目のリクエストは無い"));
            request_text(request)
        }

        // Checking以外の知らせが、それぞれ別の対応する通信の直前にあるか。pushingはPushingの直後の通信
        fn assert_progress_before_requests(
            &self,
            progress: &[(PushProgress, usize)],
            pushing: &str,
        ) {
            for (step, at) in progress {
                let expected = match step {
                    PushProgress::Checking => continue,
                    PushProgress::Uploading { .. } => "Post /git/blobs",
                    PushProgress::Committing => "Post /git/trees",
                    PushProgress::Pushing => pushing,
                };
                assert_eq!(self.request_after(*at), expected, "{step:?}");
            }
            assert!(
                progress.windows(2).all(|pair| pair[0].1 < pair[1].1),
                "{progress:?}"
            );
        }

        fn request_before(&self, at: usize) -> String {
            request_text(&self.transport.requests()[at - 1])
        }
    }

    fn bodies(requests: &[Request], method: Method, url_part: &str) -> Vec<Value> {
        requests
            .iter()
            .filter(|request| request.method == method && request.url.contains(url_part))
            .map(|request| serde_json::from_slice(request.body.as_deref().unwrap()).unwrap())
            .collect()
    }

    // リクエストを「メソッド URLの末尾」の形にする
    fn request_text(request: &Request) -> String {
        let tail = request
            .url
            .split_once("/repos/owner/blog")
            .map_or(request.url.as_str(), |(_, tail)| tail);
        format!("{:?} {tail}", request.method)
    }

    fn steps_of(progress: &[(PushProgress, usize)]) -> Vec<PushProgress> {
        progress
            .iter()
            .map(|(progress, _)| progress.clone())
            .collect()
    }

    fn path_of(files: &BTreeMap<String, (String, Vec<u8>)>, path: &str) -> (String, Vec<u8>) {
        files.get(path).cloned().expect("ファイルはあるはず")
    }

    fn body_of<'a>(files: &'a BTreeMap<String, (String, Vec<u8>)>, path: &str) -> &'a [u8] {
        files
            .get(path)
            .map(|(_, body)| body.as_slice())
            .expect("ファイルはあるはず")
    }

    // nth回目のPATCHの前に、ほかの端末がbranchを作り替える(commitsに新しい先頭を入れる)
    fn branch_hook(
        repo: &FakeRepo,
        test: &PushTest,
        commits: Arc<Mutex<Vec<String>>>,
        nth: usize,
        files: Files,
    ) {
        let hook_repo = repo.clone();
        let hook_transport = Arc::clone(&test.transport);
        repo.before(Method::Patch, &test.patch_url(BRANCH), nth, move || {
            let head = hook_repo.branch(&hook_transport, BRANCH, files);
            commits.lock().unwrap().push(head);
        });
    }

    // 下ごしらえ ---------------------------------------------------------------

    impl PushTest {
        // mainにREADME・ほかの記事・記事aがあり、初回の読み込みが済んでいる。
        // 記事aの記録のbranchは無く、mdはA_SYNCED、画像01.pngは取ってきていない
        fn article_synced(&self) -> FakeRepo {
            let repo = self.new_repo();
            repo.branch(&self.transport, "main", MAIN_FILES);
            self.sync();
            repo
        }

        // さらに記事用のbranch blog/a(mainと同じ内容)があり、読み込み済み。
        // 記録のbranchがblog/aで、commitはbranchの先頭
        fn branch_synced(&self) -> (FakeRepo, String) {
            let repo = self.article_synced();
            let head = repo.branch(&self.transport, BRANCH, MAIN_FILES);
            self.sync();
            (repo, head)
        }
    }

    // プレビュー -----------------------------------------------------------------

    #[test]
    fn 新しい記事は全行が追加でkindはadd() {
        let test = PushTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", NO_A_MAIN);
        test.write_md(&a_new());
        test.write_image("a/01.png", PNG);

        let preview = test.preview(SLUG).unwrap();
        assert_eq!(preview.title, "新しい記事");
        assert_eq!(preview.md_name, "a.md");
        assert_eq!(
            preview.added,
            6,
            "mdの行数(フロントマターを含む): {}",
            String::from_utf8_lossy(&a_new())
        );
        assert_eq!(preview.removed, 0);
        assert_eq!(preview.images_added, 1);
        assert_eq!(preview.images_removed, 0);
        assert_eq!(preview.kind, CommitKind::Add);
    }

    #[test]
    fn 記録のbranchが無い記事はkindがfixでsyncedとの差分を数える() {
        let test = PushTest::new();
        let _repo = test.article_synced();
        test.write_md(A_LOCAL);

        let preview = test.preview(SLUG).unwrap();
        assert_eq!(preview.title, "A");
        assert_eq!(preview.added, 3);
        assert_eq!(preview.removed, 2);
        assert_eq!(preview.images_added, 0);
        assert_eq!(preview.images_removed, 0);
        assert_eq!(preview.kind, CommitKind::Fix);
    }

    #[test]
    fn 記録のbranchがある記事はkindがupdate() {
        let test = PushTest::new();
        let (_repo, _head) = test.branch_synced();
        test.write_md(A_LOCAL);

        let preview = test.preview(SLUG).unwrap();
        assert_eq!(preview.kind, CommitKind::Update);
        assert_eq!(preview.added, 3);
        assert_eq!(preview.removed, 2);
    }

    #[test]
    fn 画像の差し替えは追加と削除に1件ずつ入る() {
        let test = PushTest::new();
        let _repo = test.article_synced();
        test.mark_fetched(SLUG, &[("01.png", true)]);
        test.write_image("a/01.png", PNG2);

        let preview = test.preview(SLUG).unwrap();
        assert_eq!(preview.images_added, 1);
        assert_eq!(preview.images_removed, 1);
        assert_eq!(preview.added, 0);
        assert_eq!(preview.removed, 0);
    }

    #[test]
    fn 取ってきていない画像が手元に無いのは数えない() {
        let test = PushTest::new();
        let _repo = test.article_synced();
        test.write_md(A_LOCAL);

        let preview = test.preview(SLUG).unwrap();
        assert_eq!(preview.images_added, 0);
        assert_eq!(preview.images_removed, 0);
    }

    #[test]
    fn 取ってきた画像を消したものは削除に数える() {
        let test = PushTest::new();
        let _repo = test.article_synced();
        test.mark_fetched(SLUG, &[("01.png", true)]);
        test.write_image("a/01.png", PNG);
        fs::remove_file(test.image_path("a/01.png")).unwrap();

        let preview = test.preview(SLUG).unwrap();
        assert_eq!(preview.images_added, 0);
        assert_eq!(preview.images_removed, 1);
        assert_eq!(preview.added, 0);
        assert_eq!(preview.removed, 0);
    }

    #[test]
    fn assetsの下のディレクトリのファイルは数えない() {
        let test = PushTest::new();
        let _repo = test.article_synced();
        test.write_md(A_LOCAL);
        test.write_image("a/sub/03.png", PNG3);

        let preview = test.preview(SLUG).unwrap();
        assert_eq!(preview.images_added, 0);
        assert_eq!(preview.images_removed, 0);
    }

    #[test]
    fn titleはフロントマターから読む() {
        let test = PushTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", NO_A_MAIN);
        test.write_md(&md_bytes("カスタム", "本文\n"));
        assert_eq!(test.preview(SLUG).unwrap().title, "カスタム");

        // titleの無いmdは空
        test.write_md("---\ndescription: \"\"\n---\n本文\n".as_bytes());
        assert_eq!(test.preview(SLUG).unwrap().title, "");
    }

    #[test]
    fn 変更が無ければNothingToCommit() {
        let test = PushTest::new();
        let _repo = test.article_synced();
        assert!(matches!(
            test.preview(SLUG),
            Err(GithubError::NothingToCommit)
        ));
    }

    // Commit & Push ---------------------------------------------------------------

    fn github_files(repo: &FakeRepo) -> BTreeMap<String, (String, Vec<u8>)> {
        repo.tree_files(BRANCH)
    }

    #[test]
    fn 新しい記事はベースの先頭を親にbranchを作る() {
        let test = PushTest::new();
        let repo = test.new_repo();
        let base = repo.branch(&test.transport, "main", NO_A_MAIN);
        repo.accept_writes(&test.transport);
        let a_new = a_new();
        test.write_md(&a_new);
        test.write_image("a/01.png", PNG);

        let result = test.commit_push(SLUG, "最初のcommit", None).unwrap();
        assert_eq!(result.branch, BRANCH);
        // コミットは新しいもので、親はベースの先頭
        let commits = test.post_bodies("/git/commits");
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0]["message"], "最初のcommit");
        assert_eq!(commits[0]["parents"], serde_json::json!([base.clone()]));
        // blobは記事のmdと画像
        let contents = test.blob_contents();
        assert_eq!(contents.len(), 2);
        assert!(contents.contains(&a_new));
        assert!(contents.contains(&PNG.to_vec()));
        // POST /git/refsでbranchを作る
        assert_eq!(test.post_count(&test.refs_url()), 1);
        // ツリーは、ベースの先頭のツリーに記事のパスを重ねたもの
        let trees = test.post_bodies("/git/trees");
        assert_eq!(trees.len(), 1);
        assert_eq!(trees[0]["base_tree"], repo.head_tree("main"));
        let entries = trees[0]["tree"].as_array().unwrap();
        for path in ["src/content/blog/a.md", "src/assets/a/01.png"] {
            assert!(
                entries
                    .iter()
                    .any(|entry| entry["path"] == path && !entry["sha"].is_null()),
                "{path} はあるはず: {entries:?}"
            );
        }

        // state.json: 記録はbranch・新しいコミット・送った手元の内容
        let state = test.state();
        let record = &state["articles"][SLUG];
        assert_eq!(record["branch"], BRANCH);
        assert_eq!(record["commit"], result.commit);
        assert_eq!(record["md"], blob_sha(&a_new));
        assert_eq!(record["assets"]["01.png"]["sha"], blob_sha(PNG));
        assert_eq!(record["assets"]["01.png"]["fetched"], true);
        // リモートの様子: branchが新しいコミットで、親の内容に送ったものを重ねた内容
        let branch = &state["remote"]["branches"][BRANCH];
        assert_eq!(branch["commit"], result.commit);
        assert_eq!(branch["article"]["md"], blob_sha(&a_new));
        assert_eq!(branch["article"]["assets"]["01.png"], blob_sha(PNG));
        // syncedに送ったmdが写り、食い違いの印が無い
        assert_eq!(
            fs::read_to_string(test.synced_path(SLUG)).unwrap(),
            String::from_utf8_lossy(&a_new)
        );
        assert!(state["conflicts"].as_array().unwrap().is_empty());

        // GitHub上のbranchの中身: 記事のファイルのほか、外のファイルとほかの記事が残る
        let files = github_files(&repo);
        assert_eq!(
            path_of(&files, "src/content/blog/a.md"),
            ("100644".to_string(), a_new.clone())
        );
        assert_eq!(
            path_of(&files, "src/assets/a/01.png"),
            ("100644".to_string(), PNG.to_vec())
        );
        assert_eq!(body_of(&files, "README.md"), README);
        assert_eq!(body_of(&files, "src/content/blog/other.md"), OTHER_MD);
        assert_eq!(body_of(&files, "src/assets/other/02.png"), PNG2);

        // 読み込むと、執筆中で変更が無い
        test.sync();
        let article = test.article_state(SLUG);
        assert_eq!(article.category, Category::Drafting);
        assert!(!article.dirty);
        assert!(!article.conflict);
    }

    #[test]
    fn 記録のbranchがある記事は記録したコミットを親にbranchを進める() {
        let test = PushTest::new();
        let (repo, head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);

        let result = test.commit_push(SLUG, "更新", None).unwrap();
        // PATCHでforce:falseを送る。POST /git/refsは無い
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 1);
        assert_eq!(test.post_count(&test.refs_url()), 0);
        let patches = test.patch_bodies("/git/refs/heads/");
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0]["force"], false);
        assert_eq!(patches[0]["sha"], result.commit);
        // 親は記録したコミット
        let commits = test.post_bodies("/git/commits");
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0]["parents"], serde_json::json!([head]));
        let on_github = github_files(&repo);
        assert_eq!(body_of(&on_github, "src/content/blog/a.md"), A_LOCAL);

        // 記録も替わる
        let record = &test.state()["articles"][SLUG];
        assert_eq!(record["branch"], BRANCH);
        assert_eq!(record["commit"], result.commit);
        assert_eq!(record["md"], blob_sha(A_LOCAL));
        // 読み込むと、編集中で変更が無い
        test.sync();
        let article = test.article_state(SLUG);
        assert_eq!(article.category, Category::Editing);
        assert!(!article.dirty);
    }

    #[test]
    fn 編集中の記事はベースの先頭を親にbranchを作り読み込むと編集中で変更無し() {
        let test = PushTest::new();
        let repo = test.article_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);

        let result = test.commit_push(SLUG, "修正", None).unwrap();
        // POST /git/refsで作ったので、PATCHは無い
        assert_eq!(test.post_count(&test.refs_url()), 1);
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 0);

        test.sync();
        let article = test.article_state(SLUG);
        assert_eq!(article.category, Category::Editing);
        assert!(!article.dirty);
        // 記録は、記事用のbranchの先頭に替わる
        let record = &test.state()["articles"][SLUG];
        assert_eq!(record["branch"], BRANCH);
        assert_eq!(record["commit"], result.commit);
        assert_eq!(record["md"], blob_sha(A_LOCAL));
    }

    #[test]
    fn 変えていないファイルはblobを送らない() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);

        test.commit_push(SLUG, "更新", None).unwrap();
        let contents = test.blob_contents();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0], A_LOCAL.to_vec());
    }

    #[test]
    fn 取ってきた画像を手元で差し替えるとツリーで置き換わる() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.mark_fetched(SLUG, &[("01.png", true)]);
        test.write_image("a/01.png", PNG2);

        test.commit_push(SLUG, "画像を差し替える", None).unwrap();
        // 同じパスに消すエントリを重ねない
        let entries = test.post_bodies("/git/trees")[0]["tree"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0]["path"], "src/assets/a/01.png");
        assert_eq!(entries[0]["sha"], blob_sha(PNG2));
        assert_eq!(body_of(&github_files(&repo), "src/assets/a/01.png"), PNG2);
        let state = test.state();
        assert_eq!(
            state["articles"][SLUG]["assets"]["01.png"]["sha"],
            blob_sha(PNG2)
        );
        assert_eq!(
            state["remote"]["branches"][BRANCH]["article"]["assets"]["01.png"],
            blob_sha(PNG2)
        );

        test.sync();
        assert!(!test.article_state(SLUG).dirty);
    }

    #[test]
    fn 取ってきた画像を手元で消すとツリーから外す() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.mark_fetched(SLUG, &[("01.png", true)]);
        test.write_image("a/01.png", PNG);
        fs::remove_file(test.image_path("a/01.png")).unwrap();

        test.commit_push(SLUG, "画像を消す", None).unwrap();
        // blobは送らない
        assert!(test.blob_contents().is_empty());
        // ツリーでは消すエントリがsha:nullになる
        let entries = test.post_bodies("/git/trees")[0]["tree"]
            .as_array()
            .unwrap()
            .clone();
        let removed: Vec<&Value> = entries
            .iter()
            .filter(|entry| entry["sha"].is_null())
            .collect();
        assert_eq!(removed.len(), 1, "{entries:?}");
        assert_eq!(removed[0]["path"], "src/assets/a/01.png");
        // GitHubにも画像が無い
        let on_github = github_files(&repo);
        assert!(!on_github.contains_key("src/assets/a/01.png"));

        // 読み込むと、記録からも消える
        test.sync();
        let record = &test.state()["articles"][SLUG];
        assert!(record["assets"].as_object().unwrap().is_empty());
        assert!(!test.article_state(SLUG).dirty);
    }

    #[test]
    fn 取ってきていない画像はツリーに触らない() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);

        test.commit_push(SLUG, "更新", None).unwrap();
        // ツリーのエントリに画像のパスは無い
        let trees = test.post_bodies("/git/trees");
        let entries = trees[0]["tree"].as_array().unwrap();
        assert!(
            !entries.iter().any(|entry| entry["path"]
                .as_str()
                .is_some_and(|path| path.contains("src/assets/a/"))),
            "{entries:?}"
        );
        // GitHub上には残る
        let on_github = github_files(&repo);
        assert_eq!(body_of(&on_github, "src/assets/a/01.png"), PNG);
        // 記録では取ってきていないもののまま
        test.sync();
        let record = &test.state()["articles"][SLUG];
        assert_eq!(record["assets"]["01.png"]["sha"], blob_sha(PNG));
        assert_eq!(record["assets"]["01.png"]["fetched"], false);
    }

    #[test]
    fn 親にあるファイルのmodeを保つ() {
        let test = PushTest::new();
        let repo = test.new_repo();
        repo.branch_with_entries(
            &test.transport,
            "main",
            &[("src/assets/a/01.png", PNG)],
            &[("src/content/blog/a.md", "100755", "blob")],
        );
        test.sync();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);

        test.commit_push(SLUG, "更新", None).unwrap();
        let entries = test.post_bodies("/git/trees")[0]["tree"]
            .as_array()
            .unwrap()
            .clone();
        let entry = entries
            .iter()
            .find(|entry| entry["path"] == "src/content/blog/a.md")
            .expect("mdのエントリはあるはず");
        assert_eq!(entry["mode"], "100755");
        // GitHub上でもmodeは替わらない
        let on_github = github_files(&repo);
        assert_eq!(path_of(&on_github, "src/content/blog/a.md").0, "100755");
    }

    #[test]
    fn Pushしたら食い違いの印を外す() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.edit_state(|state| {
            state["conflicts"] = serde_json::json!(["a"]);
        });
        test.write_md(A_LOCAL);

        test.commit_push(SLUG, "更新", None).unwrap();
        let state = test.state();
        assert!(state["conflicts"].as_array().unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(test.synced_path(SLUG)).unwrap(),
            String::from_utf8_lossy(A_LOCAL)
        );
    }

    #[test]
    fn branchを進められずリモートの変更と重ならなければ作り直す() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.mark_fetched(SLUG, &[("01.png", true)]);
        test.write_image("a/01.png", PNG);
        test.write_md(A_LOCAL);
        let other = Arc::new(Mutex::new(Vec::new()));
        branch_hook(&repo, &test, Arc::clone(&other), 1, OTHER_ADDS_IMAGE);

        test.commit_push(SLUG, "更新", None).unwrap();
        // PATCHは2回。2回目が通る
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 2);
        assert_eq!(other.lock().unwrap().len(), 1);
        // GitHub上には、手元のmdの変更と、ほかの端末が足した画像の両方がある
        let on_github = github_files(&repo);
        assert_eq!(body_of(&on_github, "src/content/blog/a.md"), A_LOCAL);
        assert_eq!(body_of(&on_github, "src/assets/a/01.png"), PNG);
        assert_eq!(body_of(&on_github, "src/assets/a/02.png"), PNG2);

        // 読み込む前: 記録は手元の内容のままで、リモートの様子は取り込んだ内容
        let state = test.state();
        let record = &state["articles"][SLUG];
        assert_eq!(record["md"], blob_sha(A_LOCAL));
        assert!(record["assets"].get("02.png").is_none(), "{record}");
        let branch = &state["remote"]["branches"][BRANCH]["article"];
        assert_eq!(branch["md"], blob_sha(A_LOCAL));
        assert_eq!(branch["assets"]["02.png"], blob_sha(PNG2));

        // 読み込みの後: 手元にmdの変更が残り、足された画像は取ってきていないものの記録で、
        // 変更無し
        test.sync();
        let record = &test.state()["articles"][SLUG];
        assert_eq!(record["md"], blob_sha(A_LOCAL));
        assert_eq!(record["assets"]["01.png"]["fetched"], true);
        assert_eq!(record["assets"]["02.png"]["fetched"], false);
        assert!(!test.article_state(SLUG).dirty);
        assert!(test.state()["conflicts"].as_array().unwrap().is_empty());
    }

    #[test]
    fn リモートで消えた画像は作り直しの後の読み込みで手元からも消える() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.mark_fetched(SLUG, &[("01.png", true)]);
        test.write_image("a/01.png", PNG);
        test.write_md(A_LOCAL);
        branch_hook(
            &repo,
            &test,
            Arc::new(Mutex::new(Vec::new())),
            1,
            OTHER_REMOVES_IMAGE,
        );

        test.commit_push(SLUG, "更新", None).unwrap();
        // GitHub上では、ほかの端末が消したまま
        assert!(!github_files(&repo).contains_key("src/assets/a/01.png"));

        // 読み込むと、手元からも消える
        test.sync();
        assert!(!test.image_path("a/01.png").exists());
        assert!(!test.article_state(SLUG).dirty);
    }

    #[test]
    fn 同じ中身への変更は重なりとみなさない() {
        // 手元とリモートを同じmdにする
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        branch_hook(
            &repo,
            &test,
            Arc::new(Mutex::new(Vec::new())),
            1,
            OTHER_MAKES_SAME_MD,
        );

        test.commit_push(SLUG, "更新", None).unwrap();
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 2);
        let on_github = github_files(&repo);
        assert_eq!(body_of(&on_github, "src/content/blog/a.md"), A_LOCAL);

        // 両方で同じ画像を消しても同じ
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.mark_fetched(SLUG, &[("01.png", true)]);
        test.write_image("a/01.png", PNG);
        fs::remove_file(test.image_path("a/01.png")).unwrap();
        branch_hook(
            &repo,
            &test,
            Arc::new(Mutex::new(Vec::new())),
            1,
            OTHER_REMOVES_IMAGE,
        );

        test.commit_push(SLUG, "更新", None).unwrap();
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 2);
        assert!(!github_files(&repo).contains_key("src/assets/a/01.png"));
    }

    #[test]
    fn 手元で消した画像をリモートで替えていればConflict() {
        let test = PushTest::new();
        let (repo, head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.mark_fetched(SLUG, &[("01.png", true)]);
        test.write_image("a/01.png", PNG);
        fs::remove_file(test.image_path("a/01.png")).unwrap();
        let other = Arc::new(Mutex::new(Vec::new()));
        branch_hook(&repo, &test, Arc::clone(&other), 1, OTHER_REPLACES_IMAGE);

        let error = test.commit_push(SLUG, "更新", None).unwrap_err();
        match error {
            GithubError::Conflict { head, md, images } => {
                assert_eq!(head, other.lock().unwrap()[0]);
                assert!(!md);
                assert_eq!(images, vec!["01.png".to_string()]);
            }
            other => panic!("Conflictのはず: {other:?}"),
        }
        // 記録のcommitは替わらず、手元で消したことは変更として残る
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 1);
        assert_eq!(test.state()["articles"][SLUG]["commit"], head);
        assert!(test.article_state(SLUG).dirty);
    }

    #[test]
    fn 手元で替えた画像をリモートで消していればConflict() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.mark_fetched(SLUG, &[("01.png", true)]);
        test.write_image("a/01.png", PNG2);
        branch_hook(
            &repo,
            &test,
            Arc::new(Mutex::new(Vec::new())),
            1,
            OTHER_REMOVES_IMAGE,
        );

        let error = test.commit_push(SLUG, "更新", None).unwrap_err();
        match error {
            GithubError::Conflict { md, images, .. } => {
                assert!(!md);
                assert_eq!(images, vec!["01.png".to_string()]);
            }
            other => panic!("Conflictのはず: {other:?}"),
        }
    }

    #[test]
    fn 重なればConflictで止めて記録もGitHubのbranchも替えない() {
        let test = PushTest::new();
        let (repo, head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        let other = Arc::new(Mutex::new(Vec::new()));
        branch_hook(&repo, &test, Arc::clone(&other), 1, OTHER_CHANGES_MD);

        let error = test.commit_push(SLUG, "更新", None).unwrap_err();
        match error {
            GithubError::Conflict { head, md, images } => {
                assert_eq!(head, other.lock().unwrap()[0]);
                assert!(md);
                assert!(images.is_empty(), "{images:?}");
            }
            other => panic!("Conflictのはず: {other:?}"),
        }
        // PATCHは1回で、branchの先頭はほかの端末のコミットのまま
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 1);
        let other_head = other.lock().unwrap()[0].clone();
        assert_eq!(repo.head(BRANCH), other_head);
        let on_github = github_files(&repo);
        assert_eq!(body_of(&on_github, "src/content/blog/a.md"), A_REMOTE);
        assert!(!on_github.contains_key("src/assets/a/02.png"));
        // 記録も替わらない
        let record = &test.state()["articles"][SLUG];
        assert_eq!(record["commit"], head);
        assert_eq!(record["md"], blob_sha(A_SYNCED));
    }

    #[test]
    fn ConflictでPushを選ぶと重なったファイルは手元の内容になりほかの変更は残る() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        let other = Arc::new(Mutex::new(Vec::new()));
        branch_hook(&repo, &test, Arc::clone(&other), 1, OTHER_CHANGES_MD_ADDS);

        // 1回目はConflict
        let first = test.commit_push(SLUG, "更新", None).unwrap_err();
        let GithubError::Conflict { head, .. } = first else {
            panic!("Conflictのはず: {first:?}")
        };
        // 2回目はoverwriteにheadを入れる
        let result = test.commit_push(SLUG, "更新", Some(&head)).unwrap();
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 2);
        // 重なったmdは手元の内容になり、ほかのリモートの変更(足された画像)は残る
        let on_github = github_files(&repo);
        assert_eq!(body_of(&on_github, "src/content/blog/a.md"), A_LOCAL);
        assert_eq!(body_of(&on_github, "src/assets/a/01.png"), PNG);
        assert_eq!(body_of(&on_github, "src/assets/a/02.png"), PNG2);

        test.sync();
        let record = &test.state()["articles"][SLUG];
        assert_eq!(record["commit"], result.commit);
        assert_eq!(record["md"], blob_sha(A_LOCAL));
        assert!(!test.article_state(SLUG).dirty);
    }

    #[test]
    fn Pushを選ぶ前にさらに進んで重なっていればもう一度Conflict() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        let other = Arc::new(Mutex::new(Vec::new()));
        branch_hook(&repo, &test, Arc::clone(&other), 1, OTHER_CHANGES_MD);
        branch_hook(&repo, &test, Arc::clone(&other), 2, OTHER_CHANGES_MD2);

        let first = test.commit_push(SLUG, "更新", None).unwrap_err();
        let GithubError::Conflict { head, .. } = first else {
            panic!("Conflictのはず: {first:?}")
        };
        let second = test.commit_push(SLUG, "更新", Some(&head)).unwrap_err();
        match second {
            GithubError::Conflict {
                head: new_head, md, ..
            } => {
                assert_eq!(new_head, other.lock().unwrap()[1]);
                assert!(md);
            }
            other => panic!("Conflictのはず: {other:?}"),
        }
    }

    #[test]
    fn 記録のbranchが無くベースの先頭でその記事が変わっていればConflict() {
        let test = PushTest::new();
        let repo = test.article_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        let base = repo.branch(&test.transport, "main", BASE_A_CHANGED);

        let error = test.commit_push(SLUG, "更新", None).unwrap_err();
        match error {
            GithubError::Conflict { head, md, images } => {
                assert_eq!(head, base);
                assert!(md);
                assert!(images.is_empty());
            }
            other => panic!("Conflictのはず: {other:?}"),
        }
        // branchは作らない
        assert_eq!(test.post_count(&test.refs_url()), 0);
        // 記録も替わらない
        let record = &test.state()["articles"][SLUG];
        assert!(record["branch"].is_null());
        assert_eq!(record["md"], blob_sha(A_SYNCED));
    }

    #[test]
    fn Pushを選んだ後に重ならない変更で進んでいれば作り直す() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        branch_hook(
            &repo,
            &test,
            Arc::new(Mutex::new(Vec::new())),
            1,
            OTHER_CHANGES_MD,
        );
        // 確かめてもらった後に、mdはそのままで画像だけ足す
        branch_hook(
            &repo,
            &test,
            Arc::new(Mutex::new(Vec::new())),
            2,
            OTHER_CHANGES_MD_ADDS,
        );

        let GithubError::Conflict { head, .. } = test.commit_push(SLUG, "更新", None).unwrap_err()
        else {
            panic!("Conflictのはず")
        };
        test.commit_push(SLUG, "更新", Some(&head)).unwrap();
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 3);
        let on_github = github_files(&repo);
        assert_eq!(body_of(&on_github, "src/content/blog/a.md"), A_LOCAL);
        assert_eq!(body_of(&on_github, "src/assets/a/02.png"), PNG2);
    }

    #[test]
    fn 記録のbranchが無い記事のConflictでPushを選ぶとbranchを作る() {
        let test = PushTest::new();
        let repo = test.article_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        let base = repo.branch(&test.transport, "main", BASE_A_CHANGED);

        let GithubError::Conflict { head, .. } = test.commit_push(SLUG, "修正", None).unwrap_err()
        else {
            panic!("Conflictのはず")
        };
        assert_eq!(head, base);
        test.commit_push(SLUG, "修正", Some(&head)).unwrap();
        // 親はベースの先頭で、ベースは替えない
        let commits = test.post_bodies("/git/commits");
        assert_eq!(
            commits.last().unwrap()["parents"],
            serde_json::json!([base])
        );
        assert_eq!(test.post_count(&test.refs_url()), 1);
        assert_eq!(repo.head("main"), base);
        assert_eq!(
            body_of(&github_files(&repo), "src/content/blog/a.md"),
            A_LOCAL
        );

        test.sync();
        let article = test.article_state(SLUG);
        assert_eq!(article.category, Category::Editing);
        assert!(!article.dirty);
    }

    #[test]
    fn 記録のbranchが無くベースでほかの記事だけが変わっていれば作る() {
        let test = PushTest::new();
        let repo = test.article_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        let base = repo.branch(&test.transport, "main", BASE_OTHER_CHANGED);

        test.commit_push(SLUG, "更新", None).unwrap();
        // 親は進んだベースの先頭で、ほかの記事の変更が残る
        let commits = test.post_bodies("/git/commits");
        assert_eq!(commits[0]["parents"], serde_json::json!([base]));
        let on_github = github_files(&repo);
        assert_eq!(body_of(&on_github, "src/content/blog/a.md"), A_LOCAL);
        assert_eq!(body_of(&on_github, "src/content/blog/other.md"), OTHER2_MD);
    }

    #[test]
    fn 記録の無い新しい記事と同じslugの記事がベースにあればConflict() {
        let test = PushTest::new();
        let repo = test.new_repo();
        let base = repo.branch(&test.transport, "main", ONLY_A_MAIN);
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);

        let error = test.commit_push(SLUG, "最初のcommit", None).unwrap_err();
        match error {
            GithubError::Conflict { head, md, .. } => {
                assert_eq!(head, base);
                assert!(md);
            }
            other => panic!("Conflictのはず: {other:?}"),
        }
        // blobもbranchも作らない
        assert!(test.blob_contents().is_empty());
        assert_eq!(test.post_count(&test.refs_url()), 0);
    }

    #[test]
    fn 記事用のbranchが既にあればBranchExists() {
        let test = PushTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", NO_A_MAIN);
        let _existing = repo.branch(&test.transport, BRANCH, MAIN_FILES);
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);

        let error = test.commit_push(SLUG, "最初のcommit", None).unwrap_err();
        match error {
            GithubError::BranchExists { branch } => assert_eq!(branch, BRANCH),
            other => panic!("BranchExistsのはず: {other:?}"),
        }
        // 記録を替えない。既にstate.jsonが無いことで確かめる
        assert!(!test.state_path.exists());
    }

    #[test]
    fn 記録したbranchが消えていればBranchGone() {
        // 押したときに既に消えていれば、何も作らない
        let test = PushTest::new();
        let (repo, head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        repo.delete_branch(BRANCH);
        test.write_md(A_LOCAL);

        assert!(matches!(
            test.commit_push(SLUG, "更新", None),
            Err(GithubError::BranchGone)
        ));
        assert!(test.post_bodies("/git/").is_empty());
        // 記録を替えない
        let record = &test.state()["articles"][SLUG];
        assert_eq!(record["branch"], BRANCH);
        assert_eq!(record["commit"], head);

        // PATCHの直前に消えたとき(PATCHが422で、読み直したrefが404)
        let test = PushTest::new();
        let (repo, head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        let hook_repo = repo.clone();
        repo.before(Method::Patch, &test.patch_url(BRANCH), 1, move || {
            hook_repo.delete_branch(BRANCH);
        });
        test.write_md(A_LOCAL);

        assert!(matches!(
            test.commit_push(SLUG, "更新", None),
            Err(GithubError::BranchGone)
        ));
        let record = &test.state()["articles"][SLUG];
        assert_eq!(record["branch"], BRANCH);
        assert_eq!(record["commit"], head);
    }

    #[test]
    fn 先頭が親と同じまま進められなければ元のエラー() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        // branchはあるまま、PATCHを422で断る
        test.transport.json(
            Method::Patch,
            &test.patch_url(BRANCH),
            422,
            serde_json::json!({ "message": "failed" }),
        );
        test.write_md(A_LOCAL);

        let error = test.commit_push(SLUG, "更新", None).unwrap_err();
        assert!(
            matches!(error, GithubError::Status { status: 422 }),
            "元のエラーのはず: {error:?}"
        );
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 1);
    }

    #[test]
    fn 作り直しが3回を超えたらPushRetryExceeded() {
        let test = PushTest::new();
        let (repo, head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        let other = Arc::new(Mutex::new(Vec::new()));
        branch_hook(&repo, &test, Arc::clone(&other), 1, HEAD_ADVANCED);
        branch_hook(&repo, &test, Arc::clone(&other), 2, HEAD_ADVANCED);
        branch_hook(&repo, &test, Arc::clone(&other), 3, HEAD_ADVANCED);
        branch_hook(&repo, &test, Arc::clone(&other), 4, HEAD_ADVANCED);

        let error = test.commit_push(SLUG, "更新", None).unwrap_err();
        assert!(
            matches!(error, GithubError::PushRetryExceeded),
            "PushRetryExceededのはず: {error:?}"
        );
        // PATCHはちょうど4回で、branchの先頭はほかの端末のコミットのまま
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 4);
        let others = other.lock().unwrap().clone();
        assert_eq!(others.len(), 4);
        assert_eq!(repo.head(BRANCH), others[3]);
        let on_github = github_files(&repo);
        assert_eq!(body_of(&on_github, "src/content/blog/a.md"), A_SYNCED);
        // それぞれの親は、その前にほかの端末が進めた先頭(最初は記録のコミット)
        let parents: Vec<Value> = test
            .post_bodies("/git/commits")
            .iter()
            .map(|body| body["parents"].clone())
            .collect();
        let expected = [
            serde_json::json!([head]),
            serde_json::json!([others[0]]),
            serde_json::json!([others[1]]),
            serde_json::json!([others[2]]),
        ];
        assert_eq!(parents, expected, "{:?} vs {expected:?}", parents[0]);
    }

    #[test]
    fn 作り直しの3回目が進められれば成功する() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        branch_hook(
            &repo,
            &test,
            Arc::new(Mutex::new(Vec::new())),
            1,
            HEAD_ADVANCED,
        );
        branch_hook(
            &repo,
            &test,
            Arc::new(Mutex::new(Vec::new())),
            2,
            HEAD_ADVANCED,
        );
        branch_hook(
            &repo,
            &test,
            Arc::new(Mutex::new(Vec::new())),
            3,
            HEAD_ADVANCED,
        );

        let result = test.commit_push(SLUG, "更新", None).unwrap();
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 4);
        // 読み込むと、変更が無くなる
        test.sync();
        let article = test.article_state(SLUG);
        assert!(!article.dirty);
        assert_eq!(
            body_of(&github_files(&repo), "src/content/blog/a.md"),
            A_LOCAL
        );
        assert_eq!(test.state()["articles"][SLUG]["commit"], result.commit);
    }

    #[test]
    fn 通信の途中で失敗したら記録を替えない() {
        let test = PushTest::new();
        let (repo, head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        // POST /git/treesを500にする
        test.transport.json(
            Method::Post,
            &test.trees_url(),
            500,
            serde_json::json!({ "message": "failed" }),
        );
        test.write_md(A_LOCAL);
        let state_before = fs::read(&test.state_path).unwrap();

        assert!(matches!(
            test.commit_push(SLUG, "更新", None),
            Err(GithubError::Status { status: 500 })
        ));
        // state.jsonもGitHubも替わらない
        assert_eq!(fs::read(&test.state_path).unwrap(), state_before);
        let on_github = github_files(&repo);
        assert_eq!(body_of(&on_github, "src/content/blog/a.md"), A_SYNCED);
        assert_eq!(repo.head(BRANCH), head);
    }

    #[test]
    fn 通信の間に向き先が替わったら記録を書かずSettingsChanged() {
        let test = PushTest::new();
        let (repo, head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        let state_before = fs::read(&test.state_path).unwrap();
        let store = Arc::clone(&test.store);
        let posts_dir = test.posts_dir.clone();
        repo.before(Method::Patch, &test.patch_url(BRANCH), 1, move || {
            // 通信の間に同じ向き先でset_posts_dirを呼び、世代を進める
            store.set_posts_dir(posts_dir.clone()).unwrap();
        });

        assert!(matches!(
            test.commit_push(SLUG, "更新", None),
            Err(GithubError::SettingsChanged)
        ));
        assert_eq!(fs::read(&test.state_path).unwrap(), state_before);
        // GitHub上のbranchは進んだまま残る
        assert_ne!(repo.head(BRANCH), head);
        assert_eq!(
            body_of(&github_files(&repo), "src/content/blog/a.md"),
            A_LOCAL
        );
        // 読み込み直すと、手元がGitHub上と同じなので記録が直る
        test.sync();
        assert!(!test.article_state(SLUG).dirty);
        assert_eq!(test.state()["articles"][SLUG]["md"], blob_sha(A_LOCAL));
    }

    #[test]
    fn 通信の間に書かれたほかの記事の記録を消さない() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        let state_path = test.state_path.clone();
        repo.before(Method::Patch, &test.patch_url(BRANCH), 1, move || {
            // 通信の間に、ほかの記事の記録のfetchedを書き換える
            let mut state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
            state["articles"]["other"]["assets"]["02.png"]["fetched"] = Value::Bool(true);
            fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        });

        test.commit_push(SLUG, "更新", None).unwrap();
        // 書き換えられたほかの記事の記録は残り、この記事の記録は新しいもの
        let state = test.state();
        assert_eq!(
            state["articles"]["other"]["assets"]["02.png"]["fetched"],
            true
        );
        assert_eq!(state["articles"][SLUG]["md"], blob_sha(A_LOCAL));
        assert!(!test.article_state(SLUG).dirty);
    }

    #[test]
    fn state_jsonが読めなければStateUnreadableで通信もしない() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        fs::write(&test.state_path, "{").unwrap();
        let requests = test.transport.requests().len();

        assert!(matches!(
            test.commit_push(SLUG, "更新", None),
            Err(GithubError::StateUnreadable)
        ));
        // 通信は始まっていない
        assert_eq!(test.transport.requests().len(), requests);
        assert_eq!(fs::read(&test.state_path).unwrap(), b"{");
    }

    // Pull -----------------------------------------------------------------------

    #[test]
    fn Pullは手元をバックアップに写してから記録を書き読み込みでリモートの内容になる() {
        let test = PushTest::new();
        let (_repo, head) = test.branch_synced();
        test.mark_fetched(SLUG, &[("01.png", true)]);
        test.write_image("a/01.png", PNG);
        test.write_md(A_LOCAL);

        test.pull(SLUG, None, BACKUP_NAME).unwrap();
        // 手元のファイルをバックアップに写す
        let backup_md = fs::read(test.backup_path(BACKUP_NAME, "src/content/blog/a.md")).unwrap();
        assert_eq!(backup_md, A_LOCAL);
        let backup_png = fs::read(test.backup_path(BACKUP_NAME, "src/assets/a/01.png")).unwrap();
        assert_eq!(backup_png, PNG);
        // 記録はbranch・読んだ先頭・手元の内容(取ってきたものとして)
        let record = &test.state()["articles"][SLUG];
        assert_eq!(record["branch"], BRANCH);
        assert_eq!(record["commit"], head);
        assert_eq!(record["md"], blob_sha(A_LOCAL));
        assert_eq!(record["assets"]["01.png"]["fetched"], true);
        // 手元のファイルは読み込みまで替えない
        assert_eq!(fs::read(test.md_path(SLUG)).unwrap(), A_LOCAL);

        // 読み込むと、手元がリモートの内容に替わり、変更が無くなり食い違いの印は無い
        test.sync();
        assert_eq!(fs::read(test.md_path(SLUG)).unwrap(), A_SYNCED);
        assert!(!test.article_state(SLUG).dirty);
        assert!(test.state()["conflicts"].as_array().unwrap().is_empty());
    }

    #[test]
    fn branchを渡したPullはそのbranchの内容の記録になる() {
        let test = PushTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", NO_A_MAIN);
        let head = repo.branch(
            &test.transport,
            BRANCH,
            &[
                ("src/content/blog/a.md", A_SYNCED),
                ("src/assets/a/01.png", PNG),
            ],
        );
        test.write_md(A_LOCAL);

        test.pull(SLUG, Some(BRANCH), BACKUP_NAME).unwrap();
        let record = &test.state()["articles"][SLUG];
        assert_eq!(record["branch"], BRANCH);
        assert_eq!(record["commit"], head);
        assert_eq!(record["md"], blob_sha(A_LOCAL));

        // 読み込むと、branchの内容になる
        test.sync();
        assert_eq!(fs::read(test.md_path(SLUG)).unwrap(), A_SYNCED);
        assert!(!test.article_state(SLUG).dirty);
        let record = &test.state()["articles"][SLUG];
        assert_eq!(record["branch"], BRANCH);
        assert_eq!(record["commit"], head);
        assert_eq!(record["md"], blob_sha(A_SYNCED));
    }

    #[test]
    fn 引き渡しのbranchが無ければベースのPullはベースの内容になる() {
        let test = PushTest::new();
        let _repo = test.article_synced();
        test.write_md(A_LOCAL);

        test.pull(SLUG, None, BACKUP_NAME).unwrap();
        let record = &test.state()["articles"][SLUG];
        assert!(record["branch"].is_null());
        assert_eq!(record["md"], blob_sha(A_LOCAL));

        // 読み込むと、ベースの内容になる
        test.sync();
        assert_eq!(fs::read(test.md_path(SLUG)).unwrap(), A_SYNCED);
        assert!(!test.article_state(SLUG).dirty);
        assert_eq!(test.state()["articles"][SLUG]["md"], blob_sha(A_SYNCED));
    }

    #[test]
    fn 記事が無ければArticleNotFoundでバックアップも記録も替えない() {
        let test = PushTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", NO_A_MAIN);
        test.write_md(A_LOCAL);

        assert!(matches!(
            test.pull(SLUG, None, BACKUP_NAME),
            Err(GithubError::ArticleNotFound)
        ));
        assert!(!test.state_path.exists());
        assert!(!test.backup_root().join(BACKUP_NAME).exists());
    }

    #[test]
    fn バックアップに写すのは手元のmdとassetsの直下の画像() {
        let test = PushTest::new();
        let _repo = test.article_synced();
        // 取ってきた画像と、記録に無い画像と、下のディレクトリの画像を置く
        test.mark_fetched(SLUG, &[("01.png", true)]);
        test.write_image("a/01.png", PNG);
        test.write_image("a/05.png", PNG3);
        test.write_image("a/sub/06.png", PNG2);

        test.pull(SLUG, None, BACKUP_NAME).unwrap();
        let backup_md = fs::read(test.backup_path(BACKUP_NAME, "src/content/blog/a.md")).unwrap();
        assert_eq!(backup_md, A_SYNCED);
        assert_eq!(
            fs::read(test.backup_path(BACKUP_NAME, "src/assets/a/01.png")).unwrap(),
            PNG
        );
        // 記録に無い画像も写す
        assert_eq!(
            fs::read(test.backup_path(BACKUP_NAME, "src/assets/a/05.png")).unwrap(),
            PNG3
        );
        // 下のディレクトリの画像は写さない
        assert!(!test
            .backup_path(BACKUP_NAME, "src/assets/a/sub/06.png")
            .exists());
    }

    #[test]
    fn バックアップが書けなければ記録を替えない() {
        let test = PushTest::new();
        let (_repo, head) = test.branch_synced();
        test.write_md(A_LOCAL);
        let state_before = fs::read(&test.state_path).unwrap();
        fs::create_dir_all(test.backup_root()).unwrap();
        // backupのディレクトリを読み取り専用にする
        let Some(read_only) = read_only_dir(&test.backup_root()) else {
            return;
        };
        let result = test.pull(SLUG, None, BACKUP_NAME);
        drop(read_only);

        assert!(result.is_err());
        // 記録も替わらない。branchは読んだ先頭のまま
        assert_eq!(
            fs::read(&test.state_path).unwrap(),
            state_before,
            "state.jsonが替わっていないはず"
        );
        assert_eq!(
            test.state()["articles"][SLUG]["commit"],
            head,
            "記録の先頭が替わっていないはず"
        );
        // バックアップも無い
        assert!(!test.backup_root().join(BACKUP_NAME).exists());
    }

    #[test]
    fn 同じ日時のバックアップがあれば2を付ける() {
        let test = PushTest::new();
        let _repo = test.article_synced();
        test.write_md(A_LOCAL);
        // 同じ名前のバックアップが既にある
        fs::create_dir_all(test.backup_path(BACKUP_NAME, "src")).unwrap();

        test.pull(SLUG, None, BACKUP_NAME).unwrap();
        let second_md = fs::read(
            test.backup_path(format!("{BACKUP_NAME}-2").as_str(), "src/content/blog/a.md"),
        )
        .unwrap();
        assert_eq!(second_md, A_LOCAL);
        assert!(test.backup_path(BACKUP_NAME, "src").exists());
        assert_eq!(test.state()["articles"][SLUG]["md"], blob_sha(A_LOCAL));
    }

    // #66: slugを変えた記事 ----------------------------------------------------

    const NEW_SLUG: &str = "b";
    const NEW_BRANCH: &str = "blog/b";

    impl PushTest {
        fn write_md_to(&self, slug: &str, content: &[u8]) {
            let path = self.md_path(slug);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn ref_url(&self, branch: &str) -> String {
            let mut segments = vec!["repos", "owner", "blog", "git", "refs", "heads"];
            segments.extend(branch.split('/'));
            url_of(&segments, &[])
        }

        fn delete_count(&self, url: &str) -> usize {
            self.transport
                .requests()
                .iter()
                .map(|request| (request.method == Method::Delete && request.url == url) as usize)
                .sum()
        }

        fn branch_exists(&self, branch: &str) -> bool {
            git::branch_head(self.transport.as_ref(), TOKEN, NOW, &self.repo_of, branch)
                .unwrap()
                .is_some()
        }

        fn tree_entries(&self) -> Vec<Value> {
            self.post_bodies("/git/trees")[0]["tree"]
                .as_array()
                .unwrap()
                .clone()
        }

        // slugを変えた後の記録と手元を作る(article::renameの実装に頼らず、記録と手元を直接動かす)
        fn renamed(&self) {
            fs::rename(self.md_path(SLUG), self.md_path(NEW_SLUG)).unwrap();
            let old_assets = self.assets_base.join(SLUG);
            if old_assets.exists() {
                fs::rename(old_assets, self.assets_base.join(NEW_SLUG)).unwrap();
            }
            if self.synced_path(SLUG).exists() {
                fs::rename(self.synced_path(SLUG), self.synced_path(NEW_SLUG)).unwrap();
            }
            self.edit_state(|state| {
                let mut record = state["articles"][SLUG].clone();
                record["renamedFrom"] = Value::String(SLUG.to_string());
                state["articles"][NEW_SLUG] = record;
                state["articles"].as_object_mut().unwrap().remove(SLUG);
                let conflicts: Vec<Value> = state["conflicts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|slug| {
                        if slug.as_str() == Some(SLUG) {
                            Value::String(NEW_SLUG.to_string())
                        } else {
                            slug.clone()
                        }
                    })
                    .collect();
                state["conflicts"] = Value::Array(conflicts);
            });
        }

        // 記事aがmainに無く(執筆中)ブランチ blog/a にあり、aをbに変えた後の記録と手元。
        // headはblog/aの先頭
        fn renamed_synced(&self) -> (FakeRepo, String) {
            let repo = self.new_repo();
            repo.branch(&self.transport, "main", NO_A_MAIN);
            let head = repo.branch(&self.transport, BRANCH, MAIN_FILES);
            self.sync();
            self.renamed();
            (repo, head)
        }
    }

    #[test]
    fn 改名した記事のpreviewは古い名前と古いブランチを出し中身が同じでも送れる() {
        let test = PushTest::new();
        let _renamed = test.renamed_synced();

        let preview = test.preview(NEW_SLUG).unwrap();

        assert_eq!(preview.md_name, "b.md");
        assert_eq!(preview.old_md_name.as_deref(), Some("a.md"));
        assert_eq!(preview.old_branch.as_deref(), Some(BRANCH));
        assert_eq!(preview.kind, CommitKind::Update);
        // 中身を変えていなくても送れる。行数は増減なし
        assert_eq!(preview.added, 0);
        assert_eq!(preview.removed, 0);
        assert_eq!(preview.images_added, 0);
        assert_eq!(preview.images_removed, 0);
    }

    #[test]
    fn 改名していない記事のpreviewは古い名前を出さない() {
        let test = PushTest::new();
        // 記事用のブランチがあっても、改名していなければ古いブランチは出さない
        let _repo = test.branch_synced();
        test.write_md(A_LOCAL);

        let preview = test.preview(SLUG).unwrap();

        assert_eq!(preview.md_name, "a.md");
        assert_eq!(preview.old_md_name, None);
        assert_eq!(preview.old_branch, None);
    }

    #[test]
    fn 改名した記事のPushは古いパスを外した新しいブランチを作り古いブランチを消す() {
        let test = PushTest::new();
        let (repo, head) = test.renamed_synced();
        repo.accept_writes(&test.transport);

        let result = test.commit_push(NEW_SLUG, "更新", None).unwrap();

        // 親は古いブランチの先頭
        let commits = test.post_bodies("/git/commits");
        assert_eq!(commits[0]["parents"], serde_json::json!([head]));
        assert_eq!(result.branch, NEW_BRANCH);
        // ツリー: 古いパスを外し、新しいパスに足す
        let entries = test.tree_entries();
        let entry = |path: &str| {
            entries
                .iter()
                .find(|entry| entry["path"] == path)
                .unwrap_or_else(|| panic!("{path} のエントリはあるはず: {entries:?}"))
                .clone()
        };
        assert!(entry("src/content/blog/a.md")["sha"].is_null());
        assert!(entry("src/assets/a/01.png")["sha"].is_null());
        assert_eq!(entry("src/content/blog/b.md")["sha"], blob_sha(A_SYNCED));
        assert_eq!(entry("src/assets/b/01.png")["sha"], blob_sha(PNG));
        // GitHub上: 新しいパスだけになる
        let files = repo.tree_files(NEW_BRANCH);
        assert!(!files.contains_key("src/content/blog/a.md"));
        assert!(!files.contains_key("src/assets/a/01.png"));
        assert_eq!(body_of(&files, "src/content/blog/b.md"), A_SYNCED);
        assert_eq!(body_of(&files, "src/assets/b/01.png"), PNG);
        // 古いブランチを消す
        assert_eq!(test.delete_count(&test.ref_url(BRANCH)), 1);
        assert!(!test.branch_exists(BRANCH));
    }

    #[test]
    fn 改名した記事のPushは変えていないファイルのblobを送らず下のディレクトリも移す() {
        let test = PushTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", NO_A_MAIN);
        repo.branch(
            &test.transport,
            BRANCH,
            &[
                ("src/content/blog/a.md", A_SYNCED),
                ("src/assets/a/01.png", PNG),
                ("src/assets/a/sub/02.png", PNG2),
            ],
        );
        test.sync();
        test.renamed();
        repo.accept_writes(&test.transport);

        test.commit_push(NEW_SLUG, "更新", None).unwrap();

        // mdも画像も中身は変わっていないのでblobを送らない
        assert!(test.blob_contents().is_empty());
        let entries = test.tree_entries();
        let sha_of = |path: &str| {
            entries
                .iter()
                .find(|entry| entry["path"] == path)
                .unwrap_or_else(|| panic!("{path} のエントリはあるはず: {entries:?}"))["sha"]
                .clone()
        };
        // 親のblobのSHAをそのまま使う。下のディレクトリのファイルも新しいslugへ移る
        assert_eq!(sha_of("src/content/blog/b.md"), blob_sha(A_SYNCED));
        assert_eq!(sha_of("src/assets/b/01.png"), blob_sha(PNG));
        assert_eq!(sha_of("src/assets/b/sub/02.png"), blob_sha(PNG2));
        // 古いパスは外す
        for path in [
            "src/content/blog/a.md",
            "src/assets/a/01.png",
            "src/assets/a/sub/02.png",
        ] {
            assert!(sha_of(path).is_null(), "{path}");
        }
    }

    #[test]
    fn 改名した記事のPush後の記録とリモートの様子が新しいslugになる() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        repo.accept_writes(&test.transport);

        let result = test.commit_push(NEW_SLUG, "更新", None).unwrap();

        let state = test.state();
        let record = &state["articles"][NEW_SLUG];
        assert_eq!(record["branch"], NEW_BRANCH);
        assert_eq!(record["commit"], result.commit);
        assert!(record.get("renamedFrom").is_none(), "{record}");
        let branches = state["remote"]["branches"].as_object().unwrap();
        assert!(branches.contains_key(NEW_BRANCH));
        assert!(!branches.contains_key(BRANCH));
        // 続く読み込み: 古いslugの記事は出ず、新しいslugは変更無し
        test.sync();
        assert!(!test.md_path(SLUG).exists());
        let article = test.article_state(NEW_SLUG);
        assert!(!article.dirty);
        assert!(!article.conflict);
    }

    #[test]
    fn 古いブランチが重ならずに進んでいればその先頭を親に作りリモートの変更も移す() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        // ほかの端末が古いブランチに画像を足す
        let advanced = repo.branch(
            &test.transport,
            BRANCH,
            &[
                ("README.md", README),
                ("src/content/blog/other.md", OTHER_MD),
                ("src/assets/other/02.png", PNG2),
                ("src/content/blog/a.md", A_SYNCED),
                ("src/assets/a/01.png", PNG),
                ("src/assets/a/02.png", PNG2),
            ],
        );
        repo.accept_writes(&test.transport);

        test.commit_push(NEW_SLUG, "更新", None).unwrap();

        let commits = test.post_bodies("/git/commits");
        assert_eq!(commits[0]["parents"], serde_json::json!([advanced]));
        // リモートで足された画像も新しいslugの下に入る
        let files = repo.tree_files(NEW_BRANCH);
        assert_eq!(body_of(&files, "src/assets/b/01.png"), PNG);
        assert_eq!(body_of(&files, "src/assets/b/02.png"), PNG2);
        assert_eq!(body_of(&files, "src/content/blog/b.md"), A_SYNCED);
        assert!(!files.contains_key("src/content/blog/a.md"));
        assert!(!files.contains_key("src/assets/a/01.png"));
    }

    #[test]
    fn 古いブランチが重なって進んでいればConflictでPushを選ぶとその先頭を親にする() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        test.write_md_to(NEW_SLUG, A_LOCAL);
        let advanced = repo.branch(&test.transport, BRANCH, OTHER_CHANGES_MD);
        repo.accept_writes(&test.transport);

        let error = test.commit_push(NEW_SLUG, "更新", None).unwrap_err();
        match error {
            GithubError::Conflict { head, md, images } => {
                assert_eq!(head, advanced);
                assert!(md);
                assert!(images.is_empty(), "{images:?}");
            }
            other => panic!("Conflictのはず: {other:?}"),
        }
        // Pushを選ぶと、その先頭を親に作り直す
        test.commit_push(NEW_SLUG, "更新", Some(&advanced)).unwrap();
        let commits = test.post_bodies("/git/commits");
        assert_eq!(
            commits.last().unwrap()["parents"],
            serde_json::json!([advanced])
        );
        let files = repo.tree_files(NEW_BRANCH);
        assert_eq!(body_of(&files, "src/content/blog/b.md"), A_LOCAL);
        assert!(!files.contains_key("src/content/blog/a.md"));
    }

    #[test]
    fn 古いブランチが消えていればBranchGoneで何も作らない() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        repo.accept_writes(&test.transport);
        repo.delete_branch(BRANCH);

        assert!(matches!(
            test.commit_push(NEW_SLUG, "更新", None),
            Err(GithubError::BranchGone)
        ));
        assert!(test.post_bodies("/git/").is_empty());
    }

    #[test]
    fn 新しいslugのブランチが既にあればBranchExistsで古いブランチを消さない() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        repo.branch(
            &test.transport,
            NEW_BRANCH,
            &[("src/content/blog/b.md", A_SYNCED)],
        );
        repo.accept_writes(&test.transport);
        let state_before = fs::read(&test.state_path).unwrap();

        match test.commit_push(NEW_SLUG, "更新", None) {
            Err(GithubError::BranchExists { branch }) => assert_eq!(branch, NEW_BRANCH),
            other => panic!("BranchExistsのはず: {other:?}"),
        }
        assert_eq!(test.delete_count(&test.ref_url(BRANCH)), 0);
        assert!(test.branch_exists(BRANCH));
        assert_eq!(fs::read(&test.state_path).unwrap(), state_before);
    }

    #[test]
    fn 作った後に古いブランチが進んでいれば消さずにkeptBranchに入れる() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        repo.accept_writes(&test.transport);
        // 新しいブランチを作る通信の後に、ほかの端末が古いブランチを進める
        let hook_repo = repo.clone();
        let hook_transport = Arc::clone(&test.transport);
        repo.before(Method::Post, &test.refs_url(), 1, move || {
            hook_repo.branch(&hook_transport, BRANCH, HEAD_ADVANCED);
        });

        let result = test.commit_push(NEW_SLUG, "更新", None).unwrap();

        assert_eq!(result.kept_branch.as_deref(), Some(BRANCH));
        assert_eq!(test.delete_count(&test.ref_url(BRANCH)), 0);
        assert!(test.branch_exists(BRANCH));
        // 記録は新しいブランチになる
        assert_eq!(test.state()["articles"][NEW_SLUG]["branch"], NEW_BRANCH);
    }

    #[test]
    fn 古いブランチのDELETEが失敗してもkeptBranchに入れて成功する() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        repo.accept_writes(&test.transport);
        test.transport.json(
            Method::Delete,
            &test.ref_url(BRANCH),
            500,
            serde_json::json!({ "message": "failed" }),
        );

        let result = test.commit_push(NEW_SLUG, "更新", None).unwrap();

        assert_eq!(result.kept_branch.as_deref(), Some(BRANCH));
        assert!(test.branch_exists(BRANCH));
        assert_eq!(test.state()["articles"][NEW_SLUG]["branch"], NEW_BRANCH);
    }

    #[test]
    #[cfg(unix)]
    fn 新しいブランチを作った後に記録を書けなければPullで直せる() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        repo.accept_writes(&test.transport);
        let Some(read_only) = read_only_dir(test.state_path.parent().unwrap()) else {
            return;
        };
        let failed = test.commit_push(NEW_SLUG, "更新", None);
        drop(read_only);

        assert!(failed.is_err());
        assert!(test.branch_exists(NEW_BRANCH));
        // もう一度Pushすると、新しいブランチが既にある
        assert!(matches!(
            test.commit_push(NEW_SLUG, "更新", None),
            Err(GithubError::BranchExists { branch }) if branch == NEW_BRANCH
        ));
        // branchを指定したPullで記録を直す
        test.pull(NEW_SLUG, Some(NEW_BRANCH), BACKUP_NAME).unwrap();
        let state = test.state();
        let record = &state["articles"][NEW_SLUG];
        assert_eq!(record["branch"], NEW_BRANCH);
        assert!(record.get("renamedFrom").is_none(), "{record}");
        test.sync();
        assert!(!test.article_state(NEW_SLUG).dirty);
    }

    #[test]
    fn 改名していない記事のPushではkeptBranchはNone() {
        let test = PushTest::new();
        let repo = test.article_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);

        let result = test.commit_push(SLUG, "更新", None).unwrap();

        assert_eq!(result.kept_branch, None);
    }

    #[test]
    fn BranchExistsからPullするとrenamedFromが外れて古いslugの記事が手元に出る() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        repo.branch(
            &test.transport,
            NEW_BRANCH,
            &[
                ("src/content/blog/b.md", A_SYNCED),
                ("src/assets/b/01.png", PNG),
            ],
        );
        repo.accept_writes(&test.transport);
        assert!(matches!(
            test.commit_push(NEW_SLUG, "更新", None),
            Err(GithubError::BranchExists { .. })
        ));

        test.pull(NEW_SLUG, Some(NEW_BRANCH), BACKUP_NAME).unwrap();

        let state = test.state();
        let record = &state["articles"][NEW_SLUG];
        assert_eq!(record["branch"], NEW_BRANCH);
        assert!(record.get("renamedFrom").is_none(), "{record}");
        // 続く読み込みで、古いブランチの記事が元のslugで手元に出る
        test.sync();
        assert_eq!(fs::read(test.md_path(SLUG)).unwrap(), A_SYNCED);
        let old = test.article_state(SLUG);
        assert!(!old.dirty);
        assert_eq!(old.category, Category::Drafting);
    }

    #[test]
    fn ConflictからPullすると古いブランチの元のslugの記事を記録しrenamedFromは残る() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        test.write_md_to(NEW_SLUG, A_LOCAL);
        let advanced = repo.branch(&test.transport, BRANCH, OTHER_CHANGES_MD);
        repo.accept_writes(&test.transport);
        assert!(matches!(
            test.commit_push(NEW_SLUG, "更新", None),
            Err(GithubError::Conflict { .. })
        ));

        test.pull(NEW_SLUG, None, BACKUP_NAME).unwrap();

        let state = test.state();
        let record = &state["articles"][NEW_SLUG];
        assert_eq!(record["commit"], advanced);
        assert_eq!(record["md"], blob_sha(A_LOCAL));
        assert_eq!(record["renamedFrom"], SLUG);
        // 続く読み込みで、リモートの内容が新しいslugに入る
        test.sync();
        assert_eq!(fs::read(test.md_path(NEW_SLUG)).unwrap(), A_REMOTE);
        assert!(!test.md_path(SLUG).exists());
    }

    #[test]
    fn 古いブランチが消す前に消えていればkeptBranchはNone() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        repo.accept_writes(&test.transport);
        // DELETEの直前に、ほかの端末が古いブランチを消す(GitHubは422を返す)
        let hook_repo = repo.clone();
        repo.before(Method::Delete, &test.ref_url(BRANCH), 1, move || {
            hook_repo.delete_branch(BRANCH);
        });

        let result = test.commit_push(NEW_SLUG, "更新", None).unwrap();

        assert_eq!(result.kept_branch, None);
        let state = test.state();
        assert!(!state["remote"]["branches"]
            .as_object()
            .unwrap()
            .contains_key(BRANCH));
    }

    #[test]
    fn 改名した記事のmdを変えていなければ古いブランチでのmdの変更を残す() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        repo.branch(&test.transport, BRANCH, OTHER_CHANGES_MD);
        repo.accept_writes(&test.transport);

        test.commit_push(NEW_SLUG, "更新", None).unwrap();

        let files = repo.tree_files(NEW_BRANCH);
        assert_eq!(body_of(&files, "src/content/blog/b.md"), A_REMOTE);
        // 続く読み込みで手元もリモートの内容になる
        test.sync();
        assert_eq!(fs::read(test.md_path(NEW_SLUG)).unwrap(), A_REMOTE);
        assert!(!test.article_state(NEW_SLUG).dirty);
    }

    #[test]
    #[cfg(unix)]
    fn 改名した記事のPushでsyncedだけ書けなくても成功にして古いブランチを消す() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        repo.accept_writes(&test.transport);
        let synced_dir = test.synced_path(NEW_SLUG).parent().unwrap().to_path_buf();
        let Some(read_only) = read_only_dir(&synced_dir) else {
            return;
        };
        let result = test.commit_push(NEW_SLUG, "更新", None);
        drop(read_only);

        let result = result.unwrap();
        assert_eq!(result.kept_branch, None);
        let state = test.state();
        let record = &state["articles"][NEW_SLUG];
        assert_eq!(record["branch"], NEW_BRANCH);
        assert!(record.get("renamedFrom").is_none(), "{record}");
        assert_eq!(test.delete_count(&test.ref_url(BRANCH)), 1);
        test.sync();
        assert!(!test.article_state(NEW_SLUG).dirty);
    }

    // 共有の画像 -----------------------------------------------------------------

    #[test]
    fn 共有の画像はCommitとPushで送らない() {
        let test = PushTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", SHARED_MAIN_FILES);
        test.sync();
        repo.accept_writes(&test.transport);
        // 手元の共有の画像を書き換えても、記事のファイルではない
        test.write_image("shared.png", PNG3);
        test.write_md(A_LOCAL);

        test.commit_push(SLUG, "共有の画像は送らない", None)
            .unwrap();
        let entries = test.post_bodies("/git/trees")[0]["tree"]
            .as_array()
            .unwrap()
            .clone();
        assert!(
            !entries
                .iter()
                .any(|entry| entry["path"] == "src/assets/shared.png"),
            "{entries:?}"
        );
    }

    #[test]
    fn Pullのバックアップに共有の画像は入らず共有の画像は残る() {
        let test = PushTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", SHARED_MAIN_FILES);
        let _head = repo.branch(&test.transport, BRANCH, SHARED_MAIN_FILES);
        test.sync();
        test.write_image("shared.png", PNG3);
        test.write_md(A_LOCAL);

        test.pull(SLUG, None, BACKUP_NAME).unwrap();
        // バックアップに写すのは記事のファイルだけ
        let backup = test.backup_path(BACKUP_NAME, "");
        assert!(backup.join("src/content/blog/a.md").exists());
        let mut shared = Vec::new();
        let mut dirs = vec![backup];
        while let Some(dir) = dirs.pop() {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.ends_with("shared.png") {
                    shared.push(path);
                }
            }
        }
        assert!(shared.is_empty(), "{shared:?}");
        assert_eq!(fs::read(test.image_path("shared.png")).unwrap(), PNG3);
    }

    // #78: Commit & Pushの進捗 -----------------------------------------------------

    #[test]
    fn 新しい記事の進捗は送るファイルごとにUploadingを出してCommittingとPushingで終わる() {
        let test = PushTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", NO_A_MAIN);
        repo.accept_writes(&test.transport);
        test.write_md(&a_new());
        test.write_image("a/01.png", PNG);
        test.write_image("a/02.png", PNG2);

        let (result, progress) = test.commit_push_progress(SLUG, "最初のcommit", None);
        result.unwrap();

        assert_eq!(
            steps_of(&progress),
            vec![
                PushProgress::Uploading {
                    current: 1,
                    total: 3
                },
                PushProgress::Uploading {
                    current: 2,
                    total: 3
                },
                PushProgress::Uploading {
                    current: 3,
                    total: 3
                },
                PushProgress::Committing,
                PushProgress::Pushing,
            ]
        );
        test.assert_progress_before_requests(&progress, "Post /git/refs");
    }

    #[test]
    fn 同じ中身の画像は1つに数えてUploadingを出す() {
        let test = PushTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", NO_A_MAIN);
        repo.accept_writes(&test.transport);
        test.write_md(&a_new());
        test.write_image("a/01.png", PNG);
        test.write_image("a/02.png", PNG);

        let (result, progress) = test.commit_push_progress(SLUG, "最初のcommit", None);
        result.unwrap();

        assert_eq!(
            steps_of(&progress),
            vec![
                PushProgress::Uploading {
                    current: 1,
                    total: 2
                },
                PushProgress::Uploading {
                    current: 2,
                    total: 2
                },
                PushProgress::Committing,
                PushProgress::Pushing,
            ]
        );
        assert_eq!(test.post_bodies("/git/blobs").len(), 2);
    }

    #[test]
    fn 記録のbranchがある記事の進捗は変えたファイルだけUploadingを出す() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);

        let (result, progress) = test.commit_push_progress(SLUG, "更新", None);
        result.unwrap();

        assert_eq!(
            steps_of(&progress),
            vec![
                PushProgress::Uploading {
                    current: 1,
                    total: 1
                },
                PushProgress::Committing,
                PushProgress::Pushing,
            ]
        );
        let (_, at) = progress.last().unwrap();
        assert_eq!(test.request_after(*at), "Patch /git/refs/heads/blog/a");
    }

    #[test]
    fn 作り直すときはCheckingからやり直し送り済みのblobをUploadingに数えない() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.mark_fetched(SLUG, &[("01.png", true)]);
        // 記録と違う中身にする。リモートが足す02.pngとは別の名前
        test.write_image("a/01.png", PNG3);
        test.write_md(A_LOCAL);
        branch_hook(
            &repo,
            &test,
            Arc::new(Mutex::new(Vec::new())),
            1,
            OTHER_ADDS_IMAGE,
        );

        let (result, progress) = test.commit_push_progress(SLUG, "更新", None);
        result.unwrap();

        assert_eq!(
            steps_of(&progress),
            vec![
                PushProgress::Uploading {
                    current: 1,
                    total: 2
                },
                PushProgress::Uploading {
                    current: 2,
                    total: 2
                },
                PushProgress::Committing,
                PushProgress::Pushing,
                PushProgress::Checking,
                PushProgress::Committing,
                PushProgress::Pushing,
            ]
        );
        // 2周目は送り済みなのでblobのPOSTは増えない
        assert_eq!(test.post_bodies("/git/blobs").len(), 2);
        test.assert_progress_before_requests(&progress, "Patch /git/refs/heads/blog/a");
        // Checkingは、失敗したPATCHの後、branchの先頭を読む前
        let (_, at) = progress
            .iter()
            .find(|(progress, _)| *progress == PushProgress::Checking)
            .expect("Checkingはあるはず");
        assert_eq!(test.request_before(*at), "Patch /git/refs/heads/blog/a");
        assert_eq!(test.request_after(*at), "Get /git/ref/heads/blog/a");
    }

    #[test]
    fn branchを進められず重なっていればCheckingの後にConflictで止まる() {
        let test = PushTest::new();
        let (repo, _head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        test.write_md(A_LOCAL);
        branch_hook(
            &repo,
            &test,
            Arc::new(Mutex::new(Vec::new())),
            1,
            OTHER_CHANGES_MD,
        );

        let (result, progress) = test.commit_push_progress(SLUG, "更新", None);

        assert!(matches!(
            result,
            Err(GithubError::Conflict { md: true, .. })
        ));
        assert_eq!(
            steps_of(&progress),
            vec![
                PushProgress::Uploading {
                    current: 1,
                    total: 1
                },
                PushProgress::Committing,
                PushProgress::Pushing,
                PushProgress::Checking,
            ]
        );
    }

    #[test]
    fn 改名した記事が送る前にConflictで止まるときは段階を知らせず何も作らない() {
        let test = PushTest::new();
        // 改名した記事の記録のbranch(blog/a)の先頭が進み、本文が重なる
        let (repo, _head) = test.renamed_synced();
        test.write_md_to(NEW_SLUG, A_LOCAL);
        repo.branch(&test.transport, BRANCH, OTHER_CHANGES_MD);
        repo.accept_writes(&test.transport);

        let (result, progress) = test.commit_push_progress(NEW_SLUG, "更新", None);

        assert!(matches!(
            result,
            Err(GithubError::Conflict { md: true, .. })
        ));
        assert!(progress.is_empty(), "{progress:?}");
        assert!(test.post_bodies("/git/blobs").is_empty());
        assert!(test.post_bodies("/git/trees").is_empty());
        assert!(test.post_bodies("/git/commits").is_empty());
    }

    #[test]
    fn 改名した記事でmdを変えていなければCommittingとPushingだけ出す() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        repo.accept_writes(&test.transport);

        let (result, progress) = test.commit_push_progress(NEW_SLUG, "更新", None);
        result.unwrap();

        assert_eq!(
            steps_of(&progress),
            vec![PushProgress::Committing, PushProgress::Pushing]
        );
        assert!(test.post_bodies("/git/blobs").is_empty());
        let (_, at) = progress.last().unwrap();
        assert_eq!(test.request_after(*at), "Post /git/refs");
    }

    #[test]
    fn 改名した記事でmdを変えるとPushingの後に古いbranchを消すまで知らせない() {
        let test = PushTest::new();
        let (repo, _head) = test.renamed_synced();
        test.write_md_to(NEW_SLUG, A_LOCAL);
        repo.accept_writes(&test.transport);

        let (result, progress) = test.commit_push_progress(NEW_SLUG, "更新", None);
        result.unwrap();

        assert_eq!(
            steps_of(&progress),
            vec![
                PushProgress::Uploading {
                    current: 1,
                    total: 1
                },
                PushProgress::Committing,
                PushProgress::Pushing,
            ]
        );
        // Pushingの直後は新しいbranchを作るPOST
        let (_, at) = progress.last().unwrap();
        assert_eq!(test.request_after(*at), "Post /git/refs");
        // 古いbranchのDELETEはその後。Pushingの後に知らせは来ない
        let requests = test.transport.requests();
        assert!(
            requests[*at..].iter().any(|request| {
                request.method == Method::Delete && request.url == test.ref_url(BRANCH)
            }),
            "{:?}",
            &requests[*at..]
        );
    }

    #[test]
    fn blobの送信が失敗したらそこで止まりエラーを返す() {
        let test = PushTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", NO_A_MAIN);
        repo.accept_writes(&test.transport);
        // blobのPOSTの1回目は成功し、2回目は500を返す
        let attempts = Arc::new(Mutex::new(0usize));
        let counter = Arc::clone(&attempts);
        test.transport.on(
            Method::Post,
            &url_of(&["repos", "owner", "blog", "git", "blobs"], &[]),
            Box::new(move |request: &Request| {
                let mut attempts = counter.lock().unwrap();
                *attempts += 1;
                if *attempts > 1 {
                    return Ok(json_response(
                        500,
                        serde_json::json!({ "message": "failed" }),
                    ));
                }
                let body: Value = serde_json::from_slice(request.body.as_deref().unwrap()).unwrap();
                let content = b64_decode(body["content"].as_str().unwrap()).unwrap();
                Ok(json_response(
                    201,
                    serde_json::json!({ "sha": blob_sha(&content) }),
                ))
            }),
        );
        test.write_md(&a_new());
        test.write_image("a/01.png", PNG);

        let (result, progress) = test.commit_push_progress(SLUG, "最初のcommit", None);

        assert!(matches!(result, Err(GithubError::Status { status: 500 })));
        assert_eq!(
            steps_of(&progress),
            vec![
                PushProgress::Uploading {
                    current: 1,
                    total: 2
                },
                PushProgress::Uploading {
                    current: 2,
                    total: 2
                },
            ]
        );
        assert_eq!(*attempts.lock().unwrap(), 2);
    }

    // 画面のPushProgress(src/api.ts)と同じ形
    #[test]
    fn PushProgressはstepで段階を分けて送る() {
        assert_eq!(
            serde_json::to_value(PushProgress::Checking).unwrap(),
            serde_json::json!({ "step": "checking" })
        );
        assert_eq!(
            serde_json::to_value(PushProgress::Uploading {
                current: 2,
                total: 4
            })
            .unwrap(),
            serde_json::json!({ "step": "uploading", "current": 2, "total": 4 })
        );
        assert_eq!(
            serde_json::to_value(PushProgress::Committing).unwrap(),
            serde_json::json!({ "step": "committing" })
        );
        assert_eq!(
            serde_json::to_value(PushProgress::Pushing).unwrap(),
            serde_json::json!({ "step": "pushing" })
        );
    }

    // #68: ベースの記事のslugの変更 -----------------------------------------------

    impl PushTest {
        // ベースの記事oldをnewに変えた後の手元と記録(renameの実装に頼らず直接動かす)
        fn mark_renamed_from_base(&self, old: &str, new: &str) {
            fs::rename(self.md_path(old), self.md_path(new)).unwrap();
            let old_assets = self.assets_base.join(old);
            if old_assets.exists() {
                fs::rename(old_assets, self.assets_base.join(new)).unwrap();
            }
            if self.synced_path(old).exists() {
                fs::rename(self.synced_path(old), self.synced_path(new)).unwrap();
            }
            self.edit_state(|state| {
                let mut record = state["articles"][old].clone();
                record["renamedFrom"] = Value::String(old.to_string());
                state["articles"][new] = record;
                state["articles"].as_object_mut().unwrap().remove(old);
            });
        }
    }

    #[test]
    fn ベースの記事のslugの変更はベースの先頭を親に新しいブランチを作る() {
        let test = PushTest::new();
        let repo = test.article_synced();
        test.mark_renamed_from_base("a", "b");
        repo.accept_writes(&test.transport);
        let base = repo.head("main");

        test.commit_push("b", "更新", None).unwrap();

        // 新しいブランチを作るPOSTだけ。古いブランチの削除はしない
        assert_eq!(test.post_count(&test.refs_url()), 1);
        assert_eq!(test.patch_count(&test.patch_url("blog/b")), 0);
        assert!(
            !test
                .transport
                .requests()
                .iter()
                .any(|request| request.method == Method::Delete),
            "ブランチの削除は送らないはず"
        );
        // 親はベースの先頭
        let commits = test.post_bodies("/git/commits");
        assert_eq!(
            commits.last().unwrap()["parents"],
            serde_json::json!([base])
        );
        // ツリーは古いパスを消して新しいパスを足す
        let trees = test.post_bodies("/git/trees");
        let entries = trees.last().unwrap()["tree"].as_array().unwrap();
        let removed: BTreeSet<&str> = entries
            .iter()
            .filter(|entry| entry["sha"].is_null())
            .map(|entry| entry["path"].as_str().unwrap())
            .collect();
        assert!(removed.contains("src/content/blog/a.md"), "{removed:?}");
        assert!(removed.contains("src/assets/a/01.png"), "{removed:?}");
        let files = repo.tree_files("blog/b");
        assert!(files.contains_key("src/content/blog/b.md"));
        assert!(files.contains_key("src/assets/b/01.png"));
        assert!(!files.contains_key("src/content/blog/a.md"));
        assert!(!files.contains_key("src/assets/a/01.png"));
    }

    #[test]
    fn ベースの記事のslugの変更を終えると記録と様子のブランチが移した記事になる() {
        let test = PushTest::new();
        let repo = test.article_synced();
        test.mark_renamed_from_base("a", "b");
        repo.accept_writes(&test.transport);

        let result = test.commit_push("b", "更新", None).unwrap();

        let state = test.state();
        let record = &state["articles"]["b"];
        assert_eq!(record["branch"], "blog/b");
        assert_eq!(record["commit"], result.commit);
        assert!(record.get("renamedFrom").is_none(), "{record}");
        let branch = &state["remote"]["branches"]["blog/b"];
        assert_eq!(branch["commit"], result.commit);
        assert_eq!(branch["slug"], "b");
        assert_eq!(branch["movedFrom"], "a");
    }

    #[test]
    fn ベースの記事のslugの変更の後に読み込むと編集中の変更なしになり元のslugは作られない() {
        let test = PushTest::new();
        let repo = test.article_synced();
        test.mark_renamed_from_base("a", "b");
        repo.accept_writes(&test.transport);
        test.commit_push("b", "更新", None).unwrap();

        test.sync();

        let state = test.article_state("b");
        assert_eq!(state.category, Category::Editing);
        assert!(!state.dirty);
        assert_eq!(state.moved_from.as_deref(), Some("a"));
        assert!(!test.md_path("a").exists(), "元のslugは作らないはず");
        assert_eq!(test.state()["articles"]["a"], Value::Null);
    }

    #[test]
    fn ベースの記事のslugの変更で取ってきていない画像は親のblobのshaで新しいパスに置く() {
        let test = PushTest::new();
        let repo = test.article_synced();
        test.mark_renamed_from_base("a", "b");
        repo.accept_writes(&test.transport);

        test.commit_push("b", "更新", None).unwrap();

        // 画像の中身は送らない
        assert!(
            test.blob_contents().iter().all(|content| content != PNG),
            "画像のblobは送らないはず"
        );
        let files = repo.tree_files("blog/b");
        // 親のblobのSHAのまま新しいパスに置く
        let (sha, body) = path_of(&files, "src/assets/b/01.png");
        assert_eq!(sha, "100644");
        assert_eq!(body, PNG);
    }

    #[test]
    fn ベースの先頭で元のslugの記事が記録から変わっていればConflict() {
        let test = PushTest::new();
        let repo = test.article_synced();
        test.mark_renamed_from_base("a", "b");
        test.write_md_to("b", A_LOCAL);
        // ほかの端末がベースのa.mdを替える
        let base = repo.branch(&test.transport, "main", BASE_A_CHANGED);
        repo.accept_writes(&test.transport);

        match test.commit_push("b", "更新", None) {
            Err(GithubError::Conflict { head, md, .. }) => {
                assert_eq!(head, base);
                assert!(md);
            }
            other => panic!("Conflictのはず: {other:?}"),
        }
        assert_eq!(test.post_count(&test.refs_url()), 0);
    }

    #[test]
    fn ベースの記事のslugの変更のConflictでPushを選ぶと手元の内容で新しいブランチを作る() {
        let test = PushTest::new();
        let repo = test.article_synced();
        test.mark_renamed_from_base("a", "b");
        test.write_md_to("b", A_LOCAL);
        let base = repo.branch(&test.transport, "main", BASE_A_CHANGED);
        repo.accept_writes(&test.transport);
        let Err(GithubError::Conflict { head, .. }) = test.commit_push("b", "更新", None) else {
            panic!("Conflictのはず");
        };

        test.commit_push("b", "更新", Some(&head)).unwrap();

        let commits = test.post_bodies("/git/commits");
        assert_eq!(
            commits.last().unwrap()["parents"],
            serde_json::json!([base])
        );
        let files = repo.tree_files("blog/b");
        assert_eq!(body_of(&files, "src/content/blog/b.md"), A_LOCAL);
        assert!(!files.contains_key("src/content/blog/a.md"));
    }

    #[test]
    fn ベースの記事のslugの変更でベースだけが本文を替えていれば重ならずベースの本文で送る() {
        let test = PushTest::new();
        let repo = test.article_synced();
        test.mark_renamed_from_base("a", "b");
        let changed = repo.branch(&test.transport, "main", BASE_A_CHANGED);
        repo.accept_writes(&test.transport);

        test.commit_push("b", "更新", None).unwrap();

        let files = repo.tree_files("blog/b");
        let base_md = &repo.tree_files("main")["src/content/blog/a.md"].1;
        assert_eq!(body_of(&files, "src/content/blog/b.md"), base_md.as_slice());
        let commits = test.post_bodies("/git/commits");
        assert_eq!(
            commits.last().unwrap()["parents"],
            serde_json::json!([changed])
        );
    }

    #[test]
    fn ベースから元のslugの記事が消えていればBranchGoneで何も作らない() {
        let test = PushTest::new();
        let repo = test.article_synced();
        test.mark_renamed_from_base("a", "b");
        // ほかの端末がベースからa.mdを消す
        repo.branch(&test.transport, "main", NO_A_MAIN);
        repo.accept_writes(&test.transport);

        assert!(matches!(
            test.commit_push("b", "更新", None),
            Err(GithubError::BranchGone)
        ));
        assert_eq!(test.post_count(&test.refs_url()), 0);
        assert_eq!(test.state()["articles"]["b"]["renamedFrom"], "a");
    }

    #[test]
    fn ベースの記事のslugの変更で同じ名前のブランチが既にあればBranchExists() {
        let test = PushTest::new();
        let repo = test.article_synced();
        test.mark_renamed_from_base("a", "b");
        repo.branch(
            &test.transport,
            "blog/b",
            &[("src/content/blog/b.md", OTHER_MD)],
        );
        repo.accept_writes(&test.transport);

        assert!(matches!(
            test.commit_push("b", "更新", None),
            Err(GithubError::BranchExists { branch }) if branch == "blog/b"
        ));
    }

    #[test]
    fn ベースの記事のslugの変更で同じ名前の空のブランチがあってもBranchExists() {
        let test = PushTest::new();
        let repo = test.article_synced();
        test.mark_renamed_from_base("a", "b");
        let base = repo.head("main");
        repo.branch_from(&test.transport, "blog/b", &base, MAIN_FILES);
        repo.accept_writes(&test.transport);

        assert!(matches!(
            test.commit_push("b", "更新", None),
            Err(GithubError::BranchExists { branch }) if branch == "blog/b"
        ));
    }

    #[test]
    fn ベースの記事のslugの変更のpreviewは古い名前を出し古いブランチを出さない() {
        let test = PushTest::new();
        let _repo = test.article_synced();
        test.mark_renamed_from_base("a", "b");

        let preview = test.preview("b").unwrap();

        assert_eq!(preview.md_name, "b.md");
        assert_eq!(preview.old_md_name.as_deref(), Some("a.md"));
        assert_eq!(preview.old_branch, None);
    }

    #[test]
    fn 移した記事のslugをさらに変えると新しいブランチを作って古いブランチを消す() {
        let test = PushTest::new();
        let repo = test.article_synced();
        test.mark_renamed_from_base("a", "b");
        repo.accept_writes(&test.transport);
        test.commit_push("b", "更新", None).unwrap();
        // 手元でslugをもう一度変える
        fs::rename(test.md_path("b"), test.md_path("c")).unwrap();
        let old_assets = test.assets_base.join("b");
        if old_assets.exists() {
            fs::rename(old_assets, test.assets_base.join("c")).unwrap();
        }
        if test.synced_path("b").exists() {
            fs::rename(test.synced_path("b"), test.synced_path("c")).unwrap();
        }
        test.edit_state(|state| {
            let mut record = state["articles"]["b"].clone();
            record["renamedFrom"] = Value::String("b".to_string());
            state["articles"]["c"] = record;
            state["articles"].as_object_mut().unwrap().remove("b");
        });

        let result = test.commit_push("c", "更新", None).unwrap();

        assert_eq!(result.branch, "blog/c");
        assert_eq!(test.post_count(&test.refs_url()), 2);
        // 古いブランチを消す
        assert_eq!(
            test.transport
                .requests()
                .iter()
                .filter(|request| request.method == Method::Delete
                    && request.url.ends_with("/refs/heads/blog/b"))
                .count(),
            1
        );
        let state = test.state();
        let record = &state["articles"]["c"];
        assert_eq!(record["branch"], "blog/c");
        assert!(record.get("renamedFrom").is_none(), "{record}");
        // 続く読み込みで、新しいブランチが同じ元の記事を移した記事になる
        test.sync();
        let state = test.state();
        assert_eq!(state["remote"]["branches"]["blog/c"]["movedFrom"], "a");
        let article = test.article_state("c");
        assert_eq!(article.category, Category::Editing);
        assert!(!article.dirty);
        assert!(!test.md_path("a").exists());
    }

    // #68: 空のブランチ ----------------------------------------------------------

    #[test]
    fn まだcommitしていない記事は送り先の空のブランチの先頭を親にPatchで進める() {
        let test = PushTest::new();
        let repo = test.new_repo();
        let base = repo.branch(&test.transport, "main", NO_A_MAIN);
        // 記事aの無い、ベースより先に進んだブランチ
        let empty = repo.branch_from(&test.transport, BRANCH, &base, NO_A_MAIN);
        test.sync();
        test.write_md(&a_new());
        test.write_image("a/01.png", PNG);
        repo.accept_writes(&test.transport);

        test.commit_push("a", "追加", None).unwrap();

        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 1);
        assert_eq!(test.post_count(&test.refs_url()), 0);
        let commits = test.post_bodies("/git/commits");
        assert_eq!(commits[0]["parents"], serde_json::json!([empty]));
    }

    #[test]
    fn 空のブランチの先頭に記事が足されていればBranchExists() {
        let test = PushTest::new();
        let repo = test.new_repo();
        let base = repo.branch(&test.transport, "main", NO_A_MAIN);
        let empty = repo.branch_from(&test.transport, BRANCH, &base, NO_A_MAIN);
        test.sync();
        test.write_md(&a_new());
        repo.accept_writes(&test.transport);
        // 読み込みの後に、ほかの端末が空のブランチに記事を足した
        let mut files: Vec<(&str, &[u8])> = NO_A_MAIN.to_vec();
        files.push(("src/content/blog/a.md", A_SYNCED));
        repo.branch_from(&test.transport, BRANCH, &empty, &files);

        assert!(matches!(
            test.commit_push("a", "追加", None),
            Err(GithubError::BranchExists { branch }) if branch == BRANCH
        ));
        assert_eq!(test.post_count(&test.refs_url()), 0);
    }

    // #68: 削除予定 ---------------------------------------------------------------

    #[test]
    fn 削除予定の記事のCommit_and_PushはNothingToCommit() {
        let test = PushTest::new();
        let repo = test.article_synced();
        test.edit_state(|state| {
            state["articles"]["a"]["deleting"] = Value::String("delete".to_string());
        });
        // 手元に変更があっても送らない
        test.write_md(A_LOCAL);
        repo.accept_writes(&test.transport);

        assert!(matches!(
            test.commit_push("a", "更新", None),
            Err(GithubError::NothingToCommit)
        ));
        assert!(test
            .transport
            .requests()
            .iter()
            .all(|request| request.method == Method::Get));
    }

    // 日付の印 -------------------------------------------------------------------

    fn updated_mark(path: &std::path::Path, slug: &str) -> bool {
        dates::read_marks(path)
            .unwrap()
            .articles
            .get(slug)
            .is_some_and(|marks| marks.updated_date)
    }

    fn marks_of(entries: &[(&str, bool, bool)]) -> dates::Marks {
        let mut marks = dates::Marks::default();
        for (slug, pub_date, updated_date) in entries {
            marks.articles.insert(
                (*slug).to_string(),
                dates::DateMarks {
                    pub_date: *pub_date,
                    updated_date: *updated_date,
                },
            );
        }
        marks
    }

    #[test]
    fn Commit_and_Pushの成功でupdatedDateの印が外れる() {
        let test = PushTest::new();
        let repo = test.article_synced();
        let path = dates::marks_path(&test.data_dir, &test.repo_of);
        dates::write_marks(&path, &marks_of(&[(SLUG, false, true)])).unwrap();

        test.write_md(A_LOCAL);
        repo.accept_writes(&test.transport);
        test.commit_push(SLUG, "更新", None).unwrap();

        assert!(!updated_mark(&path, SLUG));
    }

    #[test]
    fn Commit_and_Pushが失敗したらupdatedDateの印は残る() {
        let test = PushTest::new();
        test.article_synced();
        let path = dates::marks_path(&test.data_dir, &test.repo_of);
        dates::write_marks(&path, &marks_of(&[(SLUG, false, true)])).unwrap();

        // 手元に変更が無いので送らない
        assert!(matches!(
            test.commit_push(SLUG, "更新", None),
            Err(GithubError::NothingToCommit)
        ));

        assert!(updated_mark(&path, SLUG));
    }

    #[test]
    fn 送った後に手元のmdが変わっていればupdatedDateの印は残る() {
        let test = PushTest::new();
        let repo = test.article_synced();
        let path = dates::marks_path(&test.data_dir, &test.repo_of);
        dates::write_marks(&path, &marks_of(&[(SLUG, false, true)])).unwrap();

        test.write_md(A_LOCAL);
        repo.accept_writes(&test.transport);
        // 送っている間に手元を保存したことにする
        let changed = Cell::new(false);
        let result = commit_push(&test.input(), SLUG, "更新", None, &|_| {
            if !changed.replace(true) {
                test.write_md(b"---\ntitle: \"A\"\n---\nX-a\nX-b\nline-c\nline-d\nline-e\nZ-g\n");
            }
        });
        result.unwrap();

        assert!(updated_mark(&path, SLUG));
    }

    #[test]
    fn PullでupdatedDateの印が外れる() {
        let test = PushTest::new();
        let (_repo, _head) = test.branch_synced();
        let path = dates::marks_path(&test.data_dir, &test.repo_of);
        dates::write_marks(&path, &marks_of(&[(SLUG, false, true)])).unwrap();

        test.pull(SLUG, None, BACKUP_NAME).unwrap();

        assert!(!updated_mark(&path, SLUG));
    }
}
