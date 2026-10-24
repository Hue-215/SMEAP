import ja from '../lang/ja.json'

// 画面の文言を言語ファイル(lang/*.json)から引く。キーは`画面.要素`のドット区切りで、入れ子にしない。
// 差し込む値は`{名前}`で書き、`{`・`}`は文言そのものには使わない。

export type MessageKey = keyof typeof ja

const messages: Record<string, string> = ja

const PLACEHOLDER = /\{([A-Za-z0-9_]+)\}/g

export function t(key: MessageKey, params: Record<string, string | number> = {}): string {
  const text = messages[key] ?? key
  // 1回の走査で置き換えるので、差し込んだ値の中の`{名前}`は置き換えない
  return text.replace(PLACEHOLDER, (match, name: string) => {
    const value = params[name]
    if (value !== undefined) return String(value)
    if (import.meta.env.DEV) console.error(`Missing value for the placeholder: {${name}} in "${text}"`)
    return match
  })
}
