import type { ReactNode } from 'react'
import type { DrawerState } from './useDrawer'
import { useSwipeToClose } from './useDrawerSwipe'

// 狭い窓で畳む左のカラムの入れ物。引き出している間は、内容の上に重ね、後ろに暗幕を敷く。
// 引き出したカラムと暗幕は、左へのスワイプでも閉じる。カラムは左から滑って出入りし、暗幕は
// 引き出した割合に合わせて濃くなる(index.css)。出入りを見せるため、狭い窓では暗幕を閉じている
// 間も置いておく。
export function Drawer({ drawer, children }: { drawer: DrawerState; children: ReactNode }) {
  const swipeToClose = useSwipeToClose(drawer)
  return (
    <>
      {drawer.narrow && (
        <div
          className={drawer.shown ? 'drawer-backdrop scrim open' : 'drawer-backdrop scrim'}
          onClick={drawer.cancel}
          {...swipeToClose}
        />
      )}
      <div id={drawer.id} className={drawer.shown ? 'drawer open' : 'drawer'} {...swipeToClose}>
        {children}
      </div>
    </>
  )
}
