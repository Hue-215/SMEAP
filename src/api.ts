import { invoke } from '@tauri-apps/api/core'
import { isAndroid } from './platform'

// Rust側のエラー。中身は見ないので種類だけを受ける。
export interface StoreError {
  kind: string
}

export interface ArticleSummary {
  slug: string
  title: string
  pubDate: string
}

export interface ArticleList {
  articles: ArticleSummary[]
  broken: StoreError[]
}

export interface Article {
  slug: string
  title: string
  description: string
  pubDate: string
  // ファイル名だけ。本文の画像のような参照ではない
  heroImage: string | null
  body: string
}

export type ArticleFields = Omit<Article, 'slug'>

export function listArticles(): Promise<ArticleList> {
  return invoke<ArticleList>('list_articles')
}

export function readArticle(slug: string): Promise<Article> {
  return invoke<Article>('read_article', { slug })
}

export function saveArticle(slug: string, fields: ArticleFields): Promise<void> {
  return invoke('save_article', { slug, fields })
}

export function createArticle(slug: string): Promise<Article> {
  return invoke<Article>('create_article', { slug })
}

export function renameArticle(oldSlug: string, newSlug: string): Promise<Article> {
  return invoke<Article>('rename_article', { old: oldSlug, new: newSlug })
}

// 画像は生のバイト列で送り、slugはヘッダーで渡す(src-tauri/src/commands.rs)。Androidでは数値の配列のJSONになる
export function importBodyImage(slug: string, data: Uint8Array): Promise<string> {
  return invoke<string>('import_body_image', data, { headers: { slug } })
}

export function importHeroImage(slug: string, data: Uint8Array): Promise<string> {
  return invoke<string>('import_hero_image', data, { headers: { slug } })
}

export function importBodyImageFromPath(slug: string, path: string): Promise<string> {
  return invoke<string>('import_body_image_from_path', { slug, path })
}

// クリップボードに画像が無ければnull。Linuxだけで読める
export function importClipboardImage(slug: string): Promise<string | null> {
  return invoke<string | null>('import_clipboard_image', { slug })
}

// Rust側のExportOutcome(src-tauri/src/export.rs)とそろえる
export type ExportOutcome =
  | { kind: 'saved' }
  | { kind: 'cancelled' }
  | { kind: 'leftIncomplete'; reason: string }

export function exportZip(slug: string): Promise<ExportOutcome> {
  return invoke<ExportOutcome>('export_zip', { slug })
}

// 入力中の案内に使う。Rust側のvalidate_slug(src-tauri/src/store.rs)とそろえる
export function isValidSlug(slug: string): boolean {
  return /^[a-z0-9-]+$/.test(slug)
}

// Rust側の独自プロトコル(src-tauri/src/image_protocol.rs)のURL。AndroidのTauriは`http://{scheme}.localhost`で受ける。
// referenceは本文に書いた参照のまま渡し、記事の画像かどうかはRust側が判断する。
export function articleImageUrl(slug: string, reference: string): string {
  const path = `/${encodeURIComponent(slug)}/${encodeURIComponent(reference)}`
  return isAndroid ? `http://article-image.localhost${path}` : `article-image://localhost${path}`
}
