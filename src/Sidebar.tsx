import Icon from './Icon'
import type { ArticleSummary } from './api'

export type ArticlesState =
  | { status: 'loading' }
  | { status: 'failed' }
  | { status: 'loaded'; articles: ArticleSummary[] }

interface SidebarProps {
  articles: ArticlesState
  selectedSlug: string | null
  onSelect: (slug: string) => void
  onNew: () => void
  onSettings: () => void
}

export default function Sidebar({ articles, selectedSlug, onSelect, onNew, onSettings }: SidebarProps) {
  return (
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
          <p className="sidebar-message">記事一覧を読めませんでした</p>
        ) : articles.articles.length === 0 ? (
          <p className="sidebar-message">記事がありません</p>
        ) : (
          articles.articles.map((article) => (
            <button
              key={article.slug}
              type="button"
              className="article-row"
              aria-current={article.slug === selectedSlug ? 'true' : undefined}
              onClick={() => onSelect(article.slug)}
            >
              <span className="article-row-title">{article.title || '無題'}</span>
              <span className="article-row-detail">
                {article.slug} · {article.pubDate}
              </span>
            </button>
          ))
        )}
      </nav>
      <div className="sidebar-footer">
        <button type="button" className="sidebar-settings" onClick={onSettings}>
          <Icon name="settings" />
          設定
        </button>
      </div>
    </aside>
  )
}
