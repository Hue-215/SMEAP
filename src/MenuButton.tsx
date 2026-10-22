import { useLayoutEffect, useRef } from 'react'
import Icon from './Icon'
import type { IconName } from './Icon'
import { onPopupListKeyDown, usePopup } from './usePopup'

export interface MenuItem {
  key: string
  label: string
  icon: IconName
  disabled?: boolean
  onSelect: () => void
}

// 押すと操作の一覧を開くボタン(︙)。一覧はボタンの下に、右端を揃えて開く(index.css)。項目を
// 選ぶと閉じてから操作を呼ぶ。
export default function MenuButton({
  label,
  items,
}: {
  // ボタンの名前(読み上げ・ツールチップ)。
  label: string
  items: MenuItem[]
}) {
  const { open, toggle, close, rootRef, toggleRef, onBlur } = usePopup()
  return (
    <div className="menu" ref={rootRef} onBlur={onBlur}>
      <button
        type="button"
        ref={toggleRef}
        className="icon-button"
        aria-label={label}
        title={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={toggle}
      >
        <Icon name="more_vert" />
      </button>
      {open && <MenuList items={items} onClose={close} />}
    </div>
  )
}

// 開いている間だけ存在する。開いたら先頭の項目へフォーカスを移す。
function MenuList({ items, onClose }: { items: MenuItem[]; onClose: () => void }) {
  const listRef = useRef<HTMLUListElement>(null)
  useLayoutEffect(() => {
    listRef.current?.querySelector<HTMLElement>('[role="menuitem"]')?.focus()
  }, [])
  return (
    <ul
      className="menu-list"
      role="menu"
      ref={listRef}
      onKeyDown={(e) => onPopupListKeyDown(e, listRef.current, 'menuitem', onClose)}
    >
      {items.map((item) => (
        <li key={item.key} role="none">
          <button
            type="button"
            role="menuitem"
            className="menu-item"
            disabled={item.disabled}
            onClick={() => {
              onClose()
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
