import { useCallback, useEffect, useId, useRef, useState, useSyncExternalStore } from 'react'
import { FOCUSABLE_SELECTOR } from './keyboard'
import { useCloseOnBack } from './useCloseOnBack'

// 左のカラム(記事一覧のサイドバー)を畳む窓の幅。値はtokens.css末尾の「窓の幅に関わる値の
// 導出式」で求めたもの。
const NARROW_QUERY = '(width < 39rem)'

const narrowMedia = window.matchMedia(NARROW_QUERY)

function subscribeNarrow(onChange: () => void) {
  narrowMedia.addEventListener('change', onChange)
  return () => narrowMedia.removeEventListener('change', onChange)
}

export interface DrawerState {
  // 左のカラムを畳む幅か。
  narrow: boolean
  // 畳んだカラムを引き出しているか。広い窓では常にfalse。
  shown: boolean
  // 狭い窓ではshownと同じ。広い窓ではサイドバーを開いているか。
  expanded: boolean
  toggle: () => void
  // open・close・cancelは狭い窓の引き出しだけを開け閉めする。
  open: () => void
  // カラムの中で行き先を選んだときに閉じる。行き先の画面に移るので、フォーカスは開くボタンへ
  // 戻さない(ヘッダーごと作り直されることもある)。
  close: () => void
  // 選ばずに閉じる(Esc・暗幕)。開くボタンへフォーカスを戻す。
  cancel: () => void
  // カラムの入れ物と開くボタンのid(Drawer.tsx・TopBar.tsx)。
  id: string
  toggleId: string
}

// 左のカラムの開け閉め。狭い窓では内容に重ねて引き出し、引き出している間、呼び出し側は
// カラムの隣の内容を`inert`にして操作できなくする。
export function useDrawer(): DrawerState {
  const narrow = useSyncExternalStore(subscribeNarrow, () => narrowMedia.matches)
  const [opened, setOpened] = useState(false)
  const [docked, setDocked] = useState(true)
  // 広げたら閉じる。開いたまま残すと、次に狭めたときに引き出された状態で出る。
  if (!narrow && opened) setOpened(false)
  const shown = narrow && opened

  const id = useId()
  const toggleId = `${id}-toggle`
  const returnFocus = useRef(false)

  const cancel = useCallback(() => {
    returnFocus.current = true
    setOpened(false)
  }, [])
  // Androidの「戻る」はEscと同じく閉じる。
  useCloseOnBack(shown, cancel)

  useEffect(() => {
    if (!shown) {
      if (returnFocus.current) {
        returnFocus.current = false
        document.getElementById(toggleId)?.focus()
      }
      return
    }
    // 選んでいる記事(aria-currentを付けた行)があればそこへ、無ければ先頭へ移す。
    const column = document.getElementById(id)
    ;(
      column?.querySelector<HTMLElement>('[aria-current]') ??
      column?.querySelector<HTMLElement>(FOCUSABLE_SELECTOR)
    )?.focus()
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') cancel()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [shown, cancel, id, toggleId])

  return {
    narrow,
    shown,
    expanded: narrow ? shown : docked,
    toggle: () => {
      if (!narrow) setDocked((v) => !v)
      else if (shown) cancel()
      else setOpened(true)
    },
    open: () => setOpened(true),
    close: () => setOpened(false),
    cancel,
    id,
    toggleId,
  }
}
