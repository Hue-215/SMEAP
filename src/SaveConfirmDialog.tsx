import { useId, useState } from 'react'
import { formatSavedTime } from './autoSave'
import Dialog from './Dialog'

interface SaveConfirmDialogProps {
  title: string
  // この記事を開いてから最後に保存できた時刻
  savedAt: Date | null
  onCancel: () => void
  onDiscard: () => void
  // 保存できたらtrue。できたら閉じるのは親が行う
  onSave: () => Promise<boolean>
}

export default function SaveConfirmDialog({
  title,
  savedAt,
  onCancel,
  onDiscard,
  onSave,
}: SaveConfirmDialogProps) {
  const [saving, setSaving] = useState(false)
  const [failed, setFailed] = useState(false)
  const headingId = useId()
  const bodyId = useId()

  const since = savedAt ? `${formatSavedTime(savedAt)}の自動保存より後の変更です` : '開いてからの変更です'

  const save = () => {
    setSaving(true)
    setFailed(false)
    void onSave().then((ok) => {
      if (ok) return
      setSaving(false)
      setFailed(true)
    })
  }

  // 保存中に閉じると、保存の後の操作が残る
  const cancel = () => {
    if (!saving) onCancel()
  }

  return (
    <Dialog labelledBy={headingId} describedBy={bodyId} role="alertdialog" onCancel={cancel}>
      <div className="dialog-form">
        <h2 id={headingId} className="dialog-title">
          保存していない変更があります
        </h2>
        <div className="save-confirm-body">
          <p id={bodyId}>
            「{title || '無題'}」の、{since}。保存しない場合、この分の変更は消えます。
          </p>
          {failed && (
            <p role="alert" className="save-confirm-error">
              保存できませんでした
            </p>
          )}
        </div>
        <div className="dialog-buttons">
          <button type="button" className="save-confirm-cancel" onClick={cancel} disabled={saving}>
            キャンセル
          </button>
          <button type="button" onClick={onDiscard} disabled={saving}>
            保存しない
          </button>
          <button type="button" className="primary" onClick={save} disabled={saving}>
            保存する
          </button>
        </div>
      </div>
    </Dialog>
  )
}
