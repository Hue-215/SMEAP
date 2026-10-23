//! 本文の画像を読む独自プロトコル。パスは`/{slug}/{本文に書いた参照}`で、どちらもパーセントエンコードする。

use tauri::http::{Response, StatusCode};
use tauri::{Manager, Runtime};

use crate::store::{Store, StoreError};

pub fn register<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.register_asynchronous_uri_scheme_protocol("article-image", |ctx, request, responder| {
        let app = ctx.app_handle().clone();
        let path = request.uri().path().to_owned();
        tauri::async_runtime::spawn_blocking(move || {
            responder.respond(response_for(&app, &path));
        });
    })
}

fn response_for<R: Runtime>(app: &tauri::AppHandle<R>, path: &str) -> Response<Vec<u8>> {
    let Some((slug, reference)) = parse_path(path) else {
        return empty(StatusCode::NOT_FOUND);
    };
    let Some(content_type) = content_type(&reference) else {
        return empty(StatusCode::NOT_FOUND);
    };

    match app.state::<Store>().read_image(&slug, &reference) {
        Ok(data) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", content_type)
            .header("Cache-Control", "no-store")
            .body(data)
            .expect("固定のヘッダーでレスポンスを作れる"),
        Err(
            StoreError::ImageNotFound { .. }
            | StoreError::InvalidSlug { .. }
            | StoreError::InvalidImageReference { .. },
        ) => empty(StatusCode::NOT_FOUND),
        Err(error) => {
            eprintln!("画像を読めませんでした: {error}");
            empty(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn parse_path(path: &str) -> Option<(String, String)> {
    let (slug, reference) = path.strip_prefix('/')?.split_once('/')?;
    Some((percent_decode(slug)?, percent_decode(reference)?))
}

fn percent_decode(text: &str) -> Option<String> {
    percent_encoding::percent_decode_str(text)
        .decode_utf8()
        .ok()
        .map(|decoded| decoded.into_owned())
}

fn content_type(reference: &str) -> Option<&'static str> {
    let ext = reference.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "avif" => Some("image/avif"),
        _ => None,
    }
}

fn empty(status: StatusCode) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .body(Vec::new())
        .expect("本文の無いレスポンスを作れる")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn パスをslugと参照に分ける() {
        assert_eq!(
            parse_path("/a-1/..%2F..%2Fassets%2Fa-1%2F%E7%94%BB%20%23.png"),
            Some(("a-1".to_string(), "../../assets/a-1/画 #.png".to_string()))
        );
        assert_eq!(parse_path("/a-1"), None);
        assert_eq!(parse_path("a-1/x.png"), None);
        assert_eq!(parse_path("/a-1/%FF.png"), None);
    }

    #[test]
    fn 画像の形式だけを返す() {
        assert_eq!(content_type("../../assets/a/01.PNG"), Some("image/png"));
        assert_eq!(content_type("01.jpeg"), Some("image/jpeg"));
        // svgはスクリプトを含められるので返さない
        assert_eq!(content_type("01.svg"), None);
        assert_eq!(content_type("01"), None);
    }
}
