import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from 'react'
import type { Article, FrontmatterSettings, Warning } from './api'
import Icon from './Icon'
import { t } from './i18n'
import ImageFileInput from './ImageFileInput'

type Fields = Pick<Article, 'title' | 'description' | 'pubDate' | 'updatedDate'>
export type FieldsPatch = Partial<Fields>

interface ArticleHeaderProps {
  fields: Fields
  warnings: Warning[]
  onChange: (patch: FieldsPatch) => void
  frontmatter: FrontmatterSettings
  hero: { name: string; url: string } | null
  heroBusy: boolean
  onPickHeroImage: (file: File) => void
  onRemoveHeroImage: () => void
  readOnly: boolean
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
  readOnly: boolean
}

// 折り返して高さが伸びる、改行の入らない欄。
// field-sizingはLinuxのWebKitGTKが未対応のため、高さは自前で合わせる。
function OneLineField({ className, label, value, onChange, readOnly }: OneLineFieldProps) {
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
      readOnly={readOnly}
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
  frontmatter,
  hero,
  heroBusy,
  onPickHeroImage,
  onRemoveHeroImage,
  readOnly,
}: ArticleHeaderProps) {
  const fileInput = useRef<HTMLInputElement>(null)
  // 読めなかった画像のURL。差し替えるとURLが変わるので、読み直す
  const [brokenUrl, setBrokenUrl] = useState<string | null>(null)
  // 日付の欄は入力途中の値を持てないので制御しない。保存の結果で値が替わったら、
  // 入力中でない欄だけ書き換える
  const pubDateInput = useRef<HTMLInputElement>(null)
  const updatedDateInput = useRef<HTMLInputElement>(null)
  useEffect(() => {
    for (const [el, value] of [
      [pubDateInput.current, fields.pubDate],
      [updatedDateInput.current, fields.updatedDate],
    ] as const) {
      if (el && document.activeElement !== el && el.value !== (value ?? '')) el.value = value ?? ''
    }
  }, [fields.pubDate, fields.updatedDate])
  return (
    <header>
      {warnings.length > 0 && (
        <div className="article-warnings">
          <Icon name="warning" />
          <div className="article-warnings-lines">
            {warnings.map((warning, index) => (
              <p key={index}>{warning.message}</p>
            ))}
            {warnings.some((warning) => warning.kept) && <p>{t('article_header.kept_note')}</p>}
          </div>
        </div>
      )}
      <div className="article-head">
        <div className="article-head-text">
          <OneLineField
            className="article-title"
            label={t('article_header.title')}
            value={fields.title ?? ''}
            onChange={(title) => onChange({ title })}
            readOnly={readOnly}
          />
          <OneLineField
            className="article-description"
            label={t('article_header.description')}
            value={fields.description ?? ''}
            onChange={(description) => onChange({ description })}
            readOnly={readOnly}
          />
        </div>
        {/* ボタンの中にボタンは置けないので、入れ物で包んで外すボタンを重ねる */}
        {frontmatter.heroImage && (
          <>
            <div className="article-hero-box">
              <button
                type="button"
                className={hero === null ? 'article-hero empty' : 'article-hero'}
                disabled={heroBusy || readOnly}
                onClick={() => fileInput.current?.click()}
                aria-label={hero === null ? undefined : t('article_header.hero_replace')}
                title={hero?.name ?? t('article_header.hero_pick')}
              >
                {hero === null ? (
                  t('article_header.hero_none')
                ) : brokenUrl === hero.url ? (
                  t('article_header.hero_unreadable')
                ) : (
                  <img src={hero.url} alt="" onError={() => setBrokenUrl(hero.url)} />
                )}
              </button>
              {hero !== null && (
                <button
                  type="button"
                  className="icon-button article-hero-remove"
                  disabled={heroBusy || readOnly}
                  onClick={onRemoveHeroImage}
                  aria-label={t('article_header.hero_remove')}
                  title={t('article_header.hero_remove')}
                >
                  <Icon name="close" />
                </button>
              )}
            </div>
            <ImageFileInput ref={fileInput} onFiles={(files) => onPickHeroImage(files[0])} />
          </>
        )}
      </div>
      <div className="article-meta">
        <label className="article-meta-item">
          {t('article_header.pub_date')}
          {/* 入力途中の日付は値が空になるので制御しない。記事を切り替えたらApp.tsxのkeyで作り直す */}
          <input
            ref={pubDateInput}
            type="date"
            required
            max="9999-12-31"
            defaultValue={fields.pubDate ?? ''}
            disabled={readOnly}
            onChange={(e) => {
              const pubDate = e.target.value
              if (pubDate && e.target.validity.valid) onChange({ pubDate })
            }}
            // 入力途中の値と、フォーカスの間に保存の結果で替わった値を、持っている値に合わせる
            onBlur={(e) => {
              e.target.value = fields.pubDate ?? ''
            }}
          />
        </label>
        {frontmatter.updatedDate && (
          <>
            <label className="article-meta-item">
              {t('article_header.updated_date')}
              <input
                ref={updatedDateInput}
                type="date"
                max="9999-12-31"
                defaultValue={fields.updatedDate ?? ''}
                disabled={readOnly}
                onChange={(e) => {
                  const updatedDate = e.target.value
                  if (updatedDate && e.target.validity.valid) onChange({ updatedDate })
                }}
                onBlur={(e) => {
                  e.target.value = fields.updatedDate ?? ''
                }}
              />
            </label>
            {fields.updatedDate !== null && (
              <button
                type="button"
                className="icon-button"
                disabled={readOnly}
                onClick={() => onChange({ updatedDate: null })}
                aria-label={t('article_header.updated_date_remove')}
                title={t('article_header.updated_date_remove')}
              >
                <Icon name="close" />
              </button>
            )}
          </>
        )}
      </div>
    </header>
  )
}
