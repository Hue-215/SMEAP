import { onBackButtonPress } from '@tauri-apps/api/app'
import type { PluginListener } from '@tauri-apps/api/core'
import { useEffect, useRef } from 'react'

// Androidの「戻る」(ボタン・ジェスチャー)で閉じるもの。開いた順に積み、「戻る」では最後に積んだ
// ものだけを閉じる(開いたダイアログ → 開いた左のカラムか同期の画面 → 設定画面の順になる)。
// 積むのは効果の中で、Reactは同じ描画の中では子の効果を親より先に走らせるので、親と子が同じ描画で
// 同時に開くと順が逆になる(今はそうした組は無い)。
const stack: { close: () => void }[] = []

// 「戻る」の受け手。閉じるものがある間だけ登録する。登録している間は、Tauri本体が「戻る」を
// 既定の処理へ渡さず画面へ知らせるだけになるので、閉じるものが無くなったら外し、既定の処理
// (WebViewで戻れなければ、アプリを裏へ回す。MainActivity.onBackPressed)に任せる。
let listener: Promise<PluginListener | null> | null = null
// 今の受け手の番号。登録と解除はどちらも非同期なので、外す途中の古い受け手が残っている間に
// 新しい受け手を登録しうる。今の番号の受け手だけが閉じる(1回の「戻る」で2つ閉じない)。
let generation = 0
// 受け手を登録できない端末か(「戻る」を持たないデスクトップ)。一度失敗したら以後は試さない。
let unsupported = false

function syncListener() {
  if (stack.length > 0 && listener === null && !unsupported) {
    const mine = ++generation
    listener = onBackButtonPress(() => {
      if (mine === generation) stack.at(-1)?.close()
    }).catch(() => {
      unsupported = true
      return null
    })
  } else if (stack.length === 0 && listener !== null) {
    const registered = listener
    listener = null
    generation++
    void registered.then((l) => l?.unregister()).catch(() => {})
  }
}

// `active`の間、「戻る」で`close`を呼ぶ。`close`は描くたびに変わってよい(最新のものを呼ぶ)。
export function useCloseOnBack(active: boolean, close: () => void) {
  const closeRef = useRef(close)
  useEffect(() => {
    closeRef.current = close
  })
  useEffect(() => {
    if (!active) return
    const entry = { close: () => closeRef.current() }
    stack.push(entry)
    syncListener()
    return () => {
      stack.splice(stack.indexOf(entry), 1)
      syncListener()
    }
  }, [active])
}
