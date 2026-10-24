import {
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type Ref,
  type RefObject,
} from 'react'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import type { EditorView } from '@codemirror/view'
import {
  importBodyImage,
  importBodyImageFromPath,
  importClipboardImage,
  importHeroImage,
  type Article,
  type ArticleFields,
  type StoreError,
  type Warning,
} from './api'
import ArticleHeader, { type FieldsPatch } from './ArticleHeader'
import { AutoSaver, type SaveStatus } from './autoSave'
import Editor from './Editor'
import { insertImageRefs, insertLink } from './editor'
import { isAndroid } from './platform'

export interface ArticleEditorHandle {
  isDirty: () => boolean
  // 失敗したらfalse
  save: () => Promise<boolean>
  title: () => string
  insertLink: () => void
  insertImages: (files: File[]) => void
}

interface ArticleEditorProps {
  // 開いたときの内容。記事を切り替えたら、keyで作り直す
  article: Article
  // trueの間は決まった間隔での保存をしない
  paused: boolean
  onSaveStatus: (status: SaveStatus) => void
  onSaved: (fields: ArticleFields) => void
  // エラーとして出す文言
  onNotice: (text: string) => void
  ref: Ref<ArticleEditorHandle>
}

type HeaderFields = Omit<ArticleFields, 'body'>
type HeaderKey = keyof HeaderFields

// runは本文に入れる参照を返す。クリップボードに画像が無ければnull
interface ImageImport {
  label: string
  run: (slug: string) => Promise<string | null>
}

const fromFile = (file: File): ImageImport => ({
  label: file.name,
  run: async (slug) => importBodyImage(slug, new Uint8Array(await file.arrayBuffer())),
})

const fromPath = (path: string): ImageImport => ({
  label: path,
  run: (slug) => importBodyImageFromPath(slug, path),
})

const fromClipboard: ImageImport = { label: 'クリップボード', run: importClipboardImage }

// 取り込めたものだけを入れる
async function importImages(
  slug: string,
  view: RefObject<EditorView | null>,
  onNotice: (text: string) => void,
  imports: ImageImport[],
) {
  const refs: string[] = []
  let unsupported = 0
  let failed = 0
  for (const image of imports) {
    try {
      const ref = await image.run(slug)
      if (ref !== null) refs.push(ref)
    } catch (e) {
      console.error('画像を取り込めませんでした', image.label, e)
      if ((e as StoreError | undefined)?.kind === 'unsupportedImage') unsupported++
      else failed++
    }
  }
  if (refs.length > 0) {
    // 取り込みの間に記事を切り替えると、編集欄はもう無い。取り込んだ画像は参照されないまま残る
    if (view.current) insertImageRefs(view.current, refs)
    else console.error('取り込んだ画像を本文に入れられませんでした', slug, refs)
  }
  if (failed > 0) onNotice(`${failed + unsupported}枚の画像を取り込めませんでした`)
  else if (unsupported > 0) onNotice(`${unsupported}枚は使えない形式です`)
}

function readFields(
  latest: RefObject<HeaderFields>,
  view: RefObject<EditorView | null>,
): ArticleFields {
  if (!view.current) throw new Error('本文の編集欄が無い')
  return { ...latest.current, body: view.current.state.doc.toString() }
}

// 開いている記事の入力欄と、その自動保存。
export default function ArticleEditor({
  article,
  paused,
  onSaveStatus,
  onSaved,
  onNotice,
  ref,
}: ArticleEditorProps) {
  const [fields, setFields] = useState<HeaderFields>(() => ({
    title: article.title,
    description: article.description,
    pubDate: article.pubDate,
    heroImage: article.heroImage,
  }))
  // 値を変えた項目。そのkeyの警告は出さない
  const [edited, setEdited] = useState<Set<HeaderKey>>(() => new Set())
  // 保存のときに読む。stateは描き直すまで前の値なので、入力を受けたときにこちらも書き換える
  const latest = useRef(fields)
  const view = useRef<EditorView>(null)
  const callbacks = useRef({ onSaveStatus, onSaved, onNotice })
  useEffect(() => {
    callbacks.current = { onSaveStatus, onSaved, onNotice }
  })
  const [saver] = useState(() => new AutoSaver(article.slug))

  // 前の記事の保存の状態が見えないよう、描く前に知らせる
  useLayoutEffect(() => {
    saver.attach({
      read: () => readFields(latest, view),
      onStatus: (status) => callbacks.current.onSaveStatus(status),
      onSaved: (fields) => callbacks.current.onSaved(fields),
    })
    // Androidでは、裏へ回したアプリが知らせなく終了されることがある。確認のダイアログを出している間も保存する
    const onVisibilityChange = () => {
      if (document.hidden && saver.isDirty()) void saver.save()
    }
    document.addEventListener('visibilitychange', onVisibilityChange)
    return () => {
      document.removeEventListener('visibilitychange', onVisibilityChange)
      saver.detach()
    }
  }, [saver])

  useEffect(() => saver.setPaused(paused), [saver, paused])

  const slug = article.slug
  const runImports = (imports: ImageImport[]) =>
    void importImages(slug, view, (text) => callbacks.current.onNotice(text), imports)

  // サムネイルの選び直し。mdのheroImageはRust側が書き換えるので、saver.changed()はしない
  const pickHeroImage = async (file: File) => {
    try {
      const name = await importHeroImage(slug, new Uint8Array(await file.arrayBuffer()))
      latest.current = { ...latest.current, heroImage: name }
      setFields(latest.current)
      // 読めなかったheroImageの警告は、取り込めたので消す
      setEdited((keys) => {
        const next = new Set(keys)
        next.add('heroImage')
        return next
      })
    } catch (e) {
      console.error('サムネイルを取り込めませんでした', file.name, e)
      const unsupported = (e as StoreError | undefined)?.kind === 'unsupportedImage'
      callbacks.current.onNotice(unsupported ? '使えない形式です' : 'サムネイルを取り込めませんでした')
    }
  }

  // LinuxのWebKitGTKはファイルマネージャーからのドロップを画面へファイルとして渡さないので、Tauriが受けたパスで取り込む
  useEffect(() => {
    if (isAndroid) return
    let disposed = false
    let unlisten: (() => void) | null = null
    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type !== 'drop' || !view.current) return
        const editor = view.current
        const x = event.payload.position.x / window.devicePixelRatio
        const y = event.payload.position.y / window.devicePixelRatio
        // 本文の外に落としたらカーソルの位置に入れる
        const target = document.elementFromPoint(x, y)
        const pos = target && editor.dom.contains(target) ? editor.posAtCoords({ x, y }) : null
        if (pos !== null) editor.dispatch({ selection: { anchor: pos } })
        void importImages(
          slug,
          view,
          (text) => callbacks.current.onNotice(text),
          event.payload.paths.map(fromPath),
        )
      })
      .then((fn) => {
        if (disposed) fn()
        else unlisten = fn
      })
      .catch((e) => console.error('ドロップを受けられません', e))
    return () => {
      disposed = true
      unlisten?.()
    }
  }, [slug])

  useImperativeHandle(ref, () => ({
    isDirty: () => saver.isDirty(),
    save: () => saver.save(),
    title: () => latest.current.title ?? '',
    insertLink: () => {
      if (view.current) insertLink(view.current)
    },
    insertImages: (files) => runImports(files.map(fromFile)),
  }))

  const updateFields = (patch: FieldsPatch) => {
    const changes = Object.entries(patch) as Array<[HeaderKey, string | null]>
    if (changes.some(([key, value]) => value !== article[key])) {
      setEdited((keys) => {
        const next = new Set(keys)
        for (const [key] of changes) next.add(key)
        return next
      })
    }
    latest.current = { ...latest.current, ...patch }
    setFields(latest.current)
    saver.changed()
  }

  const warnings = article.warnings.filter(
    (warning: Warning) => warning.key === null || !edited.has(warning.key as HeaderKey),
  )

  return (
    <>
      <ArticleHeader
        fields={fields}
        warnings={warnings}
        onChange={updateFields}
        onPickHeroImage={(file) => void pickHeroImage(file)}
      />
      <Editor
        slug={article.slug}
        initialDoc={article.body}
        viewRef={view}
        onChange={() => saver.changed()}
        onPasteImage={(files) =>
          runImports(files.length > 0 ? files.map(fromFile) : isAndroid ? [] : [fromClipboard])
        }
      />
    </>
  )
}
