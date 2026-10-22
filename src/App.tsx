import { useEffect, useState } from 'react'
import { listArticles } from './api'
import { Drawer } from './Drawer'
import Sidebar from './Sidebar'
import type { ArticlesState } from './Sidebar'
import TopBar from './TopBar'
import { useSwipeToOpen } from './useDrawerSwipe'
import { useDrawer } from './useDrawer'

export default function App() {
  const drawer = useDrawer()
  // 広い窓のサイドバーの開け閉め。狭い窓では引き出し(drawer)が開け閉めを担う。
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const [selectedSlug, setSelectedSlug] = useState<string | null>(null)
  const [articles, setArticles] = useState<ArticlesState>({ status: 'loading' })

  useEffect(() => {
    listArticles()
      .then((list) => {
        if (list.broken.length > 0) console.warn('読めない記事があります', list.broken)
        setArticles({ status: 'loaded', articles: list.articles })
      })
      .catch((e) => {
        console.error('記事一覧を読めませんでした', e)
        setArticles({ status: 'failed' })
      })
  }, [])

  const selectArticle = (slug: string) => {
    setSelectedSlug(slug)
    drawer.close()
  }

  const sidebar = (
    <Sidebar
      articles={articles}
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
