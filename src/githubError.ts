import { UnsavedArticleError, type GithubError } from './api'
import { t } from './i18n'

// retryAtはunix秒
const formatRetryAt = (seconds: number) =>
  new Date(seconds * 1000).toLocaleTimeString('ja-JP', { hour: '2-digit', minute: '2-digit' })

// 失敗の文言。開いている記事の保存に失敗したときと、GitHubとの通信の種類ごとに選ぶ。
// どちらでもなければ、既存の失敗の文言
export function githubErrorText(error: unknown): string {
  if (error instanceof UnsavedArticleError) return t('settings.unsaved_article')
  const e = error as GithubError | undefined
  if (e === undefined || typeof e.kind !== 'string') return t('common.change_failed')
  switch (e.kind) {
    case 'network':
      return t('common.github_network')
    case 'rateLimited':
      return e.retryAt === null || e.retryAt === undefined
        ? t('common.github_rate_limited')
        : t('common.github_rate_limited_at', { time: formatRetryAt(e.retryAt) })
    case 'loginRequired':
      return t('common.github_login_required')
    case 'baseBranchNotFound':
      return t('common.github_base_branch_not_found')
    case 'invalidLayout':
      return t('common.github_invalid_layout', { path: e.path })
    case 'stateUnreadable':
      return t('common.github_state_unreadable')
    case 'io':
      return t('common.github_io', { path: e.path })
    case 'status':
    case 'invalidResponse':
    case 'redirect':
    case 'urlNotAllowed':
      return t('common.github_failed')
    default:
      return t('common.change_failed')
  }
}
