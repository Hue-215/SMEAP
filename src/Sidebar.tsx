import { useId, useState } from 'react'
import type { ArticleSummary, BrokenArticle, UnreadableEntry } from './api'
import Dialog from './Dialog'
import Icon from './Icon'

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
}

function BrokenDialog({ broken, onClose }: { broken: BrokenArticle; onClose: () => void }) {
  const headingId = useId()
  return (
    <Dialog labelledBy={headingId} onCancel={onClose}>
      <div className="dialog-form">
        <h2 id={headingId} className="dialog-title">
          読めない記事
        </h2>
        <div className="dialog-detail">
          <p>slug: {broken.slug}</p>
          <p>理由: {broken.reason}</p>
          <p className="dialog-detail-path">{broken.path}</p>
        </div>
        <div className="dialog-buttons">
          <button type="button" onClick={onClose}>
            閉じる
          </button>
        </div>
      </div>
    </Dialog>
  )
}

function UnreadableDialog({
  unreadable,
  onClose,
}: {
  unreadable: UnreadableEntry[]
  onClose: () => void
}) {
  const headingId = useId()
  return (
    <Dialog labelledBy={headingId} onCancel={onClose}>
      <div className="dialog-form">
        <h2 id={headingId} className="dialog-title">
          読めない項目
        </h2>
        <div className="dialog-detail">
          {unreadable.map((entry, index) => (
            <p key={`${entry.path} ${index}`}>
              <span className="dialog-detail-path">{entry.path}</span>
              <br />
              {entry.reason}
            </p>
          ))}
        </div>
        <div className="dialog-buttons">
          <button type="button" onClick={onClose}>
            閉じる
          </button>
        </div>
      </div>
    </Dialog>
  )
}

export default function Sidebar({ articles, selectedSlug, onSelect, onNew, onSettings }: SidebarProps) {
  const [brokenSlug, setBrokenSlug] = useState<string | null>(null)
  const [unreadableOpen, setUnreadableOpen] = useState(false)
  const brokenDialog =
    articles.status === 'loaded' && brokenSlug !== null
      ? (articles.broken.find((b) => b.slug === brokenSlug) ?? null)
      : null
  const unreadableDialog = articles.status === 'loaded' ? articles.unreadable : []

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
                >
                  <span className="article-row-title">読めない記事</span>
                  <span className="article-row-detail">{broken.slug}</span>
                </button>
              ))}
              {articles.unreadable.length > 0 && (
                <button
                  type="button"
                  className="sidebar-unreadable"
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
      {brokenDialog && <BrokenDialog broken={brokenDialog} onClose={() => setBrokenSlug(null)} />}
      {unreadableOpen && (
        <UnreadableDialog unreadable={unreadableDialog} onClose={() => setUnreadableOpen(false)} />
      )}
    </>
  )
}
