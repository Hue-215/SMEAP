import { useEffect, useId, useRef, useState } from 'react'
import {
  githubCancelLogin,
  githubStartLogin,
  openGithubPage,
  type GithubWaitLogin,
} from './api'
import Dialog from './Dialog'
import Icon from './Icon'
import { githubErrorText } from './githubError'
import { t } from './i18n'

interface GithubLoginDialogProps {
  // 承認を待つ。待つ前に開いている記事を保存することと、loggedInで返った後の読み直しはAppが行う
  onWait: () => Promise<GithubWaitLogin>
  // 承認された。閉じるのは親が行う
  onDone: () => void
  onCancel: () => void
}

// デバイスフローでログインするダイアログ。開くとコードを取り、承認を待つ。やり直すとコードから取り直す
export default function GithubLoginDialog({ onWait, onDone, onCancel }: GithubLoginDialogProps) {
  // startingはコードを取りに行っている間。expiredとdeniedは、待ちが終わってやり直せる状態
  const [phase, setPhase] = useState<'starting' | 'waiting' | 'expired' | 'denied' | 'failed'>(
    'starting',
  )
  const [code, setCode] = useState<string | null>(null)
  const [failure, setFailure] = useState<string | null>(null)
  // 始め直した試行より前の応答は捨てる
  const attempt = useRef(0)
  // 外に出た後に待ちを始めない(取り消される当ても無い待ちが残る)
  const alive = useRef(true)
  const headingId = useId()

  // コードを取り、承認を待つ。状態の更新は応答が返ってから行う
  const begin = () => {
    const id = ++attempt.current
    githubStartLogin()
      .then(async (started) => {
        if (id !== attempt.current || !alive.current) return
        setCode(started.userCode)
        setPhase('waiting')
        const result = await onWait()
        if (id !== attempt.current || !alive.current) return
        setFailure(null)
        if (result.outcome === 'loggedIn') {
          onDone()
          return
        }
        // cancelledは閉じた後なので、何もしない
        if (result.outcome === 'cancelled') return
        setPhase(result.outcome)
      })
      .catch((e) => {
        if (id !== attempt.current || !alive.current) return
        console.error('Failed to log in to GitHub', e)
        setFailure(githubErrorText(e))
        setPhase('failed')
      })
  }

  // 開いたときに一度だけ始める。受け手のeffectは依存を空にして付けっぱなしにするので、
  // 始める処理はref経由で最新のものを呼ぶ
  const beginRef = useRef<() => void>(() => {})
  useEffect(() => {
    beginRef.current = begin
  })
  // 開発時のStrictModeはeffectを2回走らせる。2回始めると、Rust側が待つコードと画面に出すコードが食い違うことがある
  const started = useRef(false)
  useEffect(() => {
    if (started.current) return
    started.current = true
    void beginRef.current()
  }, [])
  // 外に出たら、以後の応答を捨てる。StrictModeで付け直されたときは戻す
  useEffect(() => {
    alive.current = true
    return () => {
      alive.current = false
    }
  }, [])

  // やり直す
  const retry = () => {
    setPhase('starting')
    setCode(null)
    setFailure(null)
    void begin()
  }

  // 待っている間は、Rust側の待ちも止める
  const cancel = () => {
    void githubCancelLogin().catch((e) => console.error('Failed to cancel the GitHub login', e))
    onCancel()
  }

  const copyCode = () => {
    if (code === null) return
    navigator.clipboard.writeText(code).catch((e) => console.error('Failed to copy the code', e))
  }

  const openBrowser = () => {
    openGithubPage('device').catch((e) => {
      console.error('Failed to open the GitHub page', e)
      setFailure(t('common.open_browser_failed'))
    })
  }

  const status =
    failure ??
    (phase === 'waiting'
      ? t('github_login.waiting')
      : phase === 'expired'
        ? t('github_login.expired')
        : phase === 'denied'
          ? t('github_login.denied')
          : null)

  return (
    <Dialog labelledBy={headingId} onCancel={cancel}>
      <div className="dialog-form">
        <h2 id={headingId} className="dialog-title">
          {t('github_login.title')}
        </h2>
        <div className="dialog-body">
          <p>{t('github_login.note')}</p>
        </div>
        {phase === 'waiting' && code !== null && (
          <div className="github-login-code">
            <code aria-label={t('github_login.code')}>{code}</code>
            <button
              type="button"
              className="icon-button"
              aria-label={t('github_login.copy')}
              title={t('github_login.copy')}
              onClick={copyCode}
            >
              <Icon name="content_copy" />
            </button>
          </div>
        )}
        {phase === 'waiting' && (
          <button type="button" className="github-login-open" onClick={openBrowser}>
            <Icon name="open_in_new" />
            {t('github_login.open_browser')}
          </button>
        )}
        {status !== null && (
          <div className="dialog-body">
            <p
              role={failure !== null ? 'alert' : 'status'}
              className={failure !== null ? 'confirm-dialog-error' : undefined}
            >
              {status}
            </p>
          </div>
        )}
        <div className="dialog-buttons">
          <button type="button" onClick={cancel}>
            {t('common.cancel')}
          </button>
          {(phase === 'expired' || phase === 'denied' || phase === 'failed') && (
            <button type="button" className="primary" onClick={retry}>
              {t('github_login.retry')}
            </button>
          )}
        </div>
      </div>
    </Dialog>
  )
}
