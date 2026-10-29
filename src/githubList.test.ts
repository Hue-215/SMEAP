import { describe, expect, it } from 'vitest'
import type { ArticleSummary } from './api'
import {
  fetchLineText,
  groupArticles,
  type GithubArticleStates,
  type GithubCategory,
} from './githubList'

const NOW = 1_700_000_000

function article(slug: string, pub_date: string): ArticleSummary {
  return { slug, title: slug, pubDate: pub_date, warning: false }
}

function statesOf(
  articles: Partial<Record<string, { category?: GithubCategory; dirty?: boolean; conflict?: boolean }>>,
  rest: Partial<GithubArticleStates> = {},
): GithubArticleStates {
  return {
    repo: 'owner/blog',
    fetchedAt: NOW,
    stateUnreadable: false,
    articles: Object.fromEntries(
      Object.entries(articles).map(([slug, item]) => [
        slug,
        {
          category: item?.category ?? 'drafting',
          dirty: item?.dirty ?? false,
          conflict: item?.conflict ?? false,
        },
      ]),
    ) as GithubArticleStates['articles'],
    skipped: [],
    ...rest,
  }
}

// 時差に依らないよう、期待の文言の時刻は同じ関数で作る
const timeOf = (seconds: number) =>
  new Date(seconds * 1000).toLocaleTimeString('ja-JP', { hour: '2-digit', minute: '2-digit' })

describe('groupArticles', () => {
  it('記事を3つの分類に振り分ける', () => {
    // 一覧の並び(公開日の新しい順)を保ちながら、分類ごとに分ける
    const articles = [
      article('new-public', '2026-01-04'),
      article('editing', '2026-01-03'),
      article('draft', '2026-01-02'),
      article('old-public', '2026-01-01'),
    ]
    const states = statesOf({
      'new-public': { category: 'published' },
      'old-public': { category: 'published' },
      editing: { category: 'editing' },
      draft: { category: 'drafting' },
    })

    const groups = groupArticles(articles, states)
    expect(groups).toEqual([
      { category: 'drafting', rows: [{ article: articles[2], state: states.articles.draft }] },
      { category: 'editing', rows: [{ article: articles[1], state: states.articles.editing }] },
      {
        category: 'published',
        rows: [
          { article: articles[0], state: states.articles['new-public'] },
          { article: articles[3], state: states.articles['old-public'] },
        ],
      },
    ])
  })

  it('件数0の分類は出さない', () => {
    const articles = [article('a-post', '2026-01-01'), article('b-post', '2025-12-31')]
    const states = statesOf({
      'a-post': { category: 'published' },
      'b-post': { category: 'published' },
    })

    const groups = groupArticles(articles, states)
    expect(groups).toEqual([
      {
        category: 'published',
        rows: [
          { article: articles[0], state: states.articles['a-post'] },
          { article: articles[1], state: states.articles['b-post'] },
        ],
      },
    ])
  })

  it('状態の無い記事は執筆中・変更ありにする', () => {
    // 一覧の後に作った記事など、GitHub上の状態がまだ無い記事
    const articles = [article('uncommitted', '2026-01-01')]
    const groups = groupArticles(articles, statesOf({}))
    expect(groups).toEqual([
      {
        category: 'drafting',
        rows: [
          {
            article: articles[0],
            state: { category: 'drafting', dirty: true, conflict: false },
          },
        ],
      },
    ])
  })

  it('state.jsonが読めないときは分類しない', () => {
    const states = statesOf(
      {
        'a-post': { category: 'published' },
        'b-post': { category: 'drafting' },
      },
      { stateUnreadable: true },
    )
    const articles = [article('b-post', '2026-01-01'), article('a-post', '2025-12-31')]

    const groups = groupArticles(articles, states)
    expect(groups).toEqual([{ category: null, rows: [
      { article: articles[0], state: null },
      { article: articles[1], state: null },
    ] }])
  })
})

describe('fetchLineText', () => {
  it('読めたrepositoryと時刻の行', () => {
    expect(fetchLineText('owner/repo', NOW, false)).toBe(
      `owner/repo · ${timeOf(NOW)}に読み込み`,
    )
  })

  it('失敗して前の時刻があればそれを出す', () => {
    expect(fetchLineText('owner/repo', NOW, true)).toBe(
      `読み込めませんでした。${timeOf(NOW)}の内容を表示しています`,
    )
  })

  it('一度も読めていなければ読み込めなかったことだけを出す', () => {
    expect(fetchLineText('owner/repo', null, true)).toBe('読み込めませんでした')
  })
})
