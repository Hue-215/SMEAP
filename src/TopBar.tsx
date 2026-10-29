import { useRef } from 'react'
import { formatSavedTime, type SaveStatus } from './autoSave'
import type { CommitButton } from './githubCommit'
import Icon from './Icon'
import { t } from './i18n'
import ImageFileInput from './ImageFileInput'
import { isAndroid } from './platform'
import type { DrawerState } from './useDrawer'

export interface Notice {
  text: string
  error: boolean
}

interface TopBarProps {
  drawer: DrawerState
  onInsertLink: (() => void) | null
  onInsertImages: ((files: File[]) => void) | null
  // nullなら出さない
  saveStatus: SaveStatus | null
  // あれば保存の状態の代わりに出す
  notice: Notice | null
  // 記事内検索を開く。ボタンはAndroidだけに出す。nullなら押せない
  onFind: (() => void) | null
  // nullならCommit & Pushを出さない(連携していない)
  commit: { state: CommitButton; onClick: () => void } | null
}

function statusNotice(status: SaveStatus | null, notice: Notice | null): Notice {
  if (notice) return notice
  if (status?.failed) return { text: t('common.save_failed'), error: true }
  if (status?.dirty)
    return {
      text: status.savedAt ? t('topbar.unsaved_at', { time: formatSavedTime(status.savedAt) }) : t('topbar.unsaved'),
      error: false,
    }
  return {
    text: status?.savedAt ? t('topbar.saved_at', { time: formatSavedTime(status.savedAt) }) : '',
    error: false,
  }
}

function StatusText({ shown }: { shown: Notice }) {
  return (
    <span role="status" className={shown.error ? 'save-status error' : 'save-status'}>
      {shown.text}
    </span>
  )
}

export default function TopBar({
  drawer,
  onInsertLink,
  onInsertImages,
  saveStatus,
  notice,
  onFind,
  commit,
}: TopBarProps) {
  const fileInput = useRef<HTMLInputElement>(null)
  const closes = !drawer.narrow && drawer.expanded
  const sidebarLabel = t(closes ? 'topbar.close_sidebar' : 'topbar.open_sidebar')
  const shown = statusNotice(saveStatus, notice)
  return (
    <div role="toolbar" aria-label={t('topbar.toolbar_label')} className="topbar">
      <button
        type="button"
        className="icon-button"
        id={drawer.toggleId}
        onClick={drawer.toggle}
        aria-expanded={drawer.expanded}
        aria-controls={drawer.id}
        aria-label={sidebarLabel}
        title={sidebarLabel}
      >
        <Icon name={closes ? 'left_panel_close' : 'menu'} />
      </button>
      <StatusText shown={shown} />
      {commit && (
        <button
          type="button"
          className="icon-button commit-button"
          disabled={!commit.state.enabled}
          onClick={commit.onClick}
          aria-label={t('topbar.commit')}
          title={commit.state.dirty ? t('topbar.commit') : t('topbar.commit_nothing')}
        >
          <Icon name="upload" />
          {commit.state.dirty && <span className="commit-button-dirty" aria-hidden="true" />}
        </button>
      )}
      <button
        type="button"
        className="icon-button"
        disabled={onInsertLink === null}
        onClick={() => onInsertLink?.()}
        aria-label={t('topbar.insert_link')}
        title={t('topbar.insert_link')}
      >
        <Icon name="link" />
      </button>
      <button
        type="button"
        className="icon-button"
        disabled={onInsertImages === null}
        onClick={() => fileInput.current?.click()}
        aria-label={t('topbar.insert_image')}
        title={t('topbar.insert_image')}
      >
        <Icon name="image" />
      </button>
      <ImageFileInput
        ref={fileInput}
        multiple
        onFiles={(files) => onInsertImages?.(files)}
      />
      {isAndroid && (
        <button
          type="button"
          className="icon-button"
          disabled={onFind === null}
          onClick={() => onFind?.()}
          aria-label={t('topbar.find')}
          title={t('topbar.find')}
        >
          <Icon name="search" />
        </button>
      )}
    </div>
  )
}
