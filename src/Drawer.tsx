import type { ReactNode } from 'react'
import type { DrawerState } from './useDrawer'
import { useSwipeToClose } from './useDrawerSwipe'

// 出入りを見せるため、狭い窓では暗幕を閉じている間も置いておく。
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
