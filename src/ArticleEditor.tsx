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
import type { Article, ArticleFields } from './api'
import ArticleHeader, { type FieldsPatch } from './ArticleHeader'
import { AutoSaver, type SaveStatus } from './autoSave'
import Editor from './Editor'

export interface ArticleEditorHandle {
  isDirty: () => boolean
  // 失敗したらfalse
  save: () => Promise<boolean>
  title: () => string
}

interface ArticleEditorProps {
  // 開いたときの内容。記事を切り替えたら、keyで作り直す
  article: Article
  // trueの間は自動保存しない
  paused: boolean
  onSaveStatus: (status: SaveStatus) => void
  onSaved: (fields: ArticleFields) => void
  ref: Ref<ArticleEditorHandle>
}

function readFields(latest: RefObject<Article>, view: RefObject<EditorView | null>): ArticleFields {
  const { slug: _, ...fields } = latest.current
  return { ...fields, body: view.current?.state.doc.toString() ?? fields.body }
}

// 開いている記事の入力欄と、その自動保存。
export default function ArticleEditor({
  article,
  paused,
  onSaveStatus,
  onSaved,
  ref,
}: ArticleEditorProps) {
  const [fields, setFields] = useState(article)
  // 保存のときに読む。stateは描き直すまで前の値なので、入力を受けたときにこちらも書き換える
  const latest = useRef(article)
  const view = useRef<EditorView>(null)
  const callbacks = useRef({ onSaveStatus, onSaved })
  useEffect(() => {
    callbacks.current = { onSaveStatus, onSaved }
  })
  const [saver] = useState(() => new AutoSaver(article.slug))

  // 前の記事の保存の状態が見えないよう、描く前に知らせる
  useLayoutEffect(() => {
    saver.attach({
      read: () => readFields(latest, view),
      onStatus: (status) => callbacks.current.onSaveStatus(status),
      onSaved: (fields) => callbacks.current.onSaved(fields),
    })
    // Androidでは、裏へ回したアプリが知らせなく終了されることがある
    const onVisibilityChange = () => {
      if (document.hidden) saver.flush()
    }
    document.addEventListener('visibilitychange', onVisibilityChange)
    return () => {
      document.removeEventListener('visibilitychange', onVisibilityChange)
      saver.detach()
    }
  }, [saver])

  useEffect(() => saver.setPaused(paused), [saver, paused])

  useImperativeHandle(
    ref,
    () => ({
      isDirty: () => saver.isDirty(),
      save: () => saver.save(),
      title: () => latest.current.title,
    }),
    [saver],
  )

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
      />
    </>
  )
}
