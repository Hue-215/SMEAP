import { useId, useState, type ReactNode } from 'react'
import Dialog from './Dialog'
import { t } from './i18n'

interface ConfirmDialogProps {
  title: string
  children: ReactNode
  confirmLabel: string
  failedText: string
  onCancel: () => void
  // 成功ならtrue。閉じるのは親が行う
  onConfirm: () => Promise<boolean>
  // キャンセルと確定の間に置くボタン
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
  const running = state === 'running'
  const headingId = useId()
  const bodyId = useId()

  const confirm = () => {
    setState('running')
    void onConfirm().then(
      (ok) => {
        if (!ok) setState('failed')
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
              {failedText}
            </p>
          )}
        </div>
        <div className="dialog-buttons">
          <button type="button" onClick={cancel} disabled={running}>
            {t('common.cancel')}
          </button>
          {secondary && (
            <button type="button" onClick={secondary.onClick} disabled={running}>
              {secondary.label}
            </button>
          )}
          <button type="button" className="primary" onClick={confirm} disabled={running}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </Dialog>
  )
}
