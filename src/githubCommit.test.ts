import { describe, expect, it } from 'vitest'
import type { GithubArticleStates } from './githubList'
import {
  canSendMessage,
  commitButtonState,
  conflictText,
  defaultCommitMessage,
  imagesText,
  linesText,
} from './githubCommit'

const NOW = 1_700_000_000

// 手元のGitHub連携の状態。指定のない項目は公開済み・変更無しにする
function statesOf(
  articles: Partial<Record<string, { dirty?: boolean; conflict?: boolean }>>,
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
          category: 'published' as const,
          dirty: item?.dirty ?? false,
          conflict: item?.conflict ?? false,
        },
      ]),
    ) as GithubArticleStates['articles'],
    skipped: [],
    ...rest,
  }
}

describe('defaultCommitMessage', () => {
  it('kindごとのCommit messageの初期値', () => {
    expect(defaultCommitMessage('add', '新しい記事')).toBe('新しい記事を追加')
    expect(defaultCommitMessage('fix', '変更中')).toBe('変更中を修正')
    expect(defaultCommitMessage('update', '公開済み')).toBe('公開済みを更新')
  })

  it('titleが空なら無題にする', () => {
    expect(defaultCommitMessage('add', '')).toBe('無題を追加')
    expect(defaultCommitMessage('fix', '')).toBe('無題を修正')
    expect(defaultCommitMessage('update', '')).toBe('無題を更新')
  })
})

describe('conflictText', () => {
  it('本文が重なれば本文を1つ出す', () => {
    expect(conflictText('新しい記事', true, ['01.png'])).toBe(
      '「新しい記事」の本文がGitHub上でも変更されています。',
    )
  })

  it('画像だけの重なりは名前を「、」でつないで出す', () => {
    expect(conflictText('新しい記事', false, ['a.png', 'b.png'])).toBe(
      '画像 a.png、b.pngがGitHub上でも変更されています。',
    )
    expect(conflictText('新しい記事', false, ['a.png'])).toBe(
      '画像 a.pngがGitHub上でも変更されています。',
    )
  })
})

describe('linesText', () => {
  it('追加と削除の行数を+と−の出し分けで出す', () => {
    expect(linesText(12, 3)).toBe('+12 −3')
    expect(linesText(0, 3)).toBe('+0 −3')
  })
})

describe('imagesText', () => {
  it('追加と削除の件数を出す', () => {
    expect(imagesText(1, 1)).toBe('追加 1件・削除 1件')
    expect(imagesText(2, 5)).toBe('追加 2件・削除 5件')
  })

  it('0の側は出さない', () => {
    expect(imagesText(2, 0)).toBe('追加 2件')
    expect(imagesText(0, 5)).toBe('削除 5件')
  })

  it('両方0なら行を出さない', () => {
    expect(imagesText(0, 0)).toBe(null)
  })
})

describe('canSendMessage', () => {
  it('空白だけのCommit messageは送れない', () => {
    expect(canSendMessage('値を更新しました')).toBe(true)
    expect(canSendMessage('')).toBe(false)
    expect(canSendMessage('   ')).toBe(false)
    // 全角の空白も空白
    expect(canSendMessage('　')).toBe(false)
  })
})

describe('commitButtonState', () => {
  it('連携していなければnull', () => {
    expect(commitButtonState(null, 'a', false, true)).toBe(null)
  })

  it('押せるのは記事を開いていて、変更があって、読み込み中でないとき', () => {
    // GitHub上の変更のdirty
    const states = statesOf({ changed: { dirty: true }, clean: { dirty: false } })
    expect(commitButtonState(states, 'changed', false, false)).toEqual({
      enabled: true,
      dirty: true,
    })
    expect(commitButtonState(states, 'changed', true, false)).toEqual({
      enabled: false,
      dirty: true,
    })
    // 編集欄に保存していない変更
    expect(commitButtonState(states, 'clean', false, true)).toEqual({
      enabled: true,
      dirty: true,
    })
  })

  it('押せないのは、記事を開いていない・読めない・変更が無いとき', () => {
    const states = statesOf({ a: { dirty: false } })
    // 記事を開いていない
    expect(commitButtonState(states, null, false, false)).toEqual({
      enabled: false,
      dirty: false,
    })
    expect(commitButtonState(states, null, false, true)).toEqual({
      enabled: false,
      dirty: true,
    })
    // state.jsonが読めない
    expect(commitButtonState(statesOf({}, { stateUnreadable: true }), 'a', false, false)).toEqual({
      enabled: false,
      dirty: false,
    })
    expect(commitButtonState(statesOf({}, { stateUnreadable: true }), 'a', false, true)).toEqual({
      enabled: false,
      dirty: true,
    })
    // 変更が無い
    expect(commitButtonState(states, 'a', false, false)).toEqual({
      enabled: false,
      dirty: false,
    })
  })

  it('statesに無い記事は変更ありとして扱う', () => {
    const states = statesOf({ a: { dirty: false } })
    expect(commitButtonState(states, 'uncommitted', false, false)).toEqual({
      enabled: true,
      dirty: true,
    })
  })
})
