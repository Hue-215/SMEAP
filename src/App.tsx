import { useEffect, useRef, useState } from 'react'
import { getCurrentWindow } from '@tauri-apps/api/window'
import {
  deleteArticle,
  getPostsDir,
  getPostsDirName,
  listArticles,
  pickPostsDir,
  setPostsDirName,
  readArticle,
  exportZip,
  UnsavedArticleError,
  type Article,
  type EditedFields,
  type PostsDir,
  type StoreError,
} from './api'
import ArticleEditor, { type ArticleEditorHandle } from './ArticleEditor'
import { initialSaveStatus, whenSaved, type SaveStatus } from './autoSave'
import ConfirmDialog from './ConfirmDialog'
import { Drawer } from './Drawer'
import { t } from './i18n'
import { isAndroid } from './platform'
import SaveConfirmDialog from './SaveConfirmDialog'
import SearchBar from './SearchBar'
import type { FindStatus } from './search'
import Settings from './Settings'
import Sidebar from './Sidebar'
import type { ArticleRef, ArticlesState } from './Sidebar'
import SlugDialog, { type SlugDialogMode } from './SlugDialog'
import TopBar, { type Notice } from './TopBar'
import { useSwipeToOpen } from './useDrawerSwipe'
import { useDrawer } from './useDrawer'

type ArticleState =
  | { status: 'none' }
  | { status: 'loading' | 'failed'; slug: string }
  | { status: 'loaded'; slug: string; article: Article }

// 保存していない変更があるときに、確かめてから行う操作
type PendingAction = { kind: 'switch'; slug: string } | { kind: 'close' }

export default function App() {
  const drawer = useDrawer()
  const [articles, setArticles] = useState<ArticlesState>({ status: 'loading' })
  const [postsDir, setPostsDir] = useState<PostsDir | null>(null)
  const destination =
    postsDir === null
      ? null
      : postsDir.kind === 'desktop'
        ? postsDir.postsDir
        : `./src/content/${postsDir.name}`
  const [article, setArticle] = useState<ArticleState>({ status: 'none' })
  const [slugDialog, setSlugDialog] = useState<SlugDialogMode | null>(null)
  const [saveStatus, setSaveStatus] = useState<SaveStatus>(initialSaveStatus)
  const [saveConfirm, setSaveConfirm] = useState<PendingAction | null>(null)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [deleteTarget, setDeleteTarget] = useState<ArticleRef | null>(null)
  // 同じ文言が続いても出し直すよう、包んで持つ
  const [notice, setNotice] = useState<Notice | null>(null)
  const editor = useRef<ArticleEditorHandle>(null)
  const selectedSlug = article.status === 'none' ? null : article.slug
  const loaded = article.status === 'loaded' ? article : null

  // 記事内検索。文字は記事を切り替えても残す
  const [findFor, setFindFor] = useState<Article | null>(null)
  const [findQuery, setFindQuery] = useState('')
  const [findStatus, setFindStatus] = useState<FindStatus>({ current: -1, total: 0 })
  const [findFocus, setFindFocus] = useState(0)
  // 検索を開いた記事が読み込み済みの記事と違えば、閉じているとみなす
  const findOpen = loaded !== null && findFor === loaded.article

  // 後から出した読み込みが先に返ったら、前の読み込みの結果は捨てる
  const listRequest = useRef(0)
  const reloadArticles = () => {
    const request = ++listRequest.current
    listArticles()
      .then((list) => {
        if (request !== listRequest.current) return
        setArticles({
          status: 'loaded',
          articles: list.articles,
          broken: list.broken,
          unreadable: list.unreadable,
        })
      })
      .catch((e) => {
        if (request !== listRequest.current) return
        console.error('Failed to load the article list', e)
        const error = e as StoreError | string | undefined
        // Tauriは種類の無いエラーを文字列で渡す
        setArticles({
          status: 'failed',
          path: typeof error === 'string' ? undefined : error?.path,
          reason: typeof error === 'string' ? error : error?.reason,
        })
      })
  }

  useEffect(() => {
    reloadArticles()
    if (isAndroid) {
      getPostsDirName()
        .then((name) => setPostsDir({ kind: 'android', name }))
        .catch((e) => console.error('Failed to read the article directory', e))
    } else {
      getPostsDir()
        .then((location) => setPostsDir({ kind: 'desktop', ...location }))
        .catch((e) => console.error('Failed to read the article directory', e))
    }
  }, [])

  useEffect(() => {
    if (!notice) return
    const timer = setTimeout(() => setNotice(null), 5000)
    return () => clearTimeout(timer)
  }, [notice])

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
        console.error('Failed to read the article', e)
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
    setSaveConfirm(action)
    return true
  }

  // 開いている記事が無ければ保存済みとみなす
  const saveOpenArticle = () => editor.current?.save() ?? Promise.resolve(true)

  // Ctrl+F・Cmd+Fで検索を開く。受け手のeffectは依存を空にして付けっぱなしにするので、
  // 開く処理はref経由で最新のものを呼ぶ
  const openFindRef = useRef<(target: EventTarget | null) => void>(() => {})
  const openFind = (target: EventTarget | null) => {
    if (!loaded) return
    if (saveConfirm || deleteTarget || slugDialog || settingsOpen || drawer.shown) return
    if (target instanceof HTMLElement && target.closest('[role="menu"]')) return
    // 検索の箱・行の中にフォーカスがあるなら、文字を入れ直さず入力欄を選び直すだけ
    if (findFor === loaded.article && document.activeElement?.closest('[data-find]')) {
      setFindFocus((n) => n + 1)
      return
    }
    const query = editor.current?.beginFind() ?? findQuery
    setFindFor(loaded.article)
    setFindFocus((n) => n + 1)
    if (!query) return
    setFindQuery(query)
    editor.current?.find(query)
  }
  useEffect(() => {
    openFindRef.current = openFind
  })

  // 閉じる理由が何であれ、閉じたら下線を消す
  useEffect(() => {
    if (!findOpen) editor.current?.endFind()
  }, [findOpen])

  const closeFind = () => {
    setFindFor(null)
    editor.current?.focusBody()
  }

  const changeFindQuery = (query: string) => {
    setFindQuery(query)
    editor.current?.find(query)
  }

  // Ctrl+S・Cmd+Sで保存し、Ctrl+F・Cmd+Fで検索を開く(CapsLockやShift下では大文字になる)
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && (e.key === 's' || e.key === 'S')) {
        e.preventDefault()
        void saveOpenArticle()
        return
      }
      if ((e.ctrlKey || e.metaKey) && (e.key === 'f' || e.key === 'F')) {
        // 開けないときもWebViewの既定の動作は出さない
        e.preventDefault()
        openFindRef.current(e.target)
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [])

  const proceed = (action: PendingAction) => {
    setSaveConfirm(null)
    if (action.kind === 'switch') openArticle(action.slug)
    else getCurrentWindow().destroy().catch((e) => console.error('Failed to close the window', e))
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
      .catch((e) => console.error('Failed to handle the window close request', e))
    return () => {
      disposed = true
      unlisten?.()
    }
  }, [])

  // 新しい記事と、開いている記事のslugの変更は今の記事と入れ替わるので先に保存する。それ以外の
  // slugの変更では開いている記事をそのままにする
  const openSlugDialog = (mode: SlugDialogMode) => {
    if (mode.kind !== 'create' && mode.slug !== selectedSlug) {
      setSlugDialog(mode)
      return
    }
    void saveOpenArticle().then((ok) => {
      if (ok) setSlugDialog(mode)
    })
  }

  // 保存画面を出す前にzipを作るので、その間にもう一度押されても1つだけにする
  const exporting = useRef(false)
  // 書き出すのはファイルの内容なので、開いている記事なら先に保存する
  const openExportZip = (slug: string) => {
    if (exporting.current) return
    exporting.current = true
    const saved = slug === selectedSlug ? saveOpenArticle() : Promise.resolve(true)
    void saved
      .then(async (ok) => {
        if (!ok) return
        const outcome = await exportZip(slug)
        switch (outcome.kind) {
          case 'saved':
            setNotice({ text: t('app.export_done'), error: false })
            break
          case 'cancelled':
            break
          case 'leftIncomplete':
            console.error('Failed to copy the zip file', outcome.reason)
            setNotice({
              text: t('app.export_incomplete'),
              error: true,
            })
            break
          default: {
            const unknown: never = outcome
            console.error('Unknown export result', unknown)
          }
        }
      })
      .catch((e) => {
        console.error('Failed to export the zip file', e)
        setNotice({ text: t('app.export_failed'), error: true })
      })
      .finally(() => {
        exporting.current = false
      })
  }

  // 開いている記事は前のディレクトリのものなので、選び直したら閉じる
  const pickPostsDirAndReload = async () => {
    if (!(await saveOpenArticle())) throw new UnsavedArticleError()
    await whenSaved()
    const picked = await pickPostsDir()
    if (picked === null) return
    setArticle({ status: 'none' })
    setPostsDir({ kind: 'desktop', ...picked })
    reloadArticles()
  }

  const changePostsDirName = async (name: string) => {
    await setPostsDirName(name)
    setPostsDir({ kind: 'android', name })
  }

  const openSettings = () => {
    drawer.close()
    setSettingsOpen(true)
  }

  // 消すのが開いている記事なら、消せてから閉じる。未保存の変更は捨てる
  const deleteConfirmed = async (target: ArticleRef) => {
    const slug = target.slug
    try {
      // 自動保存と消す処理が重ならないよう、終わってからにする
      await whenSaved()
      await deleteArticle(slug)
      if (slug === selectedSlug) setArticle({ status: 'none' })
      setDeleteTarget(null)
      return true
    } catch (e) {
      console.error('Failed to delete the article', slug, e)
      return false
    } finally {
      reloadArticles()
    }
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
      onSettings={openSettings}
      onRenameSlug={(slug) => {
        drawer.close()
        openSlugDialog({ kind: 'rename', slug })
      }}
      onExportZip={(slug) => {
        drawer.close()
        openExportZip(slug)
      }}
      onDelete={(target) => {
        drawer.close()
        setDeleteTarget(target)
      }}
    />
  )

  // 一覧に出るのはタイトルと公開日
  const onSaved = (fields: EditedFields) => {
    if (articles.status !== 'loaded' || article.status !== 'loaded') return
    const listed = articles.articles.find((a) => a.slug === article.slug)
    if (listed?.title !== fields.title || listed.pubDate !== fields.pubDate) reloadArticles()
  }

  const swipeToOpen = useSwipeToOpen(drawer)
  // 新しい記事と、開いている記事のslugの変更は、今の記事を入れ替える。開いていない記事の
  // slugの変更は今の記事をそのままにし、一覧の読み直しだけにする
  const onDialogDone = (a: Article) => {
    setSlugDialog(null)
    reloadArticles()
    const replaces =
      slugDialog?.kind === 'create' ||
      (slugDialog?.kind === 'rename' && slugDialog.slug === selectedSlug)
    if (replaces) setArticle({ status: 'loaded', slug: a.slug, article: a })
  }

  const findBar = (variant: 'box' | 'bar') => (
    <SearchBar
      variant={variant}
      query={findQuery}
      status={findStatus}
      focusRequest={findFocus}
      onQueryChange={changeFindQuery}
      onNext={() => editor.current?.findNext()}
      onPrev={() => editor.current?.findPrev()}
      onClose={closeFind}
    />
  )

  return (
    <div className={drawer.narrow ? 'layout narrow' : 'layout'}>
      <Drawer drawer={drawer}>{sidebar}</Drawer>
      <main inert={drawer.shown} {...swipeToOpen}>
        {isAndroid && findOpen ? (
          findBar('bar')
        ) : (
          <TopBar
            drawer={drawer}
            onInsertLink={loaded ? () => editor.current?.insertLink() : null}
            onInsertImages={loaded ? (files) => editor.current?.insertImages(files) : null}
            saveStatus={loaded ? saveStatus : null}
            notice={notice}
            onFind={loaded ? () => openFindRef.current(null) : null}
          />
        )}
        <div className="editor-area">
          <div className="editor-scroll">
            <div className="editor-column">
              {article.status === 'loaded' ? (
                <ArticleEditor
                  key={article.slug}
                  ref={editor}
                  article={article.article}
                  paused={saveConfirm !== null || deleteTarget !== null}
                  onSaveStatus={setSaveStatus}
                  onSaved={onSaved}
                  onNotice={(text) => setNotice({ text, error: true })}
                  onFindStatus={setFindStatus}
                />
              ) : article.status === 'failed' ? (
                <p className="editor-message">{t('app.article_load_failed')}</p>
              ) : null}
            </div>
          </div>
          {!isAndroid && findOpen && findBar('box')}
        </div>
      </main>
      {slugDialog && (
        <SlugDialog
          mode={slugDialog}
          destination={destination}
          onDone={onDialogDone}
          onCancel={() => setSlugDialog(null)}
        />
      )}
      {saveConfirm && (
        <SaveConfirmDialog
          savedAt={saveStatus.savedAt}
          onCancel={() => setSaveConfirm(null)}
          onDiscard={() => proceed(saveConfirm)}
          onSave={async () => {
            const ok = await saveOpenArticle()
            if (ok) proceed(saveConfirm)
            return ok
          }}
        />
      )}
      {deleteTarget && (
        <ConfirmDialog
          title={t('app.delete_title')}
          confirmLabel={t('app.delete_confirm')}
          failedText={t('app.delete_failed')}
          onCancel={() => setDeleteTarget(null)}
          onConfirm={() => deleteConfirmed(deleteTarget)}
        >
          {deleteTarget.kind === 'broken'
            ? t('app.delete_message_broken', { slug: deleteTarget.slug })
            : t('app.delete_message', {
                title: deleteTarget.title || t('common.untitled'),
                slug: deleteTarget.slug,
              })}
        </ConfirmDialog>
      )}
      {settingsOpen && (
        <Settings
          narrow={drawer.narrow}
          postsDir={postsDir}
          onPickPostsDir={pickPostsDirAndReload}
          onChangePostsDirName={changePostsDirName}
          onClose={() => setSettingsOpen(false)}
        />
      )}
    </div>
  )
}
