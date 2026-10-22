import { useEffect, useState } from 'react'
import { listArticles } from './api'
import type { ArticleSummary } from './api'
import { Drawer } from './Drawer'
import Sidebar from './Sidebar'
import TopBar from './TopBar'
import { useSwipeToOpen } from './useDrawerSwipe'
import { useDrawer } from './useDrawer'

export default function App() {
  const drawer = useDrawer()
  // 広い窓のサイドバーの開け閉め。既定は開く。狭い窓では引き出し(drawer)が開け閉めを担う。
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const [selectedSlug, setSelectedSlug] = useState<string | null>(null)
  // 記事の一覧。読み込み中はnull。
  const [articles, setArticles] = useState<ArticleSummary[] | null>(null)
  const [loadFailed, setLoadFailed] = useState(false)

  useEffect(() => {
    listArticles()
      .then((list) => setArticles(list.articles))
      .catch(() => setLoadFailed(true))
  }, [])

  const selectArticle = (slug: string) => {
    setSelectedSlug(slug)
    drawer.close()
  }

  const sidebar = (
    <Sidebar
      articles={articles}
      loadFailed={loadFailed}
      selectedSlug={selectedSlug}
      onSelect={selectArticle}
    />
  )

  const swipeToOpen = useSwipeToOpen(drawer)

  return (
    <div className={drawer.narrow ? 'layout narrow' : 'layout'}>
      {(drawer.narrow || sidebarOpen) && <Drawer drawer={drawer}>{sidebar}</Drawer>}
      <main inert={drawer.shown} {...swipeToOpen}>
        <TopBar
          drawer={drawer}
          sidebarOpen={sidebarOpen}
          onToggleSidebar={() => setSidebarOpen((v) => !v)}
        />
        <div className="editor-scroll">
          <div className="editor-column" />
        </div>
      </main>
    </div>
  )
}
