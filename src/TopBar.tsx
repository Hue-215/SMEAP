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

function StatusText({ status, notice }: { status: SaveStatus | null; notice: Notice | null }) {
  const saved = status?.savedAt ? `保存済み ${formatSavedTime(status.savedAt)}` : ''
  const shown: Notice =
    notice ??
    (status?.failed
      ? { text: '保存できませんでした', error: true }
      : { text: saved, error: false })
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
  const sidebarLabel = sidebarOpen ? '記事一覧を閉じる' : '記事一覧を開く'
  return (
    <div role="toolbar" aria-label="編集" className="topbar">
      {drawer.narrow ? (
        <button
          id={drawer.toggleId}
          type="button"
          className="icon-button"
          onClick={drawer.shown ? drawer.cancel : drawer.open}
          aria-expanded={drawer.shown}
          aria-controls={drawer.id}
          aria-label="記事一覧を開く"
          title="記事一覧を開く"
        >
          <Icon name="menu" />
        </button>
      ) : (
        <button
          type="button"
          className="icon-button"
          onClick={onToggleSidebar}
          aria-expanded={sidebarOpen}
          aria-label={sidebarLabel}
          title={sidebarLabel}
        >
          <Icon name={sidebarOpen ? 'left_panel_close' : 'left_panel_open'} />
        </button>
      )}
      {drawer.narrow ? (
        <StatusText status={saveStatus} notice={notice} />
      ) : (
        <span className="topbar-spacer" aria-hidden="true" />
      )}
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
      {!drawer.narrow && (
        <>
          <span className="topbar-divider" aria-hidden="true" />
          <StatusText status={saveStatus} notice={notice} />
        </>
      )}
    </div>
  )
}
