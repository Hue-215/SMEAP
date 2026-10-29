// Commit & Pushのダイアログ。Conflictと「branchが既にあります」も、このダイアログの中で切り替えて出す
import { useEffect, useId, useRef, useState } from 'react'
import type { CommitPreview } from './api'
import Dialog from './Dialog'
import {
  canSendMessage,
  conflictText,
  defaultCommitMessage,
  imagesText,
  linesText,
  mdNameText,
} from './githubCommit'
import { githubErrorText } from './githubError'
import { t } from './i18n'

// Conflict・BranchExistsはダイアログを出したまま切り替える。そのほかは親が閉じてpushedを返す
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

function phaseTitle(phase: Phase): string {
  switch (phase.kind) {
    case 'edit':
      return t('commit.title')
    case 'conflict':
      return t('commit.conflict_title')
    case 'branchExists':
      return t('commit.branch_exists_title')
  }
}

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
    void onCommit(message, overwrite).then((result) => {
      if (result.kind === 'pushed') return
      setPhase(result)
      setRunning(false)
    }, fail)
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

  return (
    <Dialog
      labelledBy={headingId}
      describedBy={bodyId}
      role={phase.kind === 'edit' ? undefined : 'alertdialog'}
      onCancel={cancel}
    >
      <div className="dialog-form">
        <h2 id={headingId} className="dialog-title">
          {phaseTitle(phase)}
        </h2>
        {phase.kind === 'edit' ? (
          <div id={bodyId} className="commit-summary">
            <div className="commit-row">
              <span className="commit-label">{t('commit.md_label')}</span>
              <span className="commit-value">{mdNameText(preview)}</span>
              <span className="commit-lines">{linesText(preview.added, preview.removed)}</span>
            </div>
            {preview.oldBranch !== null && (
              <div className="commit-row">
                <span className="commit-value">
                  {t('commit.old_branch_deleted', { name: preview.oldBranch })}
                </span>
              </div>
            )}
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
            <label htmlFor={`${headingId}-message`} className="commit-message-label">
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
          {phase.kind === 'edit' && (
            <button
              type="button"
              className="primary"
              disabled={running || !canSendMessage(message)}
              onClick={() => commit(null)}
            >
              {t('commit.send')}
            </button>
          )}
          {phase.kind === 'conflict' && (
            <button
              type="button"
              className="primary"
              disabled={running}
              onClick={() => commit(phase.head)}
            >
              {t('commit.push')}
            </button>
          )}
        </div>
      </div>
    </Dialog>
  )
}
