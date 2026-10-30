import { useEffect, useRef, useState } from 'react'
import { getCurrentWindow } from '@tauri-apps/api/window'
import {
  DEFAULT_FRONTMATTER,
  deleteArticle,
  getFrontmatterSettings,
  getPostsDir,
  getPostsDirName,
  githubArticleState,
  githubArticleStates,
  githubCommitPreview,
  githubCommitPush,
  githubDiscardEdit,
  githubFetchImages,
  githubLogout,
  githubPull,
  githubRemovePublished,
  githubStatus,
  githubSync,
  githubWaitLogin,
  listArticles,
  pickPostsDir,
  setFrontmatterSettings,
  setPostsDirName,
  readArticle,
  exportZip,
  UnsavedArticleError,
  type Article,
  type CommitPreview,
  type EditedFields,
  type FrontmatterSettings,
  type GithubDeleting,
  type GithubError,
  type GithubSyncResult,
  type GithubWaitLogin,
  type PostsDir,
  type PushProgress,
  type SavedDates,
  type StoreError,
} from './api'
import ArticleEditor, { type ArticleEditorHandle } from './ArticleEditor'
import { initialSaveStatus, whenSaved, type SaveStatus } from './autoSave'
import CommitDialog, { type CommitPushResult } from './CommitDialog'
import ConfirmDialog from './ConfirmDialog'
import { Drawer } from './Drawer'
import { commitButtonState, progressText } from './githubCommit'
import { githubErrorText, githubFailureText } from './githubError'
import { deletesBranch, type GithubArticleStates } from './githubList'
import { t, type MessageKey } from './i18n'
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

// 公開済みの記事の削除・非公開と、その取りやめ・削除予定の印の文言
const removeTexts = {
  delete: {
    title: 'app.remove_title_delete',
    confirm: 'app.remove_confirm_delete',
    message: 'app.remove_message_delete',
    discard: 'app.discard_delete_title',
    pending: 'app.deleting_pending',
  },
  unpublish: {
    title: 'app.remove_title_unpublish',
    confirm: 'app.remove_confirm_unpublish',
    message: 'app.remove_message_unpublish',
    discard: 'app.discard_unpublish_title',
    pending: 'app.unpublishing_pending',
  },
} satisfies Record<GithubDeleting, Record<string, MessageKey>>

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
  // 読めていなければnullで、編集画面はどちらも有効として出す
  const [frontmatter, setFrontmatter] = useState<FrontmatterSettings | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<ArticleRef | null>(null)
  const [discardTarget, setDiscardTarget] = useState<ArticleRef | null>(null)
  // 公開済みの記事の削除・非公開。branchは確認の文言に出す
  const [removeTarget, setRemoveTarget] = useState<{
    target: ArticleRef
    kind: GithubDeleting
    branch: string
  } | null>(null)
  const [removeProgress, setRemoveProgress] = useState<PushProgress | null>(null)
  // 同じ文言が続いても出し直すよう、包んで持つ
  const [notice, setNotice] = useState<Notice | null>(null)
  // GitHub連携の記事ごとの状態。nullは連携していない
  const [githubStates, setGithubStates] = useState<GithubArticleStates | null>(null)
  const [syncing, setSyncing] = useState(false)
  // 最後のGitHubからの読み込みの失敗。syncBusy・settingsChangedと、保存先を戻したloginRequiredは入れない
  const [syncError, setSyncError] = useState<{ error: unknown } | null>(null)
  const syncingRef = useRef(false)
  // Commit & Pushの対象。previewがnullの間は概略を読んでいる
  const [commitTarget, setCommitTarget] = useState<{
    slug: string
    preview: CommitPreview | null
  } | null>(null)
  // 概略を読み始めてからダイアログを閉じるまで、記事の切り替えと読み込みをさせない
  const commitOpenRef = useRef(false)
  // 読み込み中に頼まれた読み込み(Repositoryの切り替えの後など)。終わった後にもう一度読み込む
  const syncAgainRef = useRef(false)
  // 開いている記事を置き換えさせる読み込みの間は、上のバーも含めて記事を変えさせない
  const [editorLocked, setEditorLocked] = useState(false)
  const editor = useRef<ArticleEditorHandle>(null)
  const selectedSlug = article.status === 'none' ? null : article.slug
  const loaded = article.status === 'loaded' ? article : null
  // 連携していなければnull
  const commitButton = commitButtonState(
    githubStates,
    loaded?.slug ?? null,
    syncing,
    saveStatus.dirty,
  )
  // 読み込みの間に開いている記事が替わっても、読み込み後の処理を今の記事に合わせるため
  const selectedSlugRef = useRef<string | null>(null)

  // 記事内検索。文字は記事を切り替えても残す
  const [findFor, setFindFor] = useState<Article | null>(null)
  const [findQuery, setFindQuery] = useState('')
  const [findStatus, setFindStatus] = useState<FindStatus>({ current: -1, total: 0 })
  const [findFocus, setFindFocus] = useState(0)
  // 検索を開いた記事が読み込み済みの記事と違えば、閉じているとみなす
  const findOpen = loaded !== null && findFor === loaded.article

  // 後から出した読み込みが先に返ったら、前の読み込みの結果は捨てる
  const listRequest = useRef(0)
  const reloadArticles = (): Promise<GithubArticleStates | null> => {
    const request = ++listRequest.current
    // 連携していないときはgithub_article_statesがnullを返す
    // 読めなかったときは前の状態を残す(nullにすると連携していない表示になる)
    const states = githubArticleStates().catch((e) => {
      console.error('Failed to read the GitHub states', e)
      return undefined
    })
    return Promise.all([listArticles(), states])
      .then(([list, githubResult]) => {
        if (request !== listRequest.current) return null
        setArticles({
          status: 'loaded',
          articles: list.articles,
          broken: list.broken,
          unreadable: list.unreadable,
        })
        if (githubResult === undefined) return null
        setGithubStates(githubResult)
        return githubResult
      })
      .catch((e) => {
        if (request !== listRequest.current) return null
        console.error('Failed to load the article list', e)
        const error = e as StoreError | string | undefined
        // Tauriは種類の無いエラーを文字列で渡す
        setArticles({
          status: 'failed',
          path: typeof error === 'string' ? undefined : error?.path,
          reason: typeof error === 'string' ? error : error?.reason,
        })
        return null
      })
  }

  // 描画のたびに最新のrunGithubSyncを入れ、起動時に1回だけ動くeffectなどから呼ぶ
  const runGithubSyncRef = useRef<() => Promise<void>>(async () => {})
  // 一覧を読み直し、連携していればGitHubから読み込む
  const reloadAndSync = () => {
    void reloadArticles().then((states) => {
      if (states !== null) void runGithubSyncRef.current()
    })
  }

  useEffect(() => {
    reloadAndSync()
    if (isAndroid) {
      getPostsDirName()
        .then((name) => setPostsDir({ kind: 'android', name }))
        .catch((e) => console.error('Failed to read the article directory', e))
    } else {
      getPostsDir()
        .then((location) => setPostsDir({ kind: 'desktop', ...location }))
        .catch((e) => console.error('Failed to read the article directory', e))
    }
    getFrontmatterSettings()
      .then(setFrontmatter)
      .catch((e) => console.error('Failed to read the frontmatter settings', e))
  }, [])

  const changeFrontmatter = async (next: FrontmatterSettings) => {
    await setFrontmatterSettings(next)
    setFrontmatter(next)
  }

  useEffect(() => {
    if (!notice) return
    const timer = setTimeout(() => setNotice(null), 5000)
    return () => clearTimeout(timer)
  }, [notice])

  useEffect(() => {
    selectedSlugRef.current = selectedSlug
  })

  const openArticle = (slug: string) => {
    setArticle({ status: 'loading', slug })
    // 切り替えた後に返ってきた前の記事の読み込みは捨てる
    const settle = (next: ArticleState) =>
      setArticle((current) =>
        current.status === 'loading' && current.slug === slug ? next : current,
      )
    whenSaved()
      .then(async () => {
        // 連携しているときは、まだ取っていない画像をGitHubから取ってから読む。取り損ねても記事は開く
        if (githubStates !== null && !githubStates.stateUnreadable) {
          try {
            const count = await githubFetchImages(slug)
            if (count > 0) {
              setNotice({ text: t('app.images_fetch_failed_count', { count }), error: true })
            }
          } catch (e) {
            console.error('Failed to fetch the images from GitHub', e)
            setNotice({ text: t('app.images_fetch_failed'), error: true })
          }
        }
        return readArticle(slug)
      })
      .then((loaded) => settle({ status: 'loaded', slug, article: loaded }))
      .catch((e) => {
        console.error('Failed to read the article', e)
        settle({ status: 'failed', slug })
      })
  }

  // 読めなかった記事は、選び直したら読み直す。
  // GitHubからの読み込み中は切り替えない。読み込みが守るのは始めたときに開いていた記事だけなので、
  // 切り替えた先の記事への入力が置き換えで消えることがある
  const selectArticle = (slug: string) => {
    drawer.close()
    if (syncingRef.current || commitOpenRef.current) return
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

  // 進む前に開いている記事を保存する
  const saveBeforeProceed = async () => {
    if (!(await saveOpenArticle())) throw new UnsavedArticleError()
    await whenSaved()
  }

  // Ctrl+F・Cmd+Fで検索を開く。受け手のeffectは依存を空にして付けっぱなしにするので、
  // 開く処理はref経由で最新のものを呼ぶ
  const openFindRef = useRef<(target: EventTarget | null) => void>(() => {})
  const openFind = (target: EventTarget | null) => {
    if (!loaded) return
    if (saveConfirm || deleteTarget || discardTarget || slugDialog || settingsOpen || drawer.shown)
      return
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
    else
      getCurrentWindow()
        .destroy()
        .catch((e) => console.error('Failed to close the window', e))
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
    await saveBeforeProceed()
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

  // 保存先が替わった後。開いている記事は前の保存先のものなので閉じ、一覧と保存先を読み直す
  const afterDestinationChange = () => {
    setArticle({ status: 'none' })
    reloadAndSync()
    if (!isAndroid) {
      getPostsDir()
        .then((location) => setPostsDir({ kind: 'desktop', ...location }))
        .catch((e) => console.error('Failed to read the article directory', e))
    }
  }

  // 保存先が替わるGitHubの操作。開いている記事を保存してから呼び、替わったら閉じて読み直す。
  // 保存できなければUnsavedArticleErrorを投げて、呼ばない
  const switchDestination = async (command: () => Promise<void>) => {
    await saveBeforeProceed()
    await command()
    afterDestinationChange()
  }

  // 読み込みでログインの切れが出たら、設定画面と同じく状態を読み、必要なら今までの保存先へ戻す
  // 保存先を戻したらtrue
  const recoverGithubLogin = async () => {
    try {
      const status = await githubStatus()
      if (!status.loginExpired) return false
      await switchDestination(githubLogout)
      return true
    } catch (e) {
      console.error('Failed to read the GitHub status', e)
      return false
    }
  }

  const closeCommit = () => {
    commitOpenRef.current = false
    setCommitTarget(null)
  }

  // 開いている記事を保存してから、Commit & Pushのダイアログに出す概略を読む
  const openCommit = async () => {
    const slug = loaded?.slug
    if (slug === undefined || commitOpenRef.current || syncingRef.current) return
    commitOpenRef.current = true
    setCommitTarget({ slug, preview: null })
    try {
      await saveBeforeProceed()
      const preview = await githubCommitPreview(slug)
      setCommitTarget({ slug, preview })
    } catch (e) {
      closeCommit()
      const error = e as GithubError
      if (error?.kind === 'loginRequired' && (await recoverGithubLogin())) return
      console.error('Failed to read the commit preview', e)
      setNotice({ text: githubErrorText(e), error: true })
    }
  }

  // ConflictとBranchExistsはダイアログに返し、そのほかはここで閉じる
  const commitPush = async (
    slug: string,
    message: string,
    overwrite: string | null,
    onProgress: (progress: PushProgress) => void,
  ): Promise<CommitPushResult> => {
    let keptBranch: string | null = null
    try {
      keptBranch = (await githubCommitPush(slug, message, overwrite, onProgress)).keptBranch
    } catch (e) {
      const error = e as GithubError
      switch (error?.kind) {
        case 'conflict':
          return { kind: 'conflict', head: error.head, md: error.md, images: error.images }
        case 'branchExists':
          return { kind: 'branchExists', branch: error.branch }
        case 'branchGone':
          closeCommit()
          setNotice({ text: t('app.branch_gone'), error: false })
          void runGithubSyncRef.current()
          return { kind: 'pushed' }
        // branchは進めたが、保存先が替わったので記録を書かなかった。読み込みで記録が直る
        case 'settingsChanged':
          closeCommit()
          setNotice({ text: t('app.push_not_recorded'), error: true })
          void runGithubSyncRef.current()
          return { kind: 'pushed' }
        case 'loginRequired':
          if (await recoverGithubLogin()) {
            closeCommit()
            setNotice({ text: t('common.github_login_required'), error: true })
            return { kind: 'pushed' }
          }
          break
      }
      console.error('Failed to commit and push', slug, e)
      throw e
    }
    closeCommit()
    setNotice({
      text:
        keptBranch === null ? t('app.push_done') : t('app.push_done_kept', { name: keptBranch }),
      error: false,
    })
    void runGithubSyncRef.current()
    return { kind: 'pushed' }
  }

  // 手元をバックアップに写してから、選んだbranchの内容に読み込み直す
  const commitPull = async (slug: string, branch: string | null) => {
    try {
      await githubPull(slug, branch)
    } catch (e) {
      const error = e as GithubError
      if (error?.kind === 'branchGone') {
        closeCommit()
        setNotice({ text: t('app.branch_gone'), error: false })
        void runGithubSyncRef.current()
        return
      }
      if (error?.kind === 'loginRequired' && (await recoverGithubLogin())) {
        closeCommit()
        setNotice({ text: t('common.github_login_required'), error: true })
        return
      }
      console.error('Failed to pull', slug, e)
      throw e
    }
    closeCommit()
    setNotice({ text: t('app.pull_done'), error: false })
    void runGithubSyncRef.current()
  }

  // 開いている記事は、編集欄に保存していない
  // 入力があるかをRust側で知れないので書き換えさせず(deferred)、ここで入力が無いと確かめてから置き換えさせる
  const syncOpenArticle = async (): Promise<GithubSyncResult> => {
    const open = selectedSlugRef.current
    const result = await githubSync(open)
    if (open === null || !result.deferred.includes(open)) return result
    // 入力できなくした画面が出てから、入力が無いことを確かめる。解くのはrunGithubSyncで開き直した後
    setEditorLocked(true)
    await new Promise((resolve) => requestAnimationFrame(resolve))
    if (!editor.current?.isDirty()) return githubSync(null)
    setEditorLocked(false)
    // 入力を保存し、手元の変更として読み直させる(GitHub上でも変わっていれば食い違いの印が付く)
    if (!(await saveOpenArticle())) return result
    await whenSaved()
    return githubSync(open)
  }

  const runGithubSync = async () => {
    // Commit & Pushのダイアログの間に読み込むと、見ている内容が替わる
    if (commitOpenRef.current) return
    if (syncingRef.current) {
      syncAgainRef.current = true
      return
    }
    syncingRef.current = true
    setSyncing(true)
    let result: GithubSyncResult | null = null
    try {
      try {
        result = await syncOpenArticle()
        setSyncError(null)
      } catch (e) {
        const error = e as GithubError
        switch (error?.kind) {
          case 'syncBusy':
          case 'settingsChanged':
            // 重なった読み込みと設定の変更は、後の読み込みで出し直すので失敗として出さない
            break
          case 'loginRequired':
            if (!(await recoverGithubLogin())) setSyncError({ error: e })
            break
          default:
            console.error('Failed to sync from GitHub', e)
            setSyncError({ error: e })
        }
      }
      await reloadArticles()
      // 開いている記事が替わっていたら開き直し、GitHub上から消えていたら閉じる
      const open = selectedSlugRef.current
      if (result !== null && open !== null) {
        if (result.removed.includes(open)) setArticle({ status: 'none' })
        else if (result.replaced.includes(open)) openArticle(open)
      }
    } finally {
      syncingRef.current = false
      setSyncing(false)
      setEditorLocked(false)
    }
    if (syncAgainRef.current) {
      syncAgainRef.current = false
      void runGithubSyncRef.current()
    }
  }
  useEffect(() => {
    runGithubSyncRef.current = runGithubSync
  })

  // 一覧の更新ボタン。開いている記事を保存してから読み込む
  const syncAfterSave = async () => {
    try {
      if (!(await saveOpenArticle())) return
      await whenSaved()
    } catch (e) {
      // 保存の失敗は編集欄が出すので、ここでは読み込まないだけにする
      console.error('Failed to save the article before the sync', e)
      return
    }
    await runGithubSync()
  }

  // ログインの待ち。Rust側はログインできると保存先を替えるので、呼ぶ前に保存しておき、返った後に閉じて読み直す
  const waitGithubLogin = async (): Promise<GithubWaitLogin> => {
    await saveBeforeProceed()
    const result = await githubWaitLogin()
    if (result.outcome === 'loggedIn') afterDestinationChange()
    return result
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
      return githubFailureText(e) ?? false
    } finally {
      reloadArticles()
    }
  }

  const branchNote = (slug: string) =>
    deletesBranch(githubStates, slug) && (
      <>
        <br />
        {t('app.delete_branch_note')}
      </>
    )

  // 開いている記事が削除予定なら、上のバーに出して編集欄を止める
  const pendingRemoval =
    loaded === null ? null : (githubStates?.articles[loaded.slug]?.deleting ?? null)
  // 取りやめる対象が削除予定なら、文言を「削除をやめる」「非公開をやめる」にする
  const discardState =
    discardTarget?.kind === 'article' ? githubStates?.articles[discardTarget.slug] : undefined
  const discardRemoval = discardState?.deleting ?? null
  const discardMovedFrom = discardState?.movedFrom ?? null
  let discardMessage: MessageKey = 'app.discard_message'
  if (discardRemoval !== null) discardMessage = 'app.discard_remove_message'
  else if (discardMovedFrom !== null) discardMessage = 'app.discard_moved_message'

  const openRemove = (target: ArticleRef, kind: GithubDeleting) => {
    drawer.close()
    githubStatus()
      .then((status) => {
        setRemoveProgress(null)
        setRemoveTarget({
          target,
          kind,
          branch: status.settings.branchFormat.replace('{slug}', target.slug),
        })
      })
      .catch((e) => {
        console.error('Failed to read the branch format', e)
        setNotice({ text: githubErrorText(e), error: true })
      })
  }

  const removeConfirmed = async (target: ArticleRef, kind: GithubDeleting) => {
    const slug = target.slug
    const open = slug === selectedSlug
    try {
      if (open) {
        if (!(await saveOpenArticle())) return false
        await whenSaved()
        setEditorLocked(true)
      }
      setRemoveProgress({ step: 'checking' })
      await githubRemovePublished(slug, kind, setRemoveProgress)
      setRemoveTarget(null)
      await reloadArticles()
      return true
    } catch (e) {
      console.error('Failed to remove the published article', slug, e)
      // 届いたか分からないときは削除予定の印が残るので、一覧を読み直す
      void reloadArticles()
      const error = e as GithubError | undefined
      if (error?.kind === 'branchExists')
        return t('app.remove_branch_exists', { branch: error.branch })
      return githubFailureText(e) ?? false
    } finally {
      if (open) setEditorLocked(false)
      setRemoveProgress(null)
    }
  }

  const openDiscard = (target: ArticleRef) => {
    drawer.close()
    setDiscardTarget(target)
  }

  // 開いている記事なら編集欄を止めて、未保存の入力を捨てる。成功したら開き直す
  const discardConfirmed = async (target: ArticleRef) => {
    const slug = target.slug
    const open = slug === selectedSlug
    try {
      if (open) setEditorLocked(true)
      await whenSaved()
      // 移した記事をやめると、元のslugの記事に戻る
      const reopened = githubStates?.articles[slug]?.movedFrom ?? slug
      await githubDiscardEdit(slug)
      setDiscardTarget(null)
      if (open) openArticle(reopened)
      return true
    } catch (e) {
      console.error('Failed to discard the edits', slug, e)
      return githubFailureText(e) ?? false
    } finally {
      if (open) setEditorLocked(false)
      // 失敗してもブランチを消し終えていることがあるので、状態を読み直す
      reloadArticles()
    }
  }

  const sidebar = (
    <Sidebar
      articles={articles}
      selectedSlug={selectedSlug}
      github={githubStates === null ? null : { states: githubStates, syncing, failed: syncError }}
      onSelect={selectArticle}
      onNew={() => {
        drawer.close()
        openSlugDialog({ kind: 'create' })
      }}
      onSync={() => void syncAfterSave()}
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
      onDiscard={openDiscard}
      onRemove={openRemove}
    />
  )

  // 一覧に出るのはタイトルと公開日。保存した記事の状態は、連携していればGitHubの記録で直す
  const onSaved = (fields: EditedFields, dates: SavedDates) => {
    if (articles.status !== 'loaded' || article.status !== 'loaded') return
    const listed = articles.articles.find((a) => a.slug === article.slug)
    if (listed?.title !== fields.title || listed.pubDate !== dates.pubDate) reloadArticles()
    if (githubStates === null) return
    const slug = article.slug
    void githubArticleState(slug)
      .then((state) => {
        // 読み取りの間に一覧が替わっていたら、一覧の読み直しに合わせる
        if (state === null) return
        setGithubStates((current) =>
          current === null || current.articles[slug] === undefined
            ? current
            : { ...current, articles: { ...current.articles, [slug]: state } },
        )
      })
      .catch((e) => console.error('Failed to read the GitHub state', e))
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
      <main inert={drawer.shown || editorLocked || commitTarget?.preview === null} {...swipeToOpen}>
        {isAndroid && findOpen ? (
          findBar('bar')
        ) : (
          <TopBar
            drawer={drawer}
            onInsertLink={
              loaded && pendingRemoval === null ? () => editor.current?.insertLink() : null
            }
            onInsertImages={
              loaded && pendingRemoval === null
                ? (files) => editor.current?.insertImages(files)
                : null
            }
            saveStatus={loaded ? saveStatus : null}
            notice={
              pendingRemoval === null
                ? notice
                : { text: t(removeTexts[pendingRemoval].pending), error: false }
            }
            onFind={loaded ? () => openFindRef.current(null) : null}
            commit={
              commitButton === null
                ? null
                : { state: commitButton, onClick: () => void openCommit() }
            }
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
                  frontmatter={frontmatter ?? DEFAULT_FRONTMATTER}
                  paused={
                    saveConfirm !== null ||
                    slugDialog !== null ||
                    deleteTarget !== null ||
                    discardTarget !== null ||
                    removeTarget !== null ||
                    syncing ||
                    commitTarget !== null ||
                    pendingRemoval !== null
                  }
                  readOnly={pendingRemoval !== null}
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
          published={
            slugDialog.kind === 'rename' &&
            githubStates?.articles[slugDialog.slug]?.category !== undefined &&
            githubStates?.articles[slugDialog.slug]?.category !== 'drafting'
          }
          onDone={onDialogDone}
          onCancel={() => setSlugDialog(null)}
        />
      )}
      {commitTarget?.preview && (
        <CommitDialog
          preview={commitTarget.preview}
          onCancel={closeCommit}
          onCommit={(message, overwrite, onProgress) =>
            commitPush(commitTarget.slug, message, overwrite, onProgress)
          }
          onPull={(branch) => commitPull(commitTarget.slug, branch)}
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
          {branchNote(deleteTarget.slug)}
        </ConfirmDialog>
      )}
      {removeTarget && (
        <ConfirmDialog
          title={t(removeTexts[removeTarget.kind].title)}
          confirmLabel={t(removeTexts[removeTarget.kind].confirm)}
          failedText={t('common.change_failed')}
          progress={removeProgress !== null ? progressText(removeProgress) : null}
          onCancel={() => setRemoveTarget(null)}
          onConfirm={() => removeConfirmed(removeTarget.target, removeTarget.kind)}
        >
          {t(removeTexts[removeTarget.kind].message, {
            title:
              (removeTarget.target.kind === 'article' && removeTarget.target.title) ||
              t('common.untitled'),
            slug: removeTarget.target.slug,
            branch: removeTarget.branch,
          })}
        </ConfirmDialog>
      )}
      {discardTarget && (
        <ConfirmDialog
          title={t(
            discardRemoval === null ? 'app.discard_title' : removeTexts[discardRemoval].discard,
          )}
          confirmLabel={t(
            discardRemoval === null ? 'app.discard_confirm' : removeTexts[discardRemoval].discard,
          )}
          failedText={t('app.discard_failed')}
          onCancel={() => setDiscardTarget(null)}
          onConfirm={() => discardConfirmed(discardTarget)}
        >
          {t(discardMessage, {
            title:
              (discardTarget.kind === 'article' && discardTarget.title) || t('common.untitled'),
            slug: discardTarget.slug,
            oldSlug: discardMovedFrom ?? '',
          })}
          {branchNote(discardTarget.slug)}
        </ConfirmDialog>
      )}
      {settingsOpen && (
        <Settings
          narrow={drawer.narrow}
          postsDir={postsDir}
          frontmatter={frontmatter}
          onChangeFrontmatter={changeFrontmatter}
          onPickPostsDir={pickPostsDirAndReload}
          onChangePostsDirName={changePostsDirName}
          onGithubSwitch={switchDestination}
          onGithubLoginWait={waitGithubLogin}
          onClose={() => setSettingsOpen(false)}
        />
      )}
    </div>
  )
}
