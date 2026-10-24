// 画面の文言がコードに直書きされていないかを確かめる。
//
// 文字列リテラル・テンプレート・JSXのテキストに日本語があれば失敗させる。構文木で見るので、
// コメントは対象にならない。ログと例外の文は英語で書くので、これも拾う。英語の直書きは
// 見分けられないため、レビューで見る。
import { readdirSync, readFileSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'

const SRC = fileURLToPath(new URL('../src', import.meta.url))
const JAPANESE = /[\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Han}]/u

function sourceFiles(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) return sourceFiles(path)
    return /\.tsx?$/.test(entry.name) ? [path] : []
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

const found = []
for (const path of sourceFiles(SRC)) {
  const source = ts.createSourceFile(path, readFileSync(path, 'utf8'), ts.ScriptTarget.Latest, true)
  const visit = (node) => {
    if (TEXT_KINDS.has(node.kind) && JAPANESE.test(node.text)) {
      const { line } = source.getLineAndCharacterOfPosition(node.getStart(source))
      found.push(`${relative(process.cwd(), path)}:${line + 1}: ${node.text.trim()}`)
    }
    ts.forEachChild(node, visit)
  }
  visit(source)
}

if (found.length > 0) {
  console.error('画面の文言は lang/*.json に置き、t() で引いてください:')
  for (const line of found) console.error(`  ${line}`)
  process.exit(1)
}
