import { useEffect, useRef, type TouchEvent } from 'react'
import type { DrawerState } from './useDrawer'

// 畳んだカラムの隣の内容に付けるもの。畳んである間、内容のどこからでも右へスワイプすれば引き出す。
// 画面の端から始めたスワイプは、Androidのジェスチャーナビゲーションでは「戻る」に取られる。
export function useSwipeToOpen(drawer: DrawerState) {
  return useDrawerDrag(drawer, 'open', drawer.narrow && !drawer.shown)
}

// 引き出したカラムと暗幕に付けるもの。左へスワイプすれば閉じる。
export function useSwipeToClose(drawer: DrawerState) {
  return useDrawerDrag(drawer, 'close', drawer.shown)
}

// 指を離したときに、開け閉めを確定する横の移動の量(px)。足りなければ元の位置へ戻す。
const SWIPE_DISTANCE = 48
// 指の動きを横へのスワイプか縦のスクロールかに分けるまでの遊び(px)。横にこれだけ動き、かつ
// 縦の2倍を超えたらスワイプとして指に付いて動かし始め、先に縦へこれだけ動いたらスクロールと
// して諦める。
const SLOP = 10

interface Gesture {
  x: number
  y: number
  // 指に付いて動かしている間の、開け閉めする向き(toward)への横の移動の量。
  moved: number
  dragging: boolean
  // カラムの幅と、カラム・暗幕を包む入れ物(.layout)。
  width: number
  container: HTMLElement
}

// 指が横に動く間、カラムを指に付いて動かし、暗幕を引き出した割合で濃くする。動かしている間は
// 入れ物に`.drawer-dragging`を付け、引き出した割合(0〜1)を`--drawer-progress`で渡す(index.css)。
// 描き直しを経ずに指に追従させるため、Reactの状態ではなく要素へ直に書く。指を離したら、
// 開け閉めする向きへ`SWIPE_DISTANCE`以上動いていれば開け閉めを確定し、どちらでも元の書き込みを
// 外してCSSの移り変わりに任せる。`enabled`が偽の間は受け手を返さない。
function useDrawerDrag(drawer: DrawerState, toward: 'open' | 'close', enabled: boolean) {
  const gesture = useRef<Gesture | null>(null)
  const sign = toward === 'open' ? 1 : -1

  // 動かしている途中の書き込みを外す。指を離したとき・受け手が外れたとき(動かしている途中に
  // Escや「戻る」で閉じた、窓が広がった等で、指を離した知らせが届かなくなる)に呼ぶ。外さないと、
  // 次に畳んだときにカラムが途中の位置に出たまま残る。
  const reset = () => {
    const g = gesture.current
    gesture.current = null
    if (!g?.dragging) return false
    g.container.classList.remove('drawer-dragging')
    g.container.style.removeProperty('--drawer-progress')
    return true
  }

  useEffect(() => {
    if (!enabled) return
    return () => {
      reset()
    }
  }, [enabled])

  const finish = (commit: boolean) => {
    const moved = gesture.current?.moved ?? 0
    if (reset() && commit && moved >= SWIPE_DISTANCE) {
      if (toward === 'open') drawer.open()
      else drawer.cancel()
    }
  }

  if (!enabled) return undefined
  return {
    onTouchStart: (e: TouchEvent<HTMLElement>) => {
      const target = e.target as Element
      const touch = e.touches[0]
      const column = document.getElementById(drawer.id)
      gesture.current =
        column?.parentElement &&
        e.touches.length === 1 &&
        // ダイアログ等は画面上は外にあっても、Reactのイベントは中の部品から届く。
        e.currentTarget.contains(target) &&
        // 横の動きを自分で使う部品(入力欄の中の文字の選択・カーソルの移動、開いた一覧)。
        !target.closest(
          'input, textarea, select, [contenteditable]:not([contenteditable="false"]), .menu-list',
        ) &&
        !scrollsSideways(target, e.currentTarget, sign)
          ? {
              x: touch.clientX,
              y: touch.clientY,
              moved: 0,
              dragging: false,
              width: column.offsetWidth,
              container: column.parentElement,
            }
          : null
    },
    onTouchMove: (e: TouchEvent<HTMLElement>) => {
      const g = gesture.current
      if (g === null || e.touches.length !== 1) return
      const dx = (e.touches[0].clientX - g.x) * sign
      const dy = Math.abs(e.touches[0].clientY - g.y)
      if (!g.dragging) {
        if (dx > SLOP && dx > dy * 2) {
          // 文字を選んでいる指の動きでは開け閉めしない(前に選んだ文字が残っている間も同じ)。
          if (window.getSelection()?.isCollapsed === false) {
            gesture.current = null
            return
          }
          g.dragging = true
          g.container.classList.add('drawer-dragging')
        } else {
          if (dy > SLOP) gesture.current = null
          return
        }
      }
      g.moved = Math.min(Math.max(dx, 0), g.width)
      const shown = g.moved / g.width
      g.container.style.setProperty(
        '--drawer-progress',
        String(toward === 'open' ? shown : 1 - shown),
      )
    },
    onTouchEnd: () => finish(true),
    onTouchCancel: () => finish(false),
  }
}

// 触れた所から`root`までに、指の向きへまだ横にスクロールできる入れ物があるか。あれば、その
// スワイプは入れ物のスクロールに使う。指が右へ動くと、中身は左の端へ向かって戻る。
function scrollsSideways(target: Element, root: Element, sign: 1 | -1): boolean {
  for (let el: Element | null = target; el && el !== root; el = el.parentElement) {
    if (el.scrollWidth <= el.clientWidth) continue
    const overflow = getComputedStyle(el).overflowX
    if (overflow !== 'auto' && overflow !== 'scroll') continue
    const canMove =
      sign > 0 ? el.scrollLeft > 0 : el.scrollLeft + el.clientWidth < el.scrollWidth - 1
    if (canMove) return true
  }
  return false
}
