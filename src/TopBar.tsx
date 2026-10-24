import { useRef } from 'react'
import { formatSavedTime, type SaveStatus } from './autoSave'
import Icon from './Icon'
import type { DrawerState } from './useDrawer'

export interface Notice {
  text: string
  error: boolean
}

interface TopBarProps {
  drawer: DrawerState
  sidebarOpen: boolean
  onToggleSidebar: () => void
  onInsertLink: (() => void) | null
  onInsertImages: ((files: File[]) => void) | null
  // nullなら出さない
  saveStatus: SaveStatus | null
  // あれば保存の状態の代わりに出す
  notice: Notice | null
}

function statusNotice(status: SaveStatus | null, notice: Notice | null): Notice {
  if (notice) return notice
  if (status?.failed) return { text: '保存できませんでした', error: true }
  return { text: status?.savedAt ? `保存済み ${formatSavedTime(status.savedAt)}` : '', error: false }
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
  sidebarOpen,
  onToggleSidebar,
  onInsertLink,
  onInsertImages,
  saveStatus,
  notice,
}: TopBarProps) {
  const fileInput = useRef<HTMLInputElement>(null)
  const closes = !drawer.narrow && sidebarOpen
  const sidebarLabel = closes ? '記事一覧を閉じる' : '記事一覧を開く'
  const shown = statusNotice(saveStatus, notice)
  return (
    <div role="toolbar" aria-label="編集" className="topbar">
      <button
        type="button"
        className="icon-button"
        id={drawer.narrow ? drawer.toggleId : undefined}
        onClick={drawer.narrow ? (drawer.shown ? drawer.cancel : drawer.open) : onToggleSidebar}
        aria-expanded={drawer.narrow ? drawer.shown : sidebarOpen}
        aria-controls={drawer.narrow ? drawer.id : undefined}
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
        aria-label="リンク"
        title="リンク"
      >
        <Icon name="link" />
      </button>
      <button
        type="button"
        className="icon-button"
        disabled={onInsertImages === null}
        onClick={() => fileInput.current?.click()}
        aria-label="画像を挿入"
        title="画像を挿入"
      >
        <Icon name="image" />
      </button>
      <input
        ref={fileInput}
        type="file"
        accept="image/*"
        multiple
        hidden
        onChange={(e) => {
          const files = Array.from(e.target.files ?? [])
          // 同じファイルを続けて選んでもchangeが起きるよう空にする
          e.target.value = ''
          if (files.length > 0) onInsertImages?.(files)
        }}
      />
    </div>
  )
}
