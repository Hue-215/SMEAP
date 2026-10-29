pub mod auth;
pub mod commands;
pub mod config;
#[cfg(test)]
pub(crate) mod fake;
pub mod git;
pub mod http;
pub mod push;
pub mod remote;
pub mod repos;
pub mod state;
pub mod sync;
pub mod token;

use serde::Serialize;

// docs/github-app.mdで登録したAppの値。公開してよい
pub const CLIENT_ID: &str = "Iv23lioXdMt6XLfnVtBp";
pub const APP_SLUG: &str = "s-m-e-a-p";

// 画面で種類ごとに文言を選べるよう、StoreErrorと同じくkindに種類を入れてJSONにする
#[derive(Debug, Serialize, thiserror::Error)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum GithubError {
    // 401、bad_refresh_token、更新用トークンの期限切れ
    #[error("login required")]
    LoginRequired,
    // 再開できる時刻(unix秒)。分からなければNone
    #[error("rate limited")]
    RateLimited { retry_at: Option<i64> },
    #[error("unexpected status: {status}")]
    Status { status: u16 },
    #[error("redirect is not followed")]
    Redirect,
    #[error("URL is not allowed")]
    UrlNotAllowed,
    #[error("network error: {reason}")]
    Network { reason: String },
    #[error("invalid response: {reason}")]
    InvalidResponse { reason: String },
    #[error("invalid branch name format")]
    InvalidBranchFormat,
    #[error("invalid branch name")]
    InvalidBranchName,
    #[error("invalid posts path")]
    InvalidPostsPath,
    #[error("invalid repository")]
    InvalidRepository,
    #[error("io error: {path}: {reason}")]
    Io { path: String, reason: String },
    #[error("base branch not found")]
    BaseBranchNotFound,
    #[error("invalid layout: {path}")]
    InvalidLayout { path: String },
    #[error("the sync state is unreadable")]
    StateUnreadable,
    #[error("a sync is already running")]
    SyncBusy,
    #[error("the settings changed during the sync")]
    SettingsChanged,
    #[error("not linked to GitHub")]
    NotLinked,
    #[error("nothing to commit")]
    NothingToCommit,
    // headは、食い違いの確認で見たときのリモートの先頭(「Push」でoverwriteに入れるもの)
    #[error("the remote has conflicting changes")]
    Conflict {
        head: String,
        md: bool,
        images: Vec<String>,
    },
    #[error("the branch already exists")]
    BranchExists { branch: String },
    #[error("the branch is gone")]
    BranchGone,
    #[error("too many retries to push")]
    PushRetryExceeded,
    #[error("the article is not found on GitHub")]
    ArticleNotFound,
}

impl GithubError {
    // 応答の中身(トークンを含むことがある)をエラー文に入れない
    pub(crate) fn invalid_field(key: &str) -> Self {
        GithubError::InvalidResponse {
            reason: format!("{key} is missing or invalid"),
        }
    }

    pub(crate) fn io(path: &std::path::Path, error: impl std::fmt::Display) -> Self {
        GithubError::Io {
            path: path.display().to_string(),
            reason: error.to_string(),
        }
    }
}

// unix秒
pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
