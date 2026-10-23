import { useEffect, useLayoutEffect, useRef, type KeyboardEvent } from 'react'
import type { Article } from './api'
import Icon from './Icon'

type Fields = Pick<Article, 'title' | 'description' | 'pubDate' | 'heroImage'>
export type FieldsPatch = Partial<Omit<Fields, 'heroImage'>>

interface ArticleHeaderProps {
  fields: Fields
  onChange: (patch: FieldsPatch) => void
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

  // 幅が変わると折り返しが変わる
  useEffect(() => {
    const el = ref.current
    if (!el) return
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
      onChange={(e) => onChange(flatten(e.target.value))}
      onKeyDown={blockEnter}
    />
  )
}

export default function ArticleHeader({ fields, onChange }: ArticleHeaderProps) {
  return (
    <header>
      <OneLineField
        className="article-title"
        label="タイトル"
        value={fields.title}
        onChange={(title) => onChange({ title })}
      />
      <OneLineField
        className="article-description"
        label="説明"
        value={fields.description}
        onChange={(description) => onChange({ description })}
      />
      <div className="article-meta">
        <label className="article-meta-item">
          公開日
          {/* 入力途中の日付は値が空になるので制御しない。記事を切り替えたらkeyで作り直す */}
          <input
            type="date"
            required
            defaultValue={fields.pubDate}
            onChange={(e) => {
              const pubDate = e.target.value
              if (pubDate) onChange({ pubDate })
            }}
          />
        </label>
        <div className="article-meta-item">
          サムネイル
          {fields.heroImage != null && (
            <span className="article-meta-value">{fields.heroImage}</span>
          )}
          {/* 画像の取り込み(#8)ができたら押せるようにする */}
          <button type="button" className="article-meta-button" disabled>
            <Icon name="image" />
            {fields.heroImage == null ? '画像を選ぶ' : '差し替える'}
          </button>
        </div>
      </div>
    </header>
  )
}
