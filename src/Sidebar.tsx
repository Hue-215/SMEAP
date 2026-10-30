import { Fragment, useId, useState, type MouseEvent, type ReactNode } from 'react'
import type { ArticleSummary, BrokenArticle, GithubDeleting, UnreadableEntry } from './api'
import { githubErrorText } from './githubError'
import {
  fetchLineText,
  groupArticles,
  rowMenuActions,
  type ArticleRow,
  type GithubArticleStates,
} from './githubList'
import ContextMenu, { type MenuItem } from './ContextMenu'
import Dialog from './Dialog'
import Icon from './Icon'
import { t } from './i18n'
import { isAndroid } from './platform'

// 一覧の行。brokenは読めない記事(BrokenArticle)
export type ArticleRef =
  { kind: 'article'; slug: string; title: string } | { kind: 'broken'; slug: string }

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

// GitHub連携中の状態。nullは連携していない
export interface SidebarGithub {
  states: GithubArticleStates
  syncing: boolean
  failed: { error: unknown } | null
}

interface SidebarProps {
  articles: ArticlesState
  selectedSlug: string | null
  github: SidebarGithub | null
  onSelect: (slug: string) => void
  onNew: () => void
  onSync: () => void
  onSettings: () => void
  onRenameSlug: (slug: string) => void
  onExportZip: (slug: string) => void
  onDelete: (target: ArticleRef) => void
  onDiscard: (target: ArticleRef) => void
  // 公開済みの記事の削除・非公開
  onRemove: (target: ArticleRef, kind: GithubDeleting) => void
}

// 削除予定の行の印と、その取りやめのメニュー
const deletingTexts = {
  delete: { icon: 'delete', label: 'sidebar.deleting', discard: 'sidebar.menu_discard_delete' },
  unpublish: {
    icon: 'visibility_off',
    label: 'sidebar.unpublishing',
    discard: 'sidebar.menu_discard_unpublish',
  },
} as const

const categoryLabel = {
  drafting: 'sidebar.group_drafting',
  editing: 'sidebar.group_editing',
  published: 'sidebar.group_published',
} as const

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
  github,
  onSelect,
  onNew,
  onSync,
  onSettings,
  onRenameSlug,
  onExportZip,
  onDelete,
  onDiscard,
  onRemove,
}: SidebarProps) {
  const [brokenSlug, setBrokenSlug] = useState<string | null>(null)
  const [unreadableOpen, setUnreadableOpen] = useState(false)
  const [syncFailedOpen, setSyncFailedOpen] = useState(false)
  const [skippedOpen, setSkippedOpen] = useState(false)
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
    const state = github?.states.articles[menu.slug]
    const deleting = state?.deleting ?? null
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
    const unpublishItem: MenuItem = {
      key: 'unpublish',
      label: t('sidebar.menu_unpublish'),
      icon: 'visibility_off',
      onSelect: () => onRemove(menu, 'unpublish'),
    }
    // 公開済みの記事の削除は、手元ではなくGitHub上の記事用のbranchにcommitを送る
    const removeItem: MenuItem = {
      key: 'remove',
      label: t('sidebar.menu_delete'),
      icon: 'delete',
      onSelect: () => onRemove(menu, 'delete'),
    }
    const discardItem: MenuItem = {
      key: 'discard',
      label: t(deleting === null ? 'sidebar.menu_discard' : deletingTexts[deleting].discard),
      icon: 'undo',
      onSelect: () => onDiscard(menu),
    }
    const actions = rowMenuActions(github?.states ?? null, menu.slug)
    const published = state?.category === 'published' && deleting === null
    return [
      ...(actions.includes('rename') ? [renameItem] : []),
      ...(isAndroid && github === null ? [exportZipItem] : []),
      ...(actions.includes('unpublish') ? [unpublishItem] : []),
      ...(actions.includes('delete') ? [published ? removeItem : deleteItem] : []),
      ...(actions.includes('discard') ? [discardItem] : []),
    ]
  }

  const groups =
    articles.status === 'loaded' ? groupArticles(articles.articles, github?.states ?? null) : []

  // 一覧の行。GitHubの状態があれば、変更と食い違いの印も出す
  const row = ({ article, state }: ArticleRow) => (
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
        {state?.conflict && (
          <span className="article-row-conflict">
            <Icon name="sync_problem" />
            <span className="visually-hidden">{t('sidebar.conflict')}</span>
          </span>
        )}
        {state?.deleting != null && (
          <span className="article-row-deleting">
            <Icon name={deletingTexts[state.deleting].icon} />
            <span className="visually-hidden">{t(deletingTexts[state.deleting].label)}</span>
          </span>
        )}
        {state?.dirty && (
          <span className="article-row-dirty">
            <span className="visually-hidden">{t('sidebar.dirty')}</span>
          </span>
        )}
      </span>
      <span className="article-row-detail">
        {article.pubDate === null ? article.slug : `${article.slug} · ${article.pubDate}`}
      </span>
    </button>
  )

  // 一覧の上の、GitHubからの読み込みの1行
  const githubLine = () => {
    if (github === null) return null
    if (github.states.stateUnreadable) {
      return (
        <p role="status" className="sidebar-github error">
          {t('common.github_state_unreadable')}
        </p>
      )
    }
    if (github.failed !== null) {
      return (
        <button
          type="button"
          className="sidebar-github error"
          onClick={() => setSyncFailedOpen(true)}
        >
          {fetchLineText(github.states.repo, github.states.fetchedAt, true)}
        </button>
      )
    }
    return (
      <p role="status" className="sidebar-github">
        {fetchLineText(github.states.repo, github.states.fetchedAt, false)}
      </p>
    )
  }

  return (
    <>
      <aside className="sidebar">
        <div className="sidebar-header">
          <span className="sidebar-title">{t('sidebar.heading')}</span>
          {github !== null && (
            <button
              type="button"
              className="icon-button"
              aria-label={t('sidebar.sync')}
              title={t('sidebar.sync')}
              onClick={onSync}
              disabled={github.syncing}
            >
              <Icon
                name="sync"
                className={github.syncing ? 'icon-spinning icon-spinning-ccw' : undefined}
              />
            </button>
          )}
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
        {githubLine()}
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
              {groups.map((group) => (
                <Fragment key={group.category ?? 'all'}>
                  {group.category !== null && (
                    <h2 className="sidebar-group">
                      {t(categoryLabel[group.category], { count: group.rows.length })}
                    </h2>
                  )}
                  {group.rows.map(row)}
                </Fragment>
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
              {github !== null && github.states.skipped.length > 0 && (
                <button
                  type="button"
                  className="sidebar-message sidebar-unreadable"
                  onClick={() => setSkippedOpen(true)}
                >
                  {t('sidebar.skipped_count', { count: github.states.skipped.length })}
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
        <DialogDetail
          title={t('sidebar.unreadable_title')}
          onClose={() => setUnreadableOpen(false)}
        >
          {articles.unreadable.map((entry, index) => (
            <p key={`${entry.path} ${index}`}>
              {entry.path}
              <br />
              {entry.reason}
            </p>
          ))}
        </DialogDetail>
      )}
      {syncFailedOpen && github !== null && github.failed !== null && (
        <DialogDetail title={t('sidebar.fetch_failed')} onClose={() => setSyncFailedOpen(false)}>
          <p>{githubErrorText(github.failed.error)}</p>
        </DialogDetail>
      )}
      {skippedOpen && github !== null && (
        <DialogDetail title={t('sidebar.skipped_title')} onClose={() => setSkippedOpen(false)}>
          {github.states.skipped.map((skipped, index) => (
            <p key={`${skipped.slug} ${skipped.branch} ${index}`}>
              {skipped.slug} · {skipped.branch}
            </p>
          ))}
        </DialogDetail>
      )}
      {rowMenu && (
        <ContextMenu
          x={rowMenu.x}
          y={rowMenu.y}
          items={rowMenuItems(rowMenu)}
          onClose={() => setRowMenu(null)}
        />
      )}
    </>
  )
}
