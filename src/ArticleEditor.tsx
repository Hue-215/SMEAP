import {
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type Ref,
  type RefObject,
} from 'react'
import type { EditorView } from '@codemirror/view'
import { importBodyImage, type Article, type ArticleFields, type StoreError } from './api'
import ArticleHeader, { type FieldsPatch } from './ArticleHeader'
import { AutoSaver, type SaveStatus } from './autoSave'
import Editor from './Editor'
import { insertImageRefs, insertLink } from './editor'

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
  // 上のバーに出す知らせ
  onNotice: (text: string) => void
  ref: Ref<ArticleEditorHandle>
}

type HeaderFields = Omit<ArticleFields, 'body'>

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

  // 取り込めたものだけを入れる
  const importImages = async (files: File[]) => {
    const refs: string[] = []
    let unsupported = false
    let failed = false
    for (const file of files) {
      try {
        refs.push(await importBodyImage(article.slug, new Uint8Array(await file.arrayBuffer())))
      } catch (e) {
        console.error('画像を取り込めませんでした', file.name, e)
        if ((e as StoreError).kind === 'unsupportedImage') unsupported = true
        else failed = true
      }
    }
    if (refs.length > 0 && view.current) insertImageRefs(view.current, refs)
    if (failed) callbacks.current.onNotice('画像を取り込めませんでした')
    else if (unsupported) callbacks.current.onNotice('この形式の画像は使えません')
  }

  useImperativeHandle(ref, () => ({
    isDirty: () => saver.isDirty(),
    save: () => saver.save(),
    title: () => latest.current.title,
    insertLink: () => {
      if (view.current) insertLink(view.current)
    },
    insertImages: (files) => void importImages(files),
  }))

  const updateFields = (patch: FieldsPatch) => {
    latest.current = { ...latest.current, ...patch }
    setFields(latest.current)
    saver.changed()
  }

  return (
    <>
      <ArticleHeader fields={fields} onChange={updateFields} />
      <Editor
        slug={article.slug}
        initialDoc={article.body}
        viewRef={view}
        onChange={() => saver.changed()}
        onImages={(files) => void importImages(files)}
      />
    </>
  )
}
