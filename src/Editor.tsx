import { useEffect, useRef, type RefObject } from 'react'
import { EditorView } from '@codemirror/view'
import { editorExtensions } from './editor'

interface EditorProps {
  slug: string
  initialDoc: string
  // 本文を読むときに使う。編集欄がある間だけ入る
  viewRef: RefObject<EditorView | null>
  onChange: () => void
  // 貼り付けやドロップで受けたファイル
  onFiles: (files: File[]) => void
}

export default function Editor({ slug, initialDoc, viewRef, onChange, onFiles }: EditorProps) {
  const container = useRef<HTMLDivElement>(null)
  const callbacks = useRef({ onChange, onFiles })
  useEffect(() => {
    callbacks.current = { onChange, onFiles }
  })

  useEffect(() => {
    const view = new EditorView({
      doc: initialDoc,
      extensions: [
        editorExtensions(slug, (files) => callbacks.current.onFiles(files)),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) callbacks.current.onChange()
        }),
      ],
      parent: container.current ?? undefined,
    })
    viewRef.current = view
    return () => {
      viewRef.current = null
      view.destroy()
    }
  }, [slug, initialDoc, viewRef])

  return <div ref={container} />
}
