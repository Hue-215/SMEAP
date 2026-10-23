import { useEffect, useLayoutEffect, useRef, type KeyboardEvent, type RefObject } from 'react'
import type { Article } from './api'
import Icon from './Icon'

type Fields = Pick<Article, 'title' | 'description' | 'pubDate' | 'heroImage'>
export type FieldsPatch = Partial<Pick<Article, 'title' | 'description' | 'pubDate'>>

interface ArticleHeaderProps {
  fields: Fields
  onChange: (patch: FieldsPatch) => void
}

// field-sizingはLinuxのWebKitGTKが未対応のため、高さは自前で合わせる。
function useAutoHeight(ref: RefObject<HTMLTextAreaElement | null>, value: string) {
  useLayoutEffect(() => {
    const el = ref.current
    if (el) fitHeight(el)
  }, [ref, value])

  // 幅が変わると折り返しが変わる
  useEffect(() => {
    const el = ref.current
    if (!el) return
    const observer = new ResizeObserver(() => fitHeight(el))
    observer.observe(el)
    return () => observer.disconnect()
  }, [ref])
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

export default function ArticleHeader({ fields, onChange }: ArticleHeaderProps) {
  const titleRef = useRef<HTMLTextAreaElement>(null)
  const descriptionRef = useRef<HTMLTextAreaElement>(null)
  useAutoHeight(titleRef, fields.title)
  useAutoHeight(descriptionRef, fields.description)

  return (
    <header>
      <textarea
        ref={titleRef}
        className="article-title"
        rows={1}
        placeholder="タイトル"
        aria-label="タイトル"
        value={fields.title}
        onChange={(e) => onChange({ title: flatten(e.target.value) })}
        onKeyDown={blockEnter}
      />
      <textarea
        ref={descriptionRef}
        className="article-description"
        rows={1}
        placeholder="説明"
        aria-label="説明"
        value={fields.description}
        onChange={(e) => onChange({ description: flatten(e.target.value) })}
        onKeyDown={blockEnter}
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
