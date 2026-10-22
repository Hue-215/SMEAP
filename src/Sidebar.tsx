import Icon from './Icon'
import type { ArticleSummary } from './api'

interface SidebarProps {
  // 記事の一覧。読み込み中はnull。
  articles: ArticleSummary[] | null
  loadFailed: boolean
  selectedSlug: string | null
  onSelect: (slug: string) => void
}

// 記事一覧のサイドバー。広い窓では並びの中に、狭い窓では.drawerの中に入る。上に新しい記事の
// ボタン、下に設定への入口を置く。選択中の記事は、左の線(アクセント色)で示す。
export default function Sidebar({ articles, loadFailed, selectedSlug, onSelect }: SidebarProps) {
  return (
    <aside className="sidebar">
      <div className="sidebar-header">
        <span className="sidebar-title">記事</span>
        <button
          type="button"
          className="icon-button"
          aria-label="新しい記事"
          title="新しい記事"
          disabled
        >
          <Icon name="add" />
        </button>
      </div>
      <nav aria-label="記事一覧" className="sidebar-nav">
        {loadFailed ? (
          <p className="sidebar-error">記事一覧を読めませんでした</p>
        ) : (
          articles?.map((article) => (
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
        <button type="button" className="sidebar-settings" disabled>
          <Icon name="settings" />
          設定
        </button>
      </div>
    </aside>
  )
}
