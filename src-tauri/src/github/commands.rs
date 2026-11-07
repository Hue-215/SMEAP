// GitHub連携のコマンド。`async`で、通信やファイル操作の間はメイン画面が止まらない

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, State};

use super::auth::{self, DeviceCode, LoginOutcome};
use super::config::{self, GithubSettings, RepoRef};
use super::dates;
use super::http::{Transport, UreqTransport, VERIFICATION_URL};
use super::repos;
use super::state::Basis;
use super::sync::{self, SyncInput, SyncLock, SyncResult};
use super::token::{TokenManager, TokenSet, TokenStore};
use super::{now, APP_SLUG, CLIENT_ID};
use crate::github::GithubError;
use crate::settings::{self, Settings};
use crate::store::{EditedFields, SavedDates, Store, StoreError};

// GithubErrorとStoreErrorはkind付きのJSONで画面へ返す。Openerだけkindが無く、文字列になる
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    Github(#[from] GithubError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Opener(#[from] tauri_plugin_opener::Error),
}

impl Serialize for CommandError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Github(error) => error.serialize(serializer),
            Self::Store(error) => error.serialize(serializer),
            Self::Opener(error) => error.serialize(serializer),
        }
    }
}

// デバイスフローの試行。device_codeは画面に渡さずここに持つ
#[derive(Clone)]
struct Attempt {
    // 始め直すと替わる。待ちが終わったときに、自分の試行がまだ続いているかを確かめる
    number: u64,
    code: DeviceCode,
    started_at: i64,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
struct Login {
    last_attempt: u64,
    state: Option<LoginState>,
}

// 始めている試行と、待ちに入ったか
struct LoginState {
    attempt: Attempt,
    waiting: bool,
}

impl Login {
    // 新しい試行を始める。前の試行の待ちは取り消す
    fn start(&mut self, code: DeviceCode, started_at: i64) {
        if let Some(previous) = &self.state {
            previous.attempt.cancelled.store(true, Ordering::SeqCst);
        }
        self.last_attempt += 1;
        self.state = Some(LoginState {
            attempt: Attempt {
                number: self.last_attempt,
                code,
                started_at,
                cancelled: Arc::new(AtomicBool::new(false)),
            },
            waiting: false,
        });
    }

    // 待ちを始める。始めた試行が無い・もう待っているときはNone
    fn begin_wait(&mut self) -> Option<Attempt> {
        let state = self.state.as_mut().filter(|state| !state.waiting)?;
        state.waiting = true;
        Some(state.attempt.clone())
    }

    // 待ちを終える。始め直されていたら、新しい試行の状態は残す
    fn end_wait(&mut self, attempt: u64) {
        if self
            .state
            .as_ref()
            .is_some_and(|state| state.attempt.number == attempt)
        {
            self.state = None;
        }
    }

    fn cancel(&mut self) {
        if let Some(state) = &self.state {
            state.attempt.cancelled.store(true, Ordering::SeqCst);
        }
    }
}

pub struct GitHub {
    transport: Arc<dyn Transport>,
    manager: TokenManager,
    login: Mutex<Login>,
    sync_lock: SyncLock,
    // 設定に記事の保存先が無いときの、今までの保存先
    default_local_dir: PathBuf,
}

impl GitHub {
    pub fn new(tokens: TokenStore, default_local_dir: PathBuf) -> Self {
        let transport: Arc<dyn Transport> = Arc::new(UreqTransport::new());
        let manager = TokenManager::new(tokens, CLIENT_ID.to_string(), transport.clone());
        Self {
            transport,
            manager,
            login: Mutex::new(Login::default()),
            sync_lock: SyncLock::default(),
            default_local_dir,
        }
    }

    pub(crate) fn tokens(&self) -> &TokenStore {
        self.manager.tokens()
    }

    fn login(&self) -> std::sync::MutexGuard<'_, Login> {
        self.login.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // トークンを消して今までの保存先へ戻す
    fn logout(&self, app: &AppHandle, store: &Store) -> Result<(), CommandError> {
        let _changes = settings::lock_changes();
        let _tokens = self.manager.lock();
        let settings = settings::read_from(&settings::file_path(app)?)?;
        settings::logout(
            store,
            self.tokens(),
            &settings::data_dir(app)?,
            local_dir(&self.default_local_dir, &settings),
        )?;
        Ok(())
    }
}

// ログアウトすると戻る、今までの保存先
fn local_dir(default_local_dir: &Path, settings: &settings::Settings) -> PathBuf {
    #[cfg(not(target_os = "android"))]
    if let Some(dir) = settings
        .posts_dir
        .as_ref()
        .filter(|dir| crate::store::check_posts_dir(dir).is_ok())
    {
        return dir.clone();
    }
    #[cfg(target_os = "android")]
    let _ = (default_local_dir, settings);
    default_local_dir.to_path_buf()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubStatus {
    pub logged_in: bool,
    // 通信できなければ、ログインしていてもNone
    pub login: Option<String>,
    pub settings: GithubSettings,
    pub token_unreadable: bool,
    // ログインし直しが必要になり、トークンを消した。保存先を戻すのは画面が記事を保存してから(github_logout)
    pub login_expired: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartLogin {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LoginResult {
    LoggedIn,
    Expired,
    Denied,
    Cancelled,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitLogin {
    pub outcome: LoginResult,
    pub login: Option<String>,
}

// 通信の結果と今の向き先から、画面に出す状態を作る
fn status_of(
    settings: &GithubSettings,
    token_read: &Result<Option<TokenSet>, GithubError>,
    access: &Option<Result<String, GithubError>>,
    user: &Option<Result<String, GithubError>>,
    posts_dir: &Path,
    local_dir: &Path,
) -> GithubStatus {
    let mut status = GithubStatus {
        logged_in: false,
        login: None,
        settings: settings.clone(),
        token_unreadable: token_read.is_err(),
        login_expired: false,
    };
    // userを確かめるのはaccessが通ったとき。2つの通信の失敗は同じ扱いにする
    let (result, message) = match access {
        Some(Ok(_)) => (user, "Failed to read the GitHub account"),
        other => (other, "Failed to get a GitHub access token"),
    };
    match result {
        Some(Ok(login)) => {
            status.logged_in = true;
            status.login = Some(login.clone());
        }
        // 通信できなければ、ログインしたまま出す
        Some(Err(GithubError::Network { .. } | GithubError::RateLimited { .. })) => {
            status.logged_in = true;
        }
        // GitHub側で認可を取り消された。手元の組を消すのは呼び出し側が行う
        Some(Err(GithubError::LoginRequired)) => {}
        Some(Err(error)) => {
            eprintln!("{message}: {error}");
            status.logged_in = true;
        }
        None => {}
    }
    // どのコマンドでトークンが消えても、向き先がGitHub用のまま残っていれば戻させる
    status.login_expired = !status.logged_in && posts_dir != local_dir;
    status
}

// 保存先は替えない(トークンの更新・削除はある)
#[tauri::command(async)]
pub fn github_status(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
) -> Result<GithubStatus, CommandError> {
    let settings = settings::read_from(&settings::file_path(&app)?)?;
    let local = local_dir(&github.default_local_dir, &settings);
    let token_read = github.tokens().read();
    // トークンが読めたときだけ、その期限とアカウントを確かめる
    let (access, user) = match &token_read {
        Ok(Some(_)) => {
            let access = github.manager.access_token(now());
            let user = access
                .as_ref()
                .ok()
                .map(|access| repos::current_user(github.transport.as_ref(), access, now()));
            (Some(access), user)
        }
        _ => (None, None),
    };
    // GitHub側で認可を取り消された。その組がまだ手元にあれば消す
    if let (Some(Ok(access)), Some(Err(GithubError::LoginRequired))) = (&access, &user) {
        github.manager.invalidate(access)?;
    }
    Ok(status_of(
        &settings.github,
        &token_read,
        &access,
        &user,
        &store.posts_dir(),
        &local,
    ))
}

#[tauri::command(async)]
pub fn github_start_login(github: State<'_, GitHub>) -> Result<StartLogin, CommandError> {
    let code = auth::request_device_code(github.transport.as_ref(), CLIENT_ID)?;
    github.login().start(code.clone(), now());
    Ok(StartLogin {
        user_code: code.user_code,
        verification_uri: code.verification_uri,
        expires_in: code.expires_in,
    })
}

#[tauri::command]
pub async fn github_wait_login(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
) -> Result<WaitLogin, CommandError> {
    let cancelled = wait_login(LoginResult::Cancelled);
    // startしていない呼び出しと、同じ試行を2つ目に待つ呼び出しは、取り消されたものとして終える
    let Some(Attempt {
        number,
        code,
        started_at,
        cancelled: waiting,
    }) = github.login().begin_wait()
    else {
        return Ok(cancelled);
    };
    // 承認されるまでコードの期限(15分)まで待つので、非同期の作業用のスレッドをふさがないよう、別のスレッドで待つ
    let transport = github.transport.clone();
    let task = waiting.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        auth::poll_until_done(
            transport.as_ref(),
            CLIENT_ID,
            &code,
            started_at,
            &now,
            &sleep,
            &task,
        )
    })
    .await
    .map_err(|error| GithubError::InvalidResponse {
        reason: format!("the login wait stopped: {error}"),
    })?;
    github.login().end_wait(number);
    match outcome? {
        // 取れた後に取り消されていたら、保存しない
        LoginOutcome::Token(_) if waiting.load(Ordering::SeqCst) => Ok(cancelled),
        LoginOutcome::Token(tokens) => {
            let file = settings::file_path(&app)?;
            let data_dir = settings::data_dir(&app)?;
            save_login(
                &data_dir,
                &file,
                &store,
                &github.manager,
                &github.default_local_dir,
                &tokens,
            )?;
            let login =
                repos::current_user(github.transport.as_ref(), &tokens.access_token, now()).ok();
            Ok(WaitLogin {
                outcome: LoginResult::LoggedIn,
                login,
            })
        }
        LoginOutcome::Expired => Ok(wait_login(LoginResult::Expired)),
        LoginOutcome::Denied => Ok(wait_login(LoginResult::Denied)),
        LoginOutcome::Cancelled => Ok(cancelled),
    }
}

// トークンを取れずに終わったときの応答
fn wait_login(outcome: LoginResult) -> WaitLogin {
    WaitLogin {
        outcome,
        login: None,
    }
}

// トークンを保存し、Repositoryを選んでいればその置き場所へ替える。替えられなければトークンも消し、ログイン前に戻す
fn save_login(
    data_dir: &Path,
    file: &Path,
    store: &Store,
    manager: &TokenManager,
    default_local_dir: &Path,
    tokens: &TokenSet,
) -> Result<(), CommandError> {
    let _changes = settings::lock_changes();
    let _tokens = manager.lock();
    manager.tokens().write(tokens)?;
    let switched = (|| {
        let settings = settings::read_from(file)?;
        let target = config::choose_posts_dir(
            local_dir(default_local_dir, &settings),
            data_dir,
            &settings.github,
            true,
        );
        settings::switch_posts_dir(store, file, &settings, target)
    })();
    if let Err(error) = switched {
        if let Err(error) = manager.tokens().delete() {
            eprintln!("Failed to remove the GitHub token after a failed login: {error}");
        }
        return Err(error.into());
    }
    Ok(())
}

#[tauri::command(async)]
pub fn github_cancel_login(github: State<'_, GitHub>) {
    github.login().cancel();
}

#[tauri::command(async)]
pub fn github_logout(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
) -> Result<(), CommandError> {
    github.logout(&app, &store)
}

#[tauri::command(async)]
pub fn github_list_repositories(github: State<'_, GitHub>) -> Result<Vec<RepoRef>, CommandError> {
    let token = github.manager.access_token(now())?;
    repos::list_repositories(github.transport.as_ref(), &token, now()).map_err(CommandError::Github)
}

// 設定を読み直して書き替え、GitHubの設定に合わせて保存先を替える
fn change_github_settings(
    app: &AppHandle,
    github: &GitHub,
    store: &Store,
    change: impl FnOnce(&mut Settings),
) -> Result<(), CommandError> {
    let _changes = settings::lock_changes();
    let file = settings::file_path(app)?;
    let data_dir = settings::data_dir(app)?;
    let mut settings = settings::read_from(&file)?;
    change(&mut settings);
    let target = config::choose_posts_dir(
        local_dir(&github.default_local_dir, &settings),
        &data_dir,
        &settings.github,
        config::linked(github.tokens(), &settings.github),
    );
    settings::switch_posts_dir(store, &file, &settings, target)?;
    Ok(())
}

#[tauri::command(async)]
pub fn github_select_repository(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
    repo: RepoRef,
) -> Result<(), CommandError> {
    config::validate_repo_part(&repo.owner)?;
    config::validate_repo_part(&repo.name)?;
    config::validate_branch_name(&repo.default_branch)?;
    change_github_settings(&app, &github, &store, |settings| {
        // base_branchはrepositoryごとに決まるので、替えたら選び直させる
        if settings.github.repo.as_ref() != Some(&repo) {
            settings.github.base_branch = None;
        }
        settings.github.repo = Some(repo);
    })
}

#[tauri::command(async)]
pub fn github_update_settings(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
    base_branch: Option<String>,
    branch_format: String,
    posts_path: String,
) -> Result<(), CommandError> {
    config::validate_branch_format(&branch_format)?;
    config::validate_posts_path(&posts_path)?;
    let base_branch = base_branch.filter(|branch| !branch.is_empty());
    if let Some(branch) = &base_branch {
        config::validate_branch_name(branch)?;
    }
    // 記事のディレクトリが変わったら、向き先も新しい置き場所へ替える
    change_github_settings(&app, &github, &store, |settings| {
        settings.github.base_branch = base_branch;
        settings.github.branch_format = branch_format;
        settings.github.posts_path = posts_path;
    })
}

// 画面で入力中に案内を出すため(validate_slugと同じ形)
#[tauri::command(async)]
pub fn validate_branch_format(format: String) -> Result<(), GithubError> {
    config::validate_branch_format(&format)
}

#[tauri::command(async)]
pub fn validate_posts_path(path: String) -> Result<(), GithubError> {
    config::validate_posts_path(&path)
}

// deviceとinstallの2つ。URLは固定で、画面から受け取らない
#[tauri::command(async)]
pub fn open_github_page(app: AppHandle, page: String) -> Result<(), CommandError> {
    use tauri_plugin_opener::OpenerExt;

    let url = match page.as_str() {
        "device" => VERIFICATION_URL.to_string(),
        "install" => format!("https://github.com/apps/{APP_SLUG}/installations/new"),
        _ => {
            return Err(StoreError::InvalidRequest {
                reason: "unknown GitHub page".to_string(),
            }
            .into())
        }
    };
    app.opener().open_url(url, None::<&str>)?;
    Ok(())
}

// 連携していなければNone。世代は連携を確かめる前に読み、その後の切り替えをsync.rsで見つける
fn with_sync_input<T, E>(
    app: &AppHandle,
    github: &GitHub,
    store: &Store,
    protect: Option<&str>,
    run: impl FnOnce(&SyncInput<'_>) -> Result<T, E>,
) -> Result<Option<T>, CommandError>
where
    CommandError: From<E>,
{
    let generation = store.generation();
    let Some(linked) = linked(app, store, github)? else {
        return Ok(None);
    };
    let token = github.manager.access_token(now())?;
    let input = SyncInput {
        protect,
        ..input_of(&linked, github, &token, store, generation)
    };
    Ok(Some(run(&input)?))
}

#[tauri::command(async)]
pub fn github_sync(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
    protect: Option<String>,
) -> Result<SyncResult, CommandError> {
    if let Some(slug) = &protect {
        crate::store::validate_slug(slug)?;
    }
    with_sync_input(&app, &github, &store, protect.as_deref(), |input| {
        github.sync_lock.run(|| sync::sync(input))
    })?
    .ok_or_else(|| GithubError::NotLinked.into())
}

// 取れなかった画像の件数
#[tauri::command(async)]
pub fn github_fetch_images(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
    slug: String,
) -> Result<usize, CommandError> {
    crate::store::validate_slug(&slug)?;
    // 取る画像が無ければトークンを取らない(期限が切れていると更新の通信をするので、オフラインで失敗する)
    let Some(linked) = linked(&app, &store, &github)? else {
        return Ok(0);
    };
    if !sync::has_wanted_images(&linked.data_dir, &linked.repo, &linked.basis, &store, &slug)? {
        return Ok(0);
    }
    let failed = with_sync_input(&app, &github, &store, None, |input| {
        sync::fetch_images(input, &slug)
    })?;
    Ok(failed.unwrap_or(0))
}

#[tauri::command(async)]
pub fn github_commit_preview(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
    slug: String,
) -> Result<super::push::CommitPreview, CommandError> {
    crate::store::validate_slug(&slug)?;
    with_sync_input(&app, &github, &store, None, |input| {
        super::push::preview(input, &slug)
    })?
    .ok_or_else(|| GithubError::NotLinked.into())
}

// commitを作ってbranchを進める。overwriteは、Conflictで「Push」を選んだときのリモートの先頭
#[tauri::command(async)]
pub fn github_commit_push(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
    slug: String,
    message: String,
    overwrite: Option<String>,
    on_progress: tauri::ipc::Channel<super::push::PushProgress>,
) -> Result<super::push::PushResult, CommandError> {
    crate::store::validate_slug(&slug)?;
    with_sync_input(&app, &github, &store, None, |input| {
        github.sync_lock.run(|| {
            super::push::commit_push(input, &slug, &message, overwrite.as_deref(), &|progress| {
                // 画面が閉じているなどで送れなくてもPushは続ける
                let _ = on_progress.send(progress);
            })
        })
    })?
    .ok_or_else(|| GithubError::NotLinked.into())
}

// 手元の記事のファイルをbackupに写してから、GitHubの内容を読み込み直すための記録に替える
#[tauri::command(async)]
pub fn github_pull(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
    slug: String,
    branch: Option<String>,
) -> Result<(), CommandError> {
    crate::store::validate_slug(&slug)?;
    with_sync_input(&app, &github, &store, None, |input| {
        github.sync_lock.run(|| {
            let backup_name = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
            super::push::pull(input, &slug, branch.as_deref(), &backup_name)
        })
    })?
    .ok_or_else(|| GithubError::NotLinked.into())
}

#[tauri::command(async)]
pub fn github_article_states(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
) -> Result<Option<sync::ArticleStates>, CommandError> {
    let Some(linked) = linked(&app, &store, &github)? else {
        return Ok(None);
    };
    Ok(Some(sync::article_states(
        &linked.data_dir,
        &linked.repo,
        &linked.basis,
        &store,
    )))
}

#[tauri::command(async)]
pub fn github_article_state(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
    slug: String,
) -> Result<Option<sync::ArticleState>, CommandError> {
    let Some(linked) = linked(&app, &store, &github)? else {
        return Ok(None);
    };
    let state = sync::article_state(&linked.data_dir, &linked.repo, &linked.basis, &store, &slug)?;
    Ok(Some(state))
}

struct Linked {
    data_dir: PathBuf,
    repo: RepoRef,
    basis: Basis,
    posts_dir: PathBuf,
    updated_date_enabled: bool,
}

fn input_of<'a>(
    linked: &'a Linked,
    github: &'a GitHub,
    token: &'a str,
    store: &'a Store,
    generation: u64,
) -> SyncInput<'a> {
    SyncInput {
        transport: github.transport.as_ref(),
        token,
        now: now(),
        data_dir: &linked.data_dir,
        repo: &linked.repo,
        basis: &linked.basis,
        store,
        generation,
        posts_dir: &linked.posts_dir,
        protect: None,
    }
}

pub(crate) fn save_article(
    app: &AppHandle,
    store: &Store,
    github: &GitHub,
    slug: &str,
    fields: EditedFields,
) -> Result<SavedDates, CommandError> {
    // 設定を読めなくても、日付を自動で替えずに保存する
    let linked = linked(app, store, github).unwrap_or_else(|error| {
        eprintln!("Failed to check the GitHub link before saving {slug}: {error}");
        None
    });
    let Some(linked) = linked else {
        return Ok(store.save_article(slug, fields, None)?);
    };
    let input = dates::SaveInput {
        data_dir: &linked.data_dir,
        repo: &linked.repo,
        basis: &linked.basis,
        store,
        updated_date_enabled: linked.updated_date_enabled,
    };
    dates::save(&input, slug, fields, &crate::store::today())
}

pub(crate) fn create_article(
    app: &AppHandle,
    github: &GitHub,
    store: &Store,
    slug: &str,
) -> Result<crate::store::Article, CommandError> {
    match linked(app, store, github)? {
        Some(linked) => super::article::create(
            &linked.data_dir,
            &linked.repo,
            &linked.basis,
            store,
            &github.sync_lock,
            slug,
        ),
        None => Ok(store.create_article(slug)?),
    }
}

pub(crate) fn rename_article(
    app: &AppHandle,
    github: &GitHub,
    store: &Store,
    old: &str,
    new: &str,
) -> Result<crate::store::Article, CommandError> {
    match linked(app, store, github)? {
        Some(linked) => super::article::rename(
            &linked.data_dir,
            &linked.repo,
            &linked.basis,
            store,
            &github.sync_lock,
            old,
            new,
        ),
        None => Ok(store.rename_article(old, new)?),
    }
}

// 記事用のブランチが無ければ、トークンを取らない(期限切れの更新で通信し、オフラインで失敗するため)
pub(crate) fn delete_article(
    app: &AppHandle,
    github: &GitHub,
    store: &Store,
    slug: &str,
) -> Result<(), CommandError> {
    let generation = store.generation();
    let Some(linked) = linked(app, store, github)? else {
        store.delete_article(slug)?;
        return Ok(());
    };
    let state = super::state::read_state(
        &super::state::state_path(&linked.data_dir, &linked.repo),
        &linked.basis,
    )?;
    let token = if super::state::article_branch(&state, slug).is_some() {
        Some(github.manager.access_token(now())?)
    } else {
        None
    };
    let input = input_of(
        &linked,
        github,
        token.as_deref().unwrap_or_default(),
        store,
        generation,
    );
    super::article::delete(&input, &github.sync_lock, slug)
}

#[tauri::command(async)]
pub fn github_discard_edit(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
    slug: String,
) -> Result<(), CommandError> {
    crate::store::validate_slug(&slug)?;
    with_sync_input(&app, &github, &store, None, |input| {
        super::article::discard(input, &github.sync_lock, &slug)
    })?
    .ok_or_else(|| GithubError::NotLinked.into())
}

#[tauri::command(async)]
pub fn github_remove_published(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
    slug: String,
    kind: super::state::Deleting,
    on_progress: tauri::ipc::Channel<super::push::PushProgress>,
) -> Result<(), CommandError> {
    crate::store::validate_slug(&slug)?;
    with_sync_input(&app, &github, &store, None, |input| {
        super::article::remove_published(input, &github.sync_lock, &slug, kind, &|progress| {
            // 画面が閉じているなどで送れなくても処理は続ける
            let _ = on_progress.send(progress);
        })
    })?
    .ok_or_else(|| GithubError::NotLinked.into())
}

// Repositoryを選んでいて、トークンがあり、保存層の向き先がそのRepositoryの記事のディレクトリのとき
fn linked(app: &AppHandle, store: &Store, github: &GitHub) -> Result<Option<Linked>, CommandError> {
    let settings = settings::read_from(&settings::file_path(app)?)?;
    let data_dir = settings::data_dir(app)?;
    let (Some(repo), Some(basis)) = (settings.github.repo.clone(), Basis::of(&settings.github))
    else {
        return Ok(None);
    };
    let posts_dir = config::github_posts_dir(&data_dir, &repo, &basis.posts_path);
    if store.posts_dir() != posts_dir || !config::linked(github.tokens(), &settings.github) {
        return Ok(None);
    }
    Ok(Some(Linked {
        data_dir,
        repo,
        basis,
        posts_dir,
        updated_date_enabled: settings.frontmatter.updated_date,
    }))
}

fn sleep(seconds: u64) {
    std::thread::sleep(std::time::Duration::from_secs(seconds));
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    use tempfile::TempDir;

    use super::*;
    use crate::github::config::github_posts_dir;
    #[cfg(unix)]
    use crate::github::fake::read_only_dir;
    use crate::github::fake::{desktop_tokens, FakeTransport};
    use crate::store::Store;

    const NOW: i64 = 1_000_000;
    const CLIENT_ID: &str = "Iv1.client";

    fn repo() -> RepoRef {
        RepoRef {
            owner: "owner".to_string(),
            name: "blog".to_string(),
            default_branch: "main".to_string(),
        }
    }

    fn settings_with_repo() -> GithubSettings {
        GithubSettings {
            repo: Some(repo()),
            ..GithubSettings::default()
        }
    }

    fn tokens() -> TokenSet {
        TokenSet {
            access_token: "ACCESS".to_string(),
            expires_at: NOW + 100,
            refresh_token: "REFRESH".to_string(),
            refresh_token_expires_at: NOW + 10_000,
        }
    }

    fn device_code() -> DeviceCode {
        DeviceCode {
            device_code: "DEVICE_CODE".to_string(),
            user_code: "ABCD-1234".to_string(),
            verification_uri: "https://github.com/login/device".to_string(),
            interval: 5,
            expires_in: 900,
        }
    }

    // Repositoryを選んでいるsettings.json。posts_dirの設定は連携していないときの保存先
    fn settings_json() -> String {
        r#"{"postsDir":"/local/src/content/blog","github":{"repo":{"owner":"owner","name":"blog","defaultBranch":"main"}}}"#
            .to_string()
    }

    fn manager_of(data_dir: &Path) -> TokenManager {
        TokenManager::new(
            desktop_tokens(data_dir).0,
            CLIENT_ID.to_string(),
            Arc::new(FakeTransport::new(vec![])),
        )
    }

    // save_loginの tests

    #[test]
    fn save_loginはrepositoryの置き場所へ替える() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let file = tmp.path().join("settings.json");
        fs::write(&file, settings_json()).unwrap();
        let local = tmp.path().join("local/src/content/blog");
        fs::create_dir_all(&local).unwrap();
        let store = Store::new(local.clone(), tmp.path().join("backup")).unwrap();
        let manager = manager_of(&data_dir);
        let default_local_dir = tmp.path().join("default/src/content/blog");
        save_login(
            &data_dir,
            &file,
            &store,
            &manager,
            &default_local_dir,
            &tokens(),
        )
        .unwrap();
        assert_eq!(
            store.posts_dir(),
            github_posts_dir(&data_dir, &repo(), "src/content/blog")
        );
        assert_eq!(manager.tokens().read().unwrap(), Some(tokens()));
        let settings = settings::read_from(&file).unwrap();
        assert_eq!(settings.github.repo, Some(repo()));
    }

    #[cfg(unix)]
    #[test]
    fn save_loginの切り替えに失敗したらトークンと設定と向き先を替えない() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        let file = tmp.path().join("settings.json");
        fs::write(&file, settings_json()).unwrap();
        let local = tmp.path().join("local/src/content/blog");
        fs::create_dir_all(&local).unwrap();
        let store = Store::new(local.clone(), tmp.path().join("backup")).unwrap();
        let manager = manager_of(&data_dir);
        let default_local_dir = tmp.path().join("default/src/content/blog");
        let Some(read_only) = read_only_dir(tmp.path()) else {
            return;
        };
        let result = save_login(
            &data_dir,
            &file,
            &store,
            &manager,
            &default_local_dir,
            &tokens(),
        );
        drop(read_only);
        assert!(result.is_err());
        // トークンは消え、settings.jsonと向き先も替わらない
        assert_eq!(manager.tokens().read().unwrap(), None);
        assert_eq!(store.posts_dir(), local);
        let settings = settings::read_from(&file).unwrap();
        assert_eq!(settings.github.repo, Some(repo()));
        assert_eq!(
            settings.posts_dir,
            Some(PathBuf::from("/local/src/content/blog"))
        );
    }

    // status_ofの tests

    #[test]
    fn status_ofは通信の失敗でもログイン中にする() {
        let settings = settings_with_repo();
        let github_dir = github_posts_dir(Path::new("/data"), &repo(), "src/content/blog");
        // アカウントの確かめの通信の失敗
        let status = status_of(
            &settings,
            &Ok(Some(tokens())),
            &Some(Ok("ACCESS".to_string())),
            &Some(Err(GithubError::Network {
                reason: "closed".to_string(),
            })),
            &github_dir,
            Path::new("/local/src/content/blog"),
        );
        assert!(status.logged_in);
        assert_eq!(status.login, None);
        assert_eq!(status.settings, settings);
        // レート制限も
        let status = status_of(
            &settings,
            &Ok(Some(tokens())),
            &Some(Ok("ACCESS".to_string())),
            &Some(Err(GithubError::RateLimited { retry_at: None })),
            &github_dir,
            Path::new("/local/src/content/blog"),
        );
        assert!(status.logged_in);
        // トークンの取り直しの通信の失敗も
        let status = status_of(
            &settings,
            &Ok(Some(tokens())),
            &Some(Err(GithubError::Network {
                reason: "closed".to_string(),
            })),
            &None,
            &github_dir,
            Path::new("/local/src/content/blog"),
        );
        assert!(status.logged_in);
        assert!(!status.login_expired);
    }

    #[test]
    fn status_ofはcurrent_userがログインし直しを返したらログインしていない() {
        let status = status_of(
            &settings_with_repo(),
            &Ok(Some(tokens())),
            &Some(Ok("ACCESS".to_string())),
            &Some(Err(GithubError::LoginRequired)),
            &github_posts_dir(Path::new("/data"), &repo(), "src/content/blog"),
            Path::new("/local/src/content/blog"),
        );
        assert!(!status.logged_in);
        // トークンを消した後の向き先なので、画面に戻させる
        assert!(status.login_expired);
    }

    #[test]
    fn status_ofはトークンが無くGitHub用の向き先のままならloginExpiredにする() {
        let status = status_of(
            &settings_with_repo(),
            &Ok(None),
            &None,
            &None,
            &github_posts_dir(Path::new("/data"), &repo(), "src/content/blog"),
            Path::new("/local/src/content/blog"),
        );
        assert!(!status.logged_in);
        assert!(status.login_expired);
    }

    #[test]
    fn status_ofはトークンが無く今までの保存先ならloginExpiredにしない() {
        let status = status_of(
            &settings_with_repo(),
            &Ok(None),
            &None,
            &None,
            Path::new("/local/src/content/blog"),
            Path::new("/local/src/content/blog"),
        );
        assert!(!status.logged_in);
        assert!(!status.login_expired);
    }

    #[test]
    fn status_ofはトークンが読めなければtokenUnreadableにする() {
        let status = status_of(
            &settings_with_repo(),
            &Err(GithubError::Io {
                path: "/data/github/token.json".to_string(),
                reason: "not a valid token file".to_string(),
            }),
            &None,
            &None,
            Path::new("/local/src/content/blog"),
            Path::new("/local/src/content/blog"),
        );
        assert!(status.token_unreadable);
        assert!(!status.logged_in);
    }

    // Loginの tests

    #[test]
    fn 始め直すと前の試行の取り消しが立つ() {
        let mut login = Login::default();
        login.start(device_code(), NOW);
        let waiting = login.begin_wait().unwrap();
        login.start(device_code(), NOW + 1);
        assert!(waiting.cancelled.load(Ordering::SeqCst));
        // 新しい試行は取り消されていない
        let next = login.begin_wait().unwrap();
        assert!(!next.cancelled.load(Ordering::SeqCst));
    }

    #[test]
    fn 同じ試行を2つ目に待とうとすると断る() {
        let mut login = Login::default();
        login.start(device_code(), NOW);
        assert!(login.begin_wait().is_some());
        assert!(login.begin_wait().is_none());
        // 始めていないときも断る
        assert!(Login::default().begin_wait().is_none());
    }

    #[test]
    fn 古い試行を終えても新しい試行の状態は残す() {
        let mut login = Login::default();
        login.start(device_code(), NOW);
        let waiting = login.begin_wait().unwrap();
        login.start(device_code(), NOW + 1);
        login.end_wait(waiting.number);
        assert!(login.begin_wait().is_some());
        // 自分の試行が終わったら消える
        login.end_wait(login.last_attempt);
        assert!(login.begin_wait().is_none());
    }

    #[test]
    fn トークンを取れた後に取り消されていたら保存しない扱いになる() {
        let mut login = Login::default();
        login.start(device_code(), NOW);
        let waiting = login.begin_wait().unwrap();
        login.cancel();
        assert!(waiting.cancelled.load(Ordering::SeqCst));
    }
}
