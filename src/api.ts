import { invoke } from '@tauri-apps/api/core'

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

export function listArticles(): Promise<ArticleList> {
  return invoke<ArticleList>('list_articles')
}
