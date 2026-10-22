import { useEffect, useState } from 'react'
import { listArticles, readArticle, type Article } from './api'
import Editor from './Editor'
import { Drawer } from './Drawer'
import Sidebar from './Sidebar'
import type { ArticlesState } from './Sidebar'
import TopBar from './TopBar'
import { useSwipeToOpen } from './useDrawerSwipe'
import { useDrawer } from './useDrawer'

type ArticleState =
  | { status: 'none' }
  | { status: 'loading' }
  | { status: 'loaded'; article: Article }
  | { status: 'failed' }

export default function App() {
  const drawer = useDrawer()
  // 広い窓のサイドバーの開け閉め。狭い窓では引き出し(drawer)が開け閉めを担う。
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const [selectedSlug, setSelectedSlug] = useState<string | null>(null)
  const [articles, setArticles] = useState<ArticlesState>({ status: 'loading' })
  const [article, setArticle] = useState<ArticleState>({ status: 'none' })

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

  // 切り替えた後に返ってきた前の記事の読み込みは捨てる。
  useEffect(() => {
    if (selectedSlug === null) return
    let stale = false
    readArticle(selectedSlug)
      .then((loaded) => {
        if (!stale) setArticle({ status: 'loaded', article: loaded })
      })
      .catch((e) => {
        if (stale) return
        console.error('記事を読めませんでした', e)
        setArticle({ status: 'failed' })
      })
    return () => {
      stale = true
    }
  }, [selectedSlug])

  // 'loading'はeffectの中で同期的にsetStateしないよう、ここで入れる。同じ記事を選び直したときは
  // effectが走らないので入れない。
  const selectArticle = (slug: string) => {
    if (slug !== selectedSlug) {
      setSelectedSlug(slug)
      setArticle({ status: 'loading' })
    }
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
          <div className="editor-column">
            {article.status === 'loaded' ? (
              <Editor
                key={article.article.slug}
                slug={article.article.slug}
                doc={article.article.body}
              />
            ) : article.status === 'failed' ? (
              <p className="editor-message">記事を読めませんでした</p>
            ) : null}
          </div>
        </div>
      </main>
    </div>
  )
}
