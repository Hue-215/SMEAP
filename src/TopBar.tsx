import { useRef } from 'react'
import { formatSavedTime, type SaveStatus } from './autoSave'
import Icon from './Icon'
import MenuButton from './MenuButton'
import type { MenuItem } from './MenuButton'
import { isAndroid } from './platform'
import type { DrawerState } from './useDrawer'

// まだ作っていない操作は、押せない項目として出す。
const exportZipItem: MenuItem = {
  key: 'export-zip',
  label: 'zipで書き出す',
  icon: 'folder_zip',
  disabled: true,
  onSelect: () => {},
}

interface TopBarProps {
  drawer: DrawerState
  sidebarOpen: boolean
  onToggleSidebar: () => void
  // nullなら押せない
  onRenameSlug: (() => void) | null
  onInsertLink: (() => void) | null
  onInsertImages: ((files: File[]) => void) | null
  // nullなら出さない
  saveStatus: SaveStatus | null
  // あれば保存の状態の代わりに、エラーとして出す
  notice: string | null
  onOpenSettings: () => void
}

function StatusText({ status, notice }: { status: SaveStatus | null; notice: string | null }) {
  const error = notice ?? (status?.failed ? '保存できませんでした' : null)
  const text =
    error ?? (status?.savedAt ? `保存済み ${formatSavedTime(status.savedAt)}` : '')
  return (
    <span role="status" className={error ? 'save-status error' : 'save-status'}>
      {text}
    </span>
  )
}

export default function TopBar({
  drawer,
  sidebarOpen,
  onToggleSidebar,
  onRenameSlug,
  onInsertLink,
  onInsertImages,
  saveStatus,
  notice,
  onOpenSettings,
}: TopBarProps) {
  const fileInput = useRef<HTMLInputElement>(null)
  const sidebarLabel = sidebarOpen ? '記事一覧を閉じる' : '記事一覧を開く'
  const renameItem: MenuItem = {
    key: 'rename-slug',
    label: 'slugを変更',
    icon: 'edit',
    disabled: onRenameSlug === null,
    onSelect: () => onRenameSlug?.(),
  }
  const menuItems: MenuItem[] = [
    renameItem,
    ...(isAndroid ? [exportZipItem] : []),
    {
      key: 'settings',
      label: '設定',
      icon: 'settings',
      onSelect: onOpenSettings,
    },
  ]
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
      <MenuButton label="その他" items={menuItems} />
    </div>
  )
}
