import { useEffect, useRef } from 'react'
import Icon from './Icon'
import { t } from './i18n'
import { useCloseOnBack } from './useCloseOnBack'

// 本文の検索の進み具合。currentは0始まりで、選択がどの一致とも同じでなければ-1
export type FindStatus = { current: number; total: number }

interface SearchBarProps {
  // box: デスクトップで本文の上に浮かせる箱。bar: Androidで上のバーの代わりの行
  variant: 'box' | 'bar'
  query: string
  status: FindStatus
  // 増えるたびに、入力欄へフォーカスして文字を全選択する
  focusRequest: number
  onQueryChange: (query: string) => void
  onNext: () => void
  onPrev: () => void
  onClose: () => void
}

export default function SearchBar({
  variant,
  query,
  status,
  focusRequest,
  onQueryChange,
  onNext,
  onPrev,
  onClose,
}: SearchBarProps) {
  const input = useRef<HTMLInputElement>(null)
  useCloseOnBack(true, onClose)

  useEffect(() => {
    input.current?.focus()
    input.current?.select()
  }, [focusRequest])

  const count = !query
    ? ''
    : status.total === 0
      ? t('search.not_found')
      : status.current < 0
        ? t('search.total', { total: status.total })
        : t('search.count', { current: status.current + 1, total: status.total })

  return (
    <div
      role="search"
      data-find="true"
      className={`find find-${variant}`}
      onKeyDown={(e) => {
        if (e.key === 'Escape') {
          e.preventDefault()
          onClose()
        }
      }}
    >
      {variant === 'bar' && (
        <button
          type="button"
          className="icon-button"
          onClick={onClose}
          aria-label={t('search.close')}
          title={t('search.close')}
        >
          <Icon name="arrow_back" />
        </button>
      )}
      <label className="find-input">
        {variant === 'box' && <Icon name="search" />}
        <input
          ref={input}
          value={query}
          aria-label={t('search.label')}
          onChange={(e) => onQueryChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key !== 'Enter') return
            e.preventDefault()
            if (e.shiftKey) onPrev()
            else onNext()
          }}
        />
      </label>
      <span role="status" className="find-count">
        {count}
      </span>
      <button
        type="button"
        className="icon-button"
        onClick={onPrev}
        aria-label={t('search.prev')}
        title={t('search.prev')}
      >
        <Icon name="keyboard_arrow_up" />
      </button>
      <button
        type="button"
        className="icon-button"
        onClick={onNext}
        aria-label={t('search.next')}
        title={t('search.next')}
      >
        <Icon name="keyboard_arrow_down" />
      </button>
      {variant === 'box' && (
        <button
          type="button"
          className="icon-button"
          onClick={onClose}
          aria-label={t('search.close')}
          title={t('search.close')}
        >
          <Icon name="close" />
        </button>
      )}
    </div>
  )
}
