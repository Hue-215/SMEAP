import { useRef } from 'react'
import { formatSavedTime, type SaveStatus } from './autoSave'
import Icon from './Icon'
import { t } from './i18n'
import ImageFileInput from './ImageFileInput'
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
}

function statusNotice(status: SaveStatus | null, notice: Notice | null): Notice {
  if (notice) return notice
  if (status?.failed) return { text: t('common.save_failed'), error: true }
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
    </div>
  )
}
