// 連携しているときの記事の作成と、slugの変更・削除・非公開・編集の取りやめ(docs/spec.mdの「slugの変更・削除」)

use std::fs;
use std::path::Path;

use super::commands::CommandError;
use super::config::RepoRef;
use super::dates;
use super::push::{self, PushProgress};
use super::state::{self, ArticleFiles, AssetRecord, Basis, Deleting, LocalFiles, Record, State};
use super::sync::{self, SyncInput, SyncLock};
use super::{git, remote, GithubError};
use crate::store::{Article, Store};

// ownはslugを変える記事の今のslug
fn check_destination(state: &State, slug: &str, own: Option<&str>) -> Result<(), GithubError> {
    let renamed_from = own
        .and_then(|old| state.articles.get(old))
        .and_then(|record| record.renamed_from.clone());
    let original = renamed_from.or_else(|| own.and_then(|old| state::original_slug(state, old)));
    // 元のslugに戻すときは、そのslugのブランチもこの記事のものなので許す
    if original.as_deref() == Some(slug) {
        return Ok(());
    }
    if state
        .articles
        .values()
        .any(|record| record.renamed_from.as_deref() == Some(slug))
    {
        return Err(GithubError::SlugReserved {
            slug: slug.to_string(),
        });
    }
    // 空のブランチは、ベースに記事が無ければ行き先にできる。ただし公開済みの記事を移すときは、
    // 空のブランチの親に元の記事が無いことがあり進められないので断る
    let moves_published = own.is_some_and(|old| {
        in_base_branch(state, old) || state::original_slug(state, old).is_some()
    });
    let used = in_base_branch(state, slug)
        || state
            .remote
            .branches
            .values()
            .any(|branch| branch.slug == slug && (branch.article.is_some() || moves_published))
        || state.skipped.iter().any(|skipped| skipped.slug == slug);
    if used {
        return Err(GithubError::SlugInUse {
            slug: slug.to_string(),
        });
    }
    Ok(())
}

fn in_base_branch(state: &State, slug: &str) -> bool {
    state
        .remote
        .base
        .as_ref()
        .is_some_and(|base| base.articles.contains_key(slug))
}

// 記事用のブランチを、先頭が記録したコミットのときだけ消す。既に無ければ何もしない
pub(crate) fn remove_branch(
    input: &SyncInput<'_>,
    branch: &str,
    commit: &str,
) -> Result<(), GithubError> {
    let head = || git::branch_head(input.transport, input.token, input.now, input.repo, branch);
    match head()? {
        None => Ok(()),
        Some(head) if head != commit => Err(GithubError::BranchMoved),
        Some(_) => {
            match git::delete_ref(input.transport, input.token, input.now, input.repo, branch) {
                Ok(()) => Ok(()),
                // GitHubは無いrefの削除にも422を返すので、読み直して消えていれば成功にする。
                // 読み直せなければ、元の失敗を返す
                Err(error) => match head() {
                    Ok(None) => Ok(()),
                    Ok(Some(_)) => Err(error),
                    Err(reread) => {
                        eprintln!("Failed to read the branch {branch} after deleting it: {reread}");
                        Err(error)
                    }
                },
            }
        }
    }
}

pub fn create(
    data_dir: &Path,
    repo: &RepoRef,
    basis: &Basis,
    store: &Store,
    lock: &SyncLock,
    slug: &str,
) -> Result<Article, CommandError> {
    lock.run(|| {
        let state = state::read_state(&state::state_path(data_dir, repo), basis)?;
        check_destination(&state, slug, None)?;
        let article = store.create_article(slug)?;
        dates::clear_marks_logged(data_dir, repo, slug);
        Ok(article)
    })
}

pub fn rename(
    data_dir: &Path,
    repo: &RepoRef,
    basis: &Basis,
    store: &Store,
    lock: &SyncLock,
    old: &str,
    new: &str,
) -> Result<Article, CommandError> {
    lock.run(|| {
        let state_path = state::state_path(data_dir, repo);
        let state = state::read_state(&state_path, basis)?;
        let record = state.articles.get(old).cloned();
        // 削除予定の記事はslugを変えられない
        if record
            .as_ref()
            .is_some_and(|record| record.deleting.is_some())
        {
            return Err(GithubError::NotPublished.into());
        }
        check_destination(&state, new, Some(old))?;
        // 印はmdより先に移す。移せないまま変えると、次の保存で手で直した日付が上書きされる
        let marks_path = dates::marks_path(data_dir, repo);
        dates::rename_marks(&marks_path, old, new)?;
        let renamed = rename_files(data_dir, repo, store, &state, record, old, new);
        if renamed.is_err() {
            if let Err(error) = dates::rename_marks(&marks_path, new, old) {
                eprintln!("Failed to restore the date marks of {old}: {error}");
            }
        }
        renamed
    })
}

// 手元のファイル・synced・記録を新しいslugに移す
fn rename_files(
    data_dir: &Path,
    repo: &RepoRef,
    store: &Store,
    state: &State,
    record: Option<state::Record>,
    old: &str,
    new: &str,
) -> Result<Article, CommandError> {
    let state_path = state::state_path(data_dir, repo);
    let Some(record) = record else {
        // 手元だけ変えると、次の読み込みで元のslugの記事がブランチから作り直される
        if state::article_branch(state, old).is_some() {
            return Err(GithubError::Conflicted.into());
        }
        return Ok(store.rename_article(old, new)?);
    };
    store.rename_article_then(old, new, |article, _, _| -> Result<Article, CommandError> {
        let old_synced = state::synced_path(data_dir, repo, old);
        let new_synced = state::synced_path(data_dir, repo, new);
        let moved_synced = old_synced.exists();
        if moved_synced {
            fs::rename(&old_synced, &new_synced)
                .map_err(|error| GithubError::io(&old_synced, error))?;
        }

        let mut next = state.clone();
        next.articles.remove(old);
        // ブランチが無い(ベースの)記事も、記録を新しいslugに付け替えて残す
        let mut record = record.clone();
        let original = record
            .renamed_from
            .clone()
            .unwrap_or_else(|| old.to_string());
        record.renamed_from = (new != original).then_some(original);
        next.articles.insert(new.to_string(), record);
        // 食い違いの印も新しいslugに移す
        if next.conflicts.remove(old) {
            next.conflicts.insert(new.to_string());
        }
        if let Err(error) = state::write_state(&state_path, &next) {
            if moved_synced {
                if let Err(undo) = fs::rename(&new_synced, &old_synced) {
                    eprintln!("Failed to restore {}: {undo}", old_synced.display());
                }
            }
            return Err(error.into());
        }
        Ok(article.clone())
    })
}

pub fn delete(input: &SyncInput<'_>, lock: &SyncLock, slug: &str) -> Result<(), CommandError> {
    crate::store::validate_slug(slug)?;
    lock.run(|| {
        let state_path = state::state_path(input.data_dir, input.repo);
        let state = state::read_state(&state_path, input.basis)?;
        // 公開済みの記事と、それを移した記事は消さない(公開済みの記事は削除のcommitを送る)
        if in_base_branch(&state, slug) || state::original_slug(&state, slug).is_some() {
            return Err(GithubError::NotPublished.into());
        }
        let branch = state::article_branch(&state, slug);
        if let Some((branch, commit)) = &branch {
            remove_branch(input, branch, commit)?;
        }
        // 記録を消してから手元を消す。同じロックの中で行い、画像の取得が間に書き戻さないようにする
        input
            .store
            .delete_article_with(slug, |posts_dir| -> Result<(), CommandError> {
                sync::check_unchanged(input, posts_dir)?;
                let mut state = state::read_state(&state_path, input.basis)?;
                state.articles.remove(slug);
                state.conflicts.remove(slug);
                if let Some((branch, _)) = &branch {
                    state.remote.branches.remove(branch);
                }
                state::write_state(&state_path, &state)?;
                sync::remove_synced(input, slug)?;
                Ok(())
            })?;
        dates::clear_marks_logged(input.data_dir, input.repo, slug);
        Ok(())
    })
}

// 公開済みでない記事は、削除・非公開にできない
fn check_published(state: &State, slug: &str) -> Result<(), GithubError> {
    let Some(files) = state
        .remote
        .base
        .as_ref()
        .and_then(|base| base.articles.get(slug))
    else {
        return Err(GithubError::NotPublished);
    };
    if state
        .articles
        .get(slug)
        .is_some_and(|record| record.deleting.is_some())
    {
        return Err(GithubError::NotPublished);
    }
    // 記事用のブランチの内容がベースと違う、または空(削除予定)なら公開済みではない
    if state
        .remote
        .branches
        .values()
        .any(|branch| branch.slug == slug && branch.article.as_ref() != Some(files))
    {
        return Err(GithubError::NotPublished);
    }
    Ok(())
}

fn removal_message(title: &str, kind: Deleting) -> String {
    match kind {
        Deleting::Delete => format!("{title}を削除"),
        Deleting::Unpublish => format!("{title}を非公開に"),
    }
}

// 公開済みの記事を削除・非公開にする(docs/spec.mdの「公開済みの記事の削除・非公開」)
pub fn remove_published(
    input: &SyncInput<'_>,
    lock: &SyncLock,
    slug: &str,
    kind: Deleting,
    progress: &dyn Fn(PushProgress),
) -> Result<(), CommandError> {
    crate::store::validate_slug(slug)?;
    lock.run(|| {
        let state_path = state::state_path(input.data_dir, input.repo);
        let state = state::read_state(&state_path, input.basis)?;
        check_published(&state, slug)?;
        let base_head = git::branch_head(
            input.transport,
            input.token,
            input.now,
            input.repo,
            &input.basis.base_branch,
        )?
        .ok_or(GithubError::BaseBranchNotFound)?;
        // ベースの先頭と、そこでの記事のファイル
        let Some(base_article) = remote::read_article(
            input.transport,
            input.token,
            input.now,
            input.repo,
            input.basis,
            slug,
            &base_head,
        )?
        else {
            return Err(GithubError::ArticleNotFound.into());
        };
        let (local, md) =
            input
                .store
                .with_lock(|posts_dir, assets_base| -> Result<_, GithubError> {
                    let local = state::local_files(posts_dir, assets_base, slug)?;
                    let bodies = state::local_bodies(posts_dir, assets_base, slug)?;
                    let md = bodies.md.map(|(_, content)| content);
                    Ok((local, md))
                })?;
        let Some(md) = md else {
            return Err(GithubError::OutOfSync.into());
        };
        // 手元と記録とベースが食い違っていないか
        let record = state.articles.get(slug);
        if record.is_none_or(|record| !sync::same_content(record, &base_article.files))
            || state::is_dirty(record, &local)
        {
            return Err(GithubError::OutOfSync.into());
        }

        // 残っている記事用のブランチは、先頭が記録したコミットで、記事がベースと同じときだけ進める
        let (branch, branch_head) = match state::article_branch(&state, slug) {
            Some((branch, recorded)) => {
                let head =
                    git::branch_head(input.transport, input.token, input.now, input.repo, &branch)?;
                if let Some(head) = &head {
                    let branch_article = remote::read_article(
                        input.transport,
                        input.token,
                        input.now,
                        input.repo,
                        input.basis,
                        slug,
                        head,
                    )?;
                    if recorded != *head
                        || branch_article.is_none_or(|article| article.files != base_article.files)
                    {
                        return Err(GithubError::BranchMoved.into());
                    }
                }
                (branch, head)
            }
            // 様子に無いブランチは、無いものとして読まない
            None => (push::branch_name(input.basis, slug), None),
        };

        // ブランチを書き換える前に、削除予定の印を書く
        set_deleting(input, &state_path, slug, Some(kind))?;
        // ブランチを書き換える通信が届いたか分からないときは、印を残して次の読み込みに任せる
        let mut ref_unknown = false;
        let pushed = push_removal(
            input,
            &PushedRemoval {
                slug,
                kind,
                md: &md,
                branch: &branch,
                branch_head: branch_head.as_deref(),
                base_head: &base_head,
                modes: &base_article.modes,
            },
            progress,
            &mut ref_unknown,
        );
        let commit = match pushed {
            Ok(commit) => commit,
            Err(error) => {
                if !ref_unknown {
                    if let Err(undo) = set_deleting(input, &state_path, slug, None) {
                        eprintln!("Failed to clear the deleting mark of {slug}: {undo}");
                    }
                }
                return Err(error);
            }
        };

        // 記録と空のブランチの様子を書く。手元のファイルは替えない。画像の取得が書いたfetchedを消さないよう、
        // 読み直した記録を替える。書けなくても印は付けてあるので、次の読み込みで記録が削除予定になる
        let recorded = input
            .store
            .with_lock(|posts_dir, _| -> Result<(), GithubError> {
                sync::check_unchanged(input, posts_dir)?;
                let mut state = state::read_state(&state_path, input.basis)?;
                let record = state
                    .articles
                    .entry(slug.to_string())
                    .or_insert_with(|| base_record(&base_article.files, &base_head, &local));
                record.branch = Some(branch.clone());
                record.commit = commit.clone();
                record.deleting = Some(kind);
                state.conflicts.remove(slug);
                state.remote.branches.insert(
                    branch.clone(),
                    state::BranchSnapshot {
                        commit,
                        slug: slug.to_string(),
                        article: None,
                        moved_from: None,
                    },
                );
                state::write_state(&state_path, &state)
            });
        if let Err(error) = recorded {
            eprintln!("Failed to record the removal of {slug}: {error}");
        }
        Ok(())
    })
}

fn set_deleting(
    input: &SyncInput<'_>,
    state_path: &Path,
    slug: &str,
    kind: Option<Deleting>,
) -> Result<(), GithubError> {
    input.store.with_lock(|posts_dir, _| {
        sync::check_unchanged(input, posts_dir)?;
        let mut state = state::read_state(state_path, input.basis)?;
        let Some(record) = state.articles.get_mut(slug) else {
            return Err(GithubError::OutOfSync);
        };
        record.deleting = kind;
        state::write_state(state_path, &state)
    })
}

struct PushedRemoval<'a> {
    slug: &'a str,
    kind: Deleting,
    md: &'a [u8],
    branch: &'a str,
    // 進める記事用のブランチの先頭。Noneならブランチを作る
    branch_head: Option<&'a str>,
    base_head: &'a str,
    // 消すファイル(リポジトリのルートからのパス -> mode)
    modes: &'a std::collections::BTreeMap<String, String>,
}

// 記事のファイルを外したコミットを作り、ブランチを進めるか作る。作ったコミットを返す
fn push_removal(
    input: &SyncInput<'_>,
    removal: &PushedRemoval<'_>,
    progress: &dyn Fn(PushProgress),
    ref_unknown: &mut bool,
) -> Result<String, CommandError> {
    let title = crate::store::parse_md("", &String::from_utf8_lossy(removal.md))
        .fields
        .title
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| removal.slug.to_string());
    let parent = removal.branch_head.unwrap_or(removal.base_head);
    let root = git::commit_tree(input.transport, input.token, input.now, input.repo, parent)?;
    let entries: Vec<git::NewTreeEntry> = removal
        .modes
        .iter()
        .map(|(path, mode)| git::NewTreeEntry {
            path: path.clone(),
            mode: mode.clone(),
            sha: None,
        })
        .collect();
    progress(PushProgress::Committing);
    let tree = git::create_tree(
        input.transport,
        input.token,
        input.now,
        input.repo,
        &root,
        &entries,
    )?;
    let commit = git::create_commit(
        input.transport,
        input.token,
        input.now,
        input.repo,
        &removal_message(&title, removal.kind),
        &tree,
        &[parent],
    )?;
    progress(PushProgress::Pushing);
    let branch = removal.branch;
    let written = match removal.branch_head {
        Some(_) => git::update_ref(
            input.transport,
            input.token,
            input.now,
            input.repo,
            branch,
            &commit,
        ),
        None => git::create_ref(
            input.transport,
            input.token,
            input.now,
            input.repo,
            branch,
            &commit,
        ),
    };
    let Err(error) = written else {
        return Ok(commit);
    };
    if matches!(error, GithubError::Network { .. }) {
        *ref_unknown = true;
        return Err(error.into());
    }
    // 失敗の応答でも書き換えが届いていることがあるので、先頭を読み直して確かめる
    let head = match git::branch_head(input.transport, input.token, input.now, input.repo, branch) {
        Ok(head) => head,
        Err(reread) => {
            eprintln!("Failed to read the branch {branch} after pushing: {reread}");
            *ref_unknown = true;
            return Err(error.into());
        }
    };
    match (removal.branch_head, head) {
        (_, Some(head)) if head == commit => Ok(commit),
        (Some(_), head) if head.as_deref() != Some(parent) => Err(GithubError::BranchMoved.into()),
        (None, Some(_)) if matches!(error, GithubError::Status { status: 422 }) => {
            Err(GithubError::BranchExists {
                branch: branch.to_string(),
            }
            .into())
        }
        _ => Err(error.into()),
    }
}

// ベースの内容を元にした記録。fetchedは、手元にある画像のうちSHAが同じものだけにする
fn base_record(files: &ArticleFiles, commit: &str, local: &LocalFiles) -> Record {
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
                        fetched: local.assets.get(name) == Some(sha),
                    },
                )
            })
            .collect(),
        applying: None,
        renamed_from: None,
        deleting: None,
    }
}

// 移した記事の取りやめ。元のslugの記事をベースの内容で作り直す
fn discard_moved(
    input: &SyncInput<'_>,
    state_path: &Path,
    state: &State,
    slug: &str,
) -> Result<(), CommandError> {
    let old = state
        .articles
        .get(slug)
        .and_then(|record| record.renamed_from.clone())
        .or_else(|| {
            state
                .remote
                .branches
                .values()
                .find(|branch| branch.slug == slug)
                .and_then(|branch| branch.moved_from.clone())
        });
    let Some(old) = old else {
        return Err(GithubError::ArticleNotFound.into());
    };
    let base_head = git::branch_head(
        input.transport,
        input.token,
        input.now,
        input.repo,
        &input.basis.base_branch,
    )?
    .ok_or(GithubError::BaseBranchNotFound)?;
    let Some(article) = remote::read_article(
        input.transport,
        input.token,
        input.now,
        input.repo,
        input.basis,
        &old,
        &base_head,
    )?
    else {
        return Err(GithubError::ArticleNotFound.into());
    };
    let md = git::blob(
        input.transport,
        input.token,
        input.now,
        input.repo,
        &article.files.md,
    )?;
    // 元のslugの記事が手元に変更つきで残っていれば(移された元の記事の食い違い)、上書きしない
    let old_changed = input.store.with_lock(|posts_dir, assets_base| {
        let local_old = state::local_files(posts_dir, assets_base, &old)?;
        Ok::<_, GithubError>(
            !local_old.is_empty()
                && !state::same_as_remote(state.articles.get(&old), &local_old, &article.files),
        )
    })?;
    if old_changed {
        return Err(GithubError::OutOfSync.into());
    }
    let branch = state::article_branch(state, slug);
    if let Some((branch, commit)) = &branch {
        remove_branch(input, branch, commit)?;
    }
    // 元のslugの記事を書けてから新しいslugを消す。途中で失敗しても、次の読み込みで新しいslugは
    // まだcommitしていない記事として残り、元のslugはベースから作り直される
    input
        .store
        .with_lock(|posts_dir, assets_base| -> Result<(), GithubError> {
            sync::check_unchanged(input, posts_dir)?;
            let local_new = state::local_files(posts_dir, assets_base, slug)?;
            let local_old = state::local_files(posts_dir, assets_base, &old)?;
            let new_record = base_record(&article.files, &base_head, &local_old);
            sync::write_file(&posts_dir.join(format!("{old}.md")), &md)?;
            sync::rewrite_assets(assets_base, &old, &local_old, &new_record)?;
            sync::write_synced(input, &old, &md)?;
            let mut state = state::read_state(state_path, input.basis)?;
            state.articles.remove(slug);
            state.articles.insert(old.clone(), new_record);
            state.conflicts.remove(slug);
            state.conflicts.remove(&old);
            if let Some((branch, _)) = &branch {
                state.remote.branches.remove(branch);
            }
            state::write_state(state_path, &state)?;
            sync::remove_article(posts_dir, assets_base, slug, &local_new)?;
            sync::remove_synced(input, slug)
        })?;
    Ok(())
}

pub fn discard(input: &SyncInput<'_>, lock: &SyncLock, slug: &str) -> Result<(), CommandError> {
    crate::store::validate_slug(slug)?;
    let result = lock.run(|| {
        let state_path = state::state_path(input.data_dir, input.repo);
        let state = state::read_state(&state_path, input.basis)?;
        if !in_base_branch(&state, slug) {
            return discard_moved(input, &state_path, &state, slug);
        }
        let base_head = git::branch_head(
            input.transport,
            input.token,
            input.now,
            input.repo,
            &input.basis.base_branch,
        )?
        .ok_or(GithubError::BaseBranchNotFound)?;
        let Some(article) = remote::read_article(
            input.transport,
            input.token,
            input.now,
            input.repo,
            input.basis,
            slug,
            &base_head,
        )?
        else {
            return Err(GithubError::ArticleNotFound.into());
        };
        // ブランチを消した後に通信で失敗しないよう、mdは先に取る
        let md = git::blob(
            input.transport,
            input.token,
            input.now,
            input.repo,
            &article.files.md,
        )?;
        let branch = state::article_branch(&state, slug);
        if let Some((branch, commit)) = &branch {
            remove_branch(input, branch, commit)?;
        }
        input
            .store
            .with_lock(|posts_dir, assets_base| -> Result<(), GithubError> {
                sync::check_unchanged(input, posts_dir)?;
                let local = state::local_files(posts_dir, assets_base, slug)?;
                let new_record = base_record(&article.files, &base_head, &local);
                // mdを書けなくても、記録はベースの内容にする(次の読み込みで編集中として残す)
                let md_result = sync::write_file(&posts_dir.join(format!("{slug}.md")), &md);
                if md_result.is_ok() {
                    sync::rewrite_assets(assets_base, slug, &local, &new_record)?;
                }
                // syncedを先に書く。記録を書いた後で失敗すると、読み込みはmdが同じなので書き直さない
                sync::write_synced(input, slug, &md)?;
                let mut state = state::read_state(&state_path, input.basis)?;
                state.articles.insert(slug.to_string(), new_record);
                state.conflicts.remove(slug);
                if let Some((branch, _)) = &branch {
                    state.remote.branches.remove(branch);
                }
                state::write_state(&state_path, &state)?;
                md_result?;
                Ok(())
            })?;
        Ok(())
    });
    if result.is_ok() {
        dates::clear_updated_mark_logged(input.data_dir, input.repo, slug);
    }
    result
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::github::config::github_posts_dir;
    use crate::github::dates::{self, marks_of, updated_mark};
    use crate::github::fake::{
        blob_sha, json_response, read_only_dir, repo_ref, url_of, FakeRepo, RouteTransport,
    };
    use crate::github::http::{Method, Response};
    use crate::github::push::PushProgress;
    use crate::github::sync::{self, ArticleState};
    use crate::github::GithubError;

    const TOKEN: &str = "TOKEN";
    const NOW: i64 = 1_000_000;
    const POSTS_PATH: &str = "src/content/blog";

    const README: &[u8] = b"readme\n";
    const BASE_MD: &[u8] = b"---\ntitle: \"post\"\n---\nbase\n";
    const BRANCH_MD: &[u8] = b"---\ntitle: \"post\"\n---\nbranch\n";
    const LOCAL_MD: &[u8] = b"---\ntitle: \"post\"\n---\nlocal\n";
    const OTHER_MD: &[u8] = b"---\ntitle: \"other\"\n---\nother\n";
    const PNG: &[u8] = b"png-1\n";
    const PNG2: &[u8] = b"png-2\n";
    const PNG3: &[u8] = b"png-3\n";

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

    struct ArticleTest {
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
        lock: Arc<SyncLock>,
    }

    impl ArticleTest {
        fn new() -> Self {
            let tmp = TempDir::new().unwrap();
            let data_dir = tmp.path().join("data");
            let repo_of = repo_ref();
            let basis_of = basis();
            let posts_dir = github_posts_dir(&data_dir, &repo_of, POSTS_PATH);
            let store = Arc::new(Store::new(posts_dir.clone(), tmp.path().join("backup")).unwrap());
            let repo_part = "github/owner/blog";
            Self {
                posts_dir: posts_dir.clone(),
                assets_base: posts_dir.parent().unwrap().parent().unwrap().join("assets"),
                state_path: data_dir.join(repo_part).join("state.json"),
                synced_dir: data_dir.join(repo_part).join("synced"),
                data_dir,
                store,
                transport: Arc::new(RouteTransport::new()),
                repo_of,
                basis_of,
                lock: Arc::new(SyncLock::default()),
                _tmp: tmp,
            }
        }

        fn new_repo(&self) -> FakeRepo {
            FakeRepo::new(&self.transport, "owner", "blog")
        }

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

        fn create(&self, slug: &str) -> Result<Article, CommandError> {
            super::create(
                &self.data_dir,
                &self.repo_of,
                &self.basis_of,
                self.store.as_ref(),
                self.lock.as_ref(),
                slug,
            )
        }

        fn rename(&self, old: &str, new: &str) -> Result<Article, CommandError> {
            super::rename(
                &self.data_dir,
                &self.repo_of,
                &self.basis_of,
                self.store.as_ref(),
                self.lock.as_ref(),
                old,
                new,
            )
        }

        fn delete(&self, slug: &str) -> Result<(), CommandError> {
            super::delete(&self.input(), self.lock.as_ref(), slug)
        }

        fn discard(&self, slug: &str) -> Result<(), CommandError> {
            super::discard(&self.input(), self.lock.as_ref(), slug)
        }

        fn fetch_images(&self, slug: &str) -> Result<usize, GithubError> {
            sync::fetch_images(&self.input(), slug)
        }

        fn article_state(&self, slug: &str) -> ArticleState {
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

        fn assets_dir(&self, slug: &str) -> PathBuf {
            self.assets_base.join(slug)
        }

        fn image_path(&self, rel: &str) -> PathBuf {
            self.assets_base.join(rel)
        }

        fn write_md(&self, slug: &str, content: &[u8]) {
            let path = self.md_path(slug);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

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

        fn mark_fetched(&self, slug: &str, names: &[(&str, bool)]) {
            self.edit_state(|state| {
                for (name, fetched) in names {
                    state["articles"][slug]["assets"][name]["fetched"] = Value::Bool(*fetched);
                }
            });
        }

        // データ領域(手元の記事・synced・state.json)の全ファイル。「何も替わらない」ことを比べる
        fn files_snapshot(&self) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
            fn walk(dir: &Path, files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
                let Ok(entries) = fs::read_dir(dir) else {
                    return;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        walk(&path, files);
                    } else {
                        files.insert(path.clone(), fs::read(&path).unwrap());
                    }
                }
            }
            let mut files = std::collections::BTreeMap::new();
            walk(&self.data_dir, &mut files);
            files
        }

        // 通信の記録。methodとURLの一部で絞る
        // GET以外の通信の数
        fn write_count(&self) -> usize {
            self.transport
                .requests()
                .iter()
                .filter(|request| request.method != Method::Get)
                .count()
        }

        fn request_count(&self, method: Method, url_part: &str) -> usize {
            self.transport
                .requests()
                .iter()
                .map(|request| {
                    (request.method == method && request.url.contains(url_part)) as usize
                })
                .sum()
        }

        fn ref_url(&self, branch: &str) -> String {
            let mut segments = vec!["repos", "owner", "blog", "git", "refs", "heads"];
            segments.extend(branch.split('/'));
            url_of(&segments, &[])
        }

        // slugを変えた後の記録と手元を作る(renameの実装に頼らず、記録と手元を直接動かす)
        fn mark_renamed(&self, old: &str, new: &str) {
            fs::rename(self.md_path(old), self.md_path(new)).unwrap();
            let old_assets = self.assets_dir(old);
            if old_assets.exists() {
                fs::rename(old_assets, self.assets_dir(new)).unwrap();
            }
            if self.synced_path(old).exists() {
                fs::rename(self.synced_path(old), self.synced_path(new)).unwrap();
            }
            self.edit_state(|state| {
                let mut record = state["articles"][old].clone();
                record["renamedFrom"] = Value::String(old.to_string());
                state["articles"][new] = record;
                state["articles"].as_object_mut().unwrap().remove(old);
                let conflicts: Vec<Value> = state["conflicts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|slug| {
                        if slug.as_str() == Some(old) {
                            Value::String(new.to_string())
                        } else {
                            slug.clone()
                        }
                    })
                    .collect();
                state["conflicts"] = Value::Array(conflicts);
            });
        }
    }

    // ベースに記事が無く、記事用のブランチ blog/{slug} にだけ記事がある
    impl ArticleTest {
        fn drafting_synced(&self, slug: &str, md: &[u8], images: &[(&str, &[u8])]) -> FakeRepo {
            let repo = self.new_repo();
            repo.branch(&self.transport, "main", &[("README.md", README)]);
            let mut files = vec![(format!("src/content/blog/{slug}.md"), md)];
            for (name, content) in images {
                files.push((format!("src/assets/{slug}/{name}"), *content));
            }
            let borrowed: Vec<(&str, &[u8])> =
                files.iter().map(|(p, c)| (p.as_str(), *c)).collect();
            repo.branch(&self.transport, &format!("blog/{slug}"), &borrowed);
            self.sync();
            repo
        }
    }

    // slugの変更 ---------------------------------------------------------------

    #[test]
    fn 記録の無い記事のslugを変えても手元だけでstate_jsonは替わらない() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", &[("README.md", README)]);
        test.sync();
        test.write_md("draft", LOCAL_MD);
        test.write_image("draft/01.png", PNG);
        let state_before = fs::read(&test.state_path).unwrap();

        test.rename("draft", "renamed").unwrap();

        assert_eq!(fs::read(test.md_path("renamed")).unwrap(), LOCAL_MD);
        assert_eq!(fs::read(test.image_path("renamed/01.png")).unwrap(), PNG);
        assert!(!test.md_path("draft").exists());
        assert!(!test.assets_dir("draft").exists());
        // 記録が無いのでsyncedも作らない
        assert!(!test.synced_path("renamed").exists());
        assert_eq!(fs::read(&test.state_path).unwrap(), state_before);
    }

    #[test]
    fn 記事用のブランチのある記事のslugを変えると手元とsyncedと記録が新しいslugになる() {
        let test = ArticleTest::new();
        test.drafting_synced("old", BRANCH_MD, &[("01.png", PNG)]);
        test.mark_fetched("old", &[("01.png", true)]);
        test.write_image("old/01.png", PNG);
        // 手元で足した画像と、食い違いの印
        test.write_image("old/02.png", PNG2);
        test.edit_state(|state| state["conflicts"] = serde_json::json!(["old"]));
        let commit = test.state()["articles"]["old"]["commit"].clone();

        test.rename("old", "new").unwrap();

        assert_eq!(fs::read(test.md_path("new")).unwrap(), BRANCH_MD);
        assert!(!test.md_path("old").exists());
        assert_eq!(fs::read(test.image_path("new/01.png")).unwrap(), PNG);
        assert_eq!(fs::read(test.image_path("new/02.png")).unwrap(), PNG2);
        assert!(!test.assets_dir("old").exists());
        assert_eq!(fs::read(test.synced_path("new")).unwrap(), BRANCH_MD);
        assert!(!test.synced_path("old").exists());
        let state = test.state();
        assert!(state["articles"]["old"].is_null());
        let record = &state["articles"]["new"];
        assert_eq!(record["branch"], "blog/old");
        assert_eq!(record["commit"], commit);
        assert_eq!(record["renamedFrom"], "old");
        assert_eq!(state["conflicts"], serde_json::json!(["new"]));
    }

    #[test]
    fn slugを2回変えてもrenamedFromは最初のslugのまま() {
        let test = ArticleTest::new();
        test.drafting_synced("old", BRANCH_MD, &[]);

        test.rename("old", "mid").unwrap();
        test.rename("mid", "new").unwrap();

        let state = test.state();
        assert!(state["articles"]["mid"].is_null());
        assert_eq!(state["articles"]["new"]["renamedFrom"], "old");
    }

    #[test]
    fn 元のslugに戻すとrenamedFromが外れる() {
        let test = ArticleTest::new();
        test.drafting_synced("old", BRANCH_MD, &[]);

        test.rename("old", "new").unwrap();
        test.rename("new", "old").unwrap();

        let state = test.state();
        assert!(state["articles"]["new"].is_null());
        let record = &state["articles"]["old"];
        assert_eq!(record["branch"], "blog/old");
        assert!(record.get("renamedFrom").is_none(), "{record}");
    }

    #[test]
    fn 行き先が別の記録のrenamedFromならslugを変えられない() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", &[("README.md", README)]);
        repo.branch(
            &test.transport,
            "blog/old",
            &[("src/content/blog/old.md", BRANCH_MD)],
        );
        repo.branch(
            &test.transport,
            "blog/third",
            &[("src/content/blog/third.md", OTHER_MD)],
        );
        test.sync();
        test.mark_renamed("old", "new");
        let state_before = fs::read(&test.state_path).unwrap();

        match test.rename("third", "old") {
            Err(CommandError::Github(GithubError::SlugReserved { slug })) => {
                assert_eq!(slug, "old")
            }
            other => panic!("SlugReservedのはず: {other:?}"),
        }
        assert_eq!(fs::read(&test.state_path).unwrap(), state_before);
        assert!(test.md_path("third").exists());
    }

    #[test]
    fn 行き先がリモートの様子にあればslugを変えられない() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[("src/content/blog/post.md", BASE_MD)],
        );
        repo.branch(
            &test.transport,
            "blog/other",
            &[("src/content/blog/other.md", OTHER_MD)],
        );
        // 読めない記事(サブモジュール)
        repo.branch_with_entries(
            &test.transport,
            "blog/bad",
            &[],
            &[("src/content/blog/bad.md", "160000", "commit")],
        );
        test.sync();
        test.write_md("draft", LOCAL_MD);
        // skippedに入っている
        assert!(test.state()["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skipped| skipped["slug"] == "bad"));

        for slug in ["post", "other", "bad"] {
            match test.rename("draft", slug) {
                Err(CommandError::Github(GithubError::SlugInUse { slug: used })) => {
                    assert_eq!(used, slug)
                }
                other => panic!("SlugInUseのはず({slug}): {other:?}"),
            }
        }
        assert!(test.md_path("draft").exists());
    }

    #[test]
    fn state_jsonが読めなければslugを変えられない() {
        let test = ArticleTest::new();
        fs::create_dir_all(test.state_path.parent().unwrap()).unwrap();
        fs::write(&test.state_path, "{").unwrap();
        test.write_md("draft", LOCAL_MD);

        assert!(matches!(
            test.rename("draft", "new"),
            Err(CommandError::Github(GithubError::StateUnreadable))
        ));
        assert_eq!(fs::read(test.md_path("draft")).unwrap(), LOCAL_MD);
        assert!(!test.md_path("new").exists());
    }

    #[test]
    fn state_jsonが読めなければ削除も編集の取りやめもせず通信もしない() {
        let test = ArticleTest::new();
        test.drafting_synced("old", BRANCH_MD, &[("01.png", PNG)]);
        fs::write(&test.state_path, "{").unwrap();
        let before = test.files_snapshot();
        let requests = test.transport.requests().len();

        assert!(matches!(
            test.delete("old"),
            Err(CommandError::Github(GithubError::StateUnreadable))
        ));
        assert!(matches!(
            test.discard("old"),
            Err(CommandError::Github(GithubError::StateUnreadable))
        ));
        assert_eq!(test.transport.requests().len(), requests);
        assert_eq!(test.files_snapshot(), before);
    }

    #[test]
    #[cfg(unix)]
    fn 記録を書けなければslugの変更を元に戻す() {
        let test = ArticleTest::new();
        test.drafting_synced("old", BRANCH_MD, &[]);
        let state_before = fs::read(&test.state_path).unwrap();
        let Some(_read_only) = read_only_dir(test.state_path.parent().unwrap()) else {
            return;
        };
        let result = test.rename("old", "new");
        drop(_read_only);

        assert!(result.is_err());
        assert_eq!(fs::read(test.md_path("old")).unwrap(), BRANCH_MD);
        assert!(!test.md_path("new").exists());
        assert_eq!(fs::read(&test.state_path).unwrap(), state_before);
    }

    #[test]
    fn 連携しているときの新しい記事は行き先を確かめる() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[("src/content/blog/post.md", BASE_MD)],
        );
        repo.branch(
            &test.transport,
            "blog/old",
            &[("src/content/blog/old.md", BRANCH_MD)],
        );
        repo.branch(
            &test.transport,
            "blog/other",
            &[("src/content/blog/other.md", OTHER_MD)],
        );
        test.sync();
        test.mark_renamed("old", "new");

        // ほかの記事のrenamedFrom
        match test.create("old") {
            Err(CommandError::Github(GithubError::SlugReserved { slug })) => {
                assert_eq!(slug, "old")
            }
            other => panic!("SlugReservedのはず: {other:?}"),
        }
        // ベースの記事と記事用のブランチのslug
        for slug in ["post", "other"] {
            match test.create(slug) {
                Err(CommandError::Github(GithubError::SlugInUse { slug: used })) => {
                    assert_eq!(used, slug)
                }
                other => panic!("SlugInUseのはず({slug}): {other:?}"),
            }
        }
        assert!(!test.md_path("old").exists());
        // postとotherは読み込みで手元にあるもの。作り直していない
        assert_eq!(fs::read(test.md_path("post")).unwrap(), BASE_MD);
        assert_eq!(fs::read(test.md_path("other")).unwrap(), OTHER_MD);
    }

    // 削除 -------------------------------------------------------------------

    #[test]
    fn 記録の無い記事は手元だけ消し通信しない() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", &[("README.md", README)]);
        test.sync();
        test.write_md("draft", LOCAL_MD);
        test.write_image("draft/01.png", PNG);
        let requests = test.transport.requests().len();
        let state_before = fs::read(&test.state_path).unwrap();

        test.delete("draft").unwrap();

        assert!(!test.md_path("draft").exists());
        assert!(!test.assets_dir("draft").exists());
        assert_eq!(test.transport.requests().len(), requests);
        assert_eq!(fs::read(&test.state_path).unwrap(), state_before);
    }

    #[test]
    fn 記事用のブランチのある記事はブランチと手元と記録を消す() {
        let test = ArticleTest::new();
        let repo = test.drafting_synced("old", BRANCH_MD, &[("01.png", PNG)]);
        test.mark_fetched("old", &[("01.png", true)]);
        test.write_image("old/01.png", PNG);
        test.edit_state(|state| state["conflicts"] = serde_json::json!(["old"]));
        repo.accept_writes(&test.transport);

        test.delete("old").unwrap();

        // DELETEを送った
        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/old"),
            1
        );
        assert!(!test.md_path("old").exists());
        assert!(!test.assets_dir("old").exists());
        assert!(!test.synced_path("old").exists());
        let state = test.state();
        assert!(state["articles"]["old"].is_null());
        assert!(state["conflicts"].as_array().unwrap().is_empty());
        assert!(state["remote"]["branches"]
            .as_object()
            .unwrap()
            .get("blog/old")
            .is_none());
    }

    #[test]
    fn ブランチの先頭が記録と違えば消さずにBranchMoved() {
        let test = ArticleTest::new();
        let repo = test.drafting_synced("old", BRANCH_MD, &[]);
        let commit = test.state()["articles"]["old"]["commit"].clone();
        // ほかの端末がブランチを進める
        repo.branch(
            &test.transport,
            "blog/old",
            &[("src/content/blog/old.md", OTHER_MD)],
        );
        repo.accept_writes(&test.transport);
        let before = test.files_snapshot();

        assert!(matches!(
            test.delete("old"),
            Err(CommandError::Github(GithubError::BranchMoved))
        ));
        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/old"),
            0
        );
        assert_eq!(test.files_snapshot(), before);
        assert_eq!(test.state()["articles"]["old"]["commit"], commit);
    }

    #[test]
    fn ブランチが既に無ければ手元と記録を消して成功する() {
        let test = ArticleTest::new();
        let repo = test.drafting_synced("old", BRANCH_MD, &[("01.png", PNG)]);
        repo.accept_writes(&test.transport);
        repo.delete_branch("blog/old");

        test.delete("old").unwrap();

        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/old"),
            0
        );
        assert!(!test.md_path("old").exists());
        assert!(!test.image_path("old/01.png").exists());
        assert!(!test.synced_path("old").exists());
        assert!(test.state()["articles"]["old"].is_null());
    }

    #[test]
    fn renamedFromのある記事は古いブランチを消し以後そのslugの記事は出ない() {
        let test = ArticleTest::new();
        let repo = test.drafting_synced("old", BRANCH_MD, &[]);
        test.mark_renamed("old", "new");
        repo.accept_writes(&test.transport);

        test.delete("new").unwrap();

        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/old"),
            1
        );
        assert!(!test.md_path("new").exists());
        assert!(test.state()["articles"]["new"].is_null());
        test.sync();
        assert!(!test.md_path("old").exists());
        assert!(!test.md_path("new").exists());
    }

    #[test]
    fn ベースのブランチにある記事は削除できない() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[("src/content/blog/post.md", BASE_MD)],
        );
        test.sync();
        let state_before = fs::read(&test.state_path).unwrap();

        assert!(matches!(
            test.delete("post"),
            Err(CommandError::Github(GithubError::NotPublished))
        ));
        assert!(test.md_path("post").exists());
        assert_eq!(fs::read(&test.state_path).unwrap(), state_before);
    }

    #[test]
    #[cfg(unix)]
    fn ブランチを消した後に手元を消せなければ続く読み込みで記録だけ消す() {
        let test = ArticleTest::new();
        let repo = test.drafting_synced("old", BRANCH_MD, &[]);
        repo.accept_writes(&test.transport);
        let Some(read_only) = read_only_dir(&test.posts_dir) else {
            return;
        };
        let result = test.delete("old");
        drop(read_only);

        assert!(result.is_err());
        // ブランチは消えている
        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/old"),
            1
        );
        test.sync();
        assert!(test.state()["articles"]["old"].is_null());
        assert_eq!(fs::read(test.md_path("old")).unwrap(), BRANCH_MD);
    }

    // 編集をやめる -------------------------------------------------------------

    #[test]
    fn 編集をやめるとベースの内容に戻してブランチを消す() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        let base = repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", BASE_MD),
                ("src/assets/post/01.png", PNG),
                ("src/assets/post/02.png", PNG2),
            ],
        );
        repo.branch(
            &test.transport,
            "blog/post",
            &[
                ("src/content/blog/post.md", BRANCH_MD),
                ("src/assets/post/01.png", PNG),
                ("src/assets/post/02.png", PNG3),
                ("src/assets/post/03.png", PNG3),
            ],
        );
        test.sync();
        // 01は同じSHA、02は手元で替えた、03は手元にもある
        test.mark_fetched("post", &[("01.png", true), ("02.png", true)]);
        test.write_image("post/01.png", PNG);
        test.write_image("post/02.png", PNG3);
        test.write_image("post/03.png", PNG3);
        // 手元で足した画像と、食い違いの印
        test.write_image("post/04.png", PNG2);
        test.edit_state(|state| state["conflicts"] = serde_json::json!(["post"]));
        repo.accept_writes(&test.transport);

        test.discard("post").unwrap();

        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/post"),
            1
        );
        assert_eq!(fs::read(test.md_path("post")).unwrap(), BASE_MD);
        // SHAが同じ画像だけ残る
        assert_eq!(fs::read(test.image_path("post/01.png")).unwrap(), PNG);
        assert!(!test.image_path("post/02.png").exists());
        assert!(!test.image_path("post/03.png").exists());
        assert!(!test.image_path("post/04.png").exists());
        assert_eq!(fs::read(test.synced_path("post")).unwrap(), BASE_MD);
        let state = test.state();
        let record = &state["articles"]["post"];
        assert!(record["branch"].is_null());
        assert_eq!(record["commit"], base);
        assert_eq!(record["md"], blob_sha(BASE_MD));
        // ベースの画像の記録になり、01.pngは手元に残ったので取ってきたまま
        assert_eq!(record["assets"]["01.png"]["sha"], blob_sha(PNG));
        assert_eq!(record["assets"]["01.png"]["fetched"], true);
        assert_eq!(record["assets"]["02.png"]["sha"], blob_sha(PNG2));
        assert_eq!(record["assets"]["02.png"]["fetched"], false);
        assert!(state["conflicts"].as_array().unwrap().is_empty());
        let article = test.article_state("post");
        assert_eq!(article.category, crate::github::state::Category::Published);
        assert!(!article.dirty);
    }

    #[test]
    fn ブランチが無く手元だけ変えた記事も編集をやめるとベースの内容に戻る() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[
                ("src/content/blog/post.md", BASE_MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        test.sync();
        test.write_md("post", LOCAL_MD);
        test.write_image("post/02.png", PNG2);

        test.discard("post").unwrap();

        assert_eq!(test.request_count(Method::Delete, "/refs/heads"), 0);
        assert_eq!(fs::read(test.md_path("post")).unwrap(), BASE_MD);
        assert!(!test.image_path("post/02.png").exists());
        let record = &test.state()["articles"]["post"];
        assert!(record["branch"].is_null());
        assert_eq!(record["md"], blob_sha(BASE_MD));
    }

    #[test]
    fn ブランチの先頭が記録と違えば何も替えずにBranchMoved() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[("src/content/blog/post.md", BASE_MD)],
        );
        repo.branch(
            &test.transport,
            "blog/post",
            &[("src/content/blog/post.md", BRANCH_MD)],
        );
        test.sync();
        let commit = test.state()["articles"]["post"]["commit"].clone();
        repo.branch(
            &test.transport,
            "blog/post",
            &[("src/content/blog/post.md", OTHER_MD)],
        );
        repo.accept_writes(&test.transport);
        let before = test.files_snapshot();

        assert!(matches!(
            test.discard("post"),
            Err(CommandError::Github(GithubError::BranchMoved))
        ));
        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/post"),
            0
        );
        assert_eq!(test.files_snapshot(), before);
        assert_eq!(test.state()["articles"]["post"]["commit"], commit);
    }

    #[test]
    fn ベースに記事が無ければArticleNotFoundでブランチも手元も残る() {
        let test = ArticleTest::new();
        let repo = test.drafting_synced("old", BRANCH_MD, &[]);
        repo.accept_writes(&test.transport);

        assert!(matches!(
            test.discard("old"),
            Err(CommandError::Github(GithubError::ArticleNotFound))
        ));
        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/old"),
            0
        );
        assert_eq!(fs::read(test.md_path("old")).unwrap(), BRANCH_MD);
        assert!(test.state()["articles"]["old"].as_object().is_some());
    }

    #[test]
    fn ベースのブランチに無い記事の編集はやめられない() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", &[("README.md", README)]);
        test.sync();
        test.write_md("draft", LOCAL_MD);

        assert!(matches!(
            test.discard("draft"),
            Err(CommandError::Github(GithubError::ArticleNotFound))
        ));
        assert_eq!(fs::read(test.md_path("draft")).unwrap(), LOCAL_MD);
        assert!(test.state()["articles"]["draft"].is_null());
    }

    #[test]
    #[cfg(unix)]
    fn ブランチを消した後に手元を書き換えられなければ記録がベースの内容になる() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[("src/content/blog/post.md", BASE_MD)],
        );
        repo.branch(
            &test.transport,
            "blog/post",
            &[("src/content/blog/post.md", BRANCH_MD)],
        );
        test.sync();
        repo.accept_writes(&test.transport);
        let Some(read_only) = read_only_dir(&test.posts_dir) else {
            return;
        };
        let result = test.discard("post");
        drop(read_only);

        assert!(result.is_err());
        test.sync();
        let record = &test.state()["articles"]["post"];
        assert_eq!(record["md"], blob_sha(BASE_MD));
        assert!(record["branch"].is_null());
        // 手元は編集したままなので、編集中の記事として残る
        assert_eq!(fs::read(test.md_path("post")).unwrap(), BRANCH_MD);
        let article = test.article_state("post");
        assert_eq!(article.category, crate::github::state::Category::Editing);
    }

    // ロック -------------------------------------------------------------------

    #[test]
    fn 読み込みやPushの間に呼ぶとSyncBusyになる() {
        let test = ArticleTest::new();
        test.drafting_synced("old", BRANCH_MD, &[("01.png", PNG)]);
        let before = test.files_snapshot();
        let requests = test.transport.requests().len();
        // ロックを持ったまま、それぞれの関数を呼ぶ
        let rename = test.lock.run(|| match test.rename("old", "new") {
            Err(CommandError::Github(error)) => Err(error),
            _ => Ok(()),
        });
        assert!(matches!(rename, Err(GithubError::SyncBusy)));
        let delete = test.lock.run(|| match test.delete("old") {
            Err(CommandError::Github(error)) => Err(error),
            _ => Ok(()),
        });
        assert!(matches!(delete, Err(GithubError::SyncBusy)));
        let discard = test.lock.run(|| match test.discard("old") {
            Err(CommandError::Github(error)) => Err(error),
            _ => Ok(()),
        });
        assert!(matches!(discard, Err(GithubError::SyncBusy)));
        assert_eq!(test.transport.requests().len(), requests);
        assert_eq!(test.files_snapshot(), before);
    }

    // 画像の取得との組み合わせ -------------------------------------------------

    #[test]
    fn 改名した記事の取ってきていない画像は新しいslugの下に取る() {
        let test = ArticleTest::new();
        test.drafting_synced("old", BRANCH_MD, &[("01.png", PNG)]);
        test.rename("old", "new").unwrap();
        assert!(!test.image_path("new/01.png").exists());

        assert_eq!(test.fetch_images("new").unwrap(), 0);

        assert_eq!(fs::read(test.image_path("new/01.png")).unwrap(), PNG);
        assert!(!test.image_path("old/01.png").exists());
        assert_eq!(
            test.state()["articles"]["new"]["assets"]["01.png"]["fetched"],
            true
        );
    }

    #[test]
    fn 画像の通信の間に改名したら古いslugにも新しいslugにも書かない() {
        let test = ArticleTest::new();
        test.drafting_synced("old", BRANCH_MD, &[("01.png", PNG)]);
        let data_dir = test.data_dir.clone();
        let repo_of = test.repo_of.clone();
        let basis_of = test.basis_of.clone();
        let store = Arc::clone(&test.store);
        let lock = Arc::clone(&test.lock);
        let sha = blob_sha(PNG);
        test.transport.on(
            Method::Get,
            &url_of(
                &["repos", "owner", "blog", "git", "blobs", sha.as_str()],
                &[],
            ),
            Box::new(move |_| {
                super::rename(
                    &data_dir,
                    &repo_of,
                    &basis_of,
                    store.as_ref(),
                    lock.as_ref(),
                    "old",
                    "new",
                )
                .unwrap();
                Ok(raw_response(200, PNG.to_vec()))
            }),
        );

        test.fetch_images("old").unwrap();

        assert!(!test.image_path("old/01.png").exists());
        assert!(!test.image_path("new/01.png").exists());
        let state = test.state();
        assert!(state["articles"]["old"].is_null());
        assert_eq!(
            state["articles"]["new"]["assets"]["01.png"]["fetched"],
            false
        );
    }

    #[test]
    fn 削除のDELETEが422でもブランチが無ければ成功する() {
        let test = ArticleTest::new();
        let repo = test.drafting_synced("old", BRANCH_MD, &[]);
        repo.accept_writes(&test.transport);
        // DELETEの直前に、ほかの端末がブランチを消す
        let hook_repo = repo.clone();
        repo.before(Method::Delete, &test.ref_url("blog/old"), 1, move || {
            hook_repo.delete_branch("blog/old");
        });

        test.delete("old").unwrap();

        assert!(!test.md_path("old").exists());
        assert!(test.state()["articles"]["old"].is_null());
    }

    #[test]
    fn 削除のDELETEが失敗してブランチが残れば手元も記録も残す() {
        let test = ArticleTest::new();
        let repo = test.drafting_synced("old", BRANCH_MD, &[]);
        repo.accept_writes(&test.transport);
        test.transport.json(
            Method::Delete,
            &test.ref_url("blog/old"),
            500,
            serde_json::json!({ "message": "failed" }),
        );

        let result = test.delete("old");
        assert!(
            matches!(
                result,
                Err(CommandError::Github(GithubError::Status { status: 500 }))
            ),
            "{result:?}"
        );
        assert_eq!(fs::read(test.md_path("old")).unwrap(), BRANCH_MD);
        assert!(test.state()["articles"]["old"].as_object().is_some());
        assert!(repo.head("blog/old").starts_with("commit-"));
    }

    #[test]
    fn 削除のDELETEが404でもブランチが無ければ成功する() {
        let test = ArticleTest::new();
        let repo = test.drafting_synced("old", BRANCH_MD, &[]);
        repo.accept_writes(&test.transport);
        let hook_repo = repo.clone();
        test.transport.on(
            Method::Delete,
            &test.ref_url("blog/old"),
            Box::new(move |_| {
                hook_repo.delete_branch("blog/old");
                Ok(raw_response(404, b"{\"message\":\"Not Found\"}".to_vec()))
            }),
        );

        test.delete("old").unwrap();

        assert!(!test.md_path("old").exists());
        assert!(test.state()["articles"]["old"].is_null());
    }

    #[test]
    fn 削除のDELETEが422でブランチが残っていれば手元も記録も残す() {
        let test = ArticleTest::new();
        let repo = test.drafting_synced("old", BRANCH_MD, &[]);
        repo.accept_writes(&test.transport);
        test.transport.json(
            Method::Delete,
            &test.ref_url("blog/old"),
            422,
            serde_json::json!({ "message": "Reference update failed" }),
        );
        let before = test.files_snapshot();

        let result = test.delete("old");
        assert!(
            matches!(
                result,
                Err(CommandError::Github(GithubError::Status { status: 422 }))
            ),
            "{result:?}"
        );
        assert_eq!(test.files_snapshot(), before);
        assert!(test.state()["articles"]["old"].as_object().is_some());
    }

    #[test]
    fn 記録が無くてもリモートの様子に記事用のブランチがあれば削除で消す() {
        let test = ArticleTest::new();
        let repo = test.drafting_synced("old", BRANCH_MD, &[]);
        // 手元と食い違って記録を作らなかった記事
        test.edit_state(|state| {
            state["articles"].as_object_mut().unwrap().remove("old");
        });
        test.write_md("old", LOCAL_MD);
        repo.accept_writes(&test.transport);
        assert!(test.article_state("old").branch);

        test.delete("old").unwrap();

        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/old"),
            1
        );
        assert!(!test.md_path("old").exists());
        test.sync();
        assert!(!test.md_path("old").exists());
    }

    // 公開済みの記事postに、記事用のブランチblog/postがあり、読み込み済み
    fn editing_synced(test: &ArticleTest) -> FakeRepo {
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[("src/content/blog/post.md", BASE_MD)],
        );
        repo.branch(
            &test.transport,
            "blog/post",
            &[("src/content/blog/post.md", BRANCH_MD)],
        );
        test.sync();
        repo.accept_writes(&test.transport);
        repo
    }

    #[test]
    fn 記録が無くリモートの様子に記事用のブランチがある記事のslugは変えられない() {
        let test = ArticleTest::new();
        test.drafting_synced("old", BRANCH_MD, &[]);
        test.edit_state(|state| {
            state["articles"].as_object_mut().unwrap().remove("old");
        });
        test.write_md("old", LOCAL_MD);
        let before = test.files_snapshot();

        assert!(matches!(
            test.rename("old", "new"),
            Err(CommandError::Github(GithubError::Conflicted))
        ));
        assert_eq!(test.files_snapshot(), before);
    }

    #[test]
    fn ベースのmdを取れなければブランチを消さずに何も替えない() {
        let test = ArticleTest::new();
        let _repo = editing_synced(&test);
        let sha = blob_sha(BASE_MD);
        test.transport.json(
            Method::Get,
            &url_of(
                &["repos", "owner", "blog", "git", "blobs", sha.as_str()],
                &[],
            ),
            500,
            serde_json::json!({ "message": "failed" }),
        );
        let before = test.files_snapshot();

        assert!(matches!(
            test.discard("post"),
            Err(CommandError::Github(GithubError::Status { status: 500 }))
        ));
        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/post"),
            0
        );
        assert_eq!(test.files_snapshot(), before);
    }

    #[test]
    fn GitHubのベースから記事が消えていれば編集をやめない() {
        let test = ArticleTest::new();
        let repo = editing_synced(&test);
        // 前回の読み込みの後に、ほかの端末がベースから消した
        repo.branch(&test.transport, "main", &[("README.md", README)]);
        let before = test.files_snapshot();

        assert!(matches!(
            test.discard("post"),
            Err(CommandError::Github(GithubError::ArticleNotFound))
        ));
        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/post"),
            0
        );
        assert_eq!(test.files_snapshot(), before);
    }

    // #68: 公開済みの記事の削除・非公開 -------------------------------------------

    const PUBLISHED_FILES: &[(&str, &[u8])] = &[
        ("README.md", README),
        ("src/content/blog/post.md", BASE_MD),
        ("src/content/blog/other.md", OTHER_MD),
        ("src/assets/post/01.png", PNG),
        ("src/assets/post/sub/02.png", PNG2),
        ("src/assets/other/01.png", PNG),
        ("src/assets/shared.png", PNG3),
    ];

    impl ArticleTest {
        fn remove_published(&self, slug: &str, kind: Deleting) -> Result<(), CommandError> {
            super::remove_published(&self.input(), self.lock.as_ref(), slug, kind, &|_| {})
        }

        fn remove_published_progress(
            &self,
            slug: &str,
            kind: Deleting,
        ) -> (Result<(), CommandError>, Vec<PushProgress>) {
            let steps = RefCell::new(Vec::new());
            let result =
                super::remove_published(&self.input(), self.lock.as_ref(), slug, kind, &|step| {
                    steps.borrow_mut().push(step)
                });
            (result, steps.into_inner())
        }

        fn post_bodies(&self, url_part: &str) -> Vec<Value> {
            self.transport
                .requests()
                .iter()
                .filter(|request| request.method == Method::Post && request.url.contains(url_part))
                .map(|request| serde_json::from_slice(request.body.as_deref().unwrap()).unwrap())
                .collect()
        }

        fn refs_url(&self) -> String {
            url_of(&["repos", "owner", "blog", "git", "refs"], &[])
        }

        // ベースに公開済みの記事postと、ほかの記事・画像・リポジトリのファイルがあり、読み込み済み
        fn published_setup(&self) -> (FakeRepo, String) {
            let repo = self.new_repo();
            let base = repo.branch(&self.transport, "main", PUBLISHED_FILES);
            repo.watch_ref(&self.transport, "blog/post");
            self.sync();
            (repo, base)
        }

        // 削除のコミットの空のブランチblog/postを残し、記録を削除予定にする
        fn mark_delete_pending(&self, repo: &FakeRepo, base: &str, kind: &str) -> String {
            let commit =
                repo.branch_from(&self.transport, "blog/post", base, &[("README.md", README)]);
            self.edit_state(|state| {
                state["articles"]["post"]["branch"] = Value::String("blog/post".to_string());
                state["articles"]["post"]["commit"] = Value::String(commit.clone());
                state["articles"]["post"]["deleting"] = Value::String(kind.to_string());
                state["remote"]["branches"]["blog/post"] = serde_json::json!({
                    "commit": commit.clone(),
                    "slug": "post",
                });
            });
            commit
        }

        // ベースの記事oldをnewに移したブランチがあり、手元と記録が移した記事になっている
        fn moved_synced(&self, old: &str, new: &str, md: &[u8]) -> FakeRepo {
            let repo = self.new_repo();
            let old_path = format!("src/content/blog/{old}.md");
            let new_path = format!("src/content/blog/{new}.md");
            let base = repo.branch(
                &self.transport,
                "main",
                &[("README.md", README), (old_path.as_str(), BASE_MD)],
            );
            repo.branch_from(
                &self.transport,
                &format!("blog/{new}"),
                &base,
                &[("README.md", README), (new_path.as_str(), md)],
            );
            self.sync();
            // 元のslugの手元と記録は残さない
            if self.md_path(old).exists() {
                fs::remove_file(self.md_path(old)).unwrap();
            }
            if self.synced_path(old).exists() {
                fs::remove_file(self.synced_path(old)).unwrap();
            }
            self.edit_state(|state| {
                state["articles"].as_object_mut().unwrap().remove(old);
                state["articles"][new]["renamedFrom"] = Value::String(old.to_string());
            });
            repo
        }
    }

    #[test]
    fn 公開済みの記事を削除すると記事のファイルだけ消したコミットを作る() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        repo.accept_writes(&test.transport);

        test.remove_published("post", Deleting::Delete).unwrap();

        let files = repo.tree_files("blog/post");
        // 記事のmdとassets/post/の下(下のディレクトリも)が消える
        assert!(!files.contains_key("src/content/blog/post.md"));
        assert!(
            !files
                .keys()
                .any(|path| path.starts_with("src/assets/post/")),
            "{:?}",
            files.keys().collect::<Vec<_>>()
        );
        // ほかの記事・ほかの記事の画像・共有の画像・リポジトリのほかのファイルは残る
        for path in [
            "README.md",
            "src/content/blog/other.md",
            "src/assets/other/01.png",
            "src/assets/shared.png",
        ] {
            assert!(files.contains_key(path), "{path} は残るはず");
        }
        let commits = test.post_bodies("/git/commits");
        let message = &commits.last().unwrap()["message"];
        assert_eq!(message, "postを削除");
    }

    #[test]
    fn 非公開にするとCommit_messageは非公開にで記録の印は非公開になる() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        repo.accept_writes(&test.transport);

        test.remove_published("post", Deleting::Unpublish).unwrap();

        let commits = test.post_bodies("/git/commits");
        let message = &commits.last().unwrap()["message"];
        assert_eq!(message, "postを非公開に");
        assert_eq!(test.state()["articles"]["post"]["deleting"], "unpublish");
    }

    #[test]
    fn 削除を終えると記録と様子が削除予定になり手元は替わらない() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        repo.accept_writes(&test.transport);
        let md_before = fs::read(test.md_path("post")).unwrap();
        let synced_before = fs::read(test.synced_path("post")).unwrap();

        test.remove_published("post", Deleting::Delete).unwrap();

        let head = repo.head("blog/post");
        let state = test.state();
        let record = &state["articles"]["post"];
        assert_eq!(record["branch"], "blog/post");
        assert_eq!(record["commit"], head);
        assert_eq!(record["md"], blob_sha(BASE_MD));
        assert_eq!(record["assets"]["01.png"]["sha"], blob_sha(PNG));
        assert_eq!(record["deleting"], "delete");
        let branch = &state["remote"]["branches"]["blog/post"];
        assert_eq!(branch["commit"], head);
        assert_eq!(branch["slug"], "post");
        assert!(
            branch.get("article").is_none(),
            "様子は空のブランチになるはず: {branch}"
        );
        // 手元のファイルは替えない
        assert_eq!(fs::read(test.md_path("post")).unwrap(), md_before);
        assert_eq!(fs::read(test.synced_path("post")).unwrap(), synced_before);
    }

    #[test]
    fn 記録の印はブランチを作る通信の前に書かれている() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        repo.accept_writes(&test.transport);
        let seen = Arc::new(Mutex::new(None));
        let state_path = test.state_path.clone();
        let hook_seen = Arc::clone(&seen);
        repo.before(Method::Post, &test.refs_url(), 1, move || {
            let state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
            *hook_seen.lock().unwrap() = state["articles"]["post"]["deleting"]
                .as_str()
                .map(str::to_string);
        });

        test.remove_published("post", Deleting::Delete).unwrap();

        assert_eq!(seen.lock().unwrap().as_deref(), Some("delete"));
    }

    #[test]
    fn 内容がベースと同じ記事用のブランチが残っていればPatchで進める() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        let base = repo.branch(&test.transport, "main", PUBLISHED_FILES);
        let branch = repo.branch(&test.transport, "blog/post", PUBLISHED_FILES);
        test.sync();
        repo.accept_writes(&test.transport);

        test.remove_published("post", Deleting::Delete).unwrap();

        assert_eq!(
            test.request_count(Method::Patch, "/refs/heads/blog/post"),
            1
        );
        assert_eq!(test.request_count(Method::Post, "/git/refs"), 0);
        // 親は残ったブランチの先頭
        let commits = test.post_bodies("/git/commits");
        assert_eq!(
            commits.last().unwrap()["parents"],
            serde_json::json!([branch])
        );
        assert_ne!(repo.head("blog/post"), branch);
        assert_ne!(base, branch);
    }

    #[test]
    fn 残ったブランチの先頭が記録と違えばBranchMovedで何も替えない() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        let _base = repo.branch(&test.transport, "main", PUBLISHED_FILES);
        repo.branch(&test.transport, "blog/post", PUBLISHED_FILES);
        test.sync();
        // ほかの端末がブランチを進める
        repo.branch(
            &test.transport,
            "blog/post",
            &[
                ("src/content/blog/post.md", OTHER_MD),
                ("src/assets/post/01.png", PNG2),
            ],
        );
        repo.accept_writes(&test.transport);
        let before = test.files_snapshot();
        let commit = test.state()["articles"]["post"]["commit"].clone();

        assert!(matches!(
            test.remove_published("post", Deleting::Delete),
            Err(CommandError::Github(GithubError::BranchMoved))
        ));
        assert_eq!(
            test.request_count(Method::Patch, "/refs/heads/blog/post"),
            0
        );
        assert_eq!(test.request_count(Method::Post, "/git/refs"), 0);
        assert_eq!(test.files_snapshot(), before);
        assert_eq!(test.state()["articles"]["post"]["commit"], commit);
    }

    #[test]
    fn 残ったブランチの記事がベースと違えばBranchMoved() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        let _base = repo.branch(&test.transport, "main", PUBLISHED_FILES);
        test.sync();
        // 記事用のブランチだけがベースと違う記事を持つ。記録はベースのまま
        let branch = repo.branch(
            &test.transport,
            "blog/post",
            &[
                ("src/content/blog/post.md", BRANCH_MD),
                ("src/assets/post/01.png", PNG),
            ],
        );
        test.edit_state(|state| {
            state["articles"]["post"]["branch"] = Value::String("blog/post".to_string());
            state["articles"]["post"]["commit"] = Value::String(branch);
        });
        repo.accept_writes(&test.transport);
        let before = test.files_snapshot();

        assert!(matches!(
            test.remove_published("post", Deleting::Delete),
            Err(CommandError::Github(GithubError::BranchMoved))
        ));
        assert_eq!(
            test.request_count(Method::Patch, "/refs/heads/blog/post"),
            0
        );
        assert_eq!(test.files_snapshot(), before);
    }

    #[test]
    fn 手元の記事が記録と違えばOutOfSyncで通信で書き込まない() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        test.write_md("post", LOCAL_MD);
        repo.accept_writes(&test.transport);
        let before = test.files_snapshot();

        assert!(matches!(
            test.remove_published("post", Deleting::Delete),
            Err(CommandError::Github(GithubError::OutOfSync))
        ));
        assert_eq!(test.write_count(), 0);
        assert_eq!(test.files_snapshot(), before);
    }

    #[test]
    fn 記録がベースの今の記事と違えばOutOfSync() {
        let test = ArticleTest::new();
        let (repo, base) = test.published_setup();
        // 読み込みの後に、ほかの端末がベースの記事を替える
        repo.branch_from(
            &test.transport,
            "main",
            &base,
            &[
                ("README.md", README),
                ("src/content/blog/post.md", OTHER_MD),
                ("src/content/blog/other.md", OTHER_MD),
                ("src/assets/post/01.png", PNG),
                ("src/assets/post/sub/02.png", PNG2),
                ("src/assets/other/01.png", PNG),
                ("src/assets/shared.png", PNG3),
            ],
        );
        repo.accept_writes(&test.transport);
        let before = test.files_snapshot();

        assert!(matches!(
            test.remove_published("post", Deleting::Delete),
            Err(CommandError::Github(GithubError::OutOfSync))
        ));
        assert_eq!(test.write_count(), 0);
        assert_eq!(test.files_snapshot(), before);
    }

    #[test]
    fn ベースに記事が無ければArticleNotFound() {
        let test = ArticleTest::new();
        let (repo, base) = test.published_setup();
        // 読み込みの後に、ほかの端末がベースから記事を消す
        repo.branch_from(&test.transport, "main", &base, &[("README.md", README)]);
        repo.accept_writes(&test.transport);
        let before = test.files_snapshot();

        assert!(matches!(
            test.remove_published("post", Deleting::Delete),
            Err(CommandError::Github(GithubError::ArticleNotFound))
        ));
        assert_eq!(test.request_count(Method::Post, "/git/refs"), 0);
        assert_eq!(test.files_snapshot(), before);
    }

    #[test]
    fn 執筆中の記事はNotPublished() {
        let test = ArticleTest::new();
        let repo = test.drafting_synced("old", BRANCH_MD, &[]);
        repo.accept_writes(&test.transport);
        let before = test.files_snapshot();

        assert!(matches!(
            test.remove_published("old", Deleting::Delete),
            Err(CommandError::Github(GithubError::NotPublished))
        ));
        assert_eq!(test.files_snapshot(), before);
    }

    #[test]
    fn 編集中の記事はNotPublished() {
        let test = ArticleTest::new();
        let repo = editing_synced(&test);
        let before = test.files_snapshot();

        assert!(matches!(
            test.remove_published("post", Deleting::Delete),
            Err(CommandError::Github(GithubError::NotPublished))
        ));
        assert_eq!(test.request_count(Method::Post, "/git/refs"), 0);
        assert_eq!(test.request_count(Method::Patch, "/git/refs"), 0);
        assert_eq!(test.files_snapshot(), before);
        assert!(repo.head("blog/post").starts_with("commit-"));
    }

    #[test]
    fn 削除予定の記事はNotPublished() {
        let test = ArticleTest::new();
        let (repo, base) = test.published_setup();
        test.mark_delete_pending(&repo, &base, "delete");
        repo.accept_writes(&test.transport);
        let before = test.files_snapshot();

        assert!(matches!(
            test.remove_published("post", Deleting::Unpublish),
            Err(CommandError::Github(GithubError::NotPublished))
        ));
        assert_eq!(test.files_snapshot(), before);
    }

    #[test]
    fn Postが422でブランチがあればBranchExists() {
        let test = ArticleTest::new();
        let (repo, base) = test.published_setup();
        repo.accept_writes(&test.transport);
        // 記録に無いブランチを、POSTの直前にほかの端末が作る
        let hook_repo = repo.clone();
        let hook_transport = Arc::clone(&test.transport);
        repo.before(Method::Post, &test.refs_url(), 1, move || {
            hook_repo.branch_from(&hook_transport, "blog/post", &base, PUBLISHED_FILES);
        });

        assert!(matches!(
            test.remove_published("post", Deleting::Delete),
            Err(CommandError::Github(GithubError::BranchExists { branch })) if branch == "blog/post"
        ));
    }

    #[test]
    fn Postが422でブランチが無ければ元のエラー() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        repo.accept_writes(&test.transport);
        test.transport.json(
            Method::Post,
            &test.refs_url(),
            422,
            serde_json::json!({ "message": "Reference update failed" }),
        );

        let result = test.remove_published("post", Deleting::Delete);
        assert!(
            matches!(
                result,
                Err(CommandError::Github(GithubError::Status { status: 422 }))
            ),
            "{result:?}"
        );
    }

    #[test]
    fn Patchが失敗して先頭が親から進んでいればBranchMoved() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        let _base = repo.branch(&test.transport, "main", PUBLISHED_FILES);
        let branch = repo.branch(&test.transport, "blog/post", PUBLISHED_FILES);
        test.sync();
        repo.accept_writes(&test.transport);
        // PATCHの応答の前に、ほかの端末がブランチを進める
        let hook_repo = repo.clone();
        let hook_transport = Arc::clone(&test.transport);
        let url = test.ref_url("blog/post");
        test.transport.on(
            Method::Patch,
            &url,
            Box::new(move |_| {
                hook_repo.branch_from(
                    &hook_transport,
                    "blog/post",
                    &branch,
                    &[
                        ("src/content/blog/post.md", OTHER_MD),
                        ("src/assets/post/01.png", PNG),
                    ],
                );
                Ok(raw_response(500, b"{\"message\":\"failed\"}".to_vec()))
            }),
        );

        assert!(matches!(
            test.remove_published("post", Deleting::Delete),
            Err(CommandError::Github(GithubError::BranchMoved))
        ));
    }

    #[test]
    fn Patchが失敗して先頭が親と同じなら元のエラー() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        let _base = repo.branch(&test.transport, "main", PUBLISHED_FILES);
        repo.branch(&test.transport, "blog/post", PUBLISHED_FILES);
        test.sync();
        repo.accept_writes(&test.transport);
        test.transport.json(
            Method::Patch,
            &test.ref_url("blog/post"),
            500,
            serde_json::json!({ "message": "failed" }),
        );

        let result = test.remove_published("post", Deleting::Delete);
        assert!(
            matches!(
                result,
                Err(CommandError::Github(GithubError::Status { status: 500 }))
            ),
            "{result:?}"
        );
    }

    #[test]
    fn ブランチを作る通信が届いたか分からなければ印を残し次の読み込みで外れる() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        repo.accept_writes(&test.transport);
        test.transport.on(
            Method::Post,
            &test.refs_url(),
            Box::new(|_| {
                Err(GithubError::Network {
                    reason: "connection reset".to_string(),
                })
            }),
        );

        let result = test.remove_published("post", Deleting::Delete);
        assert!(result.is_err(), "失敗するはず");
        assert_eq!(test.state()["articles"]["post"]["deleting"], "delete");

        test.sync();

        let record = &test.state()["articles"]["post"];
        assert!(record.get("deleting").is_none(), "{record}");
        assert!(record["branch"].is_null());
    }

    #[test]
    fn 失敗の応答でもブランチが作ったコミットになっていれば成功にする() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        repo.accept_writes(&test.transport);
        let created = Arc::new(Mutex::new(None::<String>));
        // POSTは届いてブランチができたが、応答は500だった
        let fake = repo.clone();
        let transport = Arc::clone(&test.transport);
        let seen = Arc::clone(&created);
        test.transport.on(
            Method::Post,
            &test.refs_url(),
            Box::new(move |request| {
                let body: Value = serde_json::from_slice(request.body.as_deref().unwrap()).unwrap();
                let commit = body["sha"].as_str().unwrap().to_string();
                fake.point_branch(&transport, "blog/post", &commit);
                *seen.lock().unwrap() = Some(commit);
                Ok(json_response(
                    500,
                    serde_json::json!({ "message": "failed" }),
                ))
            }),
        );

        test.remove_published("post", Deleting::Unpublish).unwrap();

        let commit = created.lock().unwrap().clone().unwrap();
        let record = &test.state()["articles"]["post"];
        assert_eq!(record["branch"], "blog/post");
        assert_eq!(record["commit"], commit);
        assert_eq!(record["deleting"], "unpublish");
    }

    #[test]
    fn GitHubがブランチの作成を断ったら印を外す() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        repo.accept_writes(&test.transport);
        let record_before = test.state()["articles"]["post"].clone();
        test.transport.json(
            Method::Post,
            &test.refs_url(),
            500,
            serde_json::json!({ "message": "failed" }),
        );

        assert!(matches!(
            test.remove_published("post", Deleting::Delete),
            Err(CommandError::Github(GithubError::Status { status: 500 }))
        ));
        assert_eq!(test.state()["articles"]["post"], record_before);
    }

    #[test]
    fn 知らせる段階はCommittingとPushingの順でUploadingは出ない() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        repo.accept_writes(&test.transport);

        let (result, steps) = test.remove_published_progress("post", Deleting::Delete);

        result.unwrap();
        assert_eq!(steps, vec![PushProgress::Committing, PushProgress::Pushing]);
    }

    #[test]
    fn state_jsonが読めなければ削除も非公開もせず通信もしない() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        repo.accept_writes(&test.transport);
        fs::write(&test.state_path, "{").unwrap();
        let before = test.files_snapshot();
        let requests = test.transport.requests().len();

        assert!(matches!(
            test.remove_published("post", Deleting::Delete),
            Err(CommandError::Github(GithubError::StateUnreadable))
        ));
        assert!(matches!(
            test.remove_published("post", Deleting::Unpublish),
            Err(CommandError::Github(GithubError::StateUnreadable))
        ));
        assert_eq!(test.transport.requests().len(), requests);
        assert_eq!(test.files_snapshot(), before);
    }

    #[test]
    fn ほかの操作が動いていれば削除も非公開もSyncBusy() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        repo.accept_writes(&test.transport);
        let before = test.files_snapshot();
        let requests = test.transport.requests().len();

        let delete = test
            .lock
            .run(|| match test.remove_published("post", Deleting::Delete) {
                Err(CommandError::Github(error)) => Err(error),
                _ => Ok(()),
            });
        assert!(matches!(delete, Err(GithubError::SyncBusy)));
        let unpublish =
            test.lock.run(
                || match test.remove_published("post", Deleting::Unpublish) {
                    Err(CommandError::Github(error)) => Err(error),
                    _ => Ok(()),
                },
            );
        assert!(matches!(unpublish, Err(GithubError::SyncBusy)));
        assert_eq!(test.transport.requests().len(), requests);
        assert_eq!(test.files_snapshot(), before);
    }

    // #68: 削除・非公開の取りやめ ------------------------------------------------

    #[test]
    fn 削除予定の記事の取りやめは空のブランチを消してベースの内容に戻す() {
        for kind in ["delete", "unpublish"] {
            let test = ArticleTest::new();
            let (repo, base) = test.published_setup();
            let commit = test.mark_delete_pending(&repo, &base, kind);
            repo.accept_writes(&test.transport);

            test.discard("post").unwrap();

            assert_eq!(
                test.request_count(Method::Delete, "/refs/heads/blog/post"),
                1,
                "{kind}"
            );
            assert_eq!(fs::read(test.md_path("post")).unwrap(), BASE_MD);
            let state = test.state();
            let record = &state["articles"]["post"];
            assert!(record["branch"].is_null(), "{kind}");
            assert_eq!(record["commit"], base, "{kind}");
            assert_eq!(record["md"], blob_sha(BASE_MD), "{kind}");
            assert!(record.get("deleting").is_none(), "{kind}: {record}");
            assert!(
                state["remote"]["branches"]
                    .as_object()
                    .unwrap()
                    .get("blog/post")
                    .is_none(),
                "{kind}"
            );
            assert_ne!(repo.head("main"), commit);
        }
    }

    // #68: 移した記事の取りやめ --------------------------------------------------

    #[test]
    fn renamedFromがあり記録のブランチが無い記事の取りやめは元のslugに戻す() {
        let test = ArticleTest::new();
        let (repo, _base) = test.published_setup();
        // 公開済みの記事postをnewに変えた後の手元と記録
        test.mark_renamed("post", "new");
        repo.accept_writes(&test.transport);

        test.discard("new").unwrap();

        assert_eq!(test.request_count(Method::Delete, "/refs/heads"), 0);
        assert!(!test.md_path("new").exists());
        assert_eq!(fs::read(test.md_path("post")).unwrap(), BASE_MD);
        assert_eq!(fs::read(test.synced_path("post")).unwrap(), BASE_MD);
        assert!(!test.synced_path("new").exists());
        let state = test.state();
        assert!(state["articles"]["new"].is_null());
        let record = &state["articles"]["post"];
        assert!(record["branch"].is_null());
        assert_eq!(record["md"], blob_sha(BASE_MD));
        assert!(record.get("renamedFrom").is_none(), "{record}");
    }

    #[test]
    fn 移した記事用のブランチの取りやめはブランチを消して元のslugに戻す() {
        let test = ArticleTest::new();
        let repo = test.moved_synced("old", "new", BRANCH_MD);
        repo.accept_writes(&test.transport);

        test.discard("new").unwrap();

        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/new"),
            1
        );
        assert!(!test.md_path("new").exists());
        assert_eq!(fs::read(test.md_path("old")).unwrap(), BASE_MD);
        assert_eq!(fs::read(test.synced_path("old")).unwrap(), BASE_MD);
        let state = test.state();
        assert!(state["articles"]["new"].is_null());
        let record = &state["articles"]["old"];
        assert!(record["branch"].is_null());
        assert_eq!(record["md"], blob_sha(BASE_MD));
        assert!(record.get("renamedFrom").is_none(), "{record}");
    }

    #[test]
    fn 移した記事の取りやめで元のslugの記事が手元に変更つきで残っていればOutOfSyncで何も替えない() {
        let test = ArticleTest::new();
        let repo = test.moved_synced("old", "new", BRANCH_MD);
        test.write_md("old", LOCAL_MD);
        repo.accept_writes(&test.transport);
        let before = test.files_snapshot();

        assert!(matches!(
            test.discard("new"),
            Err(CommandError::Github(GithubError::OutOfSync))
        ));
        assert_eq!(test.write_count(), 0);
        assert_eq!(test.files_snapshot(), before);
    }

    #[test]
    fn 移した記事の取りやめでブランチの先頭が記録と違えばBranchMovedで何も替えない() {
        let test = ArticleTest::new();
        let repo = test.moved_synced("old", "new", BRANCH_MD);
        // ほかの端末がブランチを進める
        repo.branch(
            &test.transport,
            "blog/new",
            &[("src/content/blog/new.md", OTHER_MD)],
        );
        repo.accept_writes(&test.transport);
        let before = test.files_snapshot();

        assert!(matches!(
            test.discard("new"),
            Err(CommandError::Github(GithubError::BranchMoved))
        ));
        assert_eq!(
            test.request_count(Method::Delete, "/refs/heads/blog/new"),
            0
        );
        assert_eq!(test.files_snapshot(), before);
    }

    // #68: 公開済みの記事のslugの変更 ---------------------------------------------

    #[test]
    fn 公開済みの記事のslugを変えると手元とsyncedが移りrenamedFromを持つ() {
        let test = ArticleTest::new();
        let _repo = test.published_setup();

        test.rename("post", "new").unwrap();

        assert!(!test.md_path("post").exists());
        assert_eq!(fs::read(test.md_path("new")).unwrap(), BASE_MD);
        assert!(!test.synced_path("post").exists());
        assert_eq!(fs::read(test.synced_path("new")).unwrap(), BASE_MD);
        let state = test.state();
        assert!(state["articles"]["post"].is_null());
        let record = &state["articles"]["new"];
        assert!(record["branch"].is_null());
        assert_eq!(record["renamedFrom"], "post");
        assert_eq!(record["md"], blob_sha(BASE_MD));
        assert_eq!(record["assets"]["01.png"]["sha"], blob_sha(PNG));
    }

    #[test]
    fn 削除予定の記事はslugを変えられない() {
        let test = ArticleTest::new();
        let (repo, base) = test.published_setup();
        test.mark_delete_pending(&repo, &base, "delete");
        let before = test.files_snapshot();

        assert!(matches!(
            test.rename("post", "new"),
            Err(CommandError::Github(GithubError::NotPublished))
        ));
        assert_eq!(test.files_snapshot(), before);
    }

    #[test]
    fn renamedFromの元のslugへ戻すとrenamedFromが外れ記録も残る() {
        let test = ArticleTest::new();
        let _repo = test.published_setup();
        test.mark_renamed("post", "new");

        test.rename("new", "post").unwrap();

        assert!(!test.md_path("new").exists());
        assert_eq!(fs::read(test.md_path("post")).unwrap(), BASE_MD);
        let state = test.state();
        assert!(state["articles"]["new"].is_null());
        let record = &state["articles"]["post"];
        assert_eq!(record["md"], blob_sha(BASE_MD));
        assert!(record.get("renamedFrom").is_none(), "{record}");
    }

    #[test]
    fn 移した記事を移された元のslugへ戻すのは許す() {
        let test = ArticleTest::new();
        let _repo = test.moved_synced("old", "new", BRANCH_MD);

        test.rename("new", "old").unwrap();

        assert!(!test.md_path("new").exists());
        assert_eq!(fs::read(test.md_path("old")).unwrap(), BRANCH_MD);
        let state = test.state();
        assert!(state["articles"]["new"].is_null());
        let record = &state["articles"]["old"];
        assert_eq!(record["branch"], "blog/new");
        assert!(record.get("renamedFrom").is_none(), "{record}");
    }

    #[test]
    fn 行き先が空のブランチのslugでベースに記事が無ければslugを変えられる() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        let base = repo.branch(&test.transport, "main", &[("README.md", README)]);
        repo.branch_from(
            &test.transport,
            "blog/empty",
            &base,
            &[("README.md", README)],
        );
        test.sync();
        test.write_md("draft", LOCAL_MD);
        // 行き先の空のブランチが様子にある前提
        let branch = &test.state()["remote"]["branches"]["blog/empty"];
        assert!(branch.is_object(), "空のブランチが様子にあるはず");
        assert!(branch.get("article").is_none(), "{branch}");

        test.rename("draft", "empty").unwrap();

        assert_eq!(fs::read(test.md_path("empty")).unwrap(), LOCAL_MD);
        assert!(!test.md_path("draft").exists());
    }

    #[test]
    fn 行き先が削除予定のslugならSlugInUse() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        let base = repo.branch(
            &test.transport,
            "main",
            &[("README.md", README), ("src/content/blog/post.md", BASE_MD)],
        );
        repo.branch_from(
            &test.transport,
            "blog/post",
            &base,
            &[("README.md", README)],
        );
        test.sync();
        test.write_md("draft", LOCAL_MD);
        // 削除予定の空のブランチが様子にある前提
        let branch = &test.state()["remote"]["branches"]["blog/post"];
        assert!(branch.is_object(), "空のブランチが様子にあるはず");
        assert!(branch.get("article").is_none(), "{branch}");

        assert!(matches!(
            test.rename("draft", "post"),
            Err(CommandError::Github(GithubError::SlugInUse { slug })) if slug == "post"
        ));
        assert!(test.md_path("draft").exists());
    }

    #[test]
    fn 空のブランチのslugでベースに記事が無ければ新しい記事を作れる() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        let base = repo.branch(&test.transport, "main", &[("README.md", README)]);
        repo.branch_from(&test.transport, "blog/new", &base, &[("README.md", README)]);
        test.sync();
        // 行き先の空のブランチが様子にある前提
        let branch = &test.state()["remote"]["branches"]["blog/new"];
        assert!(branch.is_object(), "空のブランチが様子にあるはず");
        assert!(branch.get("article").is_none(), "{branch}");

        test.create("new").unwrap();

        assert!(test.md_path("new").exists());
    }

    // 日付の印 -------------------------------------------------------------------

    #[test]
    fn 編集の取りやめでupdatedDateの印が外れる() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(
            &test.transport,
            "main",
            &[("README.md", README), ("src/content/blog/post.md", BASE_MD)],
        );
        test.sync();
        let path = dates::marks_path(&test.data_dir, &test.repo_of);
        dates::write_marks(&path, &marks_of(&[("post", false, true)])).unwrap();

        test.discard("post").unwrap();

        assert!(!updated_mark(&path, "post"));
    }

    #[test]
    fn slugの変更が失敗したら印は元のslugに戻る() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", &[("README.md", README)]);
        test.sync();
        let path = dates::marks_path(&test.data_dir, &test.repo_of);
        test.create("draft").unwrap();
        test.create("taken").unwrap();
        dates::write_marks(&path, &marks_of(&[("draft", true, true)])).unwrap();

        // 手元に同じslugの記事があるので、ファイルを移せない
        assert!(test.rename("draft", "taken").is_err());

        let marks = dates::read_marks(&path).unwrap();
        assert!(marks.articles["draft"].pub_date);
        assert!(marks.articles["draft"].updated_date);
        assert!(!marks.articles.contains_key("taken"));
    }

    #[test]
    fn 記事の作成と削除で印が外れslugの変更で付け替わる() {
        let test = ArticleTest::new();
        let repo = test.new_repo();
        repo.branch(&test.transport, "main", &[("README.md", README)]);
        test.sync();
        let path = dates::marks_path(&test.data_dir, &test.repo_of);

        // 作成
        dates::write_marks(&path, &marks_of(&[("draft", true, true)])).unwrap();
        test.create("draft").unwrap();
        assert!(!dates::read_marks(&path)
            .unwrap()
            .articles
            .contains_key("draft"));

        // slugの変更
        dates::write_marks(
            &path,
            &marks_of(&[("draft", true, true), ("other", true, false)]),
        )
        .unwrap();
        test.rename("draft", "renamed").unwrap();
        let marks = dates::read_marks(&path).unwrap();
        assert!(!marks.articles.contains_key("draft"));
        assert!(marks.articles["renamed"].pub_date);
        assert!(marks.articles["renamed"].updated_date);
        assert!(marks.articles["other"].pub_date);

        // 削除
        test.delete("renamed").unwrap();
        let marks = dates::read_marks(&path).unwrap();
        assert!(!marks.articles.contains_key("renamed"));
        assert!(marks.articles["other"].pub_date);
    }
}
