// `async`: 同期のコマンドはメインスレッドで動き、ファイルの読み書きの間は画面が止まる

use std::borrow::Cow;
use std::fs;
use std::path::PathBuf;

use tauri::ipc::{InvokeBody, Request};
use tauri::{AppHandle, State};

use crate::clipboard;
use crate::store::{self, Article, ArticleList, EditedFields, Store, StoreError};

#[tauri::command]
pub fn validate_slug(slug: String) -> Result<(), StoreError> {
    store::validate_slug(&slug)
}

#[tauri::command(async)]
pub fn list_articles(store: State<'_, Store>) -> Result<ArticleList, StoreError> {
    store.list_articles()
}

#[tauri::command(async)]
pub fn create_article(
    app: AppHandle,
    github: State<'_, crate::github::commands::GitHub>,
    store: State<'_, Store>,
    slug: String,
) -> Result<Article, crate::github::commands::CommandError> {
    crate::github::commands::create_article(&app, &github, &store, &slug)
}

#[tauri::command(async)]
pub fn read_article(store: State<'_, Store>, slug: String) -> Result<Article, StoreError> {
    store.read_article(&slug)
}

#[tauri::command(async)]
pub fn save_article(
    store: State<'_, Store>,
    slug: String,
    fields: EditedFields,
) -> Result<(), StoreError> {
    store.save_article(&slug, fields)
}

#[tauri::command(async)]
pub fn rename_article(
    app: AppHandle,
    github: State<'_, crate::github::commands::GitHub>,
    store: State<'_, Store>,
    old: String,
    new: String,
) -> Result<Article, crate::github::commands::CommandError> {
    crate::github::commands::rename_article(&app, &github, &store, &old, &new)
}

#[tauri::command(async)]
pub fn delete_article(
    app: AppHandle,
    github: State<'_, crate::github::commands::GitHub>,
    store: State<'_, Store>,
    slug: String,
) -> Result<(), crate::github::commands::CommandError> {
    crate::github::commands::delete_article(&app, &github, &store, &slug)
}

#[tauri::command(async)]
pub fn import_body_image(
    store: State<'_, Store>,
    request: Request<'_>,
) -> Result<String, StoreError> {
    let slug = slug_of(&request)?;
    let bytes = body_bytes(request.body())?;
    store.import_body_image(&slug, &bytes)
}

#[tauri::command(async)]
pub fn import_hero_image(
    store: State<'_, Store>,
    request: Request<'_>,
) -> Result<String, StoreError> {
    let slug = slug_of(&request)?;
    let bytes = body_bytes(request.body())?;
    store.import_hero_image(&slug, &bytes)
}

#[tauri::command(async)]
pub fn remove_hero_image(store: State<'_, Store>, slug: String) -> Result<(), StoreError> {
    store.remove_hero_image(&slug)
}

// ドロップしたファイル。LinuxのWebKitGTKはファイルマネージャーからのドロップを画面へファイルとして渡さないので、
// Tauriが受けたパスから読む
#[tauri::command(async)]
pub fn import_body_image_from_path(
    store: State<'_, Store>,
    slug: String,
    path: PathBuf,
) -> Result<String, StoreError> {
    let data = fs::read(&path).map_err(|error| StoreError::io(&path, error))?;
    store.import_body_image(&slug, &data)
}

// クリップボードに画像が無ければNone
#[tauri::command(async)]
pub fn import_clipboard_image(
    app: AppHandle,
    store: State<'_, Store>,
    slug: String,
) -> Result<Option<String>, StoreError> {
    match clipboard::read_image_png(&app)? {
        Some(png) => store.import_body_image(&slug, &png).map(Some),
        None => Ok(None),
    }
}

// 画像を本文で受けるので、slugはヘッダーで受ける
fn slug_of(request: &Request<'_>) -> Result<String, StoreError> {
    request
        .headers()
        .get("slug")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .ok_or(StoreError::InvalidRequest {
            reason: "slugのヘッダーが無いか読めない".to_string(),
        })
}

// Androidでは生のバイト列を送れず、数値の配列のJSONで届く
fn body_bytes(body: &InvokeBody) -> Result<Cow<'_, [u8]>, StoreError> {
    match body {
        InvokeBody::Raw(bytes) => Ok(Cow::Borrowed(bytes)),
        InvokeBody::Json(value) => {
            let Some(values) = value.as_array() else {
                return Err(invalid_body());
            };
            let mut bytes = Vec::with_capacity(values.len());
            for value in values {
                let byte = value
                    .as_u64()
                    .filter(|&byte| byte <= u64::from(u8::MAX))
                    .ok_or_else(invalid_body)?;
                bytes.push(byte as u8);
            }
            Ok(Cow::Owned(bytes))
        }
    }
}

fn invalid_body() -> StoreError {
    StoreError::InvalidRequest {
        reason: "本文がバイト列にならない".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    #[test]
    fn rawの本文はバイト列そのまま() {
        let body = InvokeBody::Raw(vec![1, 2, 3]);
        assert_eq!(body_bytes(&body).unwrap().as_ref(), &[1u8, 2, 3][..]);
    }

    #[test]
    fn jsonの数値の配列はバイト列に集める() {
        let body = InvokeBody::Json(json!([1, 2, 3]));
        assert_eq!(body_bytes(&body).unwrap().as_ref(), &[1u8, 2, 3][..]);
    }

    #[test]
    fn バイト列にならない本文は失敗する() {
        for body in [
            InvokeBody::Json(json!([256])),
            InvokeBody::Json(json!([1, "x"])),
            InvokeBody::Json(json!("bytes")),
        ] {
            assert!(matches!(
                body_bytes(&body),
                Err(StoreError::InvalidRequest { .. })
            ));
        }
    }
}
