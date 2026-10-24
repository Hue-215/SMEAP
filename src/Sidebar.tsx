import { useId, useState, type MouseEvent, type ReactNode } from 'react'
import type { ArticleSummary, BrokenArticle, UnreadableEntry } from './api'
import ContextMenu, { type MenuItem } from './ContextMenu'
import Dialog from './Dialog'
import Icon from './Icon'
import { t } from './i18n'
import { isAndroid } from './platform'

// 一覧の行。brokenは読めない記事(BrokenArticle)
export type ArticleRef =
  | { kind: 'article'; slug: string; title: string }
  | { kind: 'broken'; slug: string }

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
  onDelete: (target: ArticleRef) => void
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
            {t('common.close')}
          </button>
        </div>
      </div>
    </Dialog>
  )
}

// 右クリック(Androidは長押し)で開いている行のメニュー
type RowMenu = ArticleRef & { x: number; y: number }

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

  // キーボードから開くと座標が0になる環境があるので、そのときは行の左下に出す
  const openRowMenu = (e: MouseEvent<HTMLElement>, target: ArticleRef) => {
    e.preventDefault()
    let { clientX: x, clientY: y } = e
    if (x === 0 && y === 0) {
      const rect = e.currentTarget.getBoundingClientRect()
      x = rect.left
      y = rect.bottom
    }
    setRowMenu({ ...target, x, y })
  }

  const rowMenuItems = (menu: RowMenu): MenuItem[] => {
    const deleteItem: MenuItem = {
      key: 'delete',
      label: t('sidebar.menu_delete'),
      icon: 'delete',
      onSelect: () => onDelete(menu),
    }
    if (menu.kind === 'broken') return [deleteItem]
    const renameItem: MenuItem = {
      key: 'rename-slug',
      label: t('sidebar.menu_rename'),
      icon: 'edit',
      onSelect: () => onRenameSlug(menu.slug),
    }
    const exportZipItem: MenuItem = {
      key: 'export-zip',
      label: t('sidebar.menu_export'),
      icon: 'folder_zip',
      onSelect: () => onExportZip(menu.slug),
    }
    return isAndroid ? [renameItem, exportZipItem, deleteItem] : [renameItem, deleteItem]
  }

  return (
    <>
      <aside className="sidebar">
        <div className="sidebar-header">
          <span className="sidebar-title">{t('sidebar.heading')}</span>
          <button
            type="button"
            className="icon-button"
            aria-label={t('sidebar.new_article')}
            title={t('sidebar.new_article')}
            onClick={onNew}
          >
            <Icon name="add" />
          </button>
        </div>
        <nav aria-label={t('sidebar.nav_label')} className="sidebar-nav">
          {articles.status === 'loading' ? null : articles.status === 'failed' ? (
            <>
              <p className="sidebar-message">{t('sidebar.load_failed')}</p>
              {articles.path && <p className="sidebar-message">{articles.path}</p>}
              {articles.reason && <p className="sidebar-message">{articles.reason}</p>}
            </>
          ) : (
            <>
              {articles.articles.length === 0 && articles.broken.length === 0 && (
                <p className="sidebar-message">{t('sidebar.empty')}</p>
              )}
              {articles.articles.map((article) => (
                <button
                  key={article.slug}
                  type="button"
                  className="article-row"
                  aria-current={article.slug === selectedSlug ? 'true' : undefined}
                  onClick={() => onSelect(article.slug)}
                  onContextMenu={(e) =>
                    openRowMenu(e, { kind: 'article', slug: article.slug, title: article.title })
                  }
                >
                  <span className="article-row-title">
                    {article.title || t('common.untitled')}
                    {article.warning && (
                      <span className="article-row-warning">
                        <Icon name="warning" />
                        <span className="visually-hidden">{t('sidebar.has_warning')}</span>
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
                  onContextMenu={(e) => openRowMenu(e, { kind: 'broken', slug: broken.slug })}
                >
                  <span className="article-row-title">{t('sidebar.broken_article')}</span>
                  <span className="article-row-detail">{broken.slug}</span>
                </button>
              ))}
              {articles.unreadable.length > 0 && (
                <button
                  type="button"
                  className="sidebar-message sidebar-unreadable"
                  onClick={() => setUnreadableOpen(true)}
                >
                  {t('sidebar.unreadable_count', { count: articles.unreadable.length })}
                </button>
              )}
            </>
          )}
        </nav>
        <div className="sidebar-footer">
          <button type="button" className="sidebar-settings" onClick={onSettings}>
            <Icon name="settings" />
            {t('sidebar.settings')}
          </button>
        </div>
      </aside>
      {brokenDialog && (
        <DialogDetail title={t('sidebar.broken_article')} onClose={() => setBrokenSlug(null)}>
          <p>slug: {brokenDialog.slug}</p>
          <p>{t('sidebar.broken_reason', { reason: brokenDialog.reason })}</p>
          <p>{brokenDialog.path}</p>
        </DialogDetail>
      )}
      {unreadableOpen && articles.status === 'loaded' && (
        <DialogDetail title={t('sidebar.unreadable_title')} onClose={() => setUnreadableOpen(false)}>
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
