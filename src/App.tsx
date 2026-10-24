import { useEffect, useRef, useState } from 'react'
import { getCurrentWindow } from '@tauri-apps/api/window'
import {
  deleteArticle,
  getPostsDir,
  getPostsDirName,
  listArticles,
  pickPostsDir,
  readArticle,
  exportZip,
  type Article,
  type EditedFields,
  type StoreError,
} from './api'
import ArticleEditor, { type ArticleEditorHandle } from './ArticleEditor'
import { initialSaveStatus, whenSaved, type SaveStatus } from './autoSave'
import ConfirmDialog from './ConfirmDialog'
import { Drawer } from './Drawer'
import { isAndroid } from './platform'
import SaveConfirmDialog from './SaveConfirmDialog'
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

interface SaveConfirm {
  action: PendingAction
  title: string
}

export default function App() {
  const drawer = useDrawer()
  const [articles, setArticles] = useState<ArticlesState>({ status: 'loading' })
  // デスクトップは記事のディレクトリの絶対パス、Androidはzipの中の`src/content/`の下の名前
  const [postsDir, setPostsDir] = useState<string | null>(null)
  const [article, setArticle] = useState<ArticleState>({ status: 'none' })
  const [slugDialog, setSlugDialog] = useState<SlugDialogMode | null>(null)
  const [saveStatus, setSaveStatus] = useState<SaveStatus>(initialSaveStatus)
  const [saveConfirm, setSaveConfirm] = useState<SaveConfirm | null>(null)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [deleteTarget, setDeleteTarget] = useState<ArticleRef | null>(null)
  // 同じ文言が続いても出し直すよう、包んで持つ
  const [notice, setNotice] = useState<Notice | null>(null)
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
        setArticles({
          status: 'loaded',
          articles: list.articles,
          broken: list.broken,
          unreadable: list.unreadable,
        })
      })
      .catch((e) => {
        if (request !== listRequest.current) return
        console.error('記事一覧を読めませんでした', e)
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
    ;(isAndroid ? getPostsDirName() : getPostsDir())
      .then(setPostsDir)
      .catch((e) => console.error('記事のディレクトリを読めませんでした', e))
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
            setNotice({ text: 'zipを書き出しました', error: false })
            break
          case 'cancelled':
            break
          case 'leftIncomplete':
            console.error('zipを書き写せませんでした', outcome.reason)
            setNotice({
              text: '書き出しが途中で止まりました。保存先のファイルを消してください',
              error: true,
            })
            break
          default: {
            const unknown: never = outcome
            console.error('書き出しの結果が分かりません', unknown)
          }
        }
      })
      .catch((e) => {
        console.error('zipを書き出せませんでした', e)
        setNotice({ text: 'zipを書き出せませんでした', error: true })
      })
      .finally(() => {
        exporting.current = false
      })
  }

  // 開いている記事は前のディレクトリのものなので、選び直したら閉じる
  const pickPostsDirAndReload = async () => {
    if (!(await saveOpenArticle())) throw new Error('開いている記事を保存できませんでした')
    await whenSaved()
    const picked = await pickPostsDir()
    if (picked === null) return
    setArticle({ status: 'none' })
    setPostsDir(picked)
    reloadArticles()
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
      console.error('記事を削除できませんでした', slug, e)
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

  return (
    <div className={drawer.narrow ? 'layout narrow' : 'layout'}>
      <Drawer drawer={drawer}>{sidebar}</Drawer>
      <main inert={drawer.shown} {...swipeToOpen}>
        <TopBar
          drawer={drawer}
          onInsertLink={loaded ? () => editor.current?.insertLink() : null}
          onInsertImages={loaded ? (files) => editor.current?.insertImages(files) : null}
          saveStatus={loaded ? saveStatus : null}
          notice={notice}
        />
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
              />
            ) : article.status === 'failed' ? (
              <p className="editor-message">記事を読めませんでした</p>
            ) : null}
          </div>
        </div>
      </main>
      {slugDialog && (
        <SlugDialog
          mode={slugDialog}
          destination={
            postsDir === null
              ? null
              : isAndroid
                ? `./src/content/${postsDir}`
                : postsDir
          }
          onDone={onDialogDone}
          onCancel={() => setSlugDialog(null)}
        />
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
      {deleteTarget && (
        <ConfirmDialog
          title="記事を削除しますか"
          confirmLabel="削除する"
          failedText="削除できませんでした"
          onCancel={() => setDeleteTarget(null)}
          onConfirm={() => deleteConfirmed(deleteTarget)}
        >
          {deleteTarget.kind === 'broken'
            ? deleteTarget.slug
            : `「${deleteTarget.title || '無題'}」(${deleteTarget.slug})`}
          を削除します。画像とバックアップも消え、元に戻せません。
        </ConfirmDialog>
      )}
      {settingsOpen && (
        <Settings
          narrow={drawer.narrow}
          postsDir={postsDir}
          onPickPostsDir={pickPostsDirAndReload}
          onPostsDirNameChange={setPostsDir}
          onClose={() => setSettingsOpen(false)}
        />
      )}
    </div>
  )
}
