import { useEffect, useRef, useState } from 'react'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { listArticles, readArticle, type Article, type ArticleFields } from './api'
import ArticleEditor, { type ArticleEditorHandle } from './ArticleEditor'
import { initialSaveStatus, whenSaved, type SaveStatus } from './autoSave'
import { Drawer } from './Drawer'
import { isAndroid } from './platform'
import SaveConfirmDialog from './SaveConfirmDialog'
import Sidebar from './Sidebar'
import type { ArticlesState } from './Sidebar'
import SlugDialog, { type SlugDialogMode } from './SlugDialog'
import TopBar from './TopBar'
import { useSwipeToOpen } from './useDrawerSwipe'
import { useDrawer } from './useDrawer'

type ArticleState =
  | { status: 'none' }
  | { status: 'loading' | 'failed'; slug: string }
  | { status: 'loaded'; slug: string; article: Article }

// 保存していない変更があるときに、確かめてから行う操作
type PendingAction = { kind: 'switch'; slug: string } | { kind: 'close' }

interface SaveConfirm {
  action: PendingAction
  title: string
}

export default function App() {
  const drawer = useDrawer()
  // 広い窓のサイドバーの開け閉め。狭い窓では引き出し(drawer)が開け閉めを担う。
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const [articles, setArticles] = useState<ArticlesState>({ status: 'loading' })
  const [article, setArticle] = useState<ArticleState>({ status: 'none' })
  const [slugDialog, setSlugDialog] = useState<SlugDialogMode | null>(null)
  const [saveStatus, setSaveStatus] = useState<SaveStatus>(initialSaveStatus)
  const [saveConfirm, setSaveConfirm] = useState<SaveConfirm | null>(null)
  // 同じ文言が続いても出し直すよう、包んで持つ
  const [notice, setNotice] = useState<{ text: string } | null>(null)
  const editor = useRef<ArticleEditorHandle>(null)
  const selectedSlug = article.status === 'none' ? null : article.slug
  const loaded = article.status === 'loaded' ? article : null

  // 後から出した読み込みが先に返ったら、前の読み込みの結果は捨てる
  const listRequest = useRef(0)
  const reloadArticles = () => {
    const request = ++listRequest.current
    listArticles()
      .then((list) => {
        if (request !== listRequest.current) return
        if (list.broken.length > 0) console.warn('読めない記事があります', list.broken)
        setArticles({ status: 'loaded', articles: list.articles })
      })
      .catch((e) => {
        if (request !== listRequest.current) return
        console.error('記事一覧を読めませんでした', e)
        setArticles({ status: 'failed' })
      })
  }

  useEffect(() => {
    reloadArticles()
  }, [])

  useEffect(() => {
    if (!notice) return
    const timer = setTimeout(() => setNotice(null), 5000)
    return () => clearTimeout(timer)
  }, [notice])

  // Tauriのドロップの受け取りを切っているので、編集欄の外にファイルを落とすとWebViewがそのファイルを開いてしまう
  useEffect(() => {
    const block = (e: DragEvent) => {
      if (e.dataTransfer?.types.includes('Files')) e.preventDefault()
    }
    document.addEventListener('dragover', block)
    document.addEventListener('drop', block)
    return () => {
      document.removeEventListener('dragover', block)
      document.removeEventListener('drop', block)
    }
  }, [])

  const openArticle = (slug: string) => {
    setArticle({ status: 'loading', slug })
    // 切り替えた後に返ってきた前の記事の読み込みは捨てる
    const settle = (next: ArticleState) =>
      setArticle((current) =>
        current.status === 'loading' && current.slug === slug ? next : current,
      )
    whenSaved()
      .then(() => readArticle(slug))
      .then((loaded) => settle({ status: 'loaded', slug, article: loaded }))
      .catch((e) => {
        console.error('記事を読めませんでした', e)
        settle({ status: 'failed', slug })
      })
  }

  // 読めなかった記事は、選び直したら読み直す。
  const selectArticle = (slug: string) => {
    drawer.close()
    if (slug === selectedSlug && article.status !== 'failed') return
    if (!askToSave({ kind: 'switch', slug })) openArticle(slug)
  }

  // 保存していない変更があれば確認のダイアログを出してtrue
  const askToSave = (action: PendingAction) => {
    const current = editor.current
    if (!current?.isDirty()) return false
    setSaveConfirm({ action, title: current.title() })
    return true
  }

  // 開いている記事が無ければ保存済みとみなす
  const saveOpenArticle = () => editor.current?.save() ?? Promise.resolve(true)

  const proceed = (action: PendingAction) => {
    setSaveConfirm(null)
    if (action.kind === 'switch') openArticle(action.slug)
    else getCurrentWindow().destroy().catch((e) => console.error('ウィンドウを閉じられませんでした', e))
  }

  useEffect(() => {
    if (isAndroid) return
    let disposed = false
    let unlisten: (() => void) | null = null
    getCurrentWindow()
      .onCloseRequested(async (event) => {
        await whenSaved()
        if (askToSave({ kind: 'close' })) event.preventDefault()
      })
      .then((fn) => {
        if (disposed) fn()
        else unlisten = fn
      })
      .catch((e) => console.error('ウィンドウを閉じる操作を受けられません', e))
    return () => {
      disposed = true
      unlisten?.()
    }
  }, [])

  // slugの変更はファイルから読んだ内容で行い、新しい記事は開いていた記事と入れ替わるので、先に保存する
  const openSlugDialog = (mode: SlugDialogMode) => {
    void saveOpenArticle().then((ok) => {
      if (ok) setSlugDialog(mode)
    })
  }

  const sidebar = (
    <Sidebar
      articles={articles}
      selectedSlug={selectedSlug}
      onSelect={selectArticle}
      onNew={() => {
        drawer.close()
        openSlugDialog({ kind: 'create' })
      }}
    />
  )

  // 一覧に出るのはタイトルと公開日
  const onSaved = (fields: ArticleFields) => {
    if (articles.status !== 'loaded' || article.status !== 'loaded') return
    const listed = articles.articles.find((a) => a.slug === article.slug)
    if (listed?.title !== fields.title || listed.pubDate !== fields.pubDate) reloadArticles()
  }

  const swipeToOpen = useSwipeToOpen(drawer)
  const onDialogDone = (a: Article) => {
    setSlugDialog(null)
    reloadArticles()
    setArticle({ status: 'loaded', slug: a.slug, article: a })
  }

  return (
    <div className={drawer.narrow ? 'layout narrow' : 'layout'}>
      {(drawer.narrow || sidebarOpen) && <Drawer drawer={drawer}>{sidebar}</Drawer>}
      <main inert={drawer.shown} {...swipeToOpen}>
        <TopBar
          drawer={drawer}
          sidebarOpen={sidebarOpen}
          onToggleSidebar={() => setSidebarOpen((v) => !v)}
          onRenameSlug={
            loaded ? () => openSlugDialog({ kind: 'rename', slug: loaded.slug }) : null
          }
          onInsertLink={loaded ? () => editor.current?.insertLink() : null}
          onInsertImages={loaded ? (files) => editor.current?.insertImages(files) : null}
          saveStatus={loaded ? saveStatus : null}
          notice={notice?.text ?? null}
        />
        <div className="editor-scroll">
          <div className="editor-column">
            {article.status === 'loaded' ? (
              <ArticleEditor
                key={article.slug}
                ref={editor}
                article={article.article}
                paused={saveConfirm !== null}
                onSaveStatus={setSaveStatus}
                onSaved={onSaved}
                onNotice={(text) => setNotice({ text })}
              />
            ) : article.status === 'failed' ? (
              <p className="editor-message">記事を読めませんでした</p>
            ) : null}
          </div>
        </div>
      </main>
      {slugDialog && (
        <SlugDialog mode={slugDialog} onDone={onDialogDone} onCancel={() => setSlugDialog(null)} />
      )}
      {saveConfirm && (
        <SaveConfirmDialog
          title={saveConfirm.title}
          savedAt={saveStatus.savedAt}
          onCancel={() => setSaveConfirm(null)}
          onDiscard={() => proceed(saveConfirm.action)}
          onSave={async () => {
            const ok = await saveOpenArticle()
            if (ok) proceed(saveConfirm.action)
            return ok
          }}
        />
      )}
    </div>
  )
}
