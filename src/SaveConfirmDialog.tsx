import { formatSavedTime } from './autoSave'
import ConfirmDialog from './ConfirmDialog'
import { t } from './i18n'

interface SaveConfirmDialogProps {
  savedAt: Date | null
  onCancel: () => void
  onDiscard: () => void
  // 保存できたらtrue。できたら閉じるのは親が行う
  onSave: () => Promise<boolean>
}

export default function SaveConfirmDialog({
  savedAt,
  onCancel,
  onDiscard,
  onSave,
}: SaveConfirmDialogProps) {
  return (
    <ConfirmDialog
      title={t('save_confirm.title')}
      confirmLabel={t('save_confirm.save')}
      failedText={t('common.save_failed')}
      onCancel={onCancel}
      onConfirm={onSave}
      secondary={{ label: t('save_confirm.discard'), onClick: onDiscard }}
    >
      {savedAt === null
        ? t('save_confirm.message_since_opened')
        : t('save_confirm.message_since_saved', {
            time: formatSavedTime(savedAt),
          })}
    </ConfirmDialog>
  )
}
