// `async`: 同期のコマンドはメインスレッドで動き、ファイルの読み書きの間は画面が止まる

use tauri::ipc::{InvokeBody, Request};
use tauri::State;

use crate::store::{self, Article, ArticleFields, ArticleList, Store, StoreError};

#[tauri::command]
pub fn validate_slug(slug: String) -> Result<(), StoreError> {
    store::validate_slug(&slug)
}

#[tauri::command(async)]
pub fn list_articles(store: State<'_, Store>) -> Result<ArticleList, StoreError> {
    store.list_articles()
}

#[tauri::command(async)]
pub fn create_article(store: State<'_, Store>, slug: String) -> Result<Article, StoreError> {
    store.create_article(&slug)
}

#[tauri::command(async)]
pub fn read_article(store: State<'_, Store>, slug: String) -> Result<Article, StoreError> {
    store.read_article(&slug)
}

#[tauri::command(async)]
pub fn save_article(
    store: State<'_, Store>,
    slug: String,
    fields: ArticleFields,
) -> Result<(), StoreError> {
    store.save_article(&slug, fields)
}

#[tauri::command(async)]
pub fn rename_article(
    store: State<'_, Store>,
    old: String,
    new: String,
) -> Result<Article, StoreError> {
    store.rename_article(&old, &new)
}

#[tauri::command(async)]
pub fn import_body_image(
    store: State<'_, Store>,
    request: Request<'_>,
) -> Result<String, StoreError> {
    let slug = slug_of(&request)?;
    store.import_body_image(&slug, raw_body(&request)?)
}

#[tauri::command(async)]
pub fn import_hero_image(
    store: State<'_, Store>,
    request: Request<'_>,
) -> Result<String, StoreError> {
    let slug = slug_of(&request)?;
    store.import_hero_image(&slug, raw_body(&request)?)
}

// 画像はJSONにせず生のバイト列で受けるので、slugはヘッダーで受ける
fn slug_of(request: &Request<'_>) -> Result<String, StoreError> {
    request
        .headers()
        .get("slug")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .ok_or(StoreError::InvalidRequest {
            reason: "slugのヘッダーが無い".to_string(),
        })
}

fn raw_body<'a>(request: &'a Request<'_>) -> Result<&'a [u8], StoreError> {
    match request.body() {
        InvokeBody::Raw(bytes) => Ok(bytes),
        _ => Err(StoreError::InvalidRequest {
            reason: "本文がバイト列でない".to_string(),
        }),
    }
}
