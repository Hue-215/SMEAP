// Commit & Pushのための判断(docs/spec.mdの「Commit & Push」)
import type { CommitKind, CommitPreview } from './api'
import type { GithubArticleStates } from './githubList'
import { t } from './i18n'

export function defaultCommitMessage(kind: CommitKind, title: string): string {
  const name = title || t('common.untitled')
  if (kind === 'add') return t('commit.message_add', { title: name })
  if (kind === 'fix') return t('commit.message_fix', { title: name })
  return t('commit.message_update', { title: name })
}

export function canSendMessage(message: string): boolean {
  return message.trim() !== ''
}

// 重なったファイルは、本文を優先して1つ出す(本文が無ければ画像の名前を「、」でつなぐ)
export function conflictText(title: string, md: boolean, images: string[]): string {
  const name = title || t('common.untitled')
  if (md) return t('commit.conflict_md', { title: name })
  return t('commit.conflict_images', { images: images.join('、') })
}

export function linesText(added: number, removed: number): string {
  return `+${added} −${removed}`
}

export function imagesText(added: number, removed: number): string | null {
  if (added === 0 && removed === 0) return null
  if (added === 0) return t('commit.images_removed', { removed })
  if (removed === 0) return t('commit.images_added', { added })
  return t('commit.images_both', { added, removed })
}

export interface CommitButton {
  enabled: boolean
  dirty: boolean
}

// 本文の行。slugを変えた記事は元の名前も出す
export function mdNameText(preview: CommitPreview): string {
  if (preview.oldMdName === null) return preview.mdName
  return `${preview.oldMdName} → ${preview.mdName}`
}

// statesがnullなら連携していないのでnull。slugは読み込み済みの記事(無ければnull)。
// statesに無い記事は変更ありとして扱う。state.jsonが読めなければ、編集欄の未保存の変更だけを見る
export function commitButtonState(
  states: GithubArticleStates | null,
  slug: string | null,
  syncing: boolean,
  editorDirty: boolean,
): CommitButton | null {
  if (states === null) return null
  const dirty =
    editorDirty ||
    (!states.stateUnreadable && slug !== null && (states.articles[slug]?.dirty ?? true))
  const enabled = !syncing && slug !== null && !states.stateUnreadable && dirty
  return { enabled, dirty }
}
