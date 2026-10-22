import Icon from './Icon'
import MenuButton from './MenuButton'
import type { MenuItem } from './MenuButton'
import { isAndroid } from './platform'
import type { DrawerState } from './useDrawer'

// 上のバーのその他のメニューの項目。まだ作っていない操作は、押せない(disabled)項目と
// して出す。Androidではメニューに「zipで書き出す」を出す。
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

const menuItems: MenuItem[] = isAndroid ? [exportZipItem, settingsItem] : [settingsItem]

interface TopBarProps {
  drawer: DrawerState
  // 広い窓のサイドバーが開いているか。
  sidebarOpen: boolean
  onToggleSidebar: () => void
  // 保存の状態の文字(例「保存済み 14:32」)。まだ渡さない。
  saveStatus?: string
}

// 画面(編集欄)の上のバー。広い窓では左端にサイドバーの開閉、右端に書式のボタンと保存の状態。
// 狭い窓では左端に記事一覧を開くボタン、右端に書式のボタンとその他のメニュー。
export default function TopBar({ drawer, sidebarOpen, onToggleSidebar, saveStatus }: TopBarProps) {
  if (drawer.narrow) {
    return (
      <div role="toolbar" aria-label="編集" className="topbar">
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
        <span className="save-status">{saveStatus}</span>
        <button type="button" className="icon-button" disabled aria-label="リンク" title="リンク">
          <Icon name="link" />
        </button>
        <button
          type="button"
          className="icon-button"
          disabled
          aria-label="画像を挿入"
          title="画像を挿入"
        >
          <Icon name="image" />
        </button>
        <MenuButton label="その他" items={menuItems} />
      </div>
    )
  }

  const listLabel = sidebarOpen ? '記事一覧を閉じる' : '記事一覧を開く'
  return (
    <div role="toolbar" aria-label="編集" className="topbar">
      <button
        type="button"
        className="icon-button"
        onClick={onToggleSidebar}
        aria-expanded={sidebarOpen}
        aria-label={listLabel}
        title={listLabel}
      >
        <Icon name={sidebarOpen ? 'left_panel_close' : 'left_panel_open'} />
      </button>
      <span className="topbar-spacer" aria-hidden="true" />
      <button type="button" className="icon-button" disabled aria-label="リンク" title="リンク">
        <Icon name="link" />
      </button>
      <button
        type="button"
        className="icon-button"
        disabled
        aria-label="画像を挿入"
        title="画像を挿入"
      >
        <Icon name="image" />
      </button>
      {saveStatus && (
        <>
          <span className="topbar-separator" aria-hidden="true" />
          <span className="save-status">{saveStatus}</span>
        </>
      )}
    </div>
  )
}
