import { useEffect, useLayoutEffect, useRef, type KeyboardEvent } from 'react'
import type { Article, Warning } from './api'
import Icon from './Icon'
import ImageFileInput from './ImageFileInput'

type Fields = Pick<Article, 'title' | 'description' | 'pubDate' | 'heroImage'>
export type FieldsPatch = Partial<Omit<Fields, 'heroImage'>>

interface ArticleHeaderProps {
  fields: Fields
  warnings: Warning[]
  onChange: (patch: FieldsPatch) => void
  onPickHeroImage: (file: File) => void
}

function fitHeight(el: HTMLTextAreaElement) {
  el.style.height = 'auto'
  el.style.height = `${el.scrollHeight}px`
}

// 貼り付けで入った改行はスペースにする
const flatten = (value: string) => value.replace(/\r\n|\n|\r/g, ' ')

const blockEnter = (e: KeyboardEvent<HTMLTextAreaElement>) => {
  if (e.key === 'Enter' && !e.nativeEvent.isComposing) e.preventDefault()
}

interface OneLineFieldProps {
  className: string
  label: string
  value: string
  onChange: (value: string) => void
}

// 折り返して高さが伸びる、改行の入らない欄。
// field-sizingはLinuxのWebKitGTKが未対応のため、高さは自前で合わせる。
function OneLineField({ className, label, value, onChange }: OneLineFieldProps) {
  const ref = useRef<HTMLTextAreaElement>(null)

  useLayoutEffect(() => {
    if (ref.current) fitHeight(ref.current)
  }, [value])

  // 幅が変わったときと書体が読み込まれたときは、折り返しが変わる
  useEffect(() => {
    const el = ref.current
    if (!el) return
    void document.fonts.ready.then(() => fitHeight(el))
    let width = el.clientWidth
    const observer = new ResizeObserver(() => {
      if (el.clientWidth === width) return
      width = el.clientWidth
      fitHeight(el)
    })
    observer.observe(el)
    return () => observer.disconnect()
  }, [])

  return (
    <textarea
      ref={ref}
      className={className}
      rows={1}
      placeholder={label}
      aria-label={label}
      value={value}
      onChange={(e) => {
        const el = e.target
        const flat = flatten(el.value)
        // 値を書き換えるとカーソルが末尾へ飛ぶので、元の位置に戻す
        if (flat !== el.value) {
          const caret = flatten(el.value.slice(0, el.selectionStart)).length
          el.value = flat
          el.setSelectionRange(caret, caret)
        }
        onChange(flat)
      }}
      onKeyDown={blockEnter}
    />
  )
}

export default function ArticleHeader({
  fields,
  warnings,
  onChange,
  onPickHeroImage,
}: ArticleHeaderProps) {
  const fileInput = useRef<HTMLInputElement>(null)
  return (
    <header>
      {warnings.length > 0 && (
        <div className="article-warnings">
          <Icon name="warning" />
          <div className="article-warnings-lines">
            {warnings.map((warning, index) => (
              <p key={index}>{warning.message}</p>
            ))}
            {warnings.some((warning) => warning.kept) && (
              <p>読めなかった項目は、入力するまで元の行のまま保存します</p>
            )}
          </div>
        </div>
      )}
      <OneLineField
        className="article-title"
        label="タイトル"
        value={fields.title ?? ''}
        onChange={(title) => onChange({ title })}
      />
      <OneLineField
        className="article-description"
        label="説明"
        value={fields.description ?? ''}
        onChange={(description) => onChange({ description })}
      />
      <div className="article-meta">
        <label className="article-meta-item">
          公開日
          {/* 入力途中の日付は値が空になるので制御しない。記事を切り替えたらApp.tsxのkeyで作り直す */}
          <input
            type="date"
            required
            max="9999-12-31"
            defaultValue={fields.pubDate ?? ''}
            onChange={(e) => {
              const pubDate = e.target.value
              if (pubDate && e.target.validity.valid) onChange({ pubDate })
            }}
            onBlur={(e) => {
              if (!e.target.value || !e.target.validity.valid)
                e.target.value = fields.pubDate ?? ''
            }}
          />
        </label>
        <div className="article-meta-item">
          サムネイル
          {fields.heroImage != null && (
            <span className="article-meta-value">{fields.heroImage}</span>
          )}
          <button
            type="button"
            className="article-meta-button"
            onClick={() => fileInput.current?.click()}
          >
            <Icon name="image" />
            {fields.heroImage == null ? '画像を選ぶ' : '差し替える'}
          </button>
          <ImageFileInput
            ref={fileInput}
            onFiles={(files) => onPickHeroImage(files[0])}
          />
        </div>
      </div>
    </header>
  )
}
