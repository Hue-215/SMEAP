import { execFile } from 'node:child_process'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'
import type { Plugin } from 'vite'
import type { License } from 'virtual:licenses'

// 作業中のカレントディレクトリによらず、このファイルの位置から求める。
const rootDir = dirname(dirname(fileURLToPath(import.meta.url)))

interface PackageManifest {
  name?: string
  version?: string
  license?: string | { type?: string }
  dependencies?: Record<string, string>
}

interface AboutJson {
  licenses: {
    id: string
    text: string
    used_by: { crate: { name: string; version: string } }[]
  }[]
}

interface CrateLicenses {
  name: string
  version: string
  ids: string[]
  texts: string[]
}

function readManifest(dir: string): PackageManifest {
  return JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8')) as PackageManifest
}

// パッケージの置き場所。Nodeと同じく、親のディレクトリから上へnode_modules/<name>を探す。
function resolvePackageDir(name: string, fromDir: string): string {
  for (let dir = fromDir; ; dir = dirname(dir)) {
    const resolved = join(dir, 'node_modules', name)
    if (existsSync(join(resolved, 'package.json'))) return resolved
    if (dirname(dir) === dir) throw new Error(`node_modulesにパッケージがありません: ${name}`)
  }
}

// package.jsonのlicenseの表示。"A OR B"でMITを含むならMIT(文としてはMITの1本だけを入れている)。
function licenseLabel(manifest: PackageManifest): string {
  const raw = typeof manifest.license === 'string' ? manifest.license : manifest.license?.type
  if (raw === undefined) return ''
  return /\bMIT\b/.test(raw) && raw.includes(' OR ') ? 'MIT' : raw
}

// ライセンス文。LICENSE*・LICENCE*・COPYING*(大文字小文字を区別しない)。.spdxは目録なので除く。
// 複数あるときは、名前にMITを含むものだけを採り、無ければ全部をつなぐ。
function licenseText(dir: string, name: string): string {
  const files = readdirSync(dir).filter(
    (file) => /^(licen[cs]e|copying)/i.test(file) && !file.toLowerCase().endsWith('.spdx'),
  )
  const withMIT = files.filter((file) => /MIT/i.test(file))
  const picked = withMIT.length === 0 ? files : withMIT
  if (picked.length === 0) throw new Error(`ライセンス文のファイルが見つかりません: ${name}`)
  return picked
    .sort()
    .map((file) => readFileSync(join(dir, file), 'utf8'))
    .join('\n\n')
}

// dependenciesを再帰的にたどって集める。devDependenciesは集めない。
function collectJsLicenses(): License[] {
  const licenses: License[] = []
  // 同じディレクトリは、複数のパッケージから参照されていても一度だけ集める
  const visited = new Set<string>()
  const visit = (fromDir: string, dependencies: Record<string, string> = {}) => {
    for (const name of Object.keys(dependencies)) {
      const pkgDir = resolvePackageDir(name, fromDir)
      if (visited.has(pkgDir)) continue
      visited.add(pkgDir)
      const manifest = readManifest(pkgDir)
      licenses.push({
        name: manifest.name ?? name,
        version: manifest.version ?? '',
        license: licenseLabel(manifest),
        text: licenseText(pkgDir, manifest.name ?? name),
      })
      visit(pkgDir, manifest.dependencies)
    }
  }
  visit(rootDir, readManifest(rootDir).dependencies)
  return licenses
}

// クレートのライセンス。cargo-aboutのJSONはライセンスごとにid・文・使うクレートを出すので、
// クレート(name@version)ごとにまとめ直す。
async function collectRustLicenses(target: string): Promise<License[]> {
  const { stdout } = await promisify(execFile)(
    'cargo',
    [
      'about',
      'generate',
      '--format',
      'json',
      '--locked',
      '--target',
      target,
      '--manifest-path',
      'src-tauri/Cargo.toml',
      '--config',
      'src-tauri/about.toml',
    ],
    // 出力のJSONは数メガバイトになる
    { cwd: rootDir, maxBuffer: 256 * 1024 * 1024 },
  )
  const about = JSON.parse(stdout) as AboutJson
  const crates = new Map<string, CrateLicenses>()
  for (const { id, text, used_by } of about.licenses) {
    for (const { crate } of used_by) {
      // アプリ自身のクレート
      if (crate.name === 'astro-notepad') continue
      const key = `${crate.name}@${crate.version}`
      const entry = crates.get(key) ?? { name: crate.name, version: crate.version, ids: [], texts: [] }
      if (!entry.ids.includes(id)) entry.ids.push(id)
      entry.texts.push(text)
      crates.set(key, entry)
    }
  }
  return [...crates.values()].map((entry) => ({
    name: entry.name,
    version: entry.version,
    license: entry.ids.join(' AND '),
    // 1つのクレートの文が複数出てくることがあるので、空行2つでつなぐ
    text: entry.texts.join('\n\n\n'),
  }))
}

// ライセンスの一覧。targetはtauri buildが渡すターゲットの三つ組で、あるときはRustのクレートも
// 集める。
async function collectLicenses(target: string | undefined): Promise<License[]> {
  const app: License = {
    name: 'Astro-Notepad',
    version: '',
    license: 'MIT',
    text: readFileSync(join(rootDir, 'LICENSE'), 'utf8'),
  }
  const rest = collectJsLicenses()
  if (target !== undefined) rest.push(...(await collectRustLicenses(target)))
  rest.push(
    {
      name: 'Noto Sans JP / Noto Serif JP',
      version: '',
      license: 'OFL-1.1',
      text: readFileSync(join(rootDir, 'public/fonts/NotoJP-LICENSE.txt'), 'utf8'),
    },
    {
      name: 'Material Symbols',
      version: '',
      license: 'Apache-2.0',
      text: readFileSync(join(rootDir, 'src/icons/MaterialSymbols-LICENSE.txt'), 'utf8'),
    },
  )
  rest.sort(
    (a, b) =>
      a.name.localeCompare(b.name, 'en', { sensitivity: 'base' }) ||
      a.version.localeCompare(b.version, 'en', { sensitivity: 'base', numeric: true }),
  )
  return [app, ...rest]
}

// 仮想モジュール`virtual:licenses`。設定画面が動的importで、ライセンスの一覧(License[])を読む。
export default function licenses(): Plugin {
  // configがloadより先に出るので、buildで動いたかどうかをここで覚えておく
  let command = 'serve'
  return {
    name: 'licenses',
    config(_, { command: envCommand }) {
      command = envCommand
    },
    resolveId(id) {
      return id === 'virtual:licenses' ? '\0virtual:licenses' : null
    },
    async load(id) {
      if (id !== '\0virtual:licenses') return null
      // クレートはtauri buildでだけ集める(CIのフロントエンドのジョブはcargo-aboutを持たないため)
      return `export default ${JSON.stringify(
        await collectLicenses(command === 'build' ? process.env.TAURI_ENV_TARGET_TRIPLE : undefined),
      )}`
    },
  }
}
