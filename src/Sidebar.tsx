import { useId, useState, type MouseEvent, type ReactNode } from 'react'
import type { ArticleSummary, BrokenArticle, UnreadableEntry } from './api'
import ContextMenu, { type MenuItem } from './ContextMenu'
import Dialog from './Dialog'
import Icon from './Icon'
import { isAndroid } from './platform'

export type ArticlesState =
  | { status: 'loading' }
  // 一覧全体が読めなかったときのTauriのエラー(src-tauri/src/store.rs)。path・reasonは種類によって持つ
  | { status: 'failed'; path?: string; reason?: string }
  | {
      status: 'loaded'
      articles: ArticleSummary[]
      broken: BrokenArticle[]
      unreadable: UnreadableEntry[]
    }

interface SidebarProps {
  articles: ArticlesState
  selectedSlug: string | null
  onSelect: (slug: string) => void
  onNew: () => void
  onSettings: () => void
  onRenameSlug: (slug: string) => void
  onExportZip: (slug: string) => void
  // titleは記事ならタイトル、壊れた記事ならnull
  onDelete: (slug: string, title: string | null) => void
}

function DialogDetail({
  title,
  onClose,
  children,
}: {
  title: string
  onClose: () => void
  children: ReactNode
}) {
  const headingId = useId()
  return (
    <Dialog labelledBy={headingId} onCancel={onClose}>
      <div className="dialog-form">
        <h2 id={headingId} className="dialog-title">
          {title}
        </h2>
        <div className="dialog-body">{children}</div>
        <div className="dialog-buttons">
          <button type="button" onClick={onClose}>
            閉じる
          </button>
        </div>
      </div>
    </Dialog>
  )
}

// 右クリック(Androidの長押し)で出す、行ごとのメニューの開き方
interface RowMenu {
  slug: string
  title: string | null
  x: number
  y: number
}

export default function Sidebar({
  articles,
  selectedSlug,
  onSelect,
  onNew,
  onSettings,
  onRenameSlug,
  onExportZip,
  onDelete,
}: SidebarProps) {
  const [brokenSlug, setBrokenSlug] = useState<string | null>(null)
  const [unreadableOpen, setUnreadableOpen] = useState(false)
  const [rowMenu, setRowMenu] = useState<RowMenu | null>(null)
  const brokenDialog =
    articles.status === 'loaded' && brokenSlug !== null
      ? (articles.broken.find((b) => b.slug === brokenSlug) ?? null)
      : null

  // キーボードのcontextmenuでは座標が出ないので、行の左下に出す
  const openRowMenu = (e: MouseEvent<HTMLElement>, slug: string, title: string | null) => {
    e.preventDefault()
    let { clientX: x, clientY: y } = e
    if (x === 0 && y === 0) {
      const rect = e.currentTarget.getBoundingClientRect()
      x = rect.left
      y = rect.bottom
    }
    setRowMenu({ slug, title, x, y })
  }

  const rowMenuItems = (menu: RowMenu): MenuItem[] => {
    const deleteItem: MenuItem = {
      key: 'delete',
      label: '削除',
      icon: 'delete',
      onSelect: () => onDelete(menu.slug, menu.title),
    }
    // 読めない記事は削除だけ
    if (menu.title === null) return [deleteItem]
    const renameItem: MenuItem = {
      key: 'rename-slug',
      label: 'slugを変更',
      icon: 'edit',
      onSelect: () => onRenameSlug(menu.slug),
    }
    const exportZipItem: MenuItem = {
      key: 'export-zip',
      label: 'zipで書き出す',
      icon: 'folder_zip',
      onSelect: () => onExportZip(menu.slug),
    }
    return isAndroid ? [renameItem, exportZipItem, deleteItem] : [renameItem, deleteItem]
  }

  return (
    <>
      <aside className="sidebar">
        <div className="sidebar-header">
          <span className="sidebar-title">記事</span>
          <button
            type="button"
            className="icon-button"
            aria-label="新しい記事"
            title="新しい記事"
            onClick={onNew}
          >
            <Icon name="add" />
          </button>
        </div>
        <nav aria-label="記事一覧" className="sidebar-nav">
          {articles.status === 'loading' ? null : articles.status === 'failed' ? (
            <>
              <p className="sidebar-message">記事一覧を読めませんでした</p>
              {articles.path && <p className="sidebar-message">{articles.path}</p>}
              {articles.reason && <p className="sidebar-message">{articles.reason}</p>}
            </>
          ) : (
            <>
              {articles.articles.length === 0 && articles.broken.length === 0 && (
                <p className="sidebar-message">記事がありません</p>
              )}
              {articles.articles.map((article) => (
                <button
                  key={article.slug}
                  type="button"
                  className="article-row"
                  aria-current={article.slug === selectedSlug ? 'true' : undefined}
                  onClick={() => onSelect(article.slug)}
                  onContextMenu={(e) => openRowMenu(e, article.slug, article.title)}
                >
                  <span className="article-row-title">
                    {article.title || '無題'}
                    {article.warning && (
                      <span className="article-row-warning">
                        <Icon name="warning" />
                        <span className="visually-hidden">警告あり</span>
                      </span>
                    )}
                  </span>
                  <span className="article-row-detail">
                    {article.pubDate === null
                      ? article.slug
                      : `${article.slug} · ${article.pubDate}`}
                  </span>
                </button>
              ))}
              {articles.broken.map((broken) => (
                <button
                  key={broken.slug}
                  type="button"
                  className="article-row article-row-broken"
                  onClick={() => setBrokenSlug(broken.slug)}
                  onContextMenu={(e) => openRowMenu(e, broken.slug, null)}
                >
                  <span className="article-row-title">読めない記事</span>
                  <span className="article-row-detail">{broken.slug}</span>
                </button>
              ))}
              {articles.unreadable.length > 0 && (
                <button
                  type="button"
                  className="sidebar-message sidebar-unreadable"
                  onClick={() => setUnreadableOpen(true)}
                >
                  読めない項目が{articles.unreadable.length}件あります
                </button>
              )}
            </>
          )}
        </nav>
        <div className="sidebar-footer">
          <button type="button" className="sidebar-settings" onClick={onSettings}>
            <Icon name="settings" />
            設定
          </button>
        </div>
      </aside>
      {brokenDialog && (
        <DialogDetail title="読めない記事" onClose={() => setBrokenSlug(null)}>
          <p>slug: {brokenDialog.slug}</p>
          <p>理由: {brokenDialog.reason}</p>
          <p>{brokenDialog.path}</p>
        </DialogDetail>
      )}
      {unreadableOpen && articles.status === 'loaded' && (
        <DialogDetail title="読めない項目" onClose={() => setUnreadableOpen(false)}>
          {articles.unreadable.map((entry, index) => (
            <p key={`${entry.path} ${index}`}>
              {entry.path}
              <br />
              {entry.reason}
            </p>
          ))}
        </DialogDetail>
      )}
      {rowMenu && (
        <ContextMenu x={rowMenu.x} y={rowMenu.y} items={rowMenuItems(rowMenu)} onClose={() => setRowMenu(null)} />
      )}
    </>
  )
}
