import { describe, expect, it } from 'vitest'
import { githubErrorText, githubFailureText } from './githubError'
import { t } from './i18n'

describe('githubErrorText', () => {
  it('slugの変更と削除の失敗をそれぞれの文言にする', () => {
    expect(githubErrorText({ kind: 'branchMoved' })).toBe(t('common.github_branch_moved'))
    expect(githubErrorText({ kind: 'slugReserved', slug: 'old' })).toBe(
      t('common.github_slug_reserved'),
    )
    expect(githubErrorText({ kind: 'slugInUse', slug: 'post' })).toBe(
      t('common.github_slug_in_use'),
    )
    expect(githubErrorText({ kind: 'conflicted' })).toBe(t('common.github_conflicted'))
  })

  it('ほかの操作の最中ならその文言にする', () => {
    expect(githubErrorText({ kind: 'syncBusy' })).toBe(t('common.github_sync_busy'))
  })

  it('公開済みでない記事の操作と食い違いをそれぞれの文言にする', () => {
    expect(githubErrorText({ kind: 'notPublished' })).toBe(t('common.github_not_published'))
    expect(githubErrorText({ kind: 'outOfSync' })).toBe(t('common.github_out_of_sync'))
  })

  it('資格情報の保存先の失敗はその文言にする', () => {
    expect(githubErrorText({ kind: 'credentialStore', reason: 'locked' })).toBe(
      t('common.github_credential_store'),
    )
  })

  it('種類ごとの文言が無い失敗は、呼び出し側の文言に任せる', () => {
    expect(githubFailureText({ kind: 'notFound' })).toBeNull()
    expect(githubErrorText({ kind: 'notFound' })).toBe(t('common.change_failed'))
  })
})
