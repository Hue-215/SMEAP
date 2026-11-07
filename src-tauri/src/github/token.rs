use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use serde::{Deserialize, Serialize};

use super::auth;
use super::http::Transport;
use super::GithubError;

// 期限の60秒前から切れた扱いにする
const EXPIRY_MARGIN: i64 = 60;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenSet {
    pub access_token: String,
    pub expires_at: i64,
    pub refresh_token: String,
    pub refresh_token_expires_at: i64,
}

// Debugは手で実装し、トークンを伏せる
impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("expires_at", &self.expires_at)
            .field("refresh_token_expires_at", &self.refresh_token_expires_at)
            .finish()
    }
}

// OSの資格情報の保存先にある、トークンの組の項目。テストではメモリ上の偽物にする
pub(crate) trait SecretStore: Send + Sync {
    fn get(&self) -> Result<Option<String>, String>;
    fn set(&self, value: &str) -> Result<(), String>;
    // 無ければ何もしない
    fn delete(&self) -> Result<(), String>;
}

// デスクトップではinitで資格情報の保存先にする。Androidとinitを呼ばないテストはtoken.jsonを使う
static SECRET: OnceLock<Box<dyn SecretStore>> = OnceLock::new();

// 起動時に、コマンドを受け付ける前に呼ぶ。token.jsonがあれば資格情報の保存先へ移す
#[cfg(not(target_os = "android"))]
pub fn init(identifier: &str, path: &Path) {
    let secret = SECRET.get_or_init(|| Box::new(keyring_store::Keyring::new(identifier)));
    if let Err(error) = migrate(secret.as_ref(), path) {
        eprintln!("Failed to move the GitHub token to the credential store: {error}");
    }
}

pub fn read(path: &Path) -> Result<Option<TokenSet>, GithubError> {
    match SECRET.get() {
        Some(secret) => {
            ensure_moved(path)?;
            read_secret(secret.as_ref())
        }
        None => read_file(path),
    }
}

// 起動時に移せなかったtoken.jsonが残っていれば、ログアウトしたように見せずにエラーで知らせる
fn ensure_moved(path: &Path) -> Result<(), GithubError> {
    if path
        .try_exists()
        .map_err(|error| GithubError::io(path, error))?
    {
        return Err(GithubError::io(path, "not moved to the credential store"));
    }
    Ok(())
}

pub fn write(path: &Path, tokens: &TokenSet) -> Result<(), GithubError> {
    match SECRET.get() {
        Some(secret) => write_secret(secret.as_ref(), path, tokens),
        None => write_file(path, tokens),
    }
}

pub fn delete(path: &Path) -> Result<(), GithubError> {
    match SECRET.get() {
        Some(secret) => delete_secret(secret.as_ref(), path),
        None => delete_file(path),
    }
}

fn secret_error(reason: String) -> GithubError {
    GithubError::CredentialStore { reason }
}

fn parse(content: &str, path: &Path) -> Result<TokenSet, GithubError> {
    serde_json::from_str(content).map_err(|_| GithubError::io(path, "not a valid token file"))
}

fn read_secret(secret: &dyn SecretStore) -> Result<Option<TokenSet>, GithubError> {
    secret
        .get()
        .map_err(secret_error)?
        .map(|content| {
            serde_json::from_str(&content)
                .map_err(|_| secret_error("not a valid token set".to_string()))
        })
        .transpose()
}

// token.jsonが残っていると次の起動で古い組が移ってくるので、先に消し、消せなければ書かずにエラーにする
fn write_secret(
    secret: &dyn SecretStore,
    path: &Path,
    tokens: &TokenSet,
) -> Result<(), GithubError> {
    delete_file(path)?;
    set_secret(secret, tokens)
}

fn set_secret(secret: &dyn SecretStore, tokens: &TokenSet) -> Result<(), GithubError> {
    let json = serde_json::to_string(tokens).expect("TokenSet should serialize");
    secret.set(&json).map_err(secret_error)
}

// token.jsonを先に消す。保存先だけ消えてtoken.jsonが残ると、次の起動で消したはずの組が移ってくるため
fn delete_secret(secret: &dyn SecretStore, path: &Path) -> Result<(), GithubError> {
    delete_file(path)?;
    secret.delete().map_err(secret_error)
}

// 保存先に組があっても上書きする。token.jsonが残っている間は保存先を書き換えないので、token.jsonの方が新しいか同じ
#[cfg_attr(target_os = "android", allow(dead_code))]
fn migrate(secret: &dyn SecretStore, path: &Path) -> Result<(), GithubError> {
    let Some(tokens) = read_file(path)? else {
        return Ok(());
    };
    // 書けなければtoken.jsonを残す
    set_secret(secret, &tokens)?;
    delete_file(path)
}

fn read_file(path: &Path) -> Result<Option<TokenSet>, GithubError> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(GithubError::io(path, error)),
    };
    parse(&content, path).map(Some)
}

fn write_file(path: &Path, tokens: &TokenSet) -> Result<(), GithubError> {
    let dir = path
        .parent()
        .ok_or_else(|| GithubError::io(path, "no parent directory"))?;
    fs::create_dir_all(dir).map_err(|error| GithubError::io(dir, error))?;
    let json = serde_json::to_vec(tokens).expect("TokenSet should serialize");
    // write_tempを使わず、NamedTempFile(unixでは0600)で書いて置き換える
    let mut tmp =
        tempfile::NamedTempFile::new_in(dir).map_err(|error| GithubError::io(dir, error))?;
    tmp.write_all(&json)
        .map_err(|error| GithubError::io(tmp.path(), error))?;
    tmp.as_file()
        .sync_all()
        .map_err(|error| GithubError::io(tmp.path(), error))?;
    tmp.persist(path)
        .map_err(|error| GithubError::io(path, error.error))?;
    Ok(())
}

fn delete_file(path: &Path) -> Result<(), GithubError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GithubError::io(path, error)),
    }
}

#[cfg(not(target_os = "android"))]
mod keyring_store {
    use super::SecretStore;

    pub(super) struct Keyring {
        service: String,
    }

    impl Keyring {
        pub(super) fn new(identifier: &str) -> Self {
            Self {
                service: identifier.to_string(),
            }
        }

        // 保存先を初期化できなかった理由は、Entry::newのNoDefaultStoreではなくstore_statusにだけ残る
        fn entry(&self) -> Result<keyring::Entry, String> {
            keyring::Entry::new(&self.service, "github").map_err(|error| {
                match (error, keyring::Entry::store_status()) {
                    (keyring::Error::NoDefaultStore, Err(cause)) => {
                        format!("the credential store is unavailable: {cause}")
                    }
                    (error, _) => error.to_string(),
                }
            })
        }
    }

    // keyringのエラーの表示はトークンを含まない
    impl SecretStore for Keyring {
        fn get(&self) -> Result<Option<String>, String> {
            got(self.entry()?.get_password())
        }

        fn set(&self, value: &str) -> Result<(), String> {
            self.entry()?
                .set_password(value)
                .map_err(|error| error.to_string())
        }

        fn delete(&self) -> Result<(), String> {
            deleted(self.entry()?.delete_credential())
        }
    }

    pub(super) fn got(result: keyring::Result<String>) -> Result<Option<String>, String> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(super) fn deleted(result: keyring::Result<()>) -> Result<(), String> {
        match result {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

pub struct TokenManager {
    path: PathBuf,
    client_id: String,
    transport: Arc<dyn Transport>,
    // 更新(通信と保存)は1つずつ。2つのスレッドが同時に期限切れを見ても、通信は1回にする。
    // ログイン・ログアウトの書き込みもこれを取り、取り直しが消したばかりのトークンを書き戻さないようにする
    refresh_lock: Mutex<()>,
}

impl TokenManager {
    pub fn new(path: PathBuf, client_id: String, transport: Arc<dyn Transport>) -> Self {
        Self {
            path,
            client_id,
            transport,
            refresh_lock: Mutex::new(()),
        }
    }

    // 期限内ならそのまま、切れていれば取り直して保存してから返す
    pub fn access_token(&self, now: i64) -> Result<String, GithubError> {
        // 読みと消す・書き替える判断は、ほかの書き込みと重ねないため最初にロックを取る
        let _guard = self.lock();
        let tokens = self.tokens(now)?;
        if tokens.expires_at > now + EXPIRY_MARGIN {
            return Ok(tokens.access_token);
        }
        match auth::refresh(
            self.transport.as_ref(),
            &self.client_id,
            &tokens.refresh_token,
            now,
        ) {
            Ok(tokens) => {
                // 古い組を捨てるのは、新しい組を書き終えてから
                write(&self.path, &tokens)?;
                Ok(tokens.access_token)
            }
            // トークンを消すのは、ログインし直しが必要なとき。通信の失敗や5xxでは残し、次にもう一度取り直す
            Err(GithubError::LoginRequired) => {
                delete(&self.path)?;
                Err(GithubError::LoginRequired)
            }
            Err(error) => Err(error),
        }
    }

    // ロックの中で呼ぶ。更新用のトークンが切れていたら、読んだ組を消してログインし直し
    fn tokens(&self, now: i64) -> Result<TokenSet, GithubError> {
        let tokens = read(&self.path)?.ok_or(GithubError::LoginRequired)?;
        if tokens.refresh_token_expires_at <= now + EXPIRY_MARGIN {
            delete(&self.path)?;
            return Err(GithubError::LoginRequired);
        }
        Ok(tokens)
    }

    // GitHub側で認可を取り消されたときに消す。読み直して、そのaccess_tokenが引数と同じ組のときだけ消す
    pub fn invalidate(&self, access_token: &str) -> Result<(), GithubError> {
        let _guard = self.lock();
        let tokens = read(&self.path)?;
        if tokens.is_some_and(|tokens| tokens.access_token == access_token) {
            delete(&self.path)?;
        }
        Ok(())
    }

    // ログイン・ログアウトでトークンを書き換える間に取る。取っている間はaccess_tokenを呼ばない(同じロックを取る)
    pub(crate) fn lock(&self) -> MutexGuard<'_, ()> {
        self.refresh_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    #[cfg(unix)]
    use crate::github::fake::read_only_dir;
    use crate::github::fake::{json_response, FakeTransport};

    const NOW: i64 = 1_000_000;
    const CLIENT_ID: &str = "Iv1.client";

    fn tokens(access_expires_at: i64, refresh_expires_at: i64) -> TokenSet {
        TokenSet {
            access_token: "ACCESS".to_string(),
            expires_at: access_expires_at,
            refresh_token: "REFRESH".to_string(),
            refresh_token_expires_at: refresh_expires_at,
        }
    }

    fn file_of(tmp: &TempDir) -> PathBuf {
        tmp.path().join("github/token.json")
    }

    fn refreshed_json() -> serde_json::Value {
        json!({
            "access_token": "NEW_ACCESS",
            "expires_in": 28800,
            "refresh_token": "NEW_REFRESH",
            "refresh_token_expires_in": 15552000
        })
    }

    fn refreshed_tokens() -> TokenSet {
        TokenSet {
            access_token: "NEW_ACCESS".to_string(),
            expires_at: NOW + 28800,
            refresh_token: "NEW_REFRESH".to_string(),
            refresh_token_expires_at: NOW + 15552000,
        }
    }

    #[test]
    fn 書いて読み直すと同じ組になる() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        let before = tokens(100, 200);
        write(&path, &before).unwrap();
        assert_eq!(read(&path).unwrap(), Some(before));
    }

    #[test]
    fn 無ければログインしていない() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(read(&file_of(&tmp)).unwrap(), None);
    }

    #[test]
    fn 読めないjsonはログインしていないにせずエラー() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "@").unwrap();
        assert!(read(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn 作ったときも上書きした後も権限は0600() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        write(&path, &tokens(100, 200)).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        write(&path, &tokens(300, 400)).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn 書き込みに失敗しても前の内容がそのまま読める() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("github");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("token.json");
        let before = tokens(100, 200);
        write(&path, &before).unwrap();
        let Some(read_only) = read_only_dir(&dir) else {
            return;
        };
        let result = write(&path, &tokens(300, 400));
        drop(read_only);
        assert!(result.is_err());
        assert_eq!(read(&path).unwrap(), Some(before));
    }

    #[test]
    fn 消すとログインしていないに戻る() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        write(&path, &tokens(100, 200)).unwrap();
        delete(&path).unwrap();
        assert_eq!(read(&path).unwrap(), None);
        // 無いときに消してもエラーにしない
        delete(&path).unwrap();
    }

    #[test]
    fn invalidateは同じaccess_tokenのときだけ消す() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        write(&path, &tokens(100, 200)).unwrap();
        let transport = Arc::new(FakeTransport::new(vec![]));
        let manager = TokenManager::new(path.clone(), CLIENT_ID.to_string(), transport);
        // 別の組が書かれていたら消さない
        manager.invalidate("OTHER").unwrap();
        assert_eq!(read(&path).unwrap(), Some(tokens(100, 200)));
        manager.invalidate("ACCESS").unwrap();
        assert_eq!(read(&path).unwrap(), None);
    }

    #[test]
    fn トークンの組のDebugにトークンが出ない() {
        let debug = format!("{:?}", tokens(100, 200));
        assert!(!debug.contains("ACCESS"));
        assert!(!debug.contains("REFRESH"));
    }

    #[test]
    fn 期限内なら通信せずに今のトークンを返す() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        write(&path, &tokens(NOW + 100, NOW + 10_000)).unwrap();
        let transport = Arc::new(FakeTransport::new(vec![]));
        let manager = TokenManager::new(path, CLIENT_ID.to_string(), transport.clone());
        assert_eq!(manager.access_token(NOW).unwrap(), "ACCESS");
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn 期限が切れていれば取り直して保存してから返す() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        // 期限の1秒前も切れた扱いにする
        write(&path, &tokens(NOW + 1, NOW + 10_000)).unwrap();
        let transport = Arc::new(FakeTransport::new(vec![Ok(json_response(
            200,
            refreshed_json(),
        ))]));
        let manager = TokenManager::new(path.clone(), CLIENT_ID.to_string(), transport.clone());
        assert_eq!(manager.access_token(NOW).unwrap(), "NEW_ACCESS");
        assert_eq!(transport.requests().len(), 1);
        // token.jsonも新しい組になっている
        assert_eq!(read(&path).unwrap(), Some(refreshed_tokens()));
    }

    #[cfg(unix)]
    #[test]
    fn 取り直しの保存に失敗したら新しいトークンを返さない() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("github");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("token.json");
        write(&path, &tokens(NOW + 1, NOW + 10_000)).unwrap();
        let transport = Arc::new(FakeTransport::new(vec![Ok(json_response(
            200,
            refreshed_json(),
        ))]));
        let Some(read_only) = read_only_dir(&dir) else {
            return;
        };
        let manager = TokenManager::new(path.clone(), CLIENT_ID.to_string(), transport);
        let result = manager.access_token(NOW);
        drop(read_only);
        // 通信は通っているので、ログインし直しではない
        let error = result.unwrap_err();
        assert!(!matches!(error, GithubError::LoginRequired));
        // 前の組はそのまま
        assert_eq!(read(&path).unwrap(), Some(tokens(NOW + 1, NOW + 10_000)));
    }

    #[test]
    fn 更新用のトークンも切れていたら通信せずにtoken_jsonを消してログインし直し() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        write(&path, &tokens(NOW + 100, NOW)).unwrap();
        let transport = Arc::new(FakeTransport::new(vec![]));
        let manager = TokenManager::new(path.clone(), CLIENT_ID.to_string(), transport);
        let error = manager.access_token(NOW).unwrap_err();
        assert!(matches!(error, GithubError::LoginRequired));
        assert!(!path.exists());
    }

    #[test]
    fn 取り直しがbad_refresh_tokenならtoken_jsonを消してログインし直し() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        write(&path, &tokens(NOW + 1, NOW + 10_000)).unwrap();
        let transport = Arc::new(FakeTransport::new(vec![Ok(json_response(
            200,
            json!({ "error": "bad_refresh_token" }),
        ))]));
        let manager = TokenManager::new(path.clone(), CLIENT_ID.to_string(), transport);
        let error = manager.access_token(NOW).unwrap_err();
        assert!(matches!(error, GithubError::LoginRequired));
        assert!(!path.exists());
    }

    #[test]
    fn 取り直しの通信の失敗と5xxではtoken_jsonを消さずにエラー() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        let before = tokens(NOW + 1, NOW + 10_000);
        write(&path, &before).unwrap();
        let transport = Arc::new(FakeTransport::new(vec![
            Err(GithubError::Network {
                reason: "closed".to_string(),
            }),
            Ok(json_response(500, json!({}))),
        ]));
        let manager = TokenManager::new(path.clone(), CLIENT_ID.to_string(), transport);
        // 消さないので、次の操作でもう一度取り直す
        for _ in 0..2 {
            let error = manager.access_token(NOW).unwrap_err();
            assert!(!matches!(error, GithubError::LoginRequired));
        }
        assert_eq!(read(&path).unwrap(), Some(before));
    }

    // 取り直しの応答を遅らせ、その間にもう1つのスレッドが期限切れのトークンを読む機会を作る
    struct SlowTransport(FakeTransport);

    impl Transport for SlowTransport {
        fn send(
            &self,
            request: &crate::github::http::Request,
        ) -> Result<crate::github::http::Response, GithubError> {
            std::thread::sleep(std::time::Duration::from_millis(200));
            self.0.send(request)
        }
    }

    #[test]
    fn 同時の取り直しは1回だけ() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        write(&path, &tokens(NOW + 1, NOW + 10_000)).unwrap();
        let transport = Arc::new(SlowTransport(FakeTransport::new(vec![Ok(json_response(
            200,
            refreshed_json(),
        ))])));
        let manager = Arc::new(TokenManager::new(
            path,
            CLIENT_ID.to_string(),
            transport.clone(),
        ));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let manager = manager.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    manager.access_token(NOW)
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap().unwrap(), "NEW_ACCESS");
        }
        assert_eq!(transport.0.requests().len(), 1);
    }

    // 資格情報の保存先の偽物。fail_*を立てた操作は失敗する
    #[derive(Default)]
    struct FakeSecret {
        value: Mutex<Option<String>>,
        fail_get: bool,
        fail_set: bool,
        fail_delete: bool,
    }

    impl FakeSecret {
        fn holding(tokens: &TokenSet) -> Self {
            Self {
                value: Mutex::new(Some(serde_json::to_string(tokens).unwrap())),
                ..Self::default()
            }
        }

        fn value(&self) -> Option<String> {
            self.value.lock().unwrap().clone()
        }
    }

    impl SecretStore for FakeSecret {
        fn get(&self) -> Result<Option<String>, String> {
            if self.fail_get {
                return Err("get failed".to_string());
            }
            Ok(self.value())
        }

        fn set(&self, value: &str) -> Result<(), String> {
            if self.fail_set {
                return Err("set failed".to_string());
            }
            *self.value.lock().unwrap() = Some(value.to_string());
            Ok(())
        }

        fn delete(&self) -> Result<(), String> {
            if self.fail_delete {
                return Err("delete failed".to_string());
            }
            *self.value.lock().unwrap() = None;
            Ok(())
        }
    }

    // ファイルの代わりにディレクトリを置き、token.jsonを消せなくする(どのOSでもremove_fileが失敗する)
    fn undeletable_file(tmp: &TempDir) -> PathBuf {
        let path = file_of(tmp);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn 資格情報の保存先に書いた組が読めtoken_jsonはできない() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        let secret = FakeSecret::default();
        write_secret(&secret, &path, &tokens(NOW + 3600, NOW + 7200)).unwrap();
        assert_eq!(
            read_secret(&secret).unwrap(),
            Some(tokens(NOW + 3600, NOW + 7200))
        );
        assert!(!path.exists());
    }

    #[test]
    fn 資格情報の保存先に無ければNoneで読めなければエラー() {
        let secret = FakeSecret::default();
        assert_eq!(read_secret(&secret).unwrap(), None);
        let broken = FakeSecret {
            value: Mutex::new(Some("@".to_string())),
            ..FakeSecret::default()
        };
        assert!(read_secret(&broken).is_err());
    }

    #[test]
    fn 資格情報の保存先に書くと残っていたtoken_jsonを消す() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        write_file(&path, &tokens(NOW, NOW)).unwrap();
        let secret = FakeSecret::default();
        write_secret(&secret, &path, &tokens(NOW + 3600, NOW + 7200)).unwrap();
        assert!(!path.exists());

        // 消せなければ、保存先に書かずにエラー
        let tmp = TempDir::new().unwrap();
        let path = undeletable_file(&tmp);
        let secret = FakeSecret::default();
        assert!(write_secret(&secret, &path, &tokens(NOW + 3600, NOW + 7200)).is_err());
        assert_eq!(secret.value(), None);
    }

    #[test]
    fn 資格情報の保存先から消すとtoken_jsonも消す() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        write_file(&path, &tokens(NOW, NOW)).unwrap();
        let secret = FakeSecret::holding(&tokens(NOW + 3600, NOW + 7200));
        delete_secret(&secret, &path).unwrap();
        assert_eq!(secret.value(), None);
        assert!(!path.exists());
        // どちらも無くても失敗しない
        delete_secret(&secret, &path).unwrap();
    }

    #[test]
    fn token_jsonを消せなければ資格情報の保存先は消さずにエラー() {
        let tmp = TempDir::new().unwrap();
        let path = undeletable_file(&tmp);
        let secret = FakeSecret::holding(&tokens(NOW + 3600, NOW + 7200));
        assert!(delete_secret(&secret, &path).is_err());
        assert!(secret.value().is_some());
    }

    #[test]
    fn 資格情報の保存先の失敗は専用のエラーにする() {
        let failing = FakeSecret {
            fail_get: true,
            ..FakeSecret::default()
        };
        assert!(matches!(
            read_secret(&failing),
            Err(GithubError::CredentialStore { .. })
        ));
        let broken = FakeSecret {
            value: Mutex::new(Some("@".to_string())),
            ..FakeSecret::default()
        };
        assert!(matches!(
            read_secret(&broken),
            Err(GithubError::CredentialStore { .. })
        ));
    }

    #[test]
    fn 資格情報の保存先の失敗はエラーにしてtoken_jsonを作らない() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        let failing = FakeSecret {
            fail_get: true,
            fail_set: true,
            fail_delete: true,
            ..FakeSecret::default()
        };
        assert!(read_secret(&failing).is_err());
        assert!(write_secret(&failing, &path, &tokens(NOW + 3600, NOW + 7200)).is_err());
        assert!(delete_secret(&failing, &path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn token_jsonを資格情報の保存先へ移す() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        write_file(&path, &tokens(NOW + 3600, NOW + 7200)).unwrap();
        // 保存先に古い組があっても、token.jsonの組で上書きする
        let secret = FakeSecret::holding(&tokens(NOW, NOW));
        migrate(&secret, &path).unwrap();
        assert_eq!(
            read_secret(&secret).unwrap(),
            Some(tokens(NOW + 3600, NOW + 7200))
        );
        assert!(!path.exists());
    }

    #[test]
    fn 壊れたtoken_jsonは移さずに残す() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "@").unwrap();
        let secret = FakeSecret::holding(&tokens(NOW, NOW));
        assert!(migrate(&secret, &path).is_err());
        assert!(path.exists());
        assert_eq!(read_secret(&secret).unwrap(), Some(tokens(NOW, NOW)));
    }

    #[test]
    fn 移せなかったtoken_jsonが残っていればエラー() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        ensure_moved(&path).unwrap();
        write_file(&path, &tokens(NOW, NOW)).unwrap();
        assert!(matches!(ensure_moved(&path), Err(GithubError::Io { .. })));
    }

    #[test]
    fn token_jsonが無ければ移さない() {
        let tmp = TempDir::new().unwrap();
        let secret = FakeSecret::holding(&tokens(NOW, NOW));
        migrate(&secret, &file_of(&tmp)).unwrap();
        assert_eq!(read_secret(&secret).unwrap(), Some(tokens(NOW, NOW)));
    }

    #[test]
    fn 資格情報の保存先に書けなければtoken_jsonを残す() {
        let tmp = TempDir::new().unwrap();
        let path = file_of(&tmp);
        write_file(&path, &tokens(NOW + 3600, NOW + 7200)).unwrap();
        let secret = FakeSecret {
            fail_set: true,
            ..FakeSecret::default()
        };
        assert!(migrate(&secret, &path).is_err());
        assert_eq!(
            read_file(&path).unwrap(),
            Some(tokens(NOW + 3600, NOW + 7200))
        );
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn keyringの結果を読み替える() {
        use super::keyring_store::{deleted, got};
        let platform = || -> Box<dyn std::error::Error + Send + Sync> { "locked".into() };

        assert_eq!(got(Ok("v".to_string())), Ok(Some("v".to_string())));
        assert_eq!(got(Err(keyring::Error::NoEntry)), Ok(None));
        for error in [
            keyring::Error::BadEncoding(vec![0xff]),
            keyring::Error::NoStorageAccess(platform()),
            keyring::Error::PlatformFailure(platform()),
            keyring::Error::NoDefaultStore,
        ] {
            assert!(got(Err(error)).is_err());
        }
        assert_eq!(deleted(Ok(())), Ok(()));
        assert_eq!(deleted(Err(keyring::Error::NoEntry)), Ok(()));
        for error in [
            keyring::Error::NoStorageAccess(platform()),
            keyring::Error::PlatformFailure(platform()),
            keyring::Error::NoDefaultStore,
        ] {
            assert!(deleted(Err(error)).is_err());
        }
    }
}
