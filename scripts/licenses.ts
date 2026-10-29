import { execFile } from 'node:child_process'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'
import type { ConfigEnv, Plugin } from 'vite'
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

// パッケージの置き場所。Nodeと同じく、依存元のディレクトリから上へnode_modules/<name>を探す。
function resolvePackageDir(name: string, fromDir: string): string {
  for (let dir = fromDir; ; dir = dirname(dir)) {
    const resolved = join(dir, 'node_modules', name)
    if (existsSync(join(resolved, 'package.json'))) return resolved
    if (dirname(dir) === dir) throw new Error(`node_modulesにパッケージがありません: ${name}`)
  }
}

// ライセンスの表示と文。文はLICENSE*・LICENCE*・COPYING*(大文字小文字を区別しない、.spdxは目録なので除く)。
// "A OR B"でMITを選べて、MITの文のファイル(LICENSE-MITなど)があれば、MITだけを載せる。
function packageLicense(dir: string, manifest: PackageManifest, name: string) {
  const raw = typeof manifest.license === 'string' ? manifest.license : manifest.license?.type
  if (!raw) throw new Error(`package.jsonにlicenseがありません: ${name}`)
  const files = readdirSync(dir)
    .filter((file) => /^(licen[cs]e|copying)/i.test(file) && !file.toLowerCase().endsWith('.spdx'))
    .sort()
  if (files.length === 0) throw new Error(`ライセンス文のファイルが見つかりません: ${name}`)
  const mitFiles = files.filter((file) => /(^|[^a-z])mit([^a-z0-9-]|$)/i.test(file))
  const chooseMIT = / OR /.test(raw) && /(^|[\s(])MIT($|[\s)])/.test(raw) && mitFiles.length > 0
  const read = (picked: string[]) =>
    picked.map((file) => readFileSync(join(dir, file), 'utf8')).join('\n\n')
  return chooseMIT ? { license: 'MIT', text: read(mitFiles) } : { license: raw, text: read(files) }
}

// dependenciesだけをたどる(dev・optional・peerは見ない)。
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
        ...packageLicense(pkgDir, manifest, manifest.name ?? name),
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
    // 出力のJSONは既定の上限(1MB)を超える
    { cwd: rootDir, maxBuffer: 256 * 1024 * 1024 },
  ).catch((e: unknown) => {
    throw new Error(
      'cargo aboutを実行できません(入っていなければ: cargo install --locked cargo-about)',
      {
        cause: e,
      },
    )
  })
  const about = JSON.parse(stdout) as AboutJson
  const crates = new Map<string, CrateLicenses>()
  for (const { id, text, used_by } of about.licenses) {
    for (const { crate } of used_by) {
      // アプリ自身は一覧の先頭に別に載せる
      if (crate.name === 'smeap') continue
      const key = `${crate.name}@${crate.version}`
      const entry = crates.get(key) ?? {
        name: crate.name,
        version: crate.version,
        ids: [],
        texts: [],
      }
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
    name: 'SMEAP',
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
  const keys = new Set<string>()
  for (const { name, version } of rest) {
    // 設定画面が行を見分けるのに使う
    const key = `${name}@${version}`
    if (keys.has(key)) throw new Error(`同じ名前と版が2つあります: ${key}`)
    keys.add(key)
  }
  return [app, ...rest]
}

// 仮想モジュール`virtual:licenses`。
export default function licenses(): Plugin {
  // configはloadより先に呼ばれる
  let command: ConfigEnv['command'] = 'serve'
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
      // クレートは、tauri buildがTAURI_ENV_*を渡したときだけ集める。素のvite build(CIのフロントエンドの
      // ジョブ)と開発サーバーでは集めない
      const target = process.env.TAURI_ENV_TARGET_TRIPLE
      if (
        command === 'build' &&
        process.env.TAURI_ENV_PLATFORM !== undefined &&
        target === undefined
      ) {
        throw new Error('tauri buildからTAURI_ENV_TARGET_TRIPLEが渡されていません')
      }
      return `export default ${JSON.stringify(
        await collectLicenses(command === 'build' ? target : undefined),
      )}`
    },
  }
}
