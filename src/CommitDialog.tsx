// Commit & Pushのダイアログ。Conflictと「branchが既にあります」も同じ状態で切り替える(App.tsx)
import { useEffect, useId, useRef, useState } from 'react'
import type { CommitPreview } from './api'
import Dialog from './Dialog'
import {
  canSendMessage,
  conflictText,
  defaultCommitMessage,
  imagesText,
  linesText,
} from './githubCommit'
import { githubErrorText } from './githubError'
import { t } from './i18n'

// 送った結果のうち、ダイアログを出したまま切り替えるもの。成功とBranchGoneは親が閉じる
export type CommitPushResult =
  | { kind: 'pushed' }
  | { kind: 'conflict'; head: string; md: boolean; images: string[] }
  | { kind: 'branchExists'; branch: string }

interface CommitDialogProps {
  preview: CommitPreview
  onCancel: () => void
  // overwriteは、Conflictで「Push」を選んだときのリモートの先頭
  onCommit: (message: string, overwrite: string | null) => Promise<CommitPushResult>
  // branchがnullなら記録のbranch(無ければベース)
  onPull: (branch: string | null) => Promise<void>
}

type Phase =
  | { kind: 'edit' }
  | { kind: 'conflict'; head: string; md: boolean; images: string[] }
  | { kind: 'branchExists'; branch: string }

export default function CommitDialog({ preview, onCancel, onCommit, onPull }: CommitDialogProps) {
  const [phase, setPhase] = useState<Phase>({ kind: 'edit' })
  const [message, setMessage] = useState(() => defaultCommitMessage(preview.kind, preview.title))
  const [running, setRunning] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const headingId = useId()
  const bodyId = useId()
  const input = useRef<HTMLInputElement>(null)

  // 開いたときにCommit messageを選んだ状態にする。Dialogが開いてから選ぶ
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      input.current?.focus()
      input.current?.select()
    })
    return () => cancelAnimationFrame(frame)
  }, [])

  const fail = (e: unknown) => {
    setError(githubErrorText(e))
    setRunning(false)
  }

  const commit = (overwrite: string | null) => {
    setRunning(true)
    setError(null)
    void onCommit(message, overwrite).then(
      (result) => {
        if (result.kind === 'conflict') {
          setPhase({
            kind: 'conflict',
            head: result.head,
            md: result.md,
            images: result.images,
          })
          setRunning(false)
        } else if (result.kind === 'branchExists') {
          setPhase({ kind: 'branchExists', branch: result.branch })
          setRunning(false)
        }
      },
      fail,
    )
  }

  const pull = (branch: string | null) => {
    setRunning(true)
    setError(null)
    void onPull(branch).then(() => {}, fail)
  }

  const cancel = () => {
    if (!running) onCancel()
  }

  const images = imagesText(preview.imagesAdded, preview.imagesRemoved)
  const title =
    phase.kind === 'conflict'
      ? t('commit.conflict_title')
      : phase.kind === 'branchExists'
        ? t('commit.branch_exists_title')
        : t('commit.title')

  return (
    <Dialog
      labelledBy={headingId}
      describedBy={bodyId}
      role={phase.kind === 'edit' ? undefined : 'alertdialog'}
      onCancel={cancel}
    >
      <div className="dialog-form">
        <h2 id={headingId} className="dialog-title">
          {title}
        </h2>
        {phase.kind === 'edit' ? (
          <div id={bodyId} className="commit-summary">
            <div className="commit-row">
              <span className="commit-label">{t('commit.md_label')}</span>
              <span className="commit-value">{preview.mdName}</span>
              <span className="commit-lines">{linesText(preview.added, preview.removed)}</span>
            </div>
            {images !== null && (
              <div className="commit-row">
                <span className="commit-label">{t('commit.images_label')}</span>
                <span className="commit-value">{images}</span>
              </div>
            )}
          </div>
        ) : (
          <div className="dialog-body">
            <p id={bodyId}>
              {phase.kind === 'conflict'
                ? conflictText(preview.title, phase.md, phase.images)
                : t('commit.branch_exists', { branch: phase.branch })}
            </p>
            {error !== null && (
              <p role="alert" className="confirm-dialog-error">
                {error}
              </p>
            )}
          </div>
        )}
        {phase.kind === 'edit' && (
          <div className="commit-message">
            <label htmlFor={`${headingId}-message`} className="commit-label">
              {t('commit.message_label')}
            </label>
            <input
              ref={input}
              id={`${headingId}-message`}
              className="slug-dialog-input"
              type="text"
              value={message}
              onChange={(e) => setMessage(e.target.value)}
            />
            {error !== null && (
              <p role="alert" className="confirm-dialog-error">
                {error}
              </p>
            )}
          </div>
        )}
        <div className="dialog-buttons">
          {phase.kind !== 'edit' && (
            <button
              type="button"
              className="left"
              disabled={running}
              onClick={() => pull(phase.kind === 'branchExists' ? phase.branch : null)}
            >
              {t('commit.pull')}
            </button>
          )}
          <button type="button" disabled={running} onClick={cancel}>
            {t('common.cancel')}
          </button>
          {phase.kind === 'edit' ? (
            <button
              type="button"
              className="primary"
              disabled={running || !canSendMessage(message)}
              onClick={() => commit(null)}
            >
              {t('commit.send')}
            </button>
          ) : phase.kind === 'conflict' ? (
            <button
              type="button"
              className="primary"
              disabled={running}
              onClick={() => commit(phase.head)}
            >
              {t('commit.push')}
            </button>
          ) : null}
        </div>
      </div>
    </Dialog>
  )
}
