use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use super::http::{
    classify, json_body, login_request, Transport, DEVICE_CODE_URL, TOKEN_URL, VERIFICATION_URL,
};
use super::token::TokenSet;
use super::GithubError;

#[derive(Clone, PartialEq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

// device_codeは、認可されるまでトークンを受け取るのに使えるので伏せる
impl std::fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceCode")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("interval", &self.interval)
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

#[derive(Debug, PartialEq)]
pub enum Poll {
    Pending,
    SlowDown { interval: u64 },
    Expired,
    Denied,
    Token(TokenSet),
}

#[derive(Debug, PartialEq)]
pub enum LoginOutcome {
    Token(TokenSet),
    Expired,
    Denied,
    Cancelled,
}

pub fn request_device_code(
    transport: &dyn Transport,
    client_id: &str,
) -> Result<DeviceCode, GithubError> {
    let request = login_request(DEVICE_CODE_URL, &[("client_id", client_id)])?;
    let response = classify(transport.send(&request)?, now())?;
    let body = json_body(&response)?;
    // Appでデバイスフローを有効にしていないとき(device_flow_disabled)など
    if let Some(error) = body.get("error").and_then(serde_json::Value::as_str) {
        return Err(endpoint_error(error));
    }
    let verification_uri = string_of(&body, "verification_uri")?;
    // 画面にもブラウザにも、決まったURLしか出さない
    if verification_uri != VERIFICATION_URL {
        return Err(GithubError::InvalidResponse {
            reason: "verification URI is not allowed".to_string(),
        });
    }
    Ok(DeviceCode {
        device_code: string_of(&body, "device_code")?,
        user_code: string_of(&body, "user_code")?,
        verification_uri,
        interval: seconds_of(&body, "interval")?,
        expires_in: seconds_of(&body, "expires_in")?,
    })
}

pub fn poll_once(
    transport: &dyn Transport,
    client_id: &str,
    device_code: &str,
    interval: u64,
    now: i64,
) -> Result<Poll, GithubError> {
    let request = login_request(
        TOKEN_URL,
        &[
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ],
    )?;
    let response = classify(transport.send(&request)?, now)?;
    let body = json_body(&response)?;
    // 待ち状態も200でerrorとして返ってくる
    match body.get("error").and_then(serde_json::Value::as_str) {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => {
            // 間隔が返ってこなければ、今の間隔の5秒後ろ
            let interval = seconds_of(&body, "interval").unwrap_or(interval + 5);
            Ok(Poll::SlowDown { interval })
        }
        Some("expired_token") => Ok(Poll::Expired),
        Some("access_denied") => Ok(Poll::Denied),
        Some(error) => Err(endpoint_error(error)),
        None => Ok(Poll::Token(token_of(&body, now)?)),
    }
}

// started_atはrequest_device_codeした時刻。sleepは秒数を受けて待つ
pub fn poll_until_done(
    transport: &dyn Transport,
    client_id: &str,
    code: &DeviceCode,
    started_at: i64,
    now: &dyn Fn() -> i64,
    sleep: &dyn Fn(u64),
    cancelled: &AtomicBool,
) -> Result<LoginOutcome, GithubError> {
    let mut interval = code.interval;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Ok(LoginOutcome::Cancelled);
        }
        if now() >= started_at + code.expires_in as i64 {
            return Ok(LoginOutcome::Expired);
        }
        match poll_once(transport, client_id, &code.device_code, interval, now())? {
            Poll::Pending => {}
            Poll::SlowDown { interval: next } => interval = next,
            Poll::Expired => return Ok(LoginOutcome::Expired),
            Poll::Denied => return Ok(LoginOutcome::Denied),
            Poll::Token(tokens) => return Ok(LoginOutcome::Token(tokens)),
        }
        sleep(interval);
    }
}

pub fn refresh(
    transport: &dyn Transport,
    client_id: &str,
    refresh_token: &str,
    now: i64,
) -> Result<TokenSet, GithubError> {
    let request = login_request(
        TOKEN_URL,
        &[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
    )?;
    let response = classify(transport.send(&request)?, now)?;
    let body = json_body(&response)?;
    // 通信の失敗や5xxは?とclassifyでそのまま伝える。ログインし直しにするのは、更新用のトークンが無効なときだけ
    match body.get("error").and_then(serde_json::Value::as_str) {
        Some("bad_refresh_token") => Err(GithubError::LoginRequired),
        Some(error) => Err(endpoint_error(error)),
        None => token_of(&body, now),
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

// errorの値はトークンではないので、理由に残す
fn endpoint_error(error: &str) -> GithubError {
    GithubError::InvalidResponse {
        reason: format!("GitHub returned {error}"),
    }
}

fn string_of(body: &serde_json::Value, key: &str) -> Result<String, GithubError> {
    body.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| GithubError::invalid_field(key))
}

fn seconds_of(body: &serde_json::Value, key: &str) -> Result<u64, GithubError> {
    body.get(key)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| GithubError::invalid_field(key))
}

// 応答の項目から、期限を今の時刻からの秒数にした組を作る
fn token_of(body: &serde_json::Value, now: i64) -> Result<TokenSet, GithubError> {
    Ok(TokenSet {
        access_token: string_of(body, "access_token")?,
        expires_at: now + seconds_of(body, "expires_in")? as i64,
        refresh_token: string_of(body, "refresh_token")?,
        refresh_token_expires_at: now + seconds_of(body, "refresh_token_expires_in")? as i64,
    })
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::AtomicBool;

    use percent_encoding::percent_decode_str;
    use serde_json::json;

    use super::*;
    use crate::github::fake::{json_response, FakeTransport};
    use crate::github::http::{Request, Response, DEVICE_CODE_URL, TOKEN_URL};

    const NOW: i64 = 1_000_000;
    const CLIENT_ID: &str = "Iv1.client";
    const ACCESS_TOKEN: &str = "ACCESS";
    const REFRESH_TOKEN: &str = "REFRESH";

    fn device_code() -> DeviceCode {
        DeviceCode {
            device_code: "DEVICE_CODE".to_string(),
            user_code: "ABCD-1234".to_string(),
            verification_uri: "https://github.com/login/device".to_string(),
            interval: 5,
            expires_in: 900,
        }
    }

    fn device_code_json() -> serde_json::Value {
        json!({
            "device_code": "DEVICE_CODE",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device",
            "interval": 5,
            "expires_in": 900
        })
    }

    fn token_json() -> serde_json::Value {
        json!({
            "access_token": ACCESS_TOKEN,
            "expires_in": 28800,
            "refresh_token": REFRESH_TOKEN,
            "refresh_token_expires_in": 15552000
        })
    }

    fn token_set() -> TokenSet {
        token_set_received_at(NOW)
    }

    // 期限は受け取った時刻から数える
    fn token_set_received_at(received_at: i64) -> TokenSet {
        TokenSet {
            access_token: ACCESS_TOKEN.to_string(),
            expires_at: received_at + 28800,
            refresh_token: REFRESH_TOKEN.to_string(),
            refresh_token_expires_at: received_at + 15552000,
        }
    }

    fn transport_with(body: serde_json::Value) -> FakeTransport {
        FakeTransport::new(vec![Ok(json_response(200, body))])
    }

    // フォームのbodyを名前と値の組にする
    fn form_of(request: &Request) -> Vec<(String, String)> {
        let body = String::from_utf8(request.body.clone().unwrap()).unwrap();
        body.split('&')
            .map(|part| {
                let (name, value) = part.split_once('=').unwrap();
                (
                    percent_decode_str(name).decode_utf8().unwrap().to_string(),
                    percent_decode_str(value).decode_utf8().unwrap().to_string(),
                )
            })
            .collect()
    }

    // 待つたびに偽の時刻を進める。待った秒数は後で確かめる
    struct FakeTime {
        now: Cell<i64>,
        sleeps: RefCell<Vec<u64>>,
    }

    impl FakeTime {
        fn new() -> Self {
            Self {
                now: Cell::new(NOW),
                sleeps: RefCell::new(Vec::new()),
            }
        }

        fn now(&self) -> i64 {
            self.now.get()
        }

        fn sleep(&self, seconds: u64) {
            self.sleeps.borrow_mut().push(seconds);
            self.now.set(self.now.get() + seconds as i64);
        }

        fn waited(&self) -> Vec<u64> {
            self.sleeps.borrow().clone()
        }
    }

    #[test]
    fn コードの取得はclient_idだけを送る() {
        let transport = transport_with(device_code_json());
        request_device_code(&transport, CLIENT_ID).unwrap();
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, DEVICE_CODE_URL);
        let form = form_of(&requests[0]);
        assert!(
            form.iter()
                .eq(&[("client_id".to_string(), CLIENT_ID.to_string())]),
            "client_idだけのはず: {form:?}"
        );
    }

    #[test]
    fn コードの取得で応答の項目を受け取る() {
        let transport = transport_with(device_code_json());
        assert_eq!(
            request_device_code(&transport, CLIENT_ID).unwrap(),
            device_code()
        );
    }

    #[test]
    fn コードの取得は決まったURL以外のverification_uriを断る() {
        let transport = transport_with(json!({
            "device_code": "DEVICE_CODE",
            "user_code": "ABCD-1234",
            "verification_uri": "https://evil.example.com/device",
            "interval": 5,
            "expires_in": 900
        }));
        assert!(request_device_code(&transport, CLIENT_ID).is_err());
    }

    #[test]
    fn 問い合わせはclient_idとdevice_codeとgrant_typeを送る() {
        let transport = transport_with(json!({ "error": "authorization_pending" }));
        poll_once(&transport, CLIENT_ID, "DEVICE_CODE", 5, NOW).unwrap();
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, TOKEN_URL);
        let form = form_of(&requests[0]);
        assert!(
            form.contains(&("client_id".to_string(), CLIENT_ID.to_string())),
            "{form:?}"
        );
        assert!(
            form.contains(&("device_code".to_string(), "DEVICE_CODE".to_string())),
            "{form:?}"
        );
        assert!(
            form.contains(&(
                "grant_type".to_string(),
                "urn:ietf:params:oauth:grant-type:device_code".to_string()
            )),
            "{form:?}"
        );
        assert!(
            form.iter().all(|(name, _)| name != "client_secret"),
            "{form:?}"
        );
    }

    #[test]
    fn 問い合わせの応答を分ける() {
        let cases = [
            (json!({ "error": "authorization_pending" }), Poll::Pending),
            (
                json!({ "error": "slow_down", "interval": 10 }),
                Poll::SlowDown { interval: 10 },
            ),
            (json!({ "error": "expired_token" }), Poll::Expired),
            (json!({ "error": "access_denied" }), Poll::Denied),
        ];
        for (body, expected) in cases {
            let transport = transport_with(body);
            assert_eq!(
                poll_once(&transport, CLIENT_ID, "DEVICE_CODE", 5, NOW).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn slow_downで間隔が返ってこなければ今の間隔の5秒後ろ() {
        let transport = transport_with(json!({ "error": "slow_down" }));
        assert_eq!(
            poll_once(&transport, CLIENT_ID, "DEVICE_CODE", 5, NOW).unwrap(),
            Poll::SlowDown { interval: 10 }
        );
    }

    #[test]
    fn トークンが返ったら期限を今の時刻からの秒数にする() {
        let transport = transport_with(token_json());
        assert_eq!(
            poll_once(&transport, CLIENT_ID, "DEVICE_CODE", 5, NOW).unwrap(),
            Poll::Token(token_set())
        );
    }

    #[test]
    fn 知らないerrorと読めないjsonはエラー() {
        let transport = transport_with(json!({ "error": "unknown_error" }));
        assert!(poll_once(&transport, CLIENT_ID, "DEVICE_CODE", 5, NOW).is_err());
        let transport = FakeTransport::new(vec![Ok(Response {
            status: 200,
            headers: vec![],
            body: b"@".to_vec(),
        })]);
        assert!(poll_once(&transport, CLIENT_ID, "DEVICE_CODE", 5, NOW).is_err());
    }

    #[test]
    fn エラーの表示とDebugにトークンが入らない() {
        // トークンを含む本文を、読めないJSONとして返す
        let transport = FakeTransport::new(vec![Ok(Response {
            status: 200,
            headers: vec![],
            body: format!(r#"{{"error":true,"access_token":"{ACCESS_TOKEN}"}}"#).into_bytes(),
        })]);
        let error = request_device_code(&transport, CLIENT_ID).unwrap_err();
        assert!(!format!("{error}").contains(ACCESS_TOKEN));
        assert!(!format!("{error:?}").contains(ACCESS_TOKEN));
        // 偽のTransportがエラーを返すときも
        let transport = FakeTransport::new(vec![Err(GithubError::Network {
            reason: "closed".to_string(),
        })]);
        let error = request_device_code(&transport, CLIENT_ID).unwrap_err();
        assert!(!format!("{error}").contains(ACCESS_TOKEN));
        assert!(!format!("{error:?}").contains(ACCESS_TOKEN));
    }

    #[test]
    fn 繰り返しは指定の間隔で待って問い合わせ直す() {
        let transport = FakeTransport::new(vec![
            Ok(json_response(
                200,
                json!({ "error": "authorization_pending" }),
            )),
            Ok(json_response(200, token_json())),
        ]);
        let time = FakeTime::new();
        let outcome = poll_until_done(
            &transport,
            CLIENT_ID,
            &device_code(),
            NOW,
            &|| time.now(),
            &|seconds| time.sleep(seconds),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, LoginOutcome::Token(token_set_received_at(NOW + 5)));
        assert_eq!(time.waited(), vec![5]);
    }

    #[test]
    fn slow_downの後は延ばした間隔で待つ() {
        let transport = FakeTransport::new(vec![
            Ok(json_response(
                200,
                json!({ "error": "slow_down", "interval": 10 }),
            )),
            Ok(json_response(200, token_json())),
        ]);
        let time = FakeTime::new();
        let outcome = poll_until_done(
            &transport,
            CLIENT_ID,
            &device_code(),
            NOW,
            &|| time.now(),
            &|seconds| time.sleep(seconds),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            outcome,
            LoginOutcome::Token(token_set_received_at(NOW + 10))
        );
        assert_eq!(time.waited(), vec![10]);
    }

    #[test]
    fn 取り消したら次の問い合わせをしないで終わる() {
        let transport = FakeTransport::new(vec![]);
        let time = FakeTime::new();
        let outcome = poll_until_done(
            &transport,
            CLIENT_ID,
            &device_code(),
            NOW,
            &|| time.now(),
            &|seconds| time.sleep(seconds),
            &AtomicBool::new(true),
        )
        .unwrap();
        assert_eq!(outcome, LoginOutcome::Cancelled);
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn 待っている間に取り消したら次の問い合わせをしないで終わる() {
        let transport = FakeTransport::new(vec![Ok(json_response(
            200,
            json!({ "error": "authorization_pending" }),
        ))]);
        let time = FakeTime::new();
        let cancelled = AtomicBool::new(false);
        let outcome = poll_until_done(
            &transport,
            CLIENT_ID,
            &device_code(),
            NOW,
            &|| time.now(),
            &|seconds| {
                time.sleep(seconds);
                cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
            },
            &cancelled,
        )
        .unwrap();
        assert_eq!(outcome, LoginOutcome::Cancelled);
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn 待っている間に期限を過ぎたら次の問い合わせをしないで期限切れで終わる() {
        let transport = FakeTransport::new(vec![Ok(json_response(
            200,
            json!({ "error": "authorization_pending" }),
        ))]);
        let time = FakeTime::new();
        let outcome = poll_until_done(
            &transport,
            CLIENT_ID,
            &device_code(),
            NOW,
            &|| time.now(),
            &|seconds| time.sleep(seconds + 900),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, LoginOutcome::Expired);
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn 更新の壊れた応答からトークンがエラーに漏れない() {
        let transport = FakeTransport::new(vec![Ok(Response {
            status: 200,
            headers: vec![],
            body: format!(r#"{{"access_token":"{ACCESS_TOKEN}","expires_in":"x"}}"#).into_bytes(),
        })]);
        let error = refresh(&transport, CLIENT_ID, REFRESH_TOKEN, NOW).unwrap_err();
        for text in [format!("{error}"), format!("{error:?}")] {
            assert!(!text.contains(ACCESS_TOKEN), "{text}");
            assert!(!text.contains(REFRESH_TOKEN), "{text}");
        }
    }

    #[test]
    fn 期限を過ぎたら問い合わせずに期限切れで終わる() {
        let transport = FakeTransport::new(vec![]);
        let time = FakeTime::new();
        time.now.set(NOW + 901);
        let outcome = poll_until_done(
            &transport,
            CLIENT_ID,
            &device_code(),
            NOW,
            &|| time.now(),
            &|seconds| time.sleep(seconds),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, LoginOutcome::Expired);
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn 更新はclient_idとgrant_typeとrefresh_tokenを送る() {
        let transport = transport_with(token_json());
        refresh(&transport, CLIENT_ID, REFRESH_TOKEN, NOW).unwrap();
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, TOKEN_URL);
        let form = form_of(&requests[0]);
        assert!(
            form.contains(&("client_id".to_string(), CLIENT_ID.to_string())),
            "{form:?}"
        );
        assert!(
            form.contains(&("grant_type".to_string(), "refresh_token".to_string())),
            "{form:?}"
        );
        assert!(
            form.contains(&("refresh_token".to_string(), REFRESH_TOKEN.to_string())),
            "{form:?}"
        );
        assert!(
            form.iter().all(|(name, _)| name != "client_secret"),
            "{form:?}"
        );
    }

    #[test]
    fn 更新で新しい組を受け取る() {
        let transport = transport_with(json!({
            "access_token": "NEW_ACCESS",
            "expires_in": 28800,
            "refresh_token": "NEW_REFRESH",
            "refresh_token_expires_in": 15552000
        }));
        assert_eq!(
            refresh(&transport, CLIENT_ID, REFRESH_TOKEN, NOW).unwrap(),
            TokenSet {
                access_token: "NEW_ACCESS".to_string(),
                expires_at: NOW + 28800,
                refresh_token: "NEW_REFRESH".to_string(),
                refresh_token_expires_at: NOW + 15552000,
            }
        );
    }

    #[test]
    fn 更新でトークンが無効ならログインし直し() {
        let transport = transport_with(json!({ "error": "bad_refresh_token" }));
        let error = refresh(&transport, CLIENT_ID, REFRESH_TOKEN, NOW).unwrap_err();
        assert!(matches!(error, GithubError::LoginRequired));
    }

    #[test]
    fn 更新で知らないerrorはログインし直しにしない() {
        let transport = transport_with(json!({ "error": "unknown_error" }));
        let error = refresh(&transport, CLIENT_ID, REFRESH_TOKEN, NOW).unwrap_err();
        assert!(!matches!(error, GithubError::LoginRequired));
    }

    #[test]
    fn 更新の通信の失敗と5xxはログインし直しにしない() {
        let transport = FakeTransport::new(vec![Err(GithubError::Network {
            reason: "closed".to_string(),
        })]);
        let error = refresh(&transport, CLIENT_ID, REFRESH_TOKEN, NOW).unwrap_err();
        assert!(!matches!(error, GithubError::LoginRequired));
        let transport = FakeTransport::new(vec![Ok(json_response(500, json!({})))]);
        let error = refresh(&transport, CLIENT_ID, REFRESH_TOKEN, NOW).unwrap_err();
        assert!(!matches!(error, GithubError::LoginRequired));
    }
}
