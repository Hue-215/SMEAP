import type { ReactNode } from 'react'
import type { DrawerState } from './useDrawer'
import { useSwipeToClose } from './useDrawerSwipe'

// 出入りを見せるため、閉じている間も置いておく。dockedOpenは広い窓での開け閉め。
export function Drawer({
  drawer,
  dockedOpen,
  children,
}: {
  drawer: DrawerState
  dockedOpen: boolean
  children: ReactNode
}) {
  const swipeToClose = useSwipeToClose(drawer)
  const shown = drawer.narrow ? drawer.shown : dockedOpen
  return (
    <>
      {drawer.narrow && (
        <div
          className={drawer.shown ? 'drawer-backdrop scrim open' : 'drawer-backdrop scrim'}
          onClick={drawer.cancel}
          {...swipeToClose}
        />
      )}
      <div id={drawer.id} className={shown ? 'drawer open' : 'drawer'} {...swipeToClose}>
        {children}
      </div>
    </>
  )
}
