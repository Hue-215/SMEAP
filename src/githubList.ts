// GitHub連携の一覧のための、手元の振り分けと読み込みの1行の文言(src/Sidebar.tsx)
import type { ArticleSummary } from './api'
import { t } from './i18n'

// GitHub上の状態ごとの分類
export type GithubCategory = 'drafting' | 'editing' | 'published'

export interface GithubArticleState {
  category: GithubCategory
  dirty: boolean
  conflict: boolean
}

export interface GithubSkipped {
  slug: string
  branch: string
  reason: string
}

export interface GithubArticleStates {
  repo: string
  fetchedAt: number | null
  stateUnreadable: boolean
  articles: Record<string, GithubArticleState>
  skipped: GithubSkipped[]
}

export interface ArticleRow {
  article: ArticleSummary
  state: GithubArticleState | null
}

// categoryがnullなら分類しない1つの並び
export interface ArticleGroup {
  category: GithubCategory | null
  rows: ArticleRow[]
}

const CATEGORIES = ['drafting', 'editing', 'published'] as const

// 連携していない(statesがnull)・state.jsonが読めないときは、分類せずに今の1つの並びにする
export function groupArticles(
  articles: ArticleSummary[],
  states: GithubArticleStates | null,
): ArticleGroup[] {
  if (states === null || states.stateUnreadable) {
    return [{ category: null, rows: articles.map((article) => ({ article, state: null })) }]
  }
  const rows = { drafting: [], editing: [], published: [] } as Record<GithubCategory, ArticleRow[]>
  for (const article of articles) {
    // 一覧の後に作った記事など、GitHub上の状態がまだ無い記事は、commitしていない変更として扱う
    const state = states.articles[article.slug] ?? { category: 'drafting', dirty: true, conflict: false }
    rows[state.category].push({ article, state })
  }
  return CATEGORIES.map((category) => ({ category, rows: rows[category] })).filter(
    (group) => group.rows.length > 0,
  )
}

// fetchedAtはunix秒。failedは最後の読み込みが失敗したか
export function fetchLineText(
  repo: string,
  fetchedAt: number | null,
  failed: boolean,
): string {
  // まだ読み込んでいないときは、repository名だけを出す
  if (fetchedAt === null) return failed ? t('sidebar.fetch_failed') : repo
  const time = new Date(fetchedAt * 1000).toLocaleTimeString('ja-JP', { hour: '2-digit', minute: '2-digit' })
  return failed ? t('sidebar.fetch_failed_at', { time }) : t('sidebar.fetched_at', { repo, time })
}
