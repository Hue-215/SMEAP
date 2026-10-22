import {
  type FocusEvent,
  type KeyboardEvent,
  type RefObject,
  useCallback,
  useEffect,
  useRef,
  useState,
} from 'react'
import { useCloseOnBack } from './useCloseOnBack'

export interface Popup {
  open: boolean
  toggle: () => void
  // 選んだ・Escで閉じる。開くボタンへフォーカスを戻す。
  close: () => void
  // 開くボタンと一覧を包む入れ物。外を押したかの判定に使う。
  rootRef: RefObject<HTMLDivElement | null>
  toggleRef: RefObject<HTMLButtonElement | null>
  // 入れ物に付ける。フォーカスが外へ移ったら閉じる。
  onBlur: (e: FocusEvent<HTMLDivElement>) => void
}

// ボタンの上下に開く一覧(選択一覧・メニュー)の開け閉め。開いている一覧は、外を押すか、
// フォーカスが外へ移るか、Androidの「戻る」で閉じる。隣り合う一覧のうち1つだけが開いている
// 状態はこれで保つ。
export function usePopup(): Popup {
  const [open, setOpen] = useState(false)
  const rootRef = useRef<HTMLDivElement>(null)
  const toggleRef = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (!open) return
    const onPointerDown = (e: PointerEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('pointerdown', onPointerDown)
    return () => document.removeEventListener('pointerdown', onPointerDown)
  }, [open])

  const close = useCallback(() => {
    setOpen(false)
    toggleRef.current?.focus()
  }, [])
  // Androidの「戻る」はEscと同じく閉じる。
  useCloseOnBack(open, close)

  return {
    open,
    toggle: () => setOpen((v) => !v),
    close,
    rootRef,
    toggleRef,
    onBlur: (e) => {
      // 行き先の無いフォーカス喪失(一覧の余白を押した等)では閉じない。外を押した場合は
      // pointerdownの側で閉じる。
      if (e.relatedTarget && !rootRef.current?.contains(e.relatedTarget)) setOpen(false)
    },
  }
}

/**
 * 開いた一覧の中のキー操作。Escで閉じ、上下の矢印で`role`の行の間をフォーカスが移る。
 */
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
