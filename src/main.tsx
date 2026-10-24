import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './tokens.css'
import './index.css'
import App from './App.tsx'

// メニュー・ダイアログを開いてフォーカスを移したときにも、ブラウザはボタンに:focus-visibleを付ける。
// 最後の入力がマウス・タッチならその枠を消す(index.css)。
const setLastInput = (input: 'pointer' | 'keyboard') => {
  const root = document.documentElement
  if (root.dataset.input !== input) root.dataset.input = input
}
document.addEventListener('pointerdown', () => setLastInput('pointer'), { capture: true })
document.addEventListener('keydown', () => setLastInput('keyboard'), { capture: true })

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
