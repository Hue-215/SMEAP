import { useEffect, useLayoutEffect, useRef, type KeyboardEvent } from 'react'
import Icon from './Icon'
import type { IconName } from './Icon'
import { useCloseOnBack } from './useCloseOnBack'

export interface MenuItem {
  key: string
  label: string
  icon: IconName
  onSelect: () => void
}

// (x, y)に開く操作の一覧。外を押すか、フォーカスが外へ移ると閉じる。Esc・Androidの「戻る」・
// 項目を選んで閉じたときは、開く前にフォーカスのあった要素へ戻す。
export default function ContextMenu({
  x,
  y,
  items,
  onClose,
}: {
  x: number
  y: number
  items: MenuItem[]
  onClose: () => void
}) {
  const listRef = useRef<HTMLUListElement>(null)
  const previousFocus = useRef(document.activeElement as HTMLElement | null)

  // はみ出すなら内側へずらす
  useLayoutEffect(() => {
    const list = listRef.current
    if (!list) return
    list.style.left = `${Math.max(0, Math.min(x, window.innerWidth - list.offsetWidth))}px`
    list.style.top = `${Math.max(0, Math.min(y, window.innerHeight - list.offsetHeight))}px`
    list.querySelector<HTMLElement>('[role="menuitem"]')?.focus()
  }, [x, y])

  const dismiss = useRef(onClose)
  useEffect(() => {
    dismiss.current = onClose
  })
  const close = () => {
    onClose()
    previousFocus.current?.focus()
  }

  useCloseOnBack(true, close)

  useEffect(() => {
    const onPointerDown = (e: PointerEvent) => {
      if (!listRef.current?.contains(e.target as Node)) dismiss.current()
    }
    document.addEventListener('pointerdown', onPointerDown)
    return () => document.removeEventListener('pointerdown', onPointerDown)
  }, [])

  return (
    <ul
      className="menu-list"
      role="menu"
      ref={listRef}
      onBlur={(e) => {
        // 行き先の無いフォーカス喪失(menu-listの余白を押した等)では閉じない。外を押した場合は
        // pointerdownの側で閉じる。
        if (e.relatedTarget && !listRef.current?.contains(e.relatedTarget)) dismiss.current()
      }}
      onKeyDown={(e) => onKeyDown(e, listRef.current, close)}
    >
      {items.map((item) => (
        <li key={item.key} role="none">
          <button
            type="button"
            role="menuitem"
            className="menu-item"
            onClick={() => {
              close()
              item.onSelect()
            }}
          >
            <Icon name={item.icon} />
            {item.label}
          </button>
        </li>
      ))}
    </ul>
  )
}

function onKeyDown(e: KeyboardEvent, list: HTMLElement | null, onClose: () => void) {
  if (e.key === 'Escape') {
    e.preventDefault()
    onClose()
    return
  }
  if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return
  e.preventDefault()
  const rows = Array.from(list?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [])
  if (rows.length === 0) return
  const at = rows.indexOf(document.activeElement as HTMLElement)
  const next = e.key === 'ArrowDown' ? Math.min(at + 1, rows.length - 1) : Math.max(at - 1, 0)
  rows[next].focus()
}
