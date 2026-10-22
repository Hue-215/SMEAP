// `async`: 同期のコマンドはメインスレッドで動き、ファイルの読み書きの間は画面が止まる

use tauri::State;

use crate::store::{self, Article, ArticleFields, ArticleSummary, Store, StoreError};

#[tauri::command]
pub fn validate_slug(slug: String) -> Result<(), StoreError> {
    store::validate_slug(&slug)
}

#[tauri::command(async)]
pub fn list_articles(store: State<'_, Store>) -> Result<Vec<ArticleSummary>, StoreError> {
    Ok(store.list_articles())
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
