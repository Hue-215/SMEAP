import ja from '../lang/ja.json'

// 決まりはdocs/spec.md。文言の中の`{英数字}`は差し込む値として読まれる

export type MessageKey = keyof typeof ja

const messages: Record<string, string> = ja

const PLACEHOLDER = /\{([A-Za-z0-9_]+)\}/g

export function t(key: MessageKey, params: Record<string, string | number> = {}): string {
  const text = messages[key] ?? key
  // 1回の走査で置き換えるので、差し込んだ値の中の`{名前}`は置き換えない
  return text.replace(PLACEHOLDER, (match, name: string) => {
    const value = params[name]
    if (value !== undefined) return String(value)
    if (import.meta.env.DEV)
      console.error(`Missing value for the placeholder: {${name}} in "${text}"`)
    return match
  })
}
