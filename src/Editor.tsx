import { useEffect, useRef } from 'react'
import { EditorView } from '@codemirror/view'
import { editorExtensions } from './editor'

interface EditorProps {
  slug: string
  doc: string
}

export default function Editor({ slug, doc }: EditorProps) {
  const container = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const view = new EditorView({
      doc,
      extensions: editorExtensions(slug),
      parent: container.current ?? undefined,
    })
    return () => view.destroy()
  }, [slug, doc])

  return <div ref={container} />
}
