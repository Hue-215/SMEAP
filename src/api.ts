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
  heroImage: string | null
  body: string
}

export function listArticles(): Promise<ArticleList> {
  return invoke<ArticleList>('list_articles')
}

export function readArticle(slug: string): Promise<Article> {
  return invoke<Article>('read_article', { slug })
}

// Rust側の独自プロトコル(src-tauri/src/image_protocol.rs)のURL。AndroidではTauriがhttpの形に替える。
export function articleImageUrl(slug: string, name: string): string {
  const path = `/${encodeURIComponent(slug)}/${encodeURIComponent(name)}`
  return isAndroid ? `http://article-image.localhost${path}` : `article-image://localhost${path}`
}
