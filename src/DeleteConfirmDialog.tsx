import { useId, useState } from 'react'
import Dialog from './Dialog'

interface DeleteConfirmDialogProps {
  slug: string
  // 壊れた記事ではnull
  title: string | null
  onCancel: () => void
  // 削除できたらtrue。閉じるのは親が行う
  onDelete: () => Promise<boolean>
}

export default function DeleteConfirmDialog({
  slug,
  title,
  onCancel,
  onDelete,
}: DeleteConfirmDialogProps) {
  const [state, setState] = useState<'idle' | 'deleting' | 'failed'>('idle')
  const deleting = state === 'deleting'
  const headingId = useId()
  const bodyId = useId()
  const target = title !== null ? `「${title || '無題'}」(${slug})` : slug

  const remove = () => {
    setState('deleting')
    void onDelete().then((ok) => {
      if (!ok) setState('failed')
    })
  }

  // 削除中に閉じると、削除の後の処理が残る
  const cancel = () => {
    if (!deleting) onCancel()
  }

  return (
    <Dialog labelledBy={headingId} describedBy={bodyId} role="alertdialog" onCancel={cancel}>
      <div className="dialog-form">
        <h2 id={headingId} className="dialog-title">
          記事を削除しますか
        </h2>
        <div className="dialog-body">
          <p id={bodyId}>{target}を削除します。画像とバックアップも消え、元に戻せません。</p>
          {state === 'failed' && (
            <p role="alert" className="save-confirm-error">
              削除できませんでした
            </p>
          )}
        </div>
        <div className="dialog-buttons">
          <button type="button" className="save-confirm-cancel" onClick={cancel} disabled={deleting}>
            キャンセル
          </button>
          <button type="button" className="primary" onClick={remove} disabled={deleting}>
            削除する
          </button>
        </div>
      </div>
    </Dialog>
  )
}
