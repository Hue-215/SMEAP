import { useEffect, useId, useRef, useState, type FormEvent } from 'react'
import { createArticle, renameArticle, isValidSlug, type Article, type StoreError } from './api'
import Dialog from './Dialog'
import { t, type MessageKey } from './i18n'

export type SlugDialogMode = { kind: 'create' } | { kind: 'rename'; slug: string }

interface SlugDialogProps {
  mode: SlugDialogMode
  destination: string | null
  onDone: (article: Article) => void
  onCancel: () => void
}

const texts = {
  create: {
    title: 'slug_dialog.create_title',
    submit: 'slug_dialog.create_submit',
    hint: 'slug_dialog.create_hint',
    failed: 'slug_dialog.create_failed',
  },
  rename: {
    title: 'slug_dialog.rename_title',
    submit: 'slug_dialog.rename_submit',
    hint: 'slug_dialog.rename_hint',
    failed: 'common.change_failed',
  },
} satisfies Record<SlugDialogMode['kind'], Record<string, MessageKey>>

const failureMessages = new Map<string, MessageKey>([
  ['alreadyExists', 'slug_dialog.already_exists'],
  ['invalidSlug', 'slug_dialog.invalid'],
  ['notFound', 'slug_dialog.not_found'],
])

export default function SlugDialog({ mode, destination, onDone, onCancel }: SlugDialogProps) {
  const text = texts[mode.kind]
  const currentSlug = mode.kind === 'rename' ? mode.slug : null
  const [value, setValue] = useState(currentSlug ?? '')
  // 送信の失敗の文言
  const [failure, setFailure] = useState<MessageKey | null>(null)
  const [submitting, setSubmitting] = useState(false)
  const inputRef = useRef<HTMLInputElement>(null)
  const headingId = useId()
  const messageId = useId()
  const inputId = useId()

  useEffect(() => {
    inputRef.current?.select()
  }, [])

  const error = failure ?? (value !== '' && !isValidSlug(value) ? 'slug_dialog.invalid' : null)
  const submitDisabled = submitting || !isValidSlug(value) || value === currentSlug
  const shownSlug = value || '{slug}'

  const onSubmit = (e: FormEvent) => {
    e.preventDefault()
    if (submitDisabled) return
    setSubmitting(true)
    const request = mode.kind === 'create' ? createArticle(value) : renameArticle(mode.slug, value)
    request.then(onDone, (err) => {
      setSubmitting(false)
      console.error('Failed to set the slug', mode.kind, err)
      const kind = (err as StoreError | undefined)?.kind
      setFailure((kind ? failureMessages.get(kind) : undefined) ?? text.failed)
    })
  }

  // 送信中に閉じると、閉じたあとに結果が反映されてしまう
  const cancel = () => {
    if (!submitting) onCancel()
  }

  return (
    <Dialog labelledBy={headingId} onCancel={cancel}>
      <form className="dialog-form" onSubmit={onSubmit}>
        <h2 id={headingId} className="dialog-title">
          {t(text.title)}
        </h2>
        <div className="slug-dialog-fields">
          <label htmlFor={inputId} className="slug-dialog-label">
            {t('slug_dialog.label')}
          </label>
          <input
            ref={inputRef}
            id={inputId}
            className="slug-dialog-input"
            type="text"
            value={value}
            onChange={(e) => {
              setValue(e.target.value)
              setFailure(null)
            }}
            autoFocus
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            aria-describedby={messageId}
            aria-invalid={error !== null || undefined}
          />
          <p
            id={messageId}
            aria-live="polite"
            className={error !== null ? 'slug-dialog-hint error' : 'slug-dialog-hint'}
          >
            {t(error ?? text.hint)}
          </p>
          {destination !== null && (
            <p className="slug-dialog-destination">
              {t('slug_dialog.destination', { path: `${destination}/${shownSlug}.md` })}
            </p>
          )}
        </div>
        <div className="dialog-buttons">
          <button type="button" onClick={cancel} disabled={submitting}>
            {t('common.cancel')}
          </button>
          <button type="submit" disabled={submitDisabled}>
            {t(text.submit)}
          </button>
        </div>
      </form>
    </Dialog>
  )
}
