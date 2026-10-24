import { invoke } from '@tauri-apps/api/core'
import { isAndroid } from './platform'

// Rust側のエラー。種類によってpath・reasonを持つ
export interface StoreError {
  kind: string
  path?: string
  reason?: string
}

export interface ArticleSummary {
  slug: string
  title: string
  pubDate: string | null
  warning: boolean
}

// slugの分かる、読めない記事
export interface BrokenArticle {
  slug: string
  path: string
  reason: string
}

// 記事に結び付かない失敗
export interface UnreadableEntry {
  path: string
  reason: string
}

export interface ArticleList {
  articles: ArticleSummary[]
  broken: BrokenArticle[]
  unreadable: UnreadableEntry[]
}

// 読み取りの警告。keyは関係する既知の項目、keptは元の行をextraに残したとき
export interface Warning {
  key: string | null
  message: string
  kept: boolean
}

export interface Article {
  slug: string
  title: string | null
  description: string | null
  pubDate: string | null
  // ファイル名だけ。本文の画像のような参照ではない
  heroImage: string | null
  body: string
  warnings: Warning[]
}

// heroImageはimportHeroImageが書き換えるので、保存では渡さない
export type EditedFields = Omit<Article, 'slug' | 'warnings' | 'heroImage'>

export function listArticles(): Promise<ArticleList> {
  return invoke<ArticleList>('list_articles')
}

export function readArticle(slug: string): Promise<Article> {
  return invoke<Article>('read_article', { slug })
}

export function saveArticle(slug: string, fields: EditedFields): Promise<void> {
  return invoke('save_article', { slug, fields })
}

export function createArticle(slug: string): Promise<Article> {
  return invoke<Article>('create_article', { slug })
}

export function renameArticle(oldSlug: string, newSlug: string): Promise<Article> {
  return invoke<Article>('rename_article', { old: oldSlug, new: newSlug })
}

export function deleteArticle(slug: string): Promise<void> {
  return invoke('delete_article', { slug })
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

export function removeHeroImage(slug: string): Promise<void> {
  return invoke('remove_hero_image', { slug })
}

// クリップボードに画像が無ければnull。Linuxだけで読める
export function importClipboardImage(slug: string): Promise<string | null> {
  return invoke<string | null>('import_clipboard_image', { slug })
}

// Rust側のPostsLocation(src-tauri/src/settings.rs)。画像の置き場所は`{slug}`を付ける前の根
export interface PostsLocation {
  postsDir: string
  assetsDir: string
}

// Androidはzipの中の`src/content/`の下の名前
export type PostsDir = ({ kind: 'desktop' } & PostsLocation) | { kind: 'android'; name: string }

export class UnsavedArticleError extends Error {}

// デスクトップだけ呼べる
export function getPostsDir(): Promise<PostsLocation> {
  return invoke<PostsLocation>('get_posts_dir')
}

// 選ばせて変える。キャンセルされたらnull。デスクトップだけ呼べる
export function pickPostsDir(): Promise<PostsLocation | null> {
  return invoke<PostsLocation | null>('pick_posts_dir')
}

// Androidだけ呼べる。zipの中の`src/content/`の下の名前
export function getPostsDirName(): Promise<string> {
  return invoke<string>('get_posts_dir_name')
}

// Androidだけ呼べる
export function setPostsDirName(name: string): Promise<void> {
  return invoke('set_posts_dir_name', { name })
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

// 記事ファイルから画像を指すときの相対パス。Rust側のassets_ref(src-tauri/src/store.rs)とそろえる
const assetsRef = (slug: string) => `../../assets/${slug}/`

// 差し替えても同じ名前のことがあるので、versionを変えて読み直させる。独自プロトコルはクエリを見ない
export function heroImageUrl(slug: string, name: string, version: number): string {
  return `${articleImageUrl(slug, assetsRef(slug) + name)}?v=${version}`
}
