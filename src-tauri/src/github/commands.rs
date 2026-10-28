// GitHub連携のコマンド。`async`で、通信やファイル操作の間はメイン画面が止まらない

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, State};

use super::auth::{self, DeviceCode, LoginOutcome};
use super::config::{self, GithubSettings, RepoRef};
use super::http::{Transport, UreqTransport, VERIFICATION_URL};
use super::repos;
use super::token::{self, TokenManager, TokenSet};
use super::{APP_SLUG, CLIENT_ID};
use crate::github::GithubError;
use crate::settings;
use crate::store::{Store, StoreError};

// GithubErrorとStoreErrorをそのまま画面へ返す。どちらもkind付きのJSONになる
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

// デバイスフローの待ち。device_codeは画面に渡さずここに持つ
struct LoginState {
    // 始め直すと替わる。待ちが終わったときに、自分の試行がまだ続いているかを確かめる
    attempt: u64,
    code: DeviceCode,
    started_at: i64,
    cancelled: Arc<AtomicBool>,
    waiting: bool,
}

#[derive(Default)]
struct Login {
    last_attempt: u64,
    state: Option<LoginState>,
}

pub struct GitHub {
    transport: Arc<dyn Transport>,
    manager: TokenManager,
    login: Mutex<Login>,
    // 設定に記事の保存先が無いときの、今までの保存先
    default_local_dir: PathBuf,
}

impl GitHub {
    pub fn new(data_dir: &Path, default_local_dir: PathBuf) -> Self {
        let transport: Arc<dyn Transport> = Arc::new(UreqTransport::new());
        let manager = TokenManager::new(
            config::token_path(data_dir),
            CLIENT_ID.to_string(),
            transport.clone(),
        );
        Self {
            transport,
            manager,
            login: Mutex::new(Login::default()),
            default_local_dir,
        }
    }

    fn login(&self) -> std::sync::MutexGuard<'_, Login> {
        self.login.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // token.jsonを消して今までの保存先へ戻す
    fn logout(&self, app: &AppHandle, store: &Store) -> Result<(), CommandError> {
        let _changes = settings::lock_changes();
        let _tokens = self.manager.lock();
        let settings = settings::current(&settings::file_path(app)?)?;
        settings::logout(store, &settings::data_dir(app)?, self.local_dir(&settings))?;
        Ok(())
    }

    // ログアウトすると戻る、今までの保存先。連携している間にも記事の保存先は変えられるので、その都度設定から求める
    fn local_dir(&self, settings: &settings::Settings) -> PathBuf {
        #[cfg(not(target_os = "android"))]
        if let Some(dir) = settings
            .posts_dir
            .as_ref()
            .filter(|dir| crate::store::check_posts_dir(dir).is_ok())
        {
            return dir.clone();
        }
        #[cfg(target_os = "android")]
        let _ = settings;
        self.default_local_dir.clone()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubStatus {
    pub logged_in: bool,
    // 通信できなければ、ログインしていてもNone
    pub login: Option<String>,
    pub repo: Option<RepoRef>,
    pub settings: GithubSettings,
    pub token_unreadable: bool,
    // ログインし直しが必要になり、token.jsonを消した。保存先を戻すのは画面が記事を保存してから(github_logout)
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
pub struct WaitLogin {
    // loggedIn | expired | denied | cancelled
    pub outcome: &'static str,
    pub login: Option<String>,
}

// 読むだけで、保存先は替えない
#[tauri::command(async)]
pub fn github_status(
    app: AppHandle,
    github: State<'_, GitHub>,
    store: State<'_, Store>,
) -> Result<GithubStatus, CommandError> {
    let settings = settings::current(&settings::file_path(&app)?)?;
    let data_dir = settings::data_dir(&app)?;
    let local_dir = github.local_dir(&settings);
    let mut status = GithubStatus {
        logged_in: false,
        login: None,
        repo: settings.github.repo.clone(),
        settings: settings.github,
        token_unreadable: false,
        login_expired: false,
    };
    match token::read(&config::token_path(&data_dir)) {
        Ok(Some(_)) => match github.manager.access_token(now()) {
            Ok(access) => match repos::current_user(github.transport.as_ref(), &access, now()) {
                Ok(login) => {
                    status.logged_in = true;
                    status.login = Some(login);
                }
                // 通信できなければ、ログインしたまま名前だけ出さない
                Err(GithubError::Network { .. } | GithubError::RateLimited { .. }) => {
                    status.logged_in = true;
                }
                // GitHub側で認可を取り消された
                Err(GithubError::LoginRequired) => {
                    let _tokens = github.manager.lock();
                    token::delete(&config::token_path(&data_dir))?;
                }
                Err(error) => {
                    eprintln!("Failed to read the GitHub account: {error}");
                    status.logged_in = true;
                }
            },
            Err(GithubError::LoginRequired) => {}
            Err(error) => {
                eprintln!("Failed to get a GitHub access token: {error}");
                status.logged_in = true;
            }
        },
        Ok(None) => {}
        Err(_) => status.token_unreadable = true,
    }
    // どのコマンドでtoken.jsonが消えても、向き先がGitHub用のまま残っていれば戻させる
    status.login_expired = !status.logged_in && store.posts_dir() != local_dir;
    Ok(status)
}

#[tauri::command(async)]
pub fn github_start_login(github: State<'_, GitHub>) -> Result<StartLogin, CommandError> {
    let code = auth::request_device_code(github.transport.as_ref(), CLIENT_ID)?;
    let mut login = github.login();
    // 前の試行の待ちは取り消す
    if let Some(previous) = &login.state {
        previous.cancelled.store(true, Ordering::SeqCst);
    }
    login.last_attempt += 1;
    login.state = Some(LoginState {
        attempt: login.last_attempt,
        code: code.clone(),
        started_at: now(),
        cancelled: Arc::new(AtomicBool::new(false)),
        waiting: false,
    });
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
    let cancelled_outcome = WaitLogin {
        outcome: "cancelled",
        login: None,
    };
    let (attempt, code, started_at, cancelled) = {
        let mut login = github.login();
        // startしていない呼び出しと、同じ試行を2つ目に待つ呼び出しは、取り消されたものとして終える
        let Some(state) = login.state.as_mut().filter(|state| !state.waiting) else {
            return Ok(cancelled_outcome);
        };
        state.waiting = true;
        (
            state.attempt,
            state.code.clone(),
            state.started_at,
            state.cancelled.clone(),
        )
    };
    // 承認されるまでコードの期限(15分)まで待つので、非同期の作業用のスレッドをふさがないよう、別のスレッドで待つ
    let transport = github.transport.clone();
    let waiting = cancelled.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        auth::poll_until_done(
            transport.as_ref(),
            CLIENT_ID,
            &code,
            started_at,
            &now,
            &sleep,
            &waiting,
        )
    })
    .await
    .map_err(|error| GithubError::InvalidResponse {
        reason: format!("the login wait stopped: {error}"),
    })?;
    // 終わった待ちを消す。始め直されていたら、新しい試行の待ちは残す
    {
        let mut login = github.login();
        if login
            .state
            .as_ref()
            .is_some_and(|state| state.attempt == attempt)
        {
            login.state = None;
        }
    }
    match outcome? {
        // 取れた後に取り消されていたら、保存しない
        LoginOutcome::Token(_) if cancelled.load(Ordering::SeqCst) => Ok(cancelled_outcome),
        LoginOutcome::Token(tokens) => {
            save_login(&app, &github, &store, &tokens)?;
            let login =
                repos::current_user(github.transport.as_ref(), &tokens.access_token, now()).ok();
            Ok(WaitLogin {
                outcome: "loggedIn",
                login,
            })
        }
        LoginOutcome::Expired => Ok(WaitLogin {
            outcome: "expired",
            login: None,
        }),
        LoginOutcome::Denied => Ok(WaitLogin {
            outcome: "denied",
            login: None,
        }),
        LoginOutcome::Cancelled => Ok(WaitLogin {
            outcome: "cancelled",
            login: None,
        }),
    }
}

// トークンを保存し、Repositoryを選んでいればその置き場所へ替える。替えられなければトークンも消し、ログイン前に戻す
fn save_login(
    app: &AppHandle,
    github: &GitHub,
    store: &Store,
    tokens: &TokenSet,
) -> Result<(), CommandError> {
    let _changes = settings::lock_changes();
    let _tokens = github.manager.lock();
    let data_dir = settings::data_dir(app)?;
    let path = config::token_path(&data_dir);
    token::write(&path, tokens)?;
    let switched = (|| {
        let file = settings::file_path(app)?;
        let settings = settings::current(&file)?;
        let target = config::choose_posts_dir(
            github.local_dir(&settings),
            &data_dir,
            &settings.github,
            true,
        );
        settings::switch_posts_dir(store, &file, &settings, target)
    })();
    if let Err(error) = switched {
        if let Err(error) = token::delete(&path) {
            eprintln!("Failed to remove the GitHub token after a failed login: {error}");
        }
        return Err(error.into());
    }
    Ok(())
}

#[tauri::command(async)]
pub fn github_cancel_login(github: State<'_, GitHub>) {
    if let Some(state) = &github.login().state {
        state.cancelled.store(true, Ordering::SeqCst);
    }
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
pub fn github_list_repositories(
    github: State<'_, GitHub>,
) -> Result<Vec<repos::Repository>, CommandError> {
    let token = github.manager.access_token(now())?;
    repos::list_repositories(github.transport.as_ref(), &token, now()).map_err(CommandError::Github)
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
    let _changes = settings::lock_changes();
    let file = settings::file_path(&app)?;
    let data_dir = settings::data_dir(&app)?;
    let mut settings = settings::current(&file)?;
    settings.github.repo = Some(repo);
    let target = config::choose_posts_dir(
        github.local_dir(&settings),
        &data_dir,
        &settings.github,
        config::linked(&data_dir, &settings.github),
    );
    settings::switch_posts_dir(&store, &file, &settings, target)?;
    Ok(())
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
    let _changes = settings::lock_changes();
    let file = settings::file_path(&app)?;
    let data_dir = settings::data_dir(&app)?;
    let mut settings = settings::current(&file)?;
    settings.github.base_branch = base_branch;
    settings.github.branch_format = branch_format;
    settings.github.posts_path = posts_path;
    // 記事のディレクトリが変わったら、向き先も新しい置き場所へ替える
    let target = config::choose_posts_dir(
        github.local_dir(&settings),
        &data_dir,
        &settings.github,
        config::linked(&data_dir, &settings.github),
    );
    settings::switch_posts_dir(&store, &file, &settings, target)?;
    Ok(())
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

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn sleep(seconds: u64) {
    std::thread::sleep(std::time::Duration::from_secs(seconds));
}
