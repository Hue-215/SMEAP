import { describe, expect, it } from 'vitest'
import { githubErrorText, githubFailureText } from './githubError'
import { t } from './i18n'

describe('githubErrorText', () => {
  it('slugの変更と削除の失敗をそれぞれの文言にする', () => {
    expect(githubErrorText({ kind: 'branchMoved' })).toBe(t('common.github_branch_moved'))
    expect(githubErrorText({ kind: 'slugReserved', slug: 'old' })).toBe(
      t('common.github_slug_reserved'),
    )
    expect(githubErrorText({ kind: 'slugInUse', slug: 'post' })).toBe(t('common.github_slug_in_use'))
    expect(githubErrorText({ kind: 'inBaseBranch' })).toBe(t('common.github_in_base_branch'))
    expect(githubErrorText({ kind: 'conflicted' })).toBe(t('common.github_conflicted'))
  })

  it('ほかの操作の最中なら待つよう伝える', () => {
    expect(githubErrorText({ kind: 'syncBusy' })).toBe(t('common.github_sync_busy'))
  })

  it('種類ごとの文言が無い失敗は、呼び出し側の文言に任せる', () => {
    expect(githubFailureText({ kind: 'notFound' })).toBeNull()
    expect(githubErrorText({ kind: 'notFound' })).toBe(t('common.change_failed'))
  })
})
