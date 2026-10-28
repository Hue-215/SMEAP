use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

use super::GithubError;

pub const API_BASE: &str = "https://api.github.com";
pub const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
pub const VERIFICATION_URL: &str = "https://github.com/login/device";

const USER_AGENT: &str = concat!("SMEAP/", env!("CARGO_PKG_VERSION"));
// githubが案内している現行のAPI版
const API_VERSION: &str = "2022-11-28";

// URLの部品に入れる値は、RFC3986のunreserved(ASCIIの英数字と-_.~)以外をpercentエンコードする
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    // commitとbranchの操作の段で使う
    #[allow(dead_code)]
    Patch,
    #[allow(dead_code)]
    Delete,
}

#[derive(Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

// Debugは手で実装し、Authorizationの値を伏せる
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &self.url)
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        if name.eq_ignore_ascii_case("authorization") {
                            (name.as_str(), "(hidden)")
                        } else {
                            (name.as_str(), value.as_str())
                        }
                    })
                    .collect::<Vec<_>>(),
            )
            // bodyにはフォームとしてトークンを含むことがあるので、長さだけ出す
            .field("body", &self.body.as_ref().map(Vec::len))
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub trait Transport: Send + Sync {
    fn send(&self, request: &Request) -> Result<Response, GithubError>;
}

pub fn check_url(url: &str) -> Result<(), GithubError> {
    let parsed = url::Url::parse(url).map_err(|_| GithubError::UrlNotAllowed)?;
    // httpsで決まったhost・portだけ。userinfo(https://user@host/)も断つ
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
    {
        return Err(GithubError::UrlNotAllowed);
    }
    // APIはどのパスでもよく、ログインのendpointは決まったパスだけ
    let ok = match parsed.host_str() {
        Some("api.github.com") => true,
        Some("github.com") => matches!(
            parsed.path(),
            "/login/device/code" | "/login/oauth/access_token"
        ),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(GithubError::UrlNotAllowed)
    }
}

// API_BASEの後ろに、segmentsを/でつないだパスとqueryを付ける。segmentに/や..があればUrlNotAllowed
pub fn api_url(segments: &[&str], query: &[(&str, &str)]) -> Result<String, GithubError> {
    let mut url = String::from(API_BASE);
    for segment in segments {
        if segment.is_empty() || segment.contains('/') || segment.contains("..") {
            return Err(GithubError::UrlNotAllowed);
        }
        url.push('/');
        url.push_str(&utf8_percent_encode(segment, UNRESERVED).to_string());
    }
    if !query.is_empty() {
        url.push('?');
        for (i, (name, value)) in query.iter().enumerate() {
            if i > 0 {
                url.push('&');
            }
            url.push_str(name);
            url.push('=');
            url.push_str(&utf8_percent_encode(value, UNRESERVED).to_string());
        }
    }
    check_url(&url)?;
    Ok(url)
}

// トークンはAPIにだけ送る。AuthorizationのあるRequestのURLはAPI_BASEの下に限る(Linkのnextがログイン用のURLを指していても送らない)
fn check_api_url(url: &str) -> Result<(), GithubError> {
    check_url(url)?;
    if url.starts_with(&format!("{API_BASE}/")) {
        Ok(())
    } else {
        Err(GithubError::UrlNotAllowed)
    }
}

pub fn api_request(
    method: Method,
    url: &str,
    token: &str,
    body: Option<serde_json::Value>,
) -> Result<Request, GithubError> {
    check_api_url(url)?;
    let mut headers = vec![
        ("Authorization".to_string(), format!("Bearer {token}")),
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("X-GitHub-Api-Version".to_string(), API_VERSION.to_string()),
        (
            "Accept".to_string(),
            "application/vnd.github+json".to_string(),
        ),
    ];
    let body = body.map(|value| serde_json::to_vec(&value).expect("a JSON value should serialize"));
    if body.is_some() {
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
    }
    Ok(Request {
        method,
        url: url.to_string(),
        headers,
        body,
    })
}

// Authorizationは付けない
pub fn login_request(url: &str, form: &[(&str, &str)]) -> Result<Request, GithubError> {
    check_url(url)?;
    let body = form
        .iter()
        .map(|(name, value)| {
            format!(
                "{}={}",
                utf8_percent_encode(name, UNRESERVED),
                utf8_percent_encode(value, UNRESERVED)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    Ok(Request {
        method: Method::Post,
        url: url.to_string(),
        headers: vec![
            (
                "Content-Type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            ),
            ("Accept".to_string(), "application/json".to_string()),
        ],
        body: Some(body.into_bytes()),
    })
}

// nowはretry-afterから再開時刻を求めるのに使う
pub fn classify(response: Response, now: i64) -> Result<Response, GithubError> {
    let status = response.status;
    if (200..300).contains(&status) {
        return Ok(response);
    }
    // リダイレクトは追わない
    if (300..400).contains(&status) {
        return Err(GithubError::Redirect);
    }
    if status == 401 {
        return Err(GithubError::LoginRequired);
    }
    if status == 429 || status == 403 {
        if let Some(retry_at) = rate_limit_retry_at(&response, now) {
            return Err(GithubError::RateLimited {
                retry_at: Some(retry_at),
            });
        }
        // ヘッダーの無い429と、残りが0でもresetの読めない403は、いつ再開できるか分からない
        let exhausted = response
            .header("x-ratelimit-remaining")
            .is_some_and(|value| value.trim() == "0");
        if status == 429 || exhausted {
            return Err(GithubError::RateLimited { retry_at: None });
        }
    }
    Err(GithubError::Status { status })
}

fn rate_limit_retry_at(response: &Response, now: i64) -> Option<i64> {
    if let Some(seconds) = response
        .header("retry-after")
        .and_then(|value| value.trim().parse::<i64>().ok())
    {
        return Some(now + seconds);
    }
    // 残りが0のときだけ、resetの時刻(unix秒)に再開できる
    response
        .header("x-ratelimit-remaining")
        .filter(|value| value.trim() == "0")
        .and_then(|_| response.header("x-ratelimit-reset"))
        .and_then(|value| value.trim().parse::<i64>().ok())
}

pub fn next_link(response: &Response) -> Option<String> {
    let header = response.header("Link")?;
    for part in header.split(',') {
        // <URL>; rel="name" の並びから、relがnextのものを取り出す
        let Some((url, rest)) = part
            .trim()
            .strip_prefix('<')
            .and_then(|rest| rest.split_once('>'))
        else {
            continue;
        };
        let rel = rest.split(';').find_map(|param| {
            let param = param.trim();
            param.strip_prefix("rel=").map(|rel| rel.trim_matches('"'))
        });
        if rel == Some("next") {
            return Some(url.trim().to_string());
        }
    }
    None
}

// レスポンスのbodyをJSONとして読む。エラー文にbody(トークンを含むことがある)を入れない
pub fn json_body(response: &Response) -> Result<serde_json::Value, GithubError> {
    serde_json::from_slice(&response.body).map_err(|_| GithubError::InvalidResponse {
        reason: "the body is not valid JSON".to_string(),
    })
}

// 本物の通信。ureqで、rustlsと同梱のルート証明書を使う
pub struct UreqTransport(ureq::Agent);

impl UreqTransport {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            // リダイレクトは追わない(max_redirectsを0にすると、応答をそのまま返す)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .timeout_connect(Some(std::time::Duration::from_secs(30)));
        Self(ureq::Agent::new_with_config(config.build()))
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

// WithBodyのbuilderはGETやDELETEのものと型が違うので、ヘッダーを付ける部分だけ共通にする
fn with_headers<B>(
    mut call: ureq::RequestBuilder<B>,
    headers: &[(String, String)],
) -> ureq::RequestBuilder<B> {
    for (name, value) in headers {
        call = call.header(name.as_str(), value.as_str());
    }
    call
}

// 通信の失敗をNetworkの理由にする。reasonにURLのクエリやヘッダーを入れない
fn network_reason(error: ureq::Error) -> String {
    match error {
        ureq::Error::Io(error) => error.to_string(),
        ureq::Error::Timeout(_) => "timed out".to_string(),
        ureq::Error::HostNotFound => "host not found".to_string(),
        ureq::Error::ConnectionFailed => "connection failed".to_string(),
        ureq::Error::BodyExceedsLimit(limit) => {
            format!("the response body exceeds {limit} bytes")
        }
        // この2つはURLを含みうる
        ureq::Error::BadUri(_) => "invalid URL".to_string(),
        ureq::Error::RequireHttpsOnly(_) => "not https".to_string(),
        // TLS・プロキシなど
        error => error.to_string(),
    }
}

impl Transport for UreqTransport {
    fn send(&self, request: &Request) -> Result<Response, GithubError> {
        // 呼び出し側で組み立てたRequestも確かめる
        if request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        {
            check_api_url(&request.url)?;
        } else {
            check_url(&request.url)?;
        }
        self.call(request).map_err(|error| GithubError::Network {
            reason: network_reason(error),
        })
    }
}

impl UreqTransport {
    fn call(&self, request: &Request) -> Result<Response, ureq::Error> {
        let response = match request.method {
            Method::Get => {
                with_headers(self.0.get(request.url.as_str()), &request.headers).call()?
            }
            Method::Delete => {
                with_headers(self.0.delete(request.url.as_str()), &request.headers).call()?
            }
            Method::Post | Method::Patch => {
                let body = request.body.as_deref().unwrap_or(&[]);
                match request.method {
                    Method::Post => {
                        with_headers(self.0.post(request.url.as_str()), &request.headers)
                            .send(body)?
                    }
                    _ => with_headers(self.0.patch(request.url.as_str()), &request.headers)
                        .send(body)?,
                }
            }
        };
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.to_string(), value.to_string()))
            })
            .collect();
        let mut body = response.into_body();
        let body = body.read_to_vec()?;
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    fn response(status: u16, headers: &[(&str, &str)]) -> Response {
        Response {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            body: Vec::new(),
        }
    }

    fn header_of<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn 宛先はGitHubのAPIとログインのページだけ通す() {
        for ok in [
            "https://api.github.com/user",
            "https://api.github.com/user/installations?per_page=100",
            "https://github.com/login/device/code",
            "https://github.com/login/oauth/access_token",
        ] {
            assert!(check_url(ok).is_ok(), "{ok} は通すはず");
        }
        for ng in [
            "http://api.github.com/user",
            "https://api.github.com.example.com/",
            "https://example.com/",
            "https://github.com/login/device",
            "https://github.com/other",
            "https://user@api.github.com/user",
            "https://api.github.com:8443/user",
        ] {
            assert!(
                matches!(check_url(ng), Err(GithubError::UrlNotAllowed)),
                "{ng} は弾くはず"
            );
        }
    }

    #[test]
    fn api_urlはAPI_BASEの後ろにパスとqueryを組み立てる() {
        assert_eq!(
            api_url(&["user", "installations"], &[("per_page", "100")]).unwrap(),
            "https://api.github.com/user/installations?per_page=100"
        );
    }

    #[test]
    fn api_urlのsegmentにスラッシュやドットドットがあったら断る() {
        for ng in [
            &["repos", "owner/sub/repo", "contents", "src"][..],
            &["repos", "..", "contents"][..],
        ] {
            assert!(
                matches!(api_url(ng, &[]), Err(GithubError::UrlNotAllowed)),
                "{ng:?} は断るはず"
            );
        }
    }

    #[test]
    fn api_urlはsegmentの特殊文字をpercentエンコードする() {
        assert_eq!(
            api_url(&["a?b", "c#d"], &[]).unwrap(),
            "https://api.github.com/a%3Fb/c%23d"
        );
        // 空のsegmentは断つ
        assert!(matches!(
            api_url(&["a", ""], &[]),
            Err(GithubError::UrlNotAllowed)
        ));
    }

    #[test]
    fn api_requestにAuthorizationとAPIのヘッダーを付ける() {
        let request =
            api_request(Method::Get, "https://api.github.com/user", "TOKEN", None).unwrap();
        assert_eq!(
            header_of(&request.headers, "Authorization"),
            Some("Bearer TOKEN")
        );
        assert_eq!(
            header_of(&request.headers, "Accept"),
            Some("application/vnd.github+json")
        );
        assert!(header_of(&request.headers, "User-Agent").is_some());
        assert!(header_of(&request.headers, "X-GitHub-Api-Version").is_some());
    }

    #[test]
    fn api_requestはトークンをAPI以外へ送らない() {
        assert!(matches!(
            api_request(Method::Get, TOKEN_URL, "TOKEN", None),
            Err(GithubError::UrlNotAllowed)
        ));
    }

    #[test]
    fn login_requestにAuthorizationは付けない() {
        let request = login_request(TOKEN_URL, &[("client_id", "Iv1.client")]).unwrap();
        assert_eq!(
            header_of(&request.headers, "Accept"),
            Some("application/json")
        );
        assert_eq!(
            header_of(&request.headers, "Authorization"),
            None,
            "Authorizationは付けないはず"
        );
        assert!(request.body.is_some());
    }

    #[test]
    fn リクエストのDebugにAuthorizationの値が出ない() {
        let request = api_request(
            Method::Get,
            "https://api.github.com/user",
            "SECRET_TOKEN",
            None,
        )
        .unwrap();
        let debug = format!("{request:?}");
        assert!(!debug.contains("SECRET_TOKEN"));
        assert!(debug.contains("https://api.github.com/user"));
    }

    #[test]
    fn classifyは2xxをそのまま返す() {
        let original = response(200, &[("Link", "x")]);
        let passed = classify(original.clone(), NOW).unwrap();
        assert_eq!(passed.status, 200);
        assert_eq!(passed.headers, original.headers);
        assert_eq!(passed.body, original.body);
        assert_eq!(classify(response(204, &[]), NOW).unwrap().status, 204);
    }

    #[test]
    fn classifyはリダイレクトを追わずに断る() {
        let result = classify(response(302, &[("Location", "https://example.com/")]), NOW);
        assert!(matches!(result, Err(GithubError::Redirect)));
    }

    #[test]
    fn classifyは401をログインし直しに分ける() {
        let result = classify(response(401, &[]), NOW);
        assert!(matches!(result, Err(GithubError::LoginRequired)));
    }

    #[test]
    fn classifyはretry_afterから再開できる時刻を求める() {
        for status in [429, 403] {
            let result = classify(response(status, &[("retry-after", "120")]), 1_000_000);
            match result {
                Err(GithubError::RateLimited { retry_at }) => {
                    assert_eq!(retry_at, Some(1_000_120))
                }
                other => panic!("{status} はレート制限のはず: {other:?}"),
            }
        }
    }

    #[test]
    fn classifyはレート制限の残りが0のときx_ratelimit_resetを再開できる時刻にする() {
        for status in [429, 403] {
            let result = classify(
                response(
                    status,
                    &[
                        ("x-ratelimit-remaining", "0"),
                        ("x-ratelimit-reset", "1800000000"),
                    ],
                ),
                1_000_000,
            );
            match result {
                Err(GithubError::RateLimited { retry_at }) => {
                    assert_eq!(retry_at, Some(1_800_000_000))
                }
                other => panic!("{status} はレート制限のはず: {other:?}"),
            }
        }
    }

    #[test]
    fn classifyは429でヘッダーが無いと再開できる時刻を持たない() {
        let result = classify(response(429, &[]), NOW);
        match result {
            Err(GithubError::RateLimited { retry_at }) => assert_eq!(retry_at, None),
            other => panic!("429 はレート制限のはず: {other:?}"),
        }
    }

    #[test]
    fn classifyはヘッダーの無い403をレート制限にしない() {
        let result = classify(response(403, &[]), NOW);
        assert!(matches!(result, Err(GithubError::Status { status: 403 })));
    }

    #[test]
    fn classifyは403で残りが0でなければステータスのエラーにする() {
        let result = classify(response(403, &[("x-ratelimit-remaining", "5")]), NOW);
        assert!(matches!(result, Err(GithubError::Status { status: 403 })));
    }

    #[test]
    fn classifyは403で残りが0でもresetが読めなければ再開できる時刻を持たない() {
        let result = classify(response(403, &[("x-ratelimit-remaining", "0")]), NOW);
        match result {
            Err(GithubError::RateLimited { retry_at }) => assert_eq!(retry_at, None),
            other => panic!("403 はレート制限のはず: {other:?}"),
        }
    }

    #[test]
    fn classifyは404と5xxをステータスのエラーにする() {
        for status in [404, 500, 502] {
            let result = classify(response(status, &[]), NOW);
            assert!(
                matches!(result, Err(GithubError::Status { status: s }) if s == status),
                "{status} はステータスのエラーのはず"
            );
        }
    }

    #[test]
    fn 送る前に宛先をもう一度確かめる() {
        let transport = UreqTransport::new();
        // Authorizationが無くても、許可の外のURLは送らない
        let request = Request {
            method: Method::Get,
            url: "https://example.com/".to_string(),
            headers: vec![],
            body: None,
        };
        assert!(matches!(
            transport.send(&request),
            Err(GithubError::UrlNotAllowed)
        ));
        // AuthorizationのあるRequestはAPIの下だけ。トークンをログイン用のURLへ送らない
        let request = Request {
            method: Method::Get,
            url: TOKEN_URL.to_string(),
            headers: vec![("Authorization".to_string(), "Bearer TOKEN".to_string())],
            body: None,
        };
        assert!(matches!(
            transport.send(&request),
            Err(GithubError::UrlNotAllowed)
        ));
    }

    #[test]
    fn next_linkはrel_nextのURLを取り出す() {
        let link = r#"<https://api.github.com/x?page=3>; rel="next", <https://api.github.com/x?page=1>; rel="prev", <https://api.github.com/x?page=5>; rel="last""#;
        let passed = response(200, &[("Link", link)]);
        assert_eq!(
            next_link(&passed),
            Some("https://api.github.com/x?page=3".to_string())
        );
    }

    #[test]
    fn next_linkはnextが無ければNone() {
        let link = r#"<https://api.github.com/x?page=5>; rel="last""#;
        assert_eq!(next_link(&response(200, &[("Link", link)])), None);
        assert_eq!(next_link(&response(200, &[])), None);
    }
}
