import { useEffect, useRef, type RefObject } from 'react'
import { EditorView } from '@codemirror/view'
import { editorExtensions } from './editor'

interface EditorProps {
  slug: string
  initialDoc: string
  // 本文を読むときに使う。編集欄がある間だけ入る
  viewRef: RefObject<EditorView | null>
  onChange: () => void
  // 貼り付けやドロップで受けた画像
  onImages: (files: File[]) => void
}

export default function Editor({ slug, initialDoc, viewRef, onChange, onImages }: EditorProps) {
  const container = useRef<HTMLDivElement>(null)
  const callbacks = useRef({ onChange, onImages })
  useEffect(() => {
    callbacks.current = { onChange, onImages }
  })

  useEffect(() => {
    const view = new EditorView({
      doc: initialDoc,
      extensions: [
        editorExtensions(slug, (files) => callbacks.current.onImages(files)),
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
