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

const settingsItem: MenuItem = {
  key: 'settings',
  label: '設定',
  icon: 'settings',
  disabled: true,
  onSelect: () => {},
}

interface TopBarProps {
  drawer: DrawerState
  sidebarOpen: boolean
  onToggleSidebar: () => void
  // nullなら押せない
  onRenameSlug: (() => void) | null
  // nullなら出さない
  saveStatus: SaveStatus | null
}

function SaveStatusText({ status }: { status: SaveStatus | null }) {
  const text = status?.failed
    ? '保存できませんでした'
    : status?.savedAt
      ? `保存済み ${formatSavedTime(status.savedAt)}`
      : ''
  return (
    <span role="status" className={status?.failed ? 'save-status error' : 'save-status'}>
      {text}
    </span>
  )
}

export default function TopBar({
  drawer,
  sidebarOpen,
  onToggleSidebar,
  onRenameSlug,
  saveStatus,
}: TopBarProps) {
  const sidebarLabel = sidebarOpen ? '記事一覧を閉じる' : '記事一覧を開く'
  const renameItem: MenuItem = {
    key: 'rename-slug',
    label: 'slugを変更',
    icon: 'edit',
    disabled: onRenameSlug === null,
    onSelect: () => onRenameSlug?.(),
  }
  const menuItems = [renameItem, ...(isAndroid ? [exportZipItem] : []), settingsItem]
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
        <SaveStatusText status={saveStatus} />
      ) : (
        <span className="topbar-spacer" aria-hidden="true" />
      )}
      <button type="button" className="icon-button" disabled aria-label="リンク" title="リンク">
        <Icon name="link" />
      </button>
      <button type="button" className="icon-button" disabled aria-label="画像を挿入" title="画像を挿入">
        <Icon name="image" />
      </button>
      {!drawer.narrow && (
        <>
          <span className="topbar-divider" aria-hidden="true" />
          <SaveStatusText status={saveStatus} />
        </>
      )}
      <MenuButton label="その他" items={menuItems} />
    </div>
  )
}
