import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import Icon from './Icon'
import type { IconName } from './Icon'
import { onPopupListKeyDown } from './usePopup'
import { useCloseOnBack } from './useCloseOnBack'

export interface MenuItem {
  key: string
  label: string
  icon: IconName
  disabled?: boolean
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
  const [pos, setPos] = useState({ x, y })

  // 開くたびに描き直すので、採寸は開いたときの1回で足りる
  useLayoutEffect(() => {
    const list = listRef.current
    if (!list) return
    // はみ出すなら内側へずらす
    setPos({
      x: Math.max(0, Math.min(x, window.innerWidth - list.offsetWidth)),
      y: Math.max(0, Math.min(y, window.innerHeight - list.offsetHeight)),
    })
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
      style={{ left: pos.x, top: pos.y }}
      onBlur={(e) => {
        // 行き先の無いフォーカス喪失(menu-listの余白を押した等)では閉じない。外を押した場合は
        // pointerdownの側で閉じる。
        if (e.relatedTarget && !listRef.current?.contains(e.relatedTarget)) dismiss.current()
      }}
      onKeyDown={(e) => onPopupListKeyDown(e, listRef.current, 'menuitem', close)}
    >
      {items.map((item) => (
        <li key={item.key} role="none">
          <button
            type="button"
            role="menuitem"
            className="menu-item"
            aria-disabled={item.disabled ? true : undefined}
            onClick={() => {
              if (item.disabled) return
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
