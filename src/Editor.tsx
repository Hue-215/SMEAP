import { useEffect, useRef, type RefObject } from 'react'
import { EditorView } from '@codemirror/view'
import type { EditorState } from '@codemirror/state'
import { editorExtensions } from './editor'

interface EditorProps {
  slug: string
  initialDoc: string
  // 本文を読むときに使う。編集欄がある間だけ入る
  viewRef: RefObject<EditorView | null>
  onChange: () => void
  // すべての更新後の状態。検索の件数の付け直しに使う
  onUpdate: (state: EditorState) => void
  // 文字の無い貼り付け。filesは空のことがある
  onPasteImage: (files: File[]) => void
}

export default function Editor({
  slug,
  initialDoc,
  viewRef,
  onChange,
  onUpdate,
  onPasteImage,
}: EditorProps) {
  const container = useRef<HTMLDivElement>(null)
  const callbacks = useRef({ onChange, onUpdate, onPasteImage })
  useEffect(() => {
    callbacks.current = { onChange, onUpdate, onPasteImage }
  })

  useEffect(() => {
    const view = new EditorView({
      doc: initialDoc,
      extensions: [
        editorExtensions(slug, (files) => callbacks.current.onPasteImage(files)),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) callbacks.current.onChange()
          callbacks.current.onUpdate(update.state)
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
