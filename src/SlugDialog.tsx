import { useEffect, useId, useRef, useState, type FormEvent } from 'react'
import { createArticle, renameArticle, isValidSlug, type Article, type StoreError } from './api'
import Dialog from './Dialog'

export type SlugDialogMode = { kind: 'create' } | { kind: 'rename'; slug: string }

interface SlugDialogProps {
  mode: SlugDialogMode
  onDone: (article: Article) => void
  onCancel: () => void
}

const invalidSlugMessage = '使えない文字があります。英小文字・数字・ハイフンだけにしてください'

const texts = {
  create: {
    title: '新しい記事',
    submit: '作成',
    hint: '英小文字・数字・ハイフンが使えます。あとから変更もできます',
    failed: '作成できませんでした',
  },
  rename: {
    title: 'slugを変更',
    submit: '変更',
    hint: '英小文字・数字・ハイフンが使えます',
    failed: '変更できませんでした',
  },
}

const failureMessages = new Map([
  ['alreadyExists', '同じslugのフォルダが既にあります'],
  ['invalidSlug', invalidSlugMessage],
  ['notFound', '元の記事が見つかりません'],
])

export default function SlugDialog({ mode, onDone, onCancel }: SlugDialogProps) {
  const text = texts[mode.kind]
  const currentSlug = mode.kind === 'rename' ? mode.slug : null
  const [value, setValue] = useState(currentSlug ?? '')
  // 送信の失敗の文言
  const [failure, setFailure] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)
  const inputRef = useRef<HTMLInputElement>(null)
  const headingId = useId()
  const messageId = useId()
  const inputId = useId()

  useEffect(() => {
    inputRef.current?.select()
  }, [])

  const error = failure ?? (value !== '' && !isValidSlug(value) ? invalidSlugMessage : null)
  const submitDisabled = submitting || !isValidSlug(value) || value === currentSlug
  const shownSlug = value || '{slug}'

  const onSubmit = (e: FormEvent) => {
    e.preventDefault()
    if (submitDisabled) return
    setSubmitting(true)
    const request = mode.kind === 'create' ? createArticle(value) : renameArticle(mode.slug, value)
    request.then(onDone, (err) => {
      setSubmitting(false)
      console.error(text.failed, err)
      const kind = (err as StoreError | undefined)?.kind
      setFailure((kind && failureMessages.get(kind)) ?? text.failed)
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
          {text.title}
        </h2>
        <div className="slug-dialog-fields">
          <label htmlFor={inputId} className="slug-dialog-label">
            slug(記事のディレクトリ名・URL)
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
            {error ?? text.hint}
          </p>
          <p className="slug-dialog-destination">
            書き出し先: ./src/content/posts/{shownSlug}.md
          </p>
        </div>
        <div className="dialog-buttons">
          <button type="button" onClick={cancel} disabled={submitting}>
            キャンセル
          </button>
          <button type="submit" disabled={submitDisabled}>
            {text.submit}
          </button>
        </div>
      </form>
    </Dialog>
  )
}
