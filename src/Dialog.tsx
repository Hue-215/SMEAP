import { useEffect, useRef, type ReactNode } from 'react'
import { useCloseOnBack } from './useCloseOnBack'

interface DialogProps {
  labelledBy: string
  // Esc・Androidの「戻る」で呼ぶ。閉じるのは親が外して行う
  onCancel: () => void
  children: ReactNode
}

// 描画されている間だけ開いているモーダル。
export default function Dialog({ labelledBy, onCancel, children }: DialogProps) {
  const ref = useRef<HTMLDialogElement>(null)
  useEffect(() => {
    ref.current?.showModal()
  }, [])
  useCloseOnBack(true, onCancel)
  return (
    <dialog
      ref={ref}
      className="dialog"
      aria-labelledby={labelledBy}
      onCancel={(e) => {
        e.preventDefault()
        onCancel()
      }}
    >
      {children}
    </dialog>
  )
}
