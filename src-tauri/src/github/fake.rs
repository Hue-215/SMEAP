use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use super::http::{Request, Response, Transport};
use super::GithubError;

// テストで通信を置き換えるための偽のTransport。受けたリクエストを記録し、用意したレスポンスを順に返す
pub struct FakeTransport {
    responses: Mutex<VecDeque<Result<Response, GithubError>>>,
    requests: Mutex<Vec<Request>>,
}

impl FakeTransport {
    pub fn new(responses: Vec<Result<Response, GithubError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Transport for FakeTransport {
    fn send(&self, request: &Request) -> Result<Response, GithubError> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        self.responses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .expect("用意したレスポンスが足りない")
    }
}

// ディレクトリを書き込めなくし、落とすときに戻す
#[cfg(unix)]
pub struct ReadOnlyDir(std::path::PathBuf);

#[cfg(unix)]
impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}

// rootなど、権限を外しても書き込めてしまう環境ではNone(書き込みの失敗を試すテストはそこで終える)
#[cfg(unix)]
pub fn read_only_dir(dir: &std::path::Path) -> Option<ReadOnlyDir> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let guard = ReadOnlyDir(dir.to_path_buf());
    let probe = dir.join(".probe");
    if std::fs::write(&probe, "").is_ok() {
        let _ = std::fs::remove_file(probe);
        return None;
    }
    Some(guard)
}

pub fn json_response(status: u16, body: serde_json::Value) -> Response {
    Response {
        status,
        headers: Vec::new(),
        body: serde_json::to_vec(&body).expect("JSONの応答は書けるはず"),
    }
}
