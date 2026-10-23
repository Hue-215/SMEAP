import { useEffect, useRef } from 'react'
import { EditorView } from '@codemirror/view'
import { editorExtensions } from './editor'

interface EditorProps {
  slug: string
  initialDoc: string
}

export default function Editor({ slug, initialDoc }: EditorProps) {
  const container = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const view = new EditorView({
      doc: initialDoc,
      extensions: editorExtensions(slug),
      parent: container.current ?? undefined,
    })
    return () => view.destroy()
  }, [slug, initialDoc])

  return <div ref={container} />
}
