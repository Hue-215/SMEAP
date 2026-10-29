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

type Phase = 'starting' | 'waiting' | 'expired' | 'denied' | 'failed'

// 待っている間と、待ちが終わってやり直せる状態に出す文言
function phaseStatus(phase: Phase): string | null {
  switch (phase) {
    case 'waiting':
      return t('github_login.waiting')
    case 'expired':
      return t('github_login.expired')
    case 'denied':
      return t('github_login.denied')
    default:
      return null
  }
}

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
  const [phase, setPhase] = useState<Phase>('starting')
  const [code, setCode] = useState<string | null>(null)
  const [failure, setFailure] = useState<string | null>(null)
  // コピーできた回数。続けて押したときに、チェックを出す2秒を数え直すため
  const [copied, setCopied] = useState(0)
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
    beginRef.current()
  }, [])
  // 外に出たら、以後の応答を捨てる。StrictModeで付け直されたときは戻す
  useEffect(() => {
    alive.current = true
    return () => {
      alive.current = false
    }
  }, [])

  useEffect(() => {
    if (copied === 0) return
    const timer = setTimeout(() => setCopied(0), 2000)
    return () => clearTimeout(timer)
  }, [copied])

  // やり直す
  const retry = () => {
    setPhase('starting')
    setCode(null)
    setFailure(null)
    begin()
  }

  // 待っている間は、Rust側の待ちも止める
  const cancel = () => {
    void githubCancelLogin().catch((e) => console.error('Failed to cancel the GitHub login', e))
    onCancel()
  }

  const copyCode = () => {
    if (code === null) return
    const id = attempt.current
    const copyFailed = t('github_login.copy_failed')
    navigator.clipboard
      .writeText(code)
      .then(() => {
        if (id !== attempt.current || !alive.current) return
        setFailure((f) => (f === copyFailed ? null : f))
        setCopied((n) => n + 1)
      })
      .catch((e) => {
        console.error('Failed to copy the code', e)
        if (id !== attempt.current || !alive.current) return
        setCopied(0)
        setFailure(copyFailed)
      })
  }

  const openBrowser = () => {
    openGithubPage('device').catch((e) => {
      console.error('Failed to open the GitHub page', e)
      setFailure(t('common.open_browser_failed'))
    })
  }

  const status = failure ?? phaseStatus(phase)

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
              aria-label={copied > 0 ? t('github_login.copied') : t('github_login.copy')}
              title={copied > 0 ? t('github_login.copied') : t('github_login.copy')}
              onClick={copyCode}
            >
              <Icon name={copied > 0 ? 'check' : 'content_copy'} />
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
