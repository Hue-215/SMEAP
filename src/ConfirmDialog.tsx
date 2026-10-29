import { useId, useState, type ReactNode } from 'react'
import Dialog from './Dialog'
import { t } from './i18n'

interface ConfirmDialogProps {
  title: string
  children: ReactNode
  confirmLabel: string
  failedText: string
  onCancel: () => void
  // 成功ならtrue。失敗ならfalseか、ダイアログに出す文言。閉じるのは親が行う
  onConfirm: () => Promise<boolean | string>
  // キャンセルの左に置くボタン。確定の左にも置かれる
  secondary?: { label: string; onClick: () => void }
}

export default function ConfirmDialog({
  title,
  children,
  confirmLabel,
  failedText,
  onCancel,
  onConfirm,
  secondary,
}: ConfirmDialogProps) {
  const [state, setState] = useState<'idle' | 'running' | 'failed'>('idle')
  const [failure, setFailure] = useState<string | null>(null)
  const running = state === 'running'
  const headingId = useId()
  const bodyId = useId()

  const confirm = () => {
    setState('running')
    setFailure(null)
    void onConfirm().then(
      (result) => {
        if (result !== true) {
          setFailure(typeof result === 'string' ? result : null)
          setState('failed')
        }
      },
      () => setState('failed'),
    )
  }

  // 処理中に閉じると、処理の後の操作が残る
  const cancel = () => {
    if (!running) onCancel()
  }

  return (
    <Dialog labelledBy={headingId} describedBy={bodyId} role="alertdialog" onCancel={cancel}>
      <div className="dialog-form">
        <h2 id={headingId} className="dialog-title">
          {title}
        </h2>
        <div className="dialog-body">
          <p id={bodyId}>{children}</p>
          {state === 'failed' && (
            <p role="alert" className="confirm-dialog-error">
              {failure ?? failedText}
            </p>
          )}
        </div>
        <div className="dialog-buttons">
          {secondary && (
            <button type="button" className="left" onClick={secondary.onClick} disabled={running}>
              {secondary.label}
            </button>
          )}
          <button type="button" onClick={cancel} disabled={running}>
            {t('common.cancel')}
          </button>
          <button type="button" className="primary" onClick={confirm} disabled={running}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </Dialog>
  )
}
