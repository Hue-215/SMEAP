//! 本文の画像を読む独自プロトコル。URIは`/{slug}/{本文に書いた参照}`で、どちらもパーセントエンコードする。

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
    let Some((slug, reference)) = path
        .strip_prefix('/')
        .and_then(|rest| rest.split_once('/'))
        .and_then(|(slug, reference)| Some((percent_decode(slug)?, percent_decode(reference)?)))
    else {
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
            StoreError::NotFound { .. }
            | StoreError::InvalidSlug { .. }
            | StoreError::InvalidImageName { .. },
        ) => empty(StatusCode::NOT_FOUND),
        Err(_) => empty(StatusCode::INTERNAL_SERVER_ERROR),
    }
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
