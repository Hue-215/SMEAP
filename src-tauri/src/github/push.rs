use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

use super::config;
use super::git::{self, NewTreeEntry};
use super::remote;
use super::state::{self, ArticleFiles, AssetRecord, Basis, BranchSnapshot, LocalBodies, Record};
use super::sync::{self, SyncInput};
use super::GithubError;

// PATCHを含めて4回branchを進められる(作り直しは3回まで。超えたらPushRetryExceeded)
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
    // 消した画像(取ってきてあったもの)。中身は送らないので名前だけ
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
        // 差し替えは追加と削除に1件ずつ数える
        let changed = local
            .assets
            .get(name)
            .is_none_or(|content| content.0 != asset.sha);
        if asset.fetched && changed {
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
fn count_lines(local: &[u8], synced: &[u8]) -> (usize, usize) {
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
        // 手元にmdが無い記事はPushできない(#66で扱う)
        return Err(GithubError::NothingToCommit);
    };
    let diff = local_diff(record.as_ref(), &local);
    if diff.empty() {
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
    let (added, removed) = count_lines(&md.1, &synced);
    Ok(CommitPreview {
        title: title_of(&md.1),
        md_name: format!("{slug}.md"),
        added,
        removed,
        images_added: diff.added.len(),
        images_removed: diff.removed.len(),
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
fn branch_name(basis: &Basis, slug: &str) -> String {
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

// blobの送り直しを避けるための中身->SHA
type BlobCache = HashMap<Vec<u8>, String>;

fn upload_blob(
    input: &SyncInput<'_>,
    blobs: &mut BlobCache,
    content: &[u8],
) -> Result<String, GithubError> {
    if let Some(sha) = blobs.get(content) {
        return Ok(sha.clone());
    }
    let sha = git::create_blob(input.transport, input.token, input.now, input.repo, content)?;
    blobs.insert(content.to_vec(), sha.clone());
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
    let slug_assets = assets_dir(input.basis, slug);
    let mut entries = Vec::new();
    if let Some((_, body)) = &diff.md {
        let path = md_path(input.basis, slug);
        let mode = modes.get(&path).map(String::as_str).unwrap_or("100644");
        let sha = upload_blob(input, blobs, body)?;
        entries.push(NewTreeEntry {
            path,
            mode: mode.to_string(),
            sha: Some(sha),
        });
    }
    for (name, (_, body)) in &diff.added {
        let path = format!("{slug_assets}/{name}");
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

// 親のコミットでの記事のファイルとmode、親のルートのツリー
struct RemoteArticle {
    files: ArticleFiles,
    modes: BTreeMap<String, String>,
    base_tree: String,
}

// parentでの記事。mdがGitHub上に無ければ、mdは空のSHAで代える
fn remote_article(
    input: &SyncInput<'_>,
    slug: &str,
    parent: &str,
) -> Result<RemoteArticle, GithubError> {
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
        Some(article) => Ok(RemoteArticle {
            files: article.files,
            modes: article.modes,
            base_tree,
        }),
        // mdが無ければ、記事はGitHub上で消されたものとして扱う
        None => Ok(RemoteArticle {
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
// この記事の記録とリモートの様子だけを替える(画像の取得が通信の間に書いた記録を消さないため)
fn save_pushed(
    input: &SyncInput<'_>,
    slug: &str,
    result: &PushResult,
    merged: ArticleFiles,
    diff: &LocalDiff,
    md: &Content,
    state_path: &std::path::Path,
) -> Result<(), GithubError> {
    input.store.with_lock(|posts_dir, _| {
        sync::check_unchanged(input, posts_dir)?;
        let mut state = state::read_state(state_path, input.basis)?;
        let record = state
            .articles
            .entry(slug.to_string())
            .or_insert_with(|| Record {
                branch: None,
                commit: String::new(),
                md: String::new(),
                assets: BTreeMap::new(),
                applying: None,
            });
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
        state.remote.branches.insert(
            result.branch.clone(),
            BranchSnapshot {
                commit: result.commit.clone(),
                slug: slug.to_string(),
                article: merged,
            },
        );
        // 食い違いの印を外す
        state.conflicts.remove(slug);
        // syncedは記録に合わせて写すので、記録を書けた後にする
        state::write_state(state_path, &state)?;
        sync::write_synced(input, slug, &md.1)
    })
}

// overwriteは、Conflictで返したheadのうち「Push」で確かめてもらったもの
pub fn commit_push(
    input: &SyncInput<'_>,
    slug: &str,
    message: &str,
    overwrite: Option<&str>,
) -> Result<PushResult, GithubError> {
    let state_path = state::state_path(input.data_dir, input.repo);
    // state.jsonは通信の前に読む。読めなければ通信もしない
    let header = state::read_state(&state_path, input.basis)?;
    let record = header.articles.get(slug);
    let branch = target_branch(record, input.basis, slug);
    // 記録のbranchがあるか。PATCHとPOST /git/refsで分ける
    let recorded_branch = record.and_then(|record| record.branch.as_deref()).is_some();

    // 手元の中身を、記録と比べるときにstoreのロックの中で読む
    let local = input
        .store
        .with_lock(|posts_dir, assets_base| state::local_bodies(posts_dir, assets_base, slug))?;
    // 手元にmdが無い記事はPushできない(#66で扱う)
    let md = local.md.clone().ok_or(GithubError::NothingToCommit)?;
    let diff = local_diff(record, &local);
    if diff.empty() {
        return Err(GithubError::NothingToCommit);
    }

    // 記録したbranchが消えていたら、何も作らずに読み込み直させる
    if recorded_branch {
        git::branch_head(input.transport, input.token, input.now, input.repo, &branch)?
            .ok_or(GithubError::BranchGone)?;
    }

    // 親を決める。overwriteがあれば、記録の有る無いにかかわらずそのコミットを親にする
    let mut parent = match overwrite {
        Some(commit) => commit.to_string(),
        None => match record.and_then(|record| record.branch.as_deref()) {
            Some(_) => record.expect("branchはある").commit.clone(),
            // 無ければ、ベースの先頭。branch名は形式の{slug}にslugを入れたもの
            None => git::branch_head(
                input.transport,
                input.token,
                input.now,
                input.repo,
                &input.basis.base_branch,
            )?
            .ok_or(GithubError::BaseBranchNotFound)?,
        },
    };
    // overwriteの初回は、確かめてもらった先頭から作り直すので重なりを確かめ直さない
    let mut confirming = overwrite.is_none();
    let mut blobs = BlobCache::default();
    // PATCHでbranchを進めた回数
    let mut attempts = 0;

    loop {
        // 親のコミットでの記事のファイルを確かめる。mdやmodeの読みは作り直しのたびに要る
        let remote_article = remote_article(input, slug, &parent)?;
        let (md_conflict, images) = conflict_of(record, &remote_article.files, &diff);
        if confirming && (md_conflict || !images.is_empty()) {
            // Conflictのheadは、確かめるときに読み直した、branchの今の先頭
            return Err(GithubError::Conflict {
                head: parent,
                md: md_conflict,
                images,
            });
        }
        let entries = tree_entries(input, slug, &diff, &remote_article.modes, &mut blobs)?;
        let tree = git::create_tree(
            input.transport,
            input.token,
            input.now,
            input.repo,
            &remote_article.base_tree,
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
        let merged = merged_files(&remote_article.files, &diff);
        if recorded_branch {
            attempts += 1;
            match git::update_ref(
                input.transport,
                input.token,
                input.now,
                input.repo,
                &branch,
                &commit,
            ) {
                Ok(()) => {
                    let result = PushResult {
                        branch: branch.clone(),
                        commit,
                    };
                    save_pushed(input, slug, &result, merged, &diff, &md, &state_path)?;
                    return Ok(result);
                }
                Err(error @ GithubError::Status { status: 422 | 404 }) => {
                    let head = git::branch_head(
                        input.transport,
                        input.token,
                        input.now,
                        input.repo,
                        &branch,
                    )?;
                    let head = match head {
                        Some(head) => head,
                        None => return Err(GithubError::BranchGone),
                    };
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
                    continue;
                }
                Err(error) => return Err(error),
            }
        } else {
            match git::create_ref(
                input.transport,
                input.token,
                input.now,
                input.repo,
                &branch,
                &commit,
            ) {
                Ok(()) => {
                    let result = PushResult {
                        branch: branch.clone(),
                        commit,
                    };
                    save_pushed(input, slug, &result, merged, &diff, &md, &state_path)?;
                    return Ok(result);
                }
                Err(error @ GithubError::Status { status: 422 }) => {
                    // 同じ名前のbranchがあった。記録は替えない。消えていれば元のエラー
                    if git::branch_head(
                        input.transport,
                        input.token,
                        input.now,
                        input.repo,
                        &branch,
                    )?
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
        slug,
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
            // 手元にmdが無い記事は、Pullで記録に書ける内容が無い。削除は#66で扱う
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
        let record = state
            .articles
            .entry(slug.to_string())
            .or_insert_with(|| Record {
                branch: None,
                commit: String::new(),
                md: String::new(),
                assets: BTreeMap::new(),
                applying: None,
            });
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
        }
        record.applying = None;
        state::write_state(&state_path, &state)?;
        Ok(())
    })
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
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::github::config::{github_posts_dir, RepoRef};
    use crate::github::fake::{
        b64_decode, blob_sha, read_only_dir, repo_ref, url_of, FakeRepo, RouteTransport,
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
    // 先頭だけ先に進める(内容は同じで、足す画像が記録に無いだけ)
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
            commit_push(&self.input(), slug, message, overwrite)
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
    }

    fn bodies(requests: &[Request], method: Method, url_part: &str) -> Vec<Value> {
        requests
            .iter()
            .filter(|request| request.method == method && request.url.contains(url_part))
            .map(|request| serde_json::from_slice(request.body.as_deref().unwrap()).unwrap())
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

    // 通ったときにGitHubを見せる共通の断わり
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
        // PATCHが422で、読み直したrefが404のとき
        let test = PushTest::new();
        let (repo, head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        repo.delete_branch(BRANCH);
        test.write_md(A_LOCAL);

        assert!(matches!(
            test.commit_push(SLUG, "更新", None),
            Err(GithubError::BranchGone)
        ));
        // 記録を替えない
        let record = &test.state()["articles"][SLUG];
        assert_eq!(record["branch"], BRANCH);
        assert_eq!(record["commit"], head);

        // PATCHが404のとき
        let test = PushTest::new();
        let (repo, head) = test.branch_synced();
        repo.accept_writes(&test.transport);
        repo.delete_branch(BRANCH);
        test.transport.json(
            Method::Patch,
            &test.patch_url(BRANCH),
            404,
            serde_json::json!({ "message": "Not Found" }),
        );
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
        // それぞれの親は、その前にほかの端末が進めた先頭(fixあり→最初は記録のコミット)
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

        let result = test.commit_push(SLUG, "更新", None).unwrap();
        assert_eq!(test.patch_count(&test.patch_url(BRANCH)), 3);
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
}
