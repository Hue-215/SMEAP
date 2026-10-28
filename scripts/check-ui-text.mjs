// 画面の文言がコードに直書きされていないかを確かめる。
//
// 文字列リテラル・テンプレート・JSXのテキストに日本語があれば失敗させる。構文木で見るので、
// コメントは対象にならない。ログと例外の文は英語で書くので、これも拾う。英語の直書きは
// 見分けられないため、レビューで見る。テスト(`*.test.ts`)は試す文字に日本語を使うので見ない。
//
// あわせて、キーを文字列リテラルで渡す`t()`の呼び出しで、渡す値の名前と文言の`{名前}`が
// 一致するかを確かめる。キーを変数で渡す呼び出しは見ない。
import { readdirSync, readFileSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'

const SRC = fileURLToPath(new URL('../src', import.meta.url))
const MESSAGES = JSON.parse(readFileSync(new URL('../lang/ja.json', import.meta.url), 'utf8'))
const JAPANESE = /[\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Han}]/u
const PLACEHOLDER = /\{([A-Za-z0-9_]+)\}/g

function sourceFiles(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) return sourceFiles(path)
    return /\.tsx?$/.test(entry.name) && !/\.test\.ts$/.test(entry.name) ? [path] : []
  })
}

const TEXT_KINDS = new Set([
  ts.SyntaxKind.StringLiteral,
  ts.SyntaxKind.NoSubstitutionTemplateLiteral,
  ts.SyntaxKind.TemplateHead,
  ts.SyntaxKind.TemplateMiddle,
  ts.SyntaxKind.TemplateTail,
  ts.SyntaxKind.JsxText,
])

// 値の名前が合わなければ、その説明を返す
function paramMismatch(call) {
  const [key, params] = call.arguments
  if (!ts.isStringLiteral(key) || !(key.text in MESSAGES)) return null
  if (params !== undefined && !ts.isObjectLiteralExpression(params)) return null
  if (params?.properties.some(ts.isSpreadAssignment)) return null
  const expected = [...MESSAGES[key.text].matchAll(PLACEHOLDER)].map((match) => match[1]).sort()
  const given = (params?.properties ?? []).map((property) => property.name.getText()).sort()
  if (expected.join() === given.join()) return null
  return `t('${key.text}') の値は {${expected.join(', ')}} だが {${given.join(', ')}} を渡している`
}

const texts = []
const params = []
for (const path of sourceFiles(SRC)) {
  const source = ts.createSourceFile(path, readFileSync(path, 'utf8'), ts.ScriptTarget.Latest, true)
  const at = (node) => `${relative(process.cwd(), path)}:${source.getLineAndCharacterOfPosition(node.getStart(source)).line + 1}`
  const visit = (node) => {
    if (TEXT_KINDS.has(node.kind) && JAPANESE.test(node.text)) texts.push(`${at(node)}: ${node.text.trim()}`)
    if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 't') {
      const mismatch = paramMismatch(node)
      if (mismatch) params.push(`${at(node)}: ${mismatch}`)
    }
    ts.forEachChild(node, visit)
  }
  visit(source)
}

if (texts.length > 0) {
  console.error('画面の文言は lang/*.json に置いて t() で引き、ログと例外の文は英語で書いてください:')
  for (const line of texts) console.error(`  ${line}`)
}
if (params.length > 0) {
  console.error('t() に渡す値の名前が文言と合いません:')
  for (const line of params) console.error(`  ${line}`)
}
if (texts.length > 0 || params.length > 0) process.exit(1)
