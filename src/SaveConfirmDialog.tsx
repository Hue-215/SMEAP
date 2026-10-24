import { formatSavedTime } from './autoSave'
import ConfirmDialog from './ConfirmDialog'

interface SaveConfirmDialogProps {
  title: string
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
  const since = savedAt ? `${formatSavedTime(savedAt)}の自動保存より後の変更です` : '開いてからの変更です'

  return (
    <ConfirmDialog
      title="保存していない変更があります"
      confirmLabel="保存する"
      failedText="保存できませんでした"
      onCancel={onCancel}
      onConfirm={onSave}
      secondary={{ label: '保存しない', onClick: onDiscard }}
    >
      「{title || '無題'}」の、{since}。保存しない場合、この分の変更は消えます。
    </ConfirmDialog>
  )
}
