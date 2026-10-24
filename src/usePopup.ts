import type { KeyboardEvent } from 'react'

// 開いた一覧の中のキー操作。Escで閉じ、上下の矢印で`role`の行の間をフォーカスが移る。
export function onPopupListKeyDown(
  e: KeyboardEvent,
  list: HTMLElement | null,
  role: 'option' | 'menuitem',
  onClose: () => void,
) {
  if (e.key === 'Escape') {
    e.preventDefault()
    onClose()
    return
  }
  if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return
  e.preventDefault()
  const rows = Array.from(list?.querySelectorAll<HTMLElement>(`[role="${role}"]`) ?? [])
  if (rows.length === 0) return
  const at = rows.indexOf(document.activeElement as HTMLElement)
  const next = e.key === 'ArrowDown' ? Math.min(at + 1, rows.length - 1) : Math.max(at - 1, 0)
  rows[next].focus()
}
