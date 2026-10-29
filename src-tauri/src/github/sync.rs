use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::config::RepoRef;
use super::http::Transport;
use super::remote;
use super::state::{
    self, Applying, ArticleFiles, AssetRecord, Basis, Category, LocalFiles, Record, RemoteSnapshot,
    Skipped, State,
};
use super::GithubError;
use crate::store::Store;

// 記事のリモートの内容。branchがNoneならベース
#[derive(Debug, Clone, Copy)]
pub struct Remote<'a> {
    pub branch: Option<&'a str>,
    pub commit: &'a str,
    pub files: &'a ArticleFiles,
}

// RemoteSnapshotから記事のリモートの内容を選ぶ(記事用のブランチ→ベース)
pub fn remote_of<'a>(snapshot: &'a RemoteSnapshot, slug: &str) -> Option<Remote<'a>> {
    for (name, branch) in &snapshot.branches {
        if branch.slug == slug {
            return Some(Remote {
                branch: Some(name.as_str()),
                commit: &branch.commit,
                files: &branch.article,
            });
        }
    }
    snapshot.base.as_ref().and_then(|base| {
        base.articles.get(slug).map(|files| Remote {
            branch: None,
            commit: &base.commit,
            files,
        })
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Nothing,
    // 手元は書き換えず、記録をこれにする(食い違いを外す)
    Record(Record),
    // 手元をリモートの内容にし、記録をこれにする
    Replace(Record),
    // 手元のファイルと記録とsyncedを消す
    RemoveLocal,
    // 記録とsyncedを消し、手元は残す
    DropRecord,
    // 手元も記録も替えず、食い違いの印を付ける
    Conflict,
}

// ファイルの一覧とSHAで比べる。元にしたブランチとcommitは比べない
fn same_content(record: &Record, files: &ArticleFiles) -> bool {
    record.md == files.md
        && record.assets.len() == files.assets.len()
        && files.assets.iter().all(|(name, sha)| {
            record
                .assets
                .get(name)
                .is_some_and(|asset| asset.sha == *sha)
        })
}

// リモートの内容を、元にする記録にする。fetchedは、手元にある画像のうちSHAが同じものだけにする
fn make_record(remote: Remote<'_>, local: Option<&LocalFiles>) -> Record {
    Record {
        branch: remote.branch.map(str::to_string),
        commit: remote.commit.to_string(),
        md: remote.files.md.clone(),
        applying: None,
        assets: remote
            .files
            .assets
            .iter()
            .map(|(name, sha)| {
                let fetched = local.is_some_and(|local| local.assets.get(name) == Some(sha));
                (
                    name.clone(),
                    AssetRecord {
                        sha: sha.clone(),
                        fetched,
                    },
                )
            })
            .collect(),
    }
}

pub fn decide(record: Option<&Record>, remote: Option<Remote<'_>>, local: &LocalFiles) -> Decision {
    let local_empty = local.md.is_none() && local.assets.is_empty();
    match (record, remote) {
        (None, None) => Decision::Nothing,
        (None, Some(remote)) => {
            if local_empty {
                // 手元に記事が無いので、取り込む
                Decision::Replace(make_record(remote, None))
            } else if state::same_as_remote(None, local, remote.files) {
                // 手元の内容がリモートと同じなら、記録を作る
                Decision::Record(make_record(remote, Some(local)))
            } else {
                Decision::Conflict
            }
        }
        (Some(record), Some(remote)) => {
            // 取り込みの途中で落ち、手元にまだ何も無い
            if record.applying.is_some() && local_empty {
                return Decision::Replace(make_record(remote, None));
            }
            if record.branch.is_some() && remote.branch.is_none() {
                // 元にした記事用のブランチが消えてベースに戻った。手元に変更があれば編集中のまま残す
                let new_record = make_record(remote, Some(local));
                return if state::is_dirty(Some(record), local) {
                    Decision::Record(new_record)
                } else {
                    Decision::Replace(new_record)
                };
            }
            // 置き換えの途中で落ちた記録は、手元が混ざっているので置き換える側で扱う
            if record.applying.is_none()
                && record.branch.as_deref() == remote.branch
                && same_content(record, remote.files)
            {
                // 内容は同じなので、commitだけ先頭に進める
                return Decision::Record(Record {
                    commit: remote.commit.to_string(),
                    ..record.clone()
                });
            }
            if state::is_dirty(Some(record), local) {
                if state::same_as_remote(Some(record), local, remote.files) {
                    // 手元はリモートと同じなので、記録だけ直す
                    Decision::Record(make_record(remote, Some(local)))
                } else {
                    Decision::Conflict
                }
            } else {
                Decision::Replace(make_record(remote, Some(local)))
            }
        }
        (Some(record), None) => {
            // 元にしたブランチが消え、ベースにも無いときは、内容がどこにも残っていないこともあるので
            // 手元からは消さない
            if local_empty || record.branch.is_some() || state::is_dirty(Some(record), local) {
                Decision::DropRecord
            } else {
                Decision::RemoveLocal
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    pub fetched_at: i64,
    pub replaced: Vec<String>,
    pub removed: Vec<String>,
    pub skipped: Vec<Skipped>,
    // protectの記事で、置き換えか削除になったが書き換えなかったもの
    pub deferred: Vec<String>,
}

pub struct SyncInput<'a> {
    pub transport: &'a dyn Transport,
    pub token: &'a str,
    pub now: i64,
    pub data_dir: &'a Path,
    pub repo: &'a RepoRef,
    pub basis: &'a Basis,
    pub store: &'a Store,
    // 読み込みを始めたときのstore.generation()と、そのときに期待する向き先
    pub generation: u64,
    pub posts_dir: &'a Path,
    // 画面で開いている記事。編集欄に保存していない入力があるかはRust側で分からないので、置き換えも削除もしない
    pub protect: Option<&'a str>,
}

pub fn sync(input: &SyncInput<'_>) -> Result<SyncResult, GithubError> {
    let state_path = state::state_path(input.data_dir, input.repo);
    // state.jsonは通信の前に読む。読めなければ通信もしない
    let state = state::read_state(&state_path, input.basis)?;

    // 前回ベースに読めない記事があれば、先頭が同じでもツリーを取り直して調べ直す
    let mut previous = state.remote.clone();
    if state
        .skipped
        .iter()
        .any(|skipped| skipped.branch == input.basis.base_branch)
    {
        if let Some(base) = &mut previous.base {
            base.commit.clear();
        }
    }
    let read = remote::read_remote(
        input.transport,
        input.token,
        input.now,
        input.repo,
        input.basis,
        &previous,
    )?;

    // 読めない記事はこの回は何も替えない(リモートに無いと見なして手元を消さないため)
    let skipped_slugs: BTreeSet<String> = read
        .skipped
        .iter()
        .map(|skipped| skipped.slug.clone())
        .collect();
    // 扱いを決める記事のslug。リモートに有るものと、覚えた記録のあるもの
    let slugs: BTreeSet<String> = read
        .snapshot
        .branches
        .values()
        .map(|branch| branch.slug.clone())
        .chain(
            read.snapshot
                .base
                .iter()
                .flat_map(|base| base.articles.keys().cloned()),
        )
        .chain(state.articles.keys().cloned())
        .filter(|slug| !skipped_slugs.contains(slug))
        .collect();

    // 記録と違うmdの中身を取り出す(この時点では、置き換えるかどうかは決めない)
    let mut md_bodies: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for slug in &slugs {
        let Some(item) = remote_of(&read.snapshot, slug) else {
            continue;
        };
        // 置き換えの途中で落ちた記事は、手元が記録のmdと違うことがあるので取り直す
        let changed = state
            .articles
            .get(slug)
            .is_none_or(|record| record.md != item.files.md || record.applying.is_some());
        if !changed || md_bodies.contains_key(&item.files.md) {
            continue;
        }
        let content = super::git::blob(
            input.transport,
            input.token,
            input.now,
            input.repo,
            item.files.md.as_str(),
        )?;
        md_bodies.insert(item.files.md.clone(), content);
    }

    // 保存層のロックを取ったまま、世代と向き先を確かめて書き換える
    input.store.with_lock(move |posts_dir, assets_base| {
        check_unchanged(input, posts_dir)?;
        // 通信の間にfetch_imagesが書いたfetchedを消さないよう、読み直したものを元にする。
        // 記録のmdを替えるのはこの関数だけで、sync_lockで1つずつ動くので、mdは通信の前と同じ
        let state = state::read_state(&state_path, input.basis)?;

        // 記事ごとに、ロックを取ったまま手元のファイルのSHAを求めて、扱いを決める
        let mut entries: Vec<(String, LocalFiles, Decision)> = Vec::new();
        let mut deferred = Vec::new();
        for slug in &slugs {
            let local = state::local_files(posts_dir, assets_base, slug)?;
            let remote = remote_of(&read.snapshot, slug);
            let record = state.articles.get(slug);
            let mut decision = decide(record, remote, &local);
            if input.protect == Some(slug.as_str())
                && matches!(decision, Decision::Replace(_) | Decision::RemoveLocal)
            {
                deferred.push(slug.clone());
                decision = Decision::Nothing;
            }
            entries.push((slug.clone(), local, decision));
        }

        // 手元を書き換える前に、その記事の記録のapplyingに新しい内容(削除なら空)を入れて書く。
        // 途中で落ちて混ざった状態になっても、次の読み込みで変更と見なさないため
        let mut applying_state = state.clone();
        let mut stamped = false;
        for (slug, _, decision) in &entries {
            let stamp = match decision {
                Decision::Replace(new_record) => Applying {
                    md: Some(new_record.md.clone()),
                    assets: new_record.assets.clone(),
                },
                Decision::RemoveLocal => Applying {
                    md: None,
                    assets: BTreeMap::new(),
                },
                _ => continue,
            };
            match (applying_state.articles.get_mut(slug), decision) {
                (Some(record), _) => record.applying = Some(stamp),
                // 初めて取り込む記事は、新しい記録をそのまま途中の記録にする
                (None, Decision::Replace(new_record)) => {
                    applying_state.articles.insert(
                        slug.clone(),
                        Record {
                            applying: Some(stamp),
                            ..new_record.clone()
                        },
                    );
                }
                (None, _) => continue,
            }
            stamped = true;
        }
        if stamped {
            state::write_state(&state_path, &applying_state)?;
        }

        // 手元・synced・記録を書き換える
        let mut replaced = Vec::new();
        let mut removed = Vec::new();
        let mut articles = state.articles.clone();
        // 食い違いの印は毎回付け直す。この回に扱わない記事(読めない記事・後回しにした開いている記事)だけ前回のものを残す
        let mut conflicts: BTreeSet<String> = state
            .conflicts
            .iter()
            .filter(|slug| skipped_slugs.contains(*slug) || deferred.contains(slug))
            .cloned()
            .collect();
        for (slug, local, decision) in entries {
            // 途中の記録が残っていれば、syncedも途中の内容のことがあるので書き直す
            let old_md = state
                .articles
                .get(&slug)
                .filter(|record| record.applying.is_none())
                .map(|record| record.md.clone());
            match decision {
                Decision::Nothing => {}
                Decision::Conflict => {
                    // 手元も記録も替えず、食い違いの印だけ立てる
                    conflicts.insert(slug.clone());
                }
                Decision::Record(new_record) => {
                    if old_md.as_deref() != Some(new_record.md.as_str()) {
                        write_synced(input, &slug, md_body(&md_bodies, &new_record.md)?)?;
                    }
                    articles.insert(slug.clone(), new_record);
                }
                Decision::Replace(new_record) => {
                    replaced.push(slug.clone());
                    if old_md.as_deref() != Some(new_record.md.as_str()) {
                        write_synced(input, &slug, md_body(&md_bodies, &new_record.md)?)?;
                    }
                    if local.md.as_deref() != Some(new_record.md.as_str()) {
                        let path = posts_dir.join(format!("{slug}.md"));
                        write_file(&path, md_body(&md_bodies, &new_record.md)?)?;
                    }
                    // 画像: SHAが記録と同じで取ってきてあるものは残す。それ以外の手元の画像は消し、
                    // 新しい記録では取ってきていないものにする
                    rewrite_assets(assets_base, &slug, &local, &new_record)?;
                    articles.insert(slug.clone(), new_record);
                }
                Decision::RemoveLocal => {
                    removed.push(slug.clone());
                    remove_article(posts_dir, assets_base, &slug, &local)?;
                    remove_synced(input, &slug)?;
                    articles.remove(&slug);
                }
                Decision::DropRecord => {
                    remove_synced(input, &slug)?;
                    articles.remove(&slug);
                }
            }
        }

        // この回の記録・食い違いの印と、リモートの様子を書く
        let final_state = State {
            fetched_at: Some(input.now),
            remote: read.snapshot,
            articles,
            conflicts,
            skipped: read.skipped.clone(),
            ..state
        };
        state::write_state(&state_path, &final_state)?;
        Ok(SyncResult {
            fetched_at: input.now,
            replaced,
            removed,
            skipped: read.skipped,
            deferred,
        })
    })
}

// 通信の間に設定や保存先が替わったら、何も書かずに断る(世代はset_posts_dirが成功するたびに進む)
fn check_unchanged(input: &SyncInput<'_>, posts_dir: &Path) -> Result<(), GithubError> {
    if input.store.generation() != input.generation || posts_dir != input.posts_dir {
        return Err(GithubError::SettingsChanged);
    }
    Ok(())
}

fn md_body<'a>(
    md_bodies: &'a BTreeMap<String, Vec<u8>>,
    md_sha: &str,
) -> Result<&'a [u8], GithubError> {
    md_bodies
        .get(md_sha)
        .map(Vec::as_slice)
        .ok_or_else(|| GithubError::InvalidResponse {
            reason: "the md blob is missing".to_string(),
        })
}

// 同じディレクトリの一時ファイルに書いてから置き換える。ディレクトリが無ければ作る
fn write_file(path: &Path, body: &[u8]) -> Result<(), GithubError> {
    let dir = path
        .parent()
        .ok_or_else(|| GithubError::io(path, "no parent directory"))?;
    std::fs::create_dir_all(dir).map_err(|error| GithubError::io(dir, error))?;
    let tmp = crate::store::write_temp(dir, body).map_err(|error| GithubError::io(dir, error))?;
    tmp.persist(path)
        .map_err(|error| GithubError::io(path, error.error))?;
    Ok(())
}

// synced/{slug}.mdに、元にしたmdの中身を写す
fn write_synced(input: &SyncInput<'_>, slug: &str, body: &[u8]) -> Result<(), GithubError> {
    write_file(&state::synced_path(input.data_dir, input.repo, slug), body)
}

fn remove_synced(input: &SyncInput<'_>, slug: &str) -> Result<(), GithubError> {
    remove_file(&state::synced_path(input.data_dir, input.repo, slug))
}

fn remove_file(path: &Path) -> Result<(), GithubError> {
    if let Err(error) = std::fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(GithubError::io(path, error));
        }
    }
    Ok(())
}

// 手元の画像を、リモートの記録に合わせて残す・消す。下のディレクトリのファイルは手元で扱わない
fn rewrite_assets(
    assets_base: &Path,
    slug: &str,
    local: &LocalFiles,
    new_record: &Record,
) -> Result<(), GithubError> {
    let dir = assets_base.join(slug);
    for (name, sha) in &local.assets {
        let kept = new_record
            .assets
            .get(name)
            .is_some_and(|asset| asset.fetched && &asset.sha == sha);
        if !kept {
            remove_file(&dir.join(name))?;
        }
    }
    Ok(())
}

// 消すのは変更が無いと確かめたファイルだけ。下のディレクトリなど、見ていないものが残ればディレクトリも残す
fn remove_article(
    posts_dir: &Path,
    assets_base: &Path,
    slug: &str,
    local: &LocalFiles,
) -> Result<(), GithubError> {
    remove_file(&posts_dir.join(format!("{slug}.md")))?;
    let dir = assets_base.join(slug);
    for name in local.assets.keys() {
        remove_file(&dir.join(name))?;
    }
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

// 同時に1つだけ。動いている間に呼ばれたらSyncBusy
#[derive(Default)]
pub struct SyncLock(Mutex<()>);

impl SyncLock {
    pub fn run<T>(&self, f: impl FnOnce() -> Result<T, GithubError>) -> Result<T, GithubError> {
        let _guard = self.0.try_lock().map_err(|_| GithubError::SyncBusy)?;
        f()
    }
}

// 取ってきていない画像のうち、下のディレクトリになく、手元にもないものの名前とSHA
pub fn wanted_images(
    data_dir: &Path,
    repo: &RepoRef,
    basis: &Basis,
    store: &Store,
    slug: &str,
) -> Result<Vec<(String, String)>, GithubError> {
    let state = state::read_state(&state::state_path(data_dir, repo), basis)?;
    let Some(record) = state.articles.get(slug) else {
        return Ok(Vec::new());
    };
    let local = store
        .with_lock(|posts_dir, assets_base| state::local_files(posts_dir, assets_base, slug))?;
    Ok(record
        .assets
        .iter()
        .filter(|(name, asset)| {
            // 名前はGitHub上のツリーから来るので、assets/{slug}/の外を指すものは取らない
            crate::store::is_filename(name) && !asset.fetched && !local.assets.contains_key(*name)
        })
        .map(|(name, asset)| (name.clone(), asset.sha.clone()))
        .collect())
}

// 取れなかった件数を返す。記録が無い記事では0。ログインの切れ・通信の失敗などは、残りも取れないのでErrにする
pub fn fetch_images(input: &SyncInput<'_>, slug: &str) -> Result<usize, GithubError> {
    let state_path = state::state_path(input.data_dir, input.repo);
    let wanted = wanted_images(input.data_dir, input.repo, input.basis, input.store, slug)?;
    let mut failed = 0;
    for (name, sha) in wanted {
        let content =
            match super::git::blob(input.transport, input.token, input.now, input.repo, &sha) {
                Ok(content) => content,
                Err(error @ (GithubError::Status { .. } | GithubError::InvalidResponse { .. })) => {
                    eprintln!("Failed to fetch an image of {slug}: {name}: {error}");
                    failed += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
        // 書くときにロックを取り、記録がまだ同じSHAで取ってきていないことと、手元にまだ無いことを
        // 確かめてから書く。どちらかが替わっていれば、取る必要が無くなっただけなので数えない
        input.store.with_lock(|posts_dir, assets_base| {
            check_unchanged(input, posts_dir)?;
            let mut state = state::read_state(&state_path, input.basis)?;
            let Some(asset) = state
                .articles
                .get_mut(slug)
                .and_then(|record| record.assets.get_mut(&name))
                .filter(|asset| !asset.fetched && asset.sha == sha)
            else {
                return Ok(());
            };
            if !crate::store::is_filename(&name) {
                return Ok(());
            }
            let path = assets_base.join(slug).join(&name);
            if path.exists() {
                return Ok(());
            }
            write_file(&path, &content)?;
            asset.fetched = true;
            state::write_state(&state_path, &state)
        })?;
    }
    Ok(failed)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleState {
    pub category: Category,
    pub dirty: bool,
    pub conflict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleStates {
    // owner/name
    pub repo: String,
    pub fetched_at: Option<i64>,
    pub state_unreadable: bool,
    // state_unreadableなら空
    pub articles: BTreeMap<String, ArticleState>,
    pub skipped: Vec<Skipped>,
}

fn article_state_of(state: &State, slug: &str, local: &LocalFiles) -> ArticleState {
    let dirty = state::is_dirty(state.articles.get(slug), local);
    ArticleState {
        category: state::category(&state.remote, slug, dirty),
        dirty,
        conflict: state.conflicts.contains(slug),
    }
}

// 手元の記事(posts_dirの下の{slug}.md)ごと
pub fn article_states(
    data_dir: &Path,
    repo: &RepoRef,
    basis: &Basis,
    store: &Store,
) -> ArticleStates {
    let repo_name = format!("{}/{}", repo.owner, repo.name);
    let Ok(state) = state::read_state(&state::state_path(data_dir, repo), basis) else {
        // 分類せずに今の一覧を出すため、記事の状態は空にする
        return ArticleStates {
            repo: repo_name,
            fetched_at: None,
            state_unreadable: true,
            articles: BTreeMap::new(),
            skipped: Vec::new(),
        };
    };
    let locals = store.with_lock(|posts_dir, assets_base| {
        let mut locals = BTreeMap::new();
        let Ok(entries) = std::fs::read_dir(posts_dir) else {
            return locals;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let file_name = entry.file_name();
            let Some(stem) = file_name.to_str().and_then(|name| name.strip_suffix(".md")) else {
                continue;
            };
            if crate::store::validate_slug(stem).is_err() {
                continue;
            }
            let local = state::local_files(posts_dir, assets_base, stem).unwrap_or_default();
            locals.insert(stem.to_string(), local);
        }
        locals
    });
    let articles = locals
        .iter()
        .map(|(slug, local)| (slug.clone(), article_state_of(&state, slug, local)))
        .collect();
    ArticleStates {
        repo: repo_name,
        fetched_at: state.fetched_at,
        state_unreadable: false,
        articles,
        skipped: state.skipped,
    }
}

// 個別の記事の状態。手元にmdが無い記事と、state.jsonが読めないときは失敗にする
pub fn article_state(
    data_dir: &Path,
    repo: &RepoRef,
    basis: &Basis,
    store: &Store,
    slug: &str,
) -> Result<ArticleState, GithubError> {
    let state = state::read_state(&state::state_path(data_dir, repo), basis)?;
    let local = store.with_lock(|posts_dir, assets_base| {
        let md = posts_dir.join(format!("{slug}.md"));
        if !md.exists() {
            return Err(GithubError::io(&md, "the article is not found"));
        }
        state::local_files(posts_dir, assets_base, slug)
    })?;
    Ok(article_state_of(&state, slug, &local))
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::sync::Arc;

    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::github::config::github_posts_dir;
    use crate::github::fake::{
        blob_sha, json_response, matching_refs_url, read_only_dir, repo_ref, url_of, FakeRepo,
        RouteTransport,
    };
    use crate::github::http::{Method, Response};
    use crate::github::state::{AssetRecord, BaseSnapshot, BranchSnapshot, Skipped, State};
    use crate::store::Store;

    const TOKEN: &str = "TOKEN";
    const NOW: i64 = 1_000_000;
    const POSTS_PATH: &str = "src/content/blog";

    const MD: &[u8] = b"post\n";
    const MD2: &[u8] = b"post2\n";
    const BRANCH_MD: &[u8] = b"branch\n";
    const OTHER_MD: &[u8] = b"other\n";
    const PNG: &[u8] = b"png1\n";
    const PNG2: &[u8] = b"png2\n";
    const PNG3: &[u8] = b"png3\n";

    fn basis() -> Basis {
        Basis {
            base_branch: "main".to_string(),
            branch_format: "blog/{slug}".to_string(),
            posts_path: POSTS_PATH.to_string(),
        }
    }

    fn raw_response(status: u16, body: Vec<u8>) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body,
        }
    }

    fn empty_state() -> State {
        State::empty(basis())
    }

    // TempDirとStoreと通信を1つのまとまりで持つ
    struct SyncTest {
        _tmp: TempDir,
        data_dir: PathBuf,
        store: Arc<Store>,
        transport: RouteTransport,
        repo_of: RepoRef,
        basis_of: Basis,
        posts_dir: PathBuf,
        assets_base: PathBuf,
        state_path: PathBuf,
        synced_dir: PathBuf,
    }

    impl SyncTest {
        fn new() -> Self {
            let tmp = TempDir::new().unwrap();
            let data_dir = tmp.path().join("data");
            let repo_of = repo_ref();
            let basis_of = basis();
            let posts_dir = github_posts_dir(&data_dir, &repo_of, POSTS_PATH);
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
                transport: RouteTransport::new(),
                repo_of,
                basis_of,
            }
        }

        fn new_repo(&self) -> FakeRepo {
            FakeRepo::new(&self.transport, "owner", "blog")
        }

        // syncとfetch_imagesに渡す入力
        fn input(&self) -> SyncInput<'_> {
            SyncInput {
                transport: &self.transport,
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

        fn sync(&self) -> Result<SyncResult, GithubError> {
            sync(&self.input())
        }

        fn fetch_images(&self, slug: &str) -> Result<usize, GithubError> {
            fetch_images(&self.input(), slug)
        }

        fn article_states(&self) -> ArticleStates {
            article_states(
                &self.data_dir,
                &self.repo_of,
                &self.basis_of,
                self.store.as_ref(),
            )
        }

        fn article_state(&self, slug: &str) -> Result<ArticleState, GithubError> {
            article_state(
                &self.data_dir,
                &self.repo_of,
                &self.basis_of,
                self.store.as_ref(),
                slug,
            )
        }

        fn md_path(&self, slug: &str) -> PathBuf {
            self.posts_dir.join(format!("{slug}.md"))
        }

        fn synced_path(&self, slug: &str) -> PathBuf {
            self.synced_dir.join(format!("{slug}.md"))
        }

        fn assets_dir(&self, slug: &str) -> PathBuf {
            self.assets_base.join(slug)
        }

        fn image_path(&self, rel: &str) -> PathBuf {
            self.assets_base.join(rel)
        }

        fn tmp_path(&self) -> &std::path::Path {
            self._tmp.path()
        }

        // 手元を直接書く
        fn write_image(&self, rel: &str, content: &[u8]) {
            let path = self.assets_base.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn state(&self) -> Value {
            serde_json::from_slice(&fs::read(&self.state_path).unwrap()).unwrap()
        }

        fn edit_state(&self, change: impl FnOnce(&mut Value)) {
            let mut state = self.state();
            change(&mut state);
            fs::write(&self.state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        }

        // 画像のfetchedを付け替える
        fn mark_fetched(&self, slug: &str, names: &[(&str, bool)]) {
            self.edit_state(|state| {
                for (name, fetched) in names {
                    state["articles"][slug]["assets"][name]["fetched"] = Value::Bool(*fetched);
                }
            });
        }

        fn assert_same_file(&self, path: &std::path::Path, expected: &[u8]) {
            assert_eq!(
                fs::read(path).unwrap(),
                expected,
                "{} が替わっていないはず",
                path.display()
            );
        }
    }

    // decideのテストの道具
    fn files_of(md: &str, assets: &[(&str, &str)]) -> ArticleFiles {
        ArticleFiles {
            md: md.to_string(),
            assets: assets
                .iter()
                .map(|(name, sha)| (name.to_string(), sha.to_string()))
                .collect(),
        }
    }

    fn record_of(commit: &str, branch: Option<&str>, assets: &[(&str, &str, bool)]) -> Record {
        Record {
            branch: branch.map(str::to_string),
            commit: commit.to_string(),
            md: MD_SHA.to_string(),
            assets: assets
                .iter()
                .map(|(name, sha, fetched)| {
                    (
                        name.to_string(),
                        AssetRecord {
                            sha: sha.to_string(),
                            fetched: *fetched,
                        },
                    )
                })
                .collect(),
            applying: None,
        }
    }

    // 手元の様子。mdと画像のSHAの組を手で持つ
    fn local_md_of(md: &str, assets: &[(&str, &str)]) -> LocalFiles {
        LocalFiles {
            md: Some(md.to_string()),
            assets: assets
                .iter()
                .map(|(name, sha)| (name.to_string(), sha.to_string()))
                .collect(),
        }
    }

    fn local_empty() -> LocalFiles {
        LocalFiles::default()
    }

    // 記録のSHAのとり決め。Rのmdが替わるテストのときは、Rのmdが記録のmdと違うものにする
    const MD_SHA: &str = "sha-md-a";
    const MD2_SHA: &str = "sha-md-b";
    const MD3_SHA: &str = "sha-md-c";

    // Rはリモートの内容
    // 判定の表: 記録無し・R無し
    #[test]
    fn 記録もリモートの様子も無ければ何もしない() {
        // 手元にファイルがあっても、GitHub上に無い記事はそのまま
        for local in [
            local_empty(),
            local_md_of("sha-local", &[("01.png", "sha-1")]),
        ] {
            assert!(matches!(decide(None, None, &local), Decision::Nothing));
        }
    }

    // 判定の表: 記録無し・R有り・手元に無い
    #[test]
    fn 記録が無くてGitHub上にあれば手元に作る() {
        let files = files_of(MD2_SHA, &[("01.png", "sha-1")]);
        let remote = super::Remote {
            branch: Some("blog/post"),
            commit: "commit-new",
            files: &files,
        };
        let decision = decide(None, Some(remote), &local_empty());
        match decision {
            Decision::Replace(record) => {
                // 手元をリモートの内容にする
                assert_eq!(record.md, MD2_SHA);
                assert_eq!(record.commit, "commit-new");
                assert_eq!(record.branch.as_deref(), Some("blog/post"));
                // 画像はまだ取ってきていない
                assert!(!record.assets["01.png"].fetched);
                assert_eq!(record.assets["01.png"].sha, "sha-1");
                assert_eq!(record.applying, None);
            }
            other => panic!("Replaceのはず: {other:?}"),
        }
    }

    // 判定の表: 記録無し・R有り・手元がRと同じ
    #[test]
    fn 記録が無くても手元がリモートと同じなら記録を作る() {
        let files = files_of(MD_SHA, &[("01.png", "sha-1"), ("02.png", "sha-2")]);
        let remote = super::Remote {
            branch: Some("blog/post"),
            commit: "commit-new",
            files: &files,
        };
        // mdと画像がどれも同じSHAで、手元がGitHub上と同じ
        let local = local_md_of(MD_SHA, &[("01.png", "sha-1"), ("02.png", "sha-2")]);
        let decision = decide(None, Some(remote), &local);
        match decision {
            Decision::Record(record) => {
                assert_eq!(record.md, MD_SHA);
                assert_eq!(record.commit, "commit-new");
                assert_eq!(record.branch.as_deref(), Some("blog/post"));
                // 手元にある画像はfetchedにする
                assert_eq!(record.assets["01.png"].sha, "sha-1");
                assert!(record.assets["01.png"].fetched);
                assert_eq!(record.assets["02.png"].sha, "sha-2");
                assert!(record.assets["02.png"].fetched);
                assert_eq!(record.applying, None);
            }
            other => panic!("Recordのはず: {other:?}"),
        }
    }

    // 判定の表: 記録無し・R有り・手元がRと違う
    #[test]
    fn 記録が無くて手元がリモートと違えば食い違いにする() {
        let files = files_of(MD_SHA, &[("01.png", "sha-1")]);
        // mdのSHAが違う
        let remote = super::Remote {
            branch: Some("blog/post"),
            commit: "commit-new",
            files: &files,
        };
        assert!(matches!(
            decide(
                None,
                Some(remote),
                &local_md_of(MD2_SHA, &[("01.png", "sha-1")])
            ),
            Decision::Conflict
        ));
        // mdは同じでも、リモートに無い画像が手元にあれば同じではない
        let remote = super::Remote {
            branch: Some("blog/post"),
            commit: "commit-new",
            files: &files,
        };
        assert!(matches!(
            decide(
                None,
                Some(remote),
                &local_md_of(MD_SHA, &[("01.png", "sha-1"), ("02.png", "sha-2")])
            ),
            Decision::Conflict
        ));
    }

    // 判定の表: 記録有り・Rが記録と同じ
    #[test]
    fn 記録とリモートが同じならcommitを先頭に直す() {
        for (commit, changed_local) in [
            // 手元に変更が無い
            ("commit-old", false),
            // 手元に変更があっても、記録のSHAはそのまま
            ("commit-old", true),
        ] {
            let files = files_of(MD_SHA, &[("01.png", "sha-1"), ("02.png", "sha-2")]);
            let remote = super::Remote {
                branch: Some("blog/post"),
                commit: "commit-new",
                files: &files,
            };
            let record = record_of(
                commit,
                Some("blog/post"),
                &[("01.png", "sha-1", true), ("02.png", "sha-2", false)],
            );
            let local = if changed_local {
                local_md_of(
                    MD_SHA,
                    &[
                        ("01.png", "sha-1"),
                        ("02.png", "sha-2"),
                        ("03.png", "sha-3"),
                    ],
                )
            } else {
                local_md_of(MD_SHA, &[("01.png", "sha-1")])
            };

            let decision = decide(Some(&record), Some(remote), &local);
            match decision {
                Decision::Record(record) => {
                    // commitだけが先頭に合う。mdとassetsのfetchedはそのまま
                    assert_eq!(record.md, MD_SHA);
                    assert_eq!(record.commit, "commit-new");
                    assert_eq!(record.branch.as_deref(), Some("blog/post"));
                    assert!(record.assets["01.png"].fetched);
                    assert!(!record.assets["02.png"].fetched);
                    assert_eq!(record.applying, None);
                    if changed_local {
                        // 手元だけの画像は、記録に足さない
                        assert!(!record.assets.contains_key("03.png"));
                    }
                }
                other => panic!("Recordのはず: {other:?}"),
            }
        }
    }

    // 判定の表: 記録有り・Rが変わった・変更無し
    #[test]
    fn リモートが変わって手元に変更がなければ置き換える() {
        let files = files_of(MD2_SHA, &[("01.png", "sha-1"), ("03.png", "sha-3")]);
        let remote = super::Remote {
            branch: Some("blog/post"),
            commit: "commit-new",
            files: &files,
        };
        let record = record_of(
            "commit-old",
            Some("blog/post"),
            &[("01.png", "sha-1", true), ("02.png", "sha-2", false)],
        );
        // 手元: mdそのまま、01.pngは同じSHA。02.pngは取ってきていない
        let local = local_md_of(MD_SHA, &[("01.png", "sha-1")]);

        let decision = decide(Some(&record), Some(remote), &local);
        match decision {
            Decision::Replace(record) => {
                assert_eq!(record.md, MD2_SHA);
                assert_eq!(record.commit, "commit-new");
                assert_eq!(record.branch.as_deref(), Some("blog/post"));
                // 置き換えの記録の画像は、手元に同じSHAで取ってきてあるものだけがfetchedのまま
                assert!(record.assets["01.png"].fetched);
                assert_eq!(record.assets["01.png"].sha, "sha-1");
                // 替わった・消えた画像は、取ってきていないものになる
                assert!(!record.assets["03.png"].fetched);
                assert!(!record.assets.contains_key("02.png"));
                assert_eq!(record.applying, None);
            }
            other => panic!("Replaceのはず: {other:?}"),
        }
    }

    // 判定の表: 記録有り・Rが変わった・変更あり・手元がRと同じ
    #[test]
    fn リモートが変わって手元が同じなら記録を直す() {
        let files2 = files_of(MD2_SHA, &[("01.png", "sha-1"), ("03.png", "sha-3")]);
        let record = record_of(
            "commit-old",
            Some("blog/post"),
            &[("01.png", "sha-1", true), ("02.png", "sha-2", false)],
        );
        // 手元: mdはリモートと同じMD2、01.pngは同じSHA
        let local = local_md_of(MD2_SHA, &[("01.png", "sha-1"), ("03.png", "sha-3")]);
        let remote = super::Remote {
            branch: Some("blog/post"),
            commit: "commit-new",
            files: &files2,
        };
        let decision = decide(Some(&record), Some(remote), &local);
        match decision {
            Decision::Record(record) => {
                assert_eq!(record.md, MD2_SHA);
                assert_eq!(record.commit, "commit-new");
                assert_eq!(record.branch.as_deref(), Some("blog/post"));
                // 手元にある画像はfetched
                assert!(record.assets["01.png"].fetched);
                assert!(record.assets["03.png"].fetched);
                assert!(!record.assets.contains_key("02.png"));
                assert_eq!(record.applying, None);
            }
            other => panic!("Recordのはず: {other:?}"),
        }
    }

    // 判定の表: 記録有り・Rが変わった・変更あり・手元がRと違う
    #[test]
    fn リモートが変わって手元が違えば食い違いにする() {
        let files2 = files_of(MD2_SHA, &[("01.png", "sha-1")]);
        let remote = super::Remote {
            branch: Some("blog/post"),
            commit: "commit-new",
            files: &files2,
        };
        let record = record_of(
            "commit-old",
            Some("blog/post"),
            &[("01.png", "sha-1", true)],
        );
        // 手元のmdが記録ともリモートとも違う
        let local = local_md_of("sha-local", &[("01.png", "sha-1")]);
        assert!(matches!(
            decide(Some(&record), Some(remote), &local),
            Decision::Conflict
        ));
    }

    // 判定の表: 元にしたブランチが替わった
    #[test]
    fn 元にしたブランチが替わったら変わったとして扱う() {
        // ベースの記事に記事用のブランチができた
        let files = files_of(MD2_SHA, &[("01.png", "sha-1")]);
        let remote = super::Remote {
            branch: Some("blog/post"),
            commit: "commit-new",
            files: &files,
        };
        // 変更無し: 置き換える
        let record = record_of("commit-old", None, &[("01.png", "sha-1", true)]);
        let local = local_md_of(MD_SHA, &[("01.png", "sha-1")]);
        let decision = decide(Some(&record), Some(remote), &local);
        match decision {
            Decision::Replace(record) => {
                assert_eq!(record.md, MD2_SHA);
                assert_eq!(record.commit, "commit-new");
                assert_eq!(record.branch.as_deref(), Some("blog/post"));
                assert!(record.assets["01.png"].fetched);
            }
            other => panic!("Replaceのはず: {other:?}"),
        }
        // 中身が同じでも、元にしたブランチだけが替われば置き換える
        let same = files_of(MD_SHA, &[("01.png", "sha-1")]);
        let switched = super::Remote {
            branch: Some("blog/post"),
            commit: "commit-new",
            files: &same,
        };
        let record = record_of("commit-old", None, &[("01.png", "sha-1", true)]);
        match decide(Some(&record), Some(switched), &local) {
            Decision::Replace(record) => {
                assert_eq!(record.branch.as_deref(), Some("blog/post"));
                assert_eq!(record.commit, "commit-new");
            }
            other => panic!("Replaceのはず: {other:?}"),
        }
        // 変更あり・手元がRと違う: 食い違い
        let record = record_of("commit-old", None, &[("01.png", "sha-1", true)]);
        let local = local_md_of("sha-local", &[("01.png", "sha-1")]);
        assert!(matches!(
            decide(Some(&record), Some(remote), &local),
            Decision::Conflict
        ));
    }

    // 判定の表: 記録有り(ベース)・R無し
    #[test]
    fn ベースから消えたら手元を消すか記録だけ消す() {
        // 変更無し: 手元からも消し、記録も消す
        let record = record_of("commit-old", None, &[("01.png", "sha-1", true)]);
        let local = local_md_of(MD_SHA, &[("01.png", "sha-1")]);
        assert!(matches!(
            decide(Some(&record), None, &local),
            Decision::RemoveLocal
        ));
        // 変更あり: 記録を消し、手元は残す
        let record = record_of("commit-old", None, &[("01.png", "sha-1", true)]);
        let local = local_md_of("sha-local", &[("01.png", "sha-1")]);
        assert!(matches!(
            decide(Some(&record), None, &local),
            Decision::DropRecord
        ));
    }

    // 判定の表: 記録有り(記事用)・ブランチが消え、ベースに有る
    #[test]
    fn 記事用のブランチが消えてベースにあればベースの内容に合わせる() {
        let base_files = files_of(MD3_SHA, &[]);
        let remote = super::Remote {
            branch: None,
            commit: "commit-base",
            files: &base_files,
        };
        // 変更無し: ベースの内容に置き換える
        let record = record_of(
            "commit-old",
            Some("blog/post"),
            &[("01.png", "sha-1", true)],
        );
        let local = local_md_of(MD_SHA, &[("01.png", "sha-1")]);
        let decision = decide(Some(&record), Some(remote), &local);
        match decision {
            Decision::Replace(record) => {
                assert_eq!(record.md, MD3_SHA);
                assert_eq!(record.commit, "commit-base");
                // 元にしたブランチはベースになる
                assert_eq!(record.branch, None);
                // ベースに無い画像は記録に入らない
                assert!(record.assets.is_empty());
            }
            other => panic!("Replaceのはず: {other:?}"),
        }
        // 変更あり: 記録をベースの内容にする。手元は残す
        let record = record_of("commit-old", Some("blog/post"), &[]);
        let local = local_md_of("sha-local", &[]);
        let decision = decide(Some(&record), Some(remote), &local);
        match decision {
            Decision::Record(record) => {
                assert_eq!(record.md, MD3_SHA);
                assert_eq!(record.commit, "commit-base");
                assert_eq!(record.branch, None);
            }
            other => panic!("Recordのはず: {other:?}"),
        }
    }

    // 判定の表: 記録有り(記事用)・ブランチが消え、ベースにも無い
    #[test]
    fn 記事用のブランチが消えてベースにも無ければ記録だけ消す() {
        // 変更の有無によらず、記録を消して手元は残す
        let record = record_of("commit-old", Some("blog/post"), &[]);
        for local in [local_md_of(MD_SHA, &[]), local_md_of("sha-local", &[])] {
            assert!(matches!(
                decide(Some(&record), None, &local),
                Decision::DropRecord
            ));
        }
    }

    #[test]
    fn 手元にファイルが無くリモートにも無ければ記録だけ消す() {
        let record = record_of(
            "commit-old",
            None,
            &[("01.png", "sha-1", true), ("02.png", "sha-2", false)],
        );
        assert!(matches!(
            decide(Some(&record), None, &local_empty()),
            Decision::DropRecord
        ));
    }

    // 判定の表: 記録のcommitは、記事用のブランチならその先頭、ベースならベースの先頭
    #[test]
    fn 記録のcommitはリモートの先頭にする() {
        // 記事用のブランチ
        let files = files_of(MD_SHA, &[]);
        let remote = super::Remote {
            branch: Some("blog/post"),
            commit: "commit-branch",
            files: &files,
        };
        let record = record_of("commit-old", Some("blog/post"), &[]);
        let decision = decide(Some(&record), Some(remote), &local_md_of(MD_SHA, &[]));
        match decision {
            Decision::Record(record) => {
                assert_eq!(record.commit, "commit-branch");
                assert_eq!(record.branch.as_deref(), Some("blog/post"));
            }
            other => panic!("Recordのはず: {other:?}"),
        }
        // ベース
        let files = files_of(MD_SHA, &[]);
        let remote = super::Remote {
            branch: None,
            commit: "commit-base",
            files: &files,
        };
        let record = record_of("commit-branch0", None, &[]);
        let decision = decide(Some(&record), Some(remote), &local_md_of(MD_SHA, &[]));
        match decision {
            Decision::Record(record) => {
                assert_eq!(record.commit, "commit-base");
                assert_eq!(record.branch, None);
            }
            other => panic!("Recordのはず: {other:?}"),
        }
    }

    // 読み込みの全体のテストの下ごしらえ
    impl SyncTest {
        // ベースのブランチにpostがあり、初回の読み込みが済んでいる
        fn post_synced(&self) -> FakeRepo {
            let repo = self.new_repo();
            repo.branch(
                &self.transport,
                "main",
                &[
                    ("src/content/blog/post.md", MD),
                    ("src/assets/post/01.png", PNG),
                ],
            );
            self.sync().unwrap();
            repo
        }

        // 置き換えの途中の失敗を試すための下ごしらえ
        fn replace_setup(&self) -> FakeRepo {
            let repo = self.new_repo();
            repo.branch(
                &self.transport,
                "main",
                &[
                    ("src/content/blog/post.md", MD),
                    ("src/assets/post/01.png", PNG),
                    ("src/assets/post/02.png", PNG2),
                ],
            );
            self.sync().unwrap();
            // 両方の画像は取ってきてある(手元にも置いてある)
            self.mark_fetched("post", &[("01.png", true), ("02.png", true)]);
            self.write_image("post/01.png", PNG);
            self.write_image("post/02.png", PNG2);
            // リモート: md替わり、01.png SHA同じ、02.png消え、03.png増え
            repo.branch(
                &self.transport,
                "main",
                &[
                    ("src/content/blog/post.md", MD2),
                    ("src/assets/post/01.png", PNG),
                    ("src/assets/post/03.png", PNG3),
                ],
            );
            repo
        }

        // 3つの画像(1つ下のディレクトリのファイルを含む)を持つ記事
        fn image_set_synced(&self) -> FakeRepo {
            let repo = self.new_repo();
            repo.branch(
                &self.transport,
                "main",
                &[
                    ("src/content/blog/post.md", MD),
                    ("src/assets/post/01.png", PNG),
                    ("src/assets/post/02.png", PNG2),
                    ("src/assets/post/sub/03.png", PNG3),
                ],
            );
            self.sync().unwrap();
            repo
        }
    }

    // 読み込みの全体: 初めての読み込み
    #[test]
    fn 初めての読み込みはベースと記事用のブランチの記事を取ってくる() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        let base_commit = repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        let branch_commit = repo.branch(
            &test.transport,
            "blog/post",
            &[
                ("src/content/blog/post.md", BRANCH_MD),
                ("src/assets/post/01.png", PNG),
            ],
        );

        let result = test.sync().unwrap();
        // mdは手元に書かれ、記事用のブランチの内容。syncedにも写る
        assert_eq!(
            fs::read_to_string(test.md_path("post")).unwrap(),
            "branch\n"
        );
        assert_eq!(
            fs::read_to_string(test.synced_path("post")).unwrap(),
            "branch\n"
        );
        // state.jsonは、mdとbranchとcommitとfetchedを持つ。画像は、取ってきていないものになる
        let state = test.state();
        let record = &state["articles"]["post"];
        assert_eq!(record["md"], blob_sha(BRANCH_MD));
        assert_eq!(record["branch"], "blog/post");
        assert_eq!(record["commit"], branch_commit);
        assert_eq!(record["assets"]["01.png"]["fetched"], false);
        assert_eq!(state["remote"]["base"]["commit"], base_commit);
        // 画像は取ってきていないので、手元に同一のファイルは無い
        assert!(!test.image_path("post/01.png").exists());
        // results.replacedとremovedはslug
        assert_eq!(result.replaced, vec!["post".to_string()]);
        assert!(result.removed.is_empty());
        assert_eq!(state["fetchedAt"], NOW);
        assert_eq!(result.fetched_at, NOW);
    }

    // 読み込みの全体: 2回目は変わっていない
    #[test]
    fn リモートが変わっていなければ2回目は手元を書き換えない() {
        let test = SyncTest::new();
        let _repo = test.post_synced();
        let state_before = test.state();
        let md_before = fs::read(test.md_path("post")).unwrap();
        let synced_before = fs::read(test.synced_path("post")).unwrap();

        let before = test.transport.requests().len();
        let result = test.sync().unwrap();
        // blobを1つも取らない
        let urls: Vec<String> = test.transport.requests()[before..]
            .iter()
            .map(|r| r.url.clone())
            .collect();
        assert!(
            !urls.iter().any(|url| url.contains("/git/blobs/")),
            "blobを取らないはず: {urls:?}"
        );
        // 変わらない: replacedとremovedは空
        assert!(result.replaced.is_empty());
        assert!(result.removed.is_empty());
        assert!(result.skipped.is_empty());
        test.assert_same_file(&test.md_path("post"), &md_before);
        test.assert_same_file(&test.synced_path("post"), &synced_before);
        // 記録も前のまま(fetchedAtは替わってよい)
        assert_eq!(test.state()["articles"], state_before["articles"]);
    }

    // 読み込みの全体: リモートで変わった記事を置き換える
    #[test]
    fn リモートで変わった記事を置き換えてsyncedも替わる() {
        let test = SyncTest::new();
        let _repo = test.replace_setup();

        let result = test.sync().unwrap();
        // mdが替わる
        assert_eq!(fs::read_to_string(test.md_path("post")).unwrap(), "post2\n");
        // syncedも替わる
        assert_eq!(
            fs::read_to_string(test.synced_path("post")).unwrap(),
            "post2\n"
        );
        let record = &test.state()["articles"]["post"];
        assert_eq!(record["md"], blob_sha(MD2));
        assert!(record["applying"].is_null());
        // 画像: SHAが同じで取ってきてあるものは残り、替わった・消えたものは手元から消える
        assert_eq!(record["assets"]["01.png"]["fetched"], true);
        assert_eq!(record["assets"]["03.png"]["fetched"], false);
        assert!(!record["assets"].as_object().unwrap().contains_key("02.png"));
        assert_eq!(fs::read(test.image_path("post/01.png")).unwrap(), PNG);
        assert!(!test.image_path("post/02.png").exists());
        assert!(!test.image_path("post/03.png").exists());
        // 置き換える記事は結果に入る
        assert_eq!(result.replaced, vec!["post".to_string()]);
        assert!(result.removed.is_empty());
    }

    // 読み込みの全体: ベースから消えた記事
    #[test]
    fn ベースから消えた変更の無い記事は手元とsyncedも消える() {
        let test = SyncTest::new();
        let repo = test.post_synced();
        // 取ってきている画像
        test.mark_fetched("post", &[("01.png", true)]);
        test.write_image("post/01.png", PNG);
        // ベースから消す
        repo.branch(&test.transport, "main", &[]);

        let result = test.sync().unwrap();
        assert_eq!(result.removed, vec!["post".to_string()]);
        assert!(result.replaced.is_empty());
        // mdも画像もsyncedも消える。手元の記事のディレクトリも消える
        assert!(!test.md_path("post").exists());
        assert!(!test.assets_dir("post").exists());
        assert!(!test.synced_path("post").exists());
        // 記録は消える
        let state = test.state();
        assert!(state["articles"]["post"].is_null());
        assert!(state["conflicts"].as_array().unwrap().is_empty());
    }

    // 読み込みの全体: 手元に変更のある記事
    #[test]
    fn 手元に変更のある記事はRが消えても変わっても手元を残す() {
        // (a) ベースから消える: 記録だけ消え、syncedも消える。手元はそのまま
        let test = SyncTest::new();
        let repo = test.post_synced();
        test.mark_fetched("post", &[("01.png", true)]);
        test.write_image("post/01.png", PNG);
        fs::write(test.md_path("post"), "edited by user\n").unwrap();
        repo.branch(&test.transport, "main", &[]);
        let result = test.sync().unwrap();
        assert_eq!(
            fs::read_to_string(test.md_path("post")).unwrap(),
            "edited by user\n"
        );
        assert_eq!(fs::read(test.image_path("post/01.png")).unwrap(), PNG);
        // 記録とsyncedだけが消える
        assert!(!test.synced_path("post").exists());
        let state = test.state();
        assert!(state["articles"]["post"].is_null());
        assert!(state["conflicts"].as_array().unwrap().is_empty());
        assert!(result.replaced.is_empty());
        assert!(result.removed.is_empty());

        // (b) リモートが変わる: 食い違いになる。手元も記録・syncedもそのまま
        let test = SyncTest::new();
        let repo = test.post_synced();
        test.mark_fetched("post", &[("01.png", true)]);
        test.write_image("post/01.png", PNG);
        fs::write(test.md_path("post"), "edited by user\n").unwrap();
        // リモートのmdが替わる
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD2),
                ("src/assets/post/01.png", PNG),
            ],
        );
        let state_before = test.state();
        test.sync().unwrap();
        let state = test.state();
        assert_eq!(
            fs::read_to_string(test.md_path("post")).unwrap(),
            "edited by user\n"
        );
        assert_eq!(fs::read(test.image_path("post/01.png")).unwrap(), PNG);
        // 記録は替わらない
        assert_eq!(state["articles"]["post"], state_before["articles"]["post"]);
        // syncedは替わらない
        assert_eq!(
            fs::read_to_string(test.synced_path("post")).unwrap(),
            "post\n"
        );
        // conflictsにslugが入る
        let conflicts = state["conflicts"].as_array().unwrap();
        assert!(
            conflicts.iter().any(|value| value == "post"),
            "{conflicts:?}"
        );

        // (c) 記事用のブランチが消えてベースに有る場合: 記録をベースの内容にする。手元は編集したまま、
        // syncedはベースの内容になる
        let test = SyncTest::new();
        let repo = test.new_repo();
        let base_commit = repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        repo.branch(
            &test.transport,
            "blog/post",
            &[
                ("src/content/blog/post.md", BRANCH_MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        test.sync().unwrap();
        fs::write(test.md_path("post"), "edited by user\n").unwrap();
        // ブランチを消す
        repo.delete_branch("blog/post");
        test.sync().unwrap();
        // 記録は、ベースの内容に替わる。branchもベースになる(None)
        let record = &test.state()["articles"]["post"];
        assert_eq!(record["md"], blob_sha(MD));
        assert_eq!(record["commit"], base_commit);
        assert!(record["branch"].is_null());
        // syncedはベースの内容になる
        assert_eq!(
            fs::read_to_string(test.synced_path("post")).unwrap(),
            "post\n"
        );
        // 手元は編集したまま
        assert_eq!(
            fs::read_to_string(test.md_path("post")).unwrap(),
            "edited by user\n"
        );
        // state.jsonのconflictsは空のはず
        let state = test.state();
        assert!(state["conflicts"].as_array().unwrap().is_empty());
    }

    // 読み込みの全体: 通信の失敗
    #[test]
    fn 通信のどこかで失敗したら手元とstate_jsonを書き換えない() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        // 一覧の応答を500にする
        let url = matching_refs_url(&["blog"]);
        test.transport.on(
            Method::Get,
            &url,
            Box::new(|_| Ok(json_response(500, serde_json::json!({"message": "failed"})))),
        );
        let result = test.sync();
        assert!(result.is_err());
        assert!(!test.state_path.exists());
        assert!(!test.md_path("post").exists());
        assert!(!test.synced_path("post").exists());
    }

    // 読み込みの全体: blobのSHAの不一致
    #[test]
    fn blobのSHAが合わない応答でも書き換えない() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        // blobのURLを上書きして、別の内容を返す
        let sha = blob_sha(MD);
        test.transport.raw(
            Method::Get,
            &url_of(
                &["repos", "owner", "blog", "git", "blobs", sha.as_str()],
                &[],
            ),
            200,
            b"mismatch\n".to_vec(),
        );
        let result = test.sync();
        assert!(result.is_err());
        assert!(!test.state_path.exists());
        assert!(!test.md_path("post").exists());
    }

    // 読み込みの全体: レート制限
    #[test]
    fn レート制限の応答でも書き換えない() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        // matching-refsを429にする
        test.transport.on(
            Method::Get,
            &matching_refs_url(&["blog"]),
            Box::new(|_| {
                Ok(json_response(
                    429,
                    serde_json::json!({"message": "too many"}),
                ))
            }),
        );
        let result = test.sync();
        assert!(matches!(result, Err(GithubError::RateLimited { .. })));
        assert!(!test.state_path.exists());
        assert!(!test.md_path("post").exists());
    }

    // 読み込みの全体: 世代の変更
    #[test]
    fn 通信の間に世代が進むと書かずにSettingsChangedを返す() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        let url = matching_refs_url(&["blog"]);
        let store = Arc::clone(&test.store);
        let posts_dir = test.posts_dir.clone();
        let refs = serde_json::json!([serde_json::json!({
            "ref": "refs/heads/main",
            "object": { "sha": "sha-main", "type": "commit" },
        })]);
        test.transport.on(
            Method::Get,
            &url,
            Box::new(move |_| {
                // 通信の間に同じ向き先でset_posts_dirを呼び、世代を進める
                let _ = store.set_posts_dir(posts_dir.clone());
                Ok(json_response(200, refs.clone()))
            }),
        );
        let generation_before = test.store.generation();

        let result = test.sync();
        assert!(matches!(result, Err(GithubError::SettingsChanged)));
        // 通信の中で世代が進んだ
        assert!(test.store.generation() > generation_before);
        assert!(!test.state_path.exists());
        assert!(!test.md_path("post").exists());
    }

    // 読み込みの全体: 保存層の向き先
    #[test]
    fn 保存層の向き先が替わっていたら書かずにSettingsChangedを返す() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        // 今の保存先とは別の、使えるパスに替える
        let other = test.tmp_path().join("other-posts");
        test.store.set_posts_dir(other).unwrap();
        let result = test.sync();
        assert!(matches!(result, Err(GithubError::SettingsChanged)));
        assert!(!test.state_path.exists());
        assert!(!test.md_path("post").exists());
    }

    // SyncLock::runは同時に1つだけ
    #[test]
    fn 動いている間に呼ぶとSyncBusyになる() {
        use std::sync::Arc as ArcInner;

        let lock = ArcInner::new(SyncLock::default());
        // 1つ目はロックを持ったまま待ち、2つ目がSyncBusyを取るのを確認する
        let (running_tx, running_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (result_tx, result_rx) = mpsc::channel::<Result<(), GithubError>>();
        {
            let lock = ArcInner::clone(&lock);
            std::thread::spawn(move || {
                let result = lock.run(move || {
                    running_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                });
                result_tx.send(result).unwrap();
            });
        }
        running_rx.recv().expect("最初の処理は始まるはず");
        let busy = lock.run(|| Ok(()));
        assert!(matches!(busy, Err(GithubError::SyncBusy)));
        release_tx.send(()).unwrap();
        assert!(result_rx.recv().unwrap().is_ok());
    }

    // state.jsonが読めないときは、stateの読み込みで止める
    #[test]
    fn state_jsonが読めなければStateUnreadableで通信もしない() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        fs::create_dir_all(test.state_path.parent().unwrap()).unwrap();
        fs::write(&test.state_path, "{").unwrap();
        let state_before = fs::read(&test.state_path).unwrap();

        let result = test.sync();
        assert!(matches!(result, Err(GithubError::StateUnreadable)));
        // 通信は始まっていない
        assert!(test.transport.requests().is_empty());
        test.assert_same_file(&test.state_path, &state_before);
    }

    // 読み込みの全体: 書き込みの途中の失敗。mdと画像とsyncedをどの順に書くかは決めていないので、
    // 書けなくした場所のほかは確かめない
    #[test]
    #[cfg(unix)]
    fn 画像のディレクトリが書けないとapplyingが残る() {
        let test = SyncTest::new();
        test.replace_setup();
        let Some(_read_only) = read_only_dir(&test.assets_dir("post")) else {
            return;
        };
        let result = test.sync();
        drop(_read_only);
        assert!(result.is_err());
        // 記録のmdは古いままで、applyingに新しいmdのSHAがある
        let record = &test.state()["articles"]["post"];
        assert_eq!(record["md"], blob_sha(MD));
        assert_eq!(record["applying"]["md"], blob_sha(MD2));
        // 消せなかった画像は残る
        assert_eq!(fs::read(test.image_path("post/02.png")).unwrap(), PNG2);
    }

    #[test]
    #[cfg(unix)]
    fn syncedが書けないとapplyingが残る() {
        let test = SyncTest::new();
        test.replace_setup();
        let Some(_read_only) = read_only_dir(&test.synced_dir) else {
            return;
        };
        let result = test.sync();
        drop(_read_only);
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(test.synced_path("post")).unwrap(),
            "post\n"
        );
        let record = &test.state()["articles"]["post"];
        assert_eq!(record["md"], blob_sha(MD));
        assert_eq!(record["applying"]["md"], blob_sha(MD2));
    }

    #[test]
    #[cfg(unix)]
    fn state_jsonが書けないと書き換えを止める() {
        let test = SyncTest::new();
        test.replace_setup();
        let state_before = fs::read(&test.state_path).unwrap();
        let Some(_read_only) = read_only_dir(test.state_path.parent().unwrap()) else {
            return;
        };
        let result = test.sync();
        drop(_read_only);
        assert!(result.is_err());
        test.assert_same_file(&test.state_path, &state_before);
        // applyingを書けないので、手元も書き換えない
        assert_eq!(fs::read_to_string(test.md_path("post")).unwrap(), "post\n");
        assert_eq!(fs::read(test.image_path("post/02.png")).unwrap(), PNG2);
    }

    // applyingが残っていても、次の読み込みで記録が直る
    #[test]
    #[cfg(unix)]
    fn applyingが残っていても次の読み込みで直る() {
        let test = SyncTest::new();
        test.replace_setup();
        let Some(_read_only) = read_only_dir(&test.synced_dir) else {
            return;
        };
        let failed = test.sync();
        drop(_read_only);
        assert!(failed.is_err());
        // 書けるように戻すと、正しい記録に直る
        let result = test.sync().unwrap();
        assert_eq!(result.replaced, vec!["post".to_string()]);
        let record = &test.state()["articles"]["post"];
        assert_eq!(record["md"], blob_sha(MD2));
        assert!(record["applying"].is_null());
        // SHAの同じ画像は取ってきたまま、新しい画像は取ってきていない
        assert_eq!(record["assets"]["01.png"]["fetched"], true);
        assert_eq!(record["assets"]["03.png"]["fetched"], false);
        assert_eq!(fs::read_to_string(test.md_path("post")).unwrap(), "post2\n");
        assert_eq!(
            fs::read_to_string(test.synced_path("post")).unwrap(),
            "post2\n"
        );
        assert!(!test.image_path("post/02.png").exists());
        assert_eq!(fs::read(test.image_path("post/01.png")).unwrap(), PNG);
        let state = test.state();
        assert!(state["conflicts"].as_array().unwrap().is_empty());
    }

    // applyingが残っていて手元が替わったら、変更として残る
    #[test]
    #[cfg(unix)]
    fn applyingがある間に手元を編集していたら変更として残る() {
        let test = SyncTest::new();
        test.replace_setup();
        let Some(_read_only) = read_only_dir(&test.synced_dir) else {
            return;
        };
        let failed = test.sync();
        drop(_read_only);
        assert!(failed.is_err());
        // applyingが残っているうちに手元を編集する
        fs::write(test.md_path("post"), "edited by user\n").unwrap();
        test.sync().unwrap();

        let state = test.state();
        let record = &state["articles"]["post"];
        assert_eq!(record["md"], blob_sha(MD));
        assert_eq!(record["applying"]["md"], blob_sha(MD2));
        assert_eq!(
            fs::read_to_string(test.md_path("post")).unwrap(),
            "edited by user\n"
        );
        // conflictがつく
        assert!(state["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "post"));
    }

    #[test]
    #[cfg(unix)]
    fn 置き換えの途中で落ちた後にリモートが元に戻っても読み込める() {
        let test = SyncTest::new();
        let repo = test.replace_setup();
        let Some(read_only) = read_only_dir(&test.assets_dir("post")) else {
            return;
        };
        assert!(test.sync().is_err());
        drop(read_only);
        // リモートを置き換える前の内容に戻す
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
                ("src/assets/post/02.png", PNG2),
            ],
        );
        test.sync().unwrap();
        assert_eq!(fs::read(test.md_path("post")).unwrap(), MD);
        assert_eq!(fs::read(test.synced_path("post")).unwrap(), MD);
        let state = test.state();
        assert!(state["articles"]["post"]["applying"].is_null());
        assert!(state["conflicts"].as_array().unwrap().is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn 初めての取り込みの途中で落ちても次の読み込みで取り込める() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        // 記事とsyncedは書けて、state.jsonだけ書けない
        for dir in [&test.posts_dir, &test.assets_base, &test.synced_dir] {
            fs::create_dir_all(dir).unwrap();
        }
        let Some(read_only) = read_only_dir(test.state_path.parent().unwrap()) else {
            return;
        };
        assert!(test.sync().is_err());
        drop(read_only);
        test.sync().unwrap();
        assert_eq!(fs::read(test.md_path("post")).unwrap(), MD);
        let state = test.state();
        assert!(state["articles"]["post"]["applying"].is_null());
        assert!(state["conflicts"].as_array().unwrap().is_empty());
        assert!(!test.article_state("post").unwrap().dirty);
    }

    #[test]
    fn 記録の無い記事の食い違いの印はリモートから消えると外れる() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", &[("src/content/blog/post.md", MD)]);
        fs::create_dir_all(&test.posts_dir).unwrap();
        fs::write(test.md_path("post"), "local\n").unwrap();
        test.sync().unwrap();
        assert_eq!(test.state()["conflicts"], serde_json::json!(["post"]));
        repo.branch(&test.transport, "main", &[]);
        test.sync().unwrap();
        assert!(test.state()["conflicts"].as_array().unwrap().is_empty());
        assert_eq!(fs::read_to_string(test.md_path("post")).unwrap(), "local\n");
    }

    #[test]
    fn 読み込みの通信の間に画像の取得が立てたfetchedは消さない() {
        let test = SyncTest::new();
        let _repo = test.post_synced();
        let state_path = test.state_path.clone();
        test.transport.on(
            Method::Get,
            &matching_refs_url(&["blog"]),
            Box::new(move |_| {
                let mut state: Value =
                    serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
                state["articles"]["post"]["assets"]["01.png"]["fetched"] = Value::Bool(true);
                fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
                Ok(json_response(200, serde_json::json!([])))
            }),
        );
        test.write_image("post/01.png", PNG);
        test.sync().unwrap();
        assert_eq!(
            test.state()["articles"]["post"]["assets"]["01.png"]["fetched"],
            true
        );
    }

    // 食い違いの印は毎回付け直す: Rが記録と同じに戻れば外れる
    #[test]
    fn 食い違いの印はRが元に戻れば外れる() {
        let test = SyncTest::new();
        let repo = test.post_synced();
        // 手元を変えて、リモートも変えると食い違いになる
        fs::write(test.md_path("post"), "edited by user\n").unwrap();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD2),
                ("src/assets/post/01.png", PNG),
            ],
        );
        test.sync().unwrap();
        assert!(test.state()["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "post"));
        // リモートが記録と同じに戻ると、印が外れる
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        test.sync().unwrap();
        let conflicts = test.state()["conflicts"]
            .clone()
            .as_array()
            .unwrap()
            .clone();
        assert!(conflicts.is_empty(), "{conflicts:?}");
        // 手元はまだ編集のまま
        assert_eq!(
            fs::read_to_string(test.md_path("post")).unwrap(),
            "edited by user\n"
        );
    }

    #[test]
    fn 記事を消しても下のディレクトリのファイルは残す() {
        let test = SyncTest::new();
        let repo = test.post_synced();
        test.write_image("post/sub/keep.png", PNG2);
        repo.branch(&test.transport, "main", &[]);
        let result = test.sync().unwrap();
        assert_eq!(result.removed, vec!["post".to_string()]);
        assert!(!test.md_path("post").exists());
        assert_eq!(
            fs::read(test.image_path("post/sub/keep.png")).unwrap(),
            PNG2
        );
    }

    // 読めない記事は手元も記録もそのまま
    #[test]
    fn 読めない記事は手元も記録も前回のまま() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        test.sync().unwrap();
        let state_before = test.state();
        // ベースのmdがblobでなくなる(サブモジュール)
        let base_commit = repo.branch_with_entries(
            &test.transport,
            "main",
            &[("src/assets/post/01.png", PNG)],
            &[("src/content/blog/post.md", "160000", "commit")],
        );
        // 手元とstate.jsonの両方が替わらない
        let state_md_before = fs::read(test.md_path("post")).unwrap();
        let result = test.sync().unwrap();
        assert_eq!(fs::read(test.md_path("post")).unwrap(), state_md_before);
        let state = test.state();
        // 記録もベースのコミットも前回のまま
        assert_eq!(
            state["articles"]["post"]["md"],
            state_before["articles"]["post"]["md"]
        );
        assert_eq!(state["remote"]["base"]["commit"], base_commit);
        // skippedにslugとbranch
        let skipped: Vec<(String, String)> = result
            .skipped
            .iter()
            .map(|s| (s.slug.clone(), s.branch.clone()))
            .collect();
        assert_eq!(skipped, vec![("post".to_string(), "main".to_string())]);
    }

    #[test]
    fn 前回の様子に無い記事が読めなくても手元を消さない() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", &[("src/content/blog/post.md", MD)]);
        test.sync().unwrap();
        // 前回の様子が無い(basisを替えた後など)ところで、ベースのmdがサブモジュールになる
        test.edit_state(|state| state["remote"]["base"] = Value::Null);
        repo.branch_with_entries(
            &test.transport,
            "main",
            &[],
            &[("src/content/blog/post.md", "160000", "commit")],
        );
        let result = test.sync().unwrap();
        assert!(result.removed.is_empty());
        assert_eq!(fs::read(test.md_path("post")).unwrap(), MD);
        assert!(!test.state()["articles"]["post"].is_null());
    }

    #[test]
    fn 開いている記事は置き換えも削除もせずdeferredに入れる() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", &[("src/content/blog/post.md", MD)]);
        test.sync().unwrap();
        let state_before = test.state();
        let protected = || {
            sync(&SyncInput {
                protect: Some("post"),
                ..test.input()
            })
            .unwrap()
        };
        // 置き換えになるとき
        repo.branch(
            &test.transport,
            "main",
            &[("src/content/blog/post.md", MD2)],
        );
        let result = protected();
        assert_eq!(result.deferred, vec!["post".to_string()]);
        assert!(result.replaced.is_empty());
        assert_eq!(fs::read(test.md_path("post")).unwrap(), MD);
        assert_eq!(
            test.state()["articles"]["post"],
            state_before["articles"]["post"]
        );
        // 削除になるとき
        repo.branch(&test.transport, "main", &[]);
        let result = protected();
        assert_eq!(result.deferred, vec!["post".to_string()]);
        assert!(result.removed.is_empty());
        assert!(test.md_path("post").exists());
        // 守らなければ消す
        let result = test.sync().unwrap();
        assert!(result.deferred.is_empty());
        assert_eq!(result.removed, vec!["post".to_string()]);
    }

    // 記事用のブランチの別の記事には触れない
    #[test]
    fn 記事用のブランチの別の記事には触れない() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", &[("src/content/blog/post.md", MD)]);
        repo.branch(
            &test.transport,
            "blog/post",
            &[
                ("src/content/blog/post.md", BRANCH_MD),
                ("src/assets/post/01.png", PNG),
                // 別の記事の様子
                ("src/content/blog/other.md", OTHER_MD),
                ("src/assets/other/02.png", PNG2),
            ],
        );

        test.sync().unwrap();
        let state = test.state();
        let articles = state["remote"]["base"]["articles"].as_object().unwrap();
        let branches = state["remote"]["branches"].as_object().unwrap();
        // baseの様子はmainだけ。branchはblog/post
        assert_eq!(articles.len(), 1);
        assert_eq!(branches.len(), 1);
        // blog/postの中の別の記事には触れない
        assert!(!test.md_path("other").exists());
        assert!(!test.assets_dir("other").exists());
    }

    // 手元だけの記事(記録無し・R無し)は残る
    #[test]
    fn 手元だけの記事は残る() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        test.sync().unwrap();
        // 手元だけでcommitしていない記事を作る
        fs::write(test.md_path("draft"), "draft\n").unwrap();

        let result = test.sync().unwrap();
        assert!(result.replaced.is_empty());
        assert!(result.removed.is_empty());
        assert_eq!(
            fs::read_to_string(test.md_path("draft")).unwrap(),
            "draft\n"
        );
        // 記録は作らないし食い違いにもしない
        assert!(test.state()["articles"]["draft"].is_null());
        assert!(!test.state()["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "draft"));
        assert_eq!(fs::read_to_string(test.md_path("post")).unwrap(), "post\n");
    }

    // 置き換えと削除の結果は、replacedとremovedになる
    #[test]
    fn 置き換えた記事と消した記事が結果に入る() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/content/blog/gone.md", OTHER_MD),
            ],
        );
        test.sync().unwrap();
        // postのmdが替わり、goneのmdが消える
        repo.branch(
            &test.transport,
            "main",
            &[("src/content/blog/post.md", MD2)],
        );
        let result = test.sync().unwrap();
        assert_eq!(result.replaced, vec!["post".to_string()]);
        assert_eq!(result.removed, vec!["gone".to_string()]);
        assert!(result.skipped.is_empty());
        assert_eq!(fs::read_to_string(test.md_path("post")).unwrap(), "post2\n");
        assert!(!test.md_path("gone").exists());
    }

    // 画像の取得: 取ってきていない画像を取る
    #[test]
    fn 取ってきていない画像を取ってfetchedにする() {
        let test = SyncTest::new();
        let _repo = test.image_set_synced();
        let count = test.fetch_images("post").unwrap();
        assert_eq!(count, 0);
        // 手元に画像が置かれ、fetchedになる
        assert_eq!(fs::read(test.image_path("post/01.png")).unwrap(), PNG);
        assert_eq!(fs::read(test.image_path("post/02.png")).unwrap(), PNG2);
        let record = &test.state()["articles"]["post"];
        assert_eq!(record["assets"]["01.png"]["fetched"], true);
        assert_eq!(record["assets"]["02.png"]["fetched"], true);
        // 下のディレクトリにある画像は、取ってこない
        assert!(
            !test.image_path("post/sub/03.png").exists(),
            "下のディレクトリの画像は取らないはず"
        );
        assert_eq!(record["assets"]["sub/03.png"]["fetched"], false);
    }

    // 画像の取得: 取ってきた画像と手元にある画像は取らない
    #[test]
    fn 取ってきた画像と手元にある画像は取らない() {
        // 全部取った後は、ふたたび呼んでもblobを取らない
        let test = SyncTest::new();
        let _repo = test.image_set_synced();
        assert_eq!(test.fetch_images("post").unwrap(), 0);
        let before = test.transport.requests().len();
        assert_eq!(test.fetch_images("post").unwrap(), 0);
        let urls: Vec<String> = test.transport.requests()[before..]
            .iter()
            .map(|r| r.url.clone())
            .collect();
        assert!(
            !urls.iter().any(|url| url.contains("/git/blobs/")),
            "blobを取らないはず: {urls:?}"
        );

        // 手元に同じSHAのファイルが既にあれば取らない
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        test.sync().unwrap();
        fs::create_dir_all(test.assets_dir("post")).unwrap();
        fs::write(test.image_path("post/01.png"), PNG).unwrap();
        let before = test.transport.requests().len();
        assert_eq!(test.fetch_images("post").unwrap(), 0);
        let urls: Vec<String> = test.transport.requests()[before..]
            .iter()
            .map(|r| r.url.clone())
            .collect();
        assert!(!urls.iter().any(|url| url.contains("/git/blobs/")));
        // fileは書き換わらず、fetchedも替わらない
        assert_eq!(fs::read(test.image_path("post/01.png")).unwrap(), PNG);
        assert_eq!(
            test.state()["articles"]["post"]["assets"]["01.png"]["fetched"],
            false
        );
    }

    // 画像の取得: 下のディレクトリの画像は取らない
    #[test]
    fn 下のディレクトリの画像は取らない() {
        let test = SyncTest::new();
        let _repo = test.image_set_synced();
        let count = test.fetch_images("post").unwrap();
        assert_eq!(count, 0);
        assert!(!test.assets_dir("post").join("sub").exists());
    }

    // 画像の取得: 1つ取れなくても残りは取り、取れなかった件数を返す
    #[test]
    fn assetsの外を指す名前の画像は取らない() {
        let test = SyncTest::new();
        let _repo = test.post_synced();
        test.edit_state(|state| {
            state["articles"]["post"]["assets"]["../evil.png"] =
                serde_json::json!({ "sha": blob_sha(PNG), "fetched": false });
        });
        test.fetch_images("post").unwrap();
        assert!(!test.assets_base.join("evil.png").exists());
        assert_eq!(fs::read(test.image_path("post/01.png")).unwrap(), PNG);
    }

    #[test]
    fn 画像を1つ取れなくても残りは取り取れなかった件数を返す() {
        let test = SyncTest::new();
        let _repo = test.image_set_synced();
        // 2つ目のblobを404にする
        let sha = blob_sha(PNG2);
        test.transport.on(
            Method::Get,
            &url_of(
                &["repos", "owner", "blog", "git", "blobs", sha.as_str()],
                &[],
            ),
            Box::new(|_| {
                Ok(json_response(
                    404,
                    serde_json::json!({"message": "not found"}),
                ))
            }),
        );
        let count = test.fetch_images("post").unwrap();
        assert_eq!(count, 1);
        // 取れたものは手元とstate.jsonに、取れなかったものは取ってきていないまま
        assert_eq!(fs::read(test.image_path("post/01.png")).unwrap(), PNG);
        assert!(!test.image_path("post/02.png").exists());
        let record = &test.state()["articles"]["post"];
        assert_eq!(record["assets"]["01.png"]["fetched"], true);
        assert_eq!(record["assets"]["02.png"]["fetched"], false);
    }

    // 画像の取得: SHAが違う応答は、書かずに取れなかった件数に入れる
    #[test]
    fn 画像のSHAが違う応答は書かずに件数に入れる() {
        let test = SyncTest::new();
        let _repo = test.image_set_synced();
        // 2つ目のblobに別の内容を返す
        let sha = blob_sha(PNG2);
        test.transport.raw(
            Method::Get,
            &url_of(
                &["repos", "owner", "blog", "git", "blobs", sha.as_str()],
                &[],
            ),
            200,
            b"mismatch\n".to_vec(),
        );
        let count = test.fetch_images("post").unwrap();
        assert_eq!(count, 1);
        // 書かれていないので、fetchedも替わらない
        assert!(!test.image_path("post/02.png").exists());
        let record = &test.state()["articles"]["post"];
        assert_eq!(record["assets"]["02.png"]["fetched"], false);
        assert_eq!(record["assets"]["02.png"]["sha"], blob_sha(PNG2));
        // ほかの画像は取れている
        assert_eq!(fs::read(test.image_path("post/01.png")).unwrap(), PNG);
        assert_eq!(record["assets"]["01.png"]["fetched"], true);
    }

    // 画像の取得: 通信の間に記録が替わっていたら書かない
    #[test]
    fn 通信の間に記録が替わっていたら書かない() {
        // 記録が勝手に消える
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        test.sync().unwrap();
        let sha = blob_sha(PNG);
        let state_path = test.state_path.clone();
        test.transport.on(
            Method::Get,
            &url_of(
                &["repos", "owner", "blog", "git", "blobs", sha.as_str()],
                &[],
            ),
            Box::new(move |_| {
                // 通信の間に、state.jsonの記事の記録を消す
                let mut state: Value =
                    serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
                state["articles"].as_object_mut().unwrap().remove("post");
                fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
                let content = PNG.to_vec();
                Ok(raw_response(200, content))
            }),
        );
        test.fetch_images("post").unwrap();
        // 手元に書かれていない
        assert!(!test.image_path("post/01.png").exists());

        // 記録のSHAが替わる
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        test.sync().unwrap();
        let sha = blob_sha(PNG);
        let state_path = test.state_path.clone();
        test.transport.on(
            Method::Get,
            &url_of(
                &["repos", "owner", "blog", "git", "blobs", sha.as_str()],
                &[],
            ),
            Box::new(move |_| {
                let mut state: Value =
                    serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
                state["articles"]["post"]["assets"]["01.png"]["sha"] =
                    Value::String("sha-changed".to_string());
                fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
                let content = PNG.to_vec();
                Ok(raw_response(200, content))
            }),
        );
        test.fetch_images("post").unwrap();
        // 手元に書かれていない
        assert!(!test.image_path("post/01.png").exists());
        let record = &test.state()["articles"]["post"];
        assert_eq!(record["assets"]["01.png"]["sha"], "sha-changed");
        assert_eq!(record["assets"]["01.png"]["fetched"], false);
    }

    // 画像の取得: 通信の間に手元に同じ名前のファイルができたら書かない
    #[test]
    fn 通信の間に手元に同名のファイルができたら書かない() {
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        test.sync().unwrap();
        let sha = blob_sha(PNG);
        let image_path = test.image_path("post/01.png");
        test.transport.on(
            Method::Get,
            &url_of(
                &["repos", "owner", "blog", "git", "blobs", sha.as_str()],
                &[],
            ),
            Box::new(move |_| {
                // 手元に同じ名前のファイルを置く
                fs::create_dir_all(image_path.parent().unwrap()).unwrap();
                fs::write(&image_path, b"handmade\n").unwrap();
                let content = PNG.to_vec();
                Ok(raw_response(200, content))
            }),
        );
        test.fetch_images("post").unwrap();
        // 手元は、置いたファイルのまま。fetchedにはならない
        assert_eq!(
            fs::read(test.image_path("post/01.png")).unwrap(),
            b"handmade\n"
        );
        assert_eq!(
            test.state()["articles"]["post"]["assets"]["01.png"]["fetched"],
            false
        );
    }

    // 画像の取得: 記録が無い記事と、state.jsonが無いときは、何もしない
    #[test]
    fn 記録の無い記事とstate_jsonが無いときは何もしない() {
        // 記録が無い記事
        let test = SyncTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", &[("src/content/blog/post.md", MD)]);
        test.sync().unwrap();
        let before = test.transport.requests().len();
        assert_eq!(test.fetch_images("draft").unwrap(), 0);
        assert_eq!(test.transport.requests().len(), before);

        // state.jsonが無い
        let test = SyncTest::new();
        assert_eq!(test.fetch_images("post").unwrap(), 0);
        assert!(test.transport.requests().is_empty());
    }

    // 記事ごとの状態: 分類と印
    #[test]
    fn 記事ごとの状態は手元の記事ごとに分類と印を返す() {
        let test = SyncTest::new();
        // 手元の記事: post(公開済み)・edit(編集中)・draft(執筆中)
        fs::create_dir_all(&test.posts_dir).unwrap();
        fs::write(test.md_path("post"), "post\n").unwrap();
        fs::write(test.md_path("edit"), "edited\n").unwrap();
        fs::write(test.md_path("draft"), "draft\n").unwrap();
        // state.jsonに、それぞれの分類になる様子を置く
        let mut state = empty_state();
        state.fetched_at = Some(NOW);
        state.remote.base = Some(BaseSnapshot {
            commit: "commit-base".to_string(),
            articles: BTreeMap::from([
                (
                    "post".to_string(),
                    files_of(blob_sha(b"post\n").as_str(), &[]),
                ),
                (
                    "edit".to_string(),
                    files_of(blob_sha(b"base\n").as_str(), &[]),
                ),
            ]),
        });
        state.remote.branches = BTreeMap::from([(
            "blog/post".to_string(),
            BranchSnapshot {
                commit: "commit-post".to_string(),
                slug: "post".to_string(),
                article: files_of(blob_sha(b"post\n").as_str(), &[]),
            },
        )]);
        state.articles.insert(
            "post".to_string(),
            Record {
                branch: Some("blog/post".to_string()),
                commit: "commit-post".to_string(),
                md: blob_sha(b"post\n"),
                assets: BTreeMap::new(),
                applying: None,
            },
        );
        state.articles.insert(
            "edit".to_string(),
            Record {
                branch: None,
                commit: "commit-base".to_string(),
                md: blob_sha(b"base\n"),
                assets: BTreeMap::new(),
                applying: None,
            },
        );
        state.conflicts.insert("edit".to_string());
        state.skipped = vec![Skipped {
            slug: "skip".to_string(),
            branch: "blog/skip".to_string(),
            reason: "unreadable".to_string(),
        }];
        crate::github::state::write_state(&test.state_path, &state).unwrap();

        let states = test.article_states();
        assert_eq!(states.repo, "owner/blog");
        assert_eq!(states.fetched_at, Some(NOW));
        assert!(!states.state_unreadable);
        let value = &states.articles["post"];
        assert_eq!(value.category, Category::Published);
        assert!(!value.dirty);
        assert!(!value.conflict);
        let value = &states.articles["edit"];
        assert_eq!(value.category, Category::Editing);
        assert!(value.dirty);
        assert!(value.conflict);
        let value = &states.articles["draft"];
        assert_eq!(value.category, Category::Drafting);
        assert!(value.dirty);
        assert!(!value.conflict);
        assert_eq!(states.skipped.len(), 1);
        assert_eq!(states.skipped[0].slug, "skip");
        // 個別の記事の状態も、同じになる
        assert_eq!(test.article_state("edit").unwrap(), states.articles["edit"]);
        // 手元に無い記事はErrになる
        assert!(test.article_state("absent").is_err());
    }

    // state.jsonが読めないときは分類しない
    #[test]
    fn state_jsonが読めなければarticle_statesは分類せずstateUnreadableにする() {
        let test = SyncTest::new();
        fs::create_dir_all(test.state_path.parent().unwrap()).unwrap();
        fs::write(&test.state_path, "{").unwrap();
        let states = test.article_states();
        assert!(states.state_unreadable);
        assert!(states.articles.is_empty());
        // 個別にもErrになる
        assert!(matches!(
            test.article_state("post"),
            Err(GithubError::StateUnreadable)
        ));
    }

    #[test]
    fn state_jsonが無ければ手元の記事はまだcommitしていない記事になる() {
        let test = SyncTest::new();
        fs::create_dir_all(&test.posts_dir).unwrap();
        fs::write(test.md_path("post"), "post\n").unwrap();
        let states = test.article_states();
        assert_eq!(states.repo, "owner/blog");
        assert_eq!(states.fetched_at, None);
        assert!(!states.state_unreadable);
        assert_eq!(
            states.articles["post"],
            ArticleState {
                category: Category::Drafting,
                dirty: true,
                conflict: false,
            }
        );
    }
}
