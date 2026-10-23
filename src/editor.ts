import {
  defaultKeymap,
  history,
  historyKeymap,
} from '@codemirror/commands'
import {
  HighlightStyle,
  syntaxHighlighting,
  syntaxTree,
} from '@codemirror/language'
import {
  GFM,
  type MarkdownConfig,
} from '@lezer/markdown'
import {
  styleTags,
  tags,
} from '@lezer/highlight'
import {
  commonmarkLanguage,
  markdown,
} from '@codemirror/lang-markdown'
import {
  Decoration,
  drawSelection,
  EditorView,
  keymap,
  ViewPlugin,
  WidgetType,
  type DecorationSet,
  type ViewUpdate,
} from '@codemirror/view'
import {
  EditorState,
  RangeSetBuilder,
  StateField,
  type Extension,
  type Range,
} from '@codemirror/state'
import { articleImageUrl } from './api'

// ベースの"Link/... Image/...": tags.linkは記号とURLにも下線を引き継ぐ。リンクは文字の部分にだけ
// 下線を付け(リンク内の太字などには付かない)、画像は記法全体を灰色にする。
const urlTags: MarkdownConfig = {
  props: [
    styleTags({
      Link: tags.link,
      'Link/URL Image/URL': tags.processingInstruction,
      'Image/...': tags.processingInstruction,
    }),
  ],
}

const highlight = HighlightStyle.define([
  { tag: tags.processingInstruction, color: 'var(--color-text-secondary)' },
  { tag: [tags.link, tags.url], textDecoration: 'underline' },
  {
    tag: tags.strong,
    fontFamily: 'var(--font-family)',
    fontWeight: 'var(--font-weight-strong)',
  },
  { tag: tags.emphasis, fontStyle: 'italic' },
  { tag: tags.strikethrough, textDecoration: 'line-through' },
  { tag: tags.monospace, fontFamily: 'var(--font-family-mono)' },
])

const headingLine = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet

    constructor(view: EditorView) {
      this.decorations = headingDecorations(view)
    }

    update(update: ViewUpdate) {
      if (
        update.docChanged ||
        update.viewportChanged ||
        syntaxTree(update.state) !== syntaxTree(update.startState)
      ) {
        this.decorations = headingDecorations(update.view)
      }
    }
  },
  { decorations: (plugin) => plugin.decorations },
)

function headingDecorations(view: EditorView): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>()
  for (const { from, to } of view.visibleRanges) {
    syntaxTree(view.state).iterate({
      from,
      to,
      enter: (node) => {
        const level = headingLevel(node.name)
        if (level === null) return
        // Setext見出しは下線の行も含めて複数行になる
        const decoration = Decoration.line({ class: `cm-heading-${level}` })
        const doc = view.state.doc
        for (let n = doc.lineAt(node.from).number; n <= doc.lineAt(node.to).number; n++) {
          const lineStart = doc.line(n).from
          builder.add(lineStart, lineStart, decoration)
        }
        return false
      },
    })
  }
  return builder.finish()
}

function headingLevel(nodeName: string): number | null {
  const match = /^(?:ATXHeading|SetextHeading)([1-6])$/.exec(nodeName)
  return match ? Number(match[1]) : null
}

// ブロックのウィジェットはViewPluginから出せないのでStateFieldに置く。構文木は後から解析が
// 進むことがあるので、木が替わったときも作り直す。
function imageField(slug: string): StateField<DecorationSet> {
  return StateField.define<DecorationSet>({
    create: (state) => imageDecorations(state, slug),
    update: (decorations, tr) =>
      tr.docChanged || syntaxTree(tr.state) !== syntaxTree(tr.startState)
        ? imageDecorations(tr.state, slug)
        : decorations,
    provide: (field) => EditorView.decorations.from(field),
  })
}

function imageDecorations(state: EditorState, slug: string): DecorationSet {
  const widgets: Array<Range<Decoration>> = []
  syntaxTree(state).iterate({
    enter: (node) => {
      if (node.name !== 'Image') return
      const urlNode = node.node.getChild('URL')
      if (!urlNode) return
      const reference = state.sliceDoc(urlNode.from, urlNode.to)
      if (isExternal(reference)) return
      const line = state.doc.lineAt(node.from)
      widgets.push(
        Decoration.widget({
          widget: new ImageWidget(articleImageUrl(slug, reference), reference),
          block: true,
          side: 1,
        }).range(line.to),
      )
    },
  })
  return Decoration.set(widgets, true)
}

// スキームの付いたURLと`//`で始まるURLは外部の画像とみなし、読み込まない。
function isExternal(reference: string): boolean {
  return /^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(reference)
}

class ImageWidget extends WidgetType {
  readonly url: string
  readonly reference: string

  constructor(url: string, reference: string) {
    super()
    this.url = url
    this.reference = reference
  }

  override eq(other: ImageWidget): boolean {
    return other.url === this.url
  }

  override toDOM(): HTMLElement {
    const wrap = document.createElement('div')
    wrap.className = 'cm-image'
    const img = document.createElement('img')
    img.src = this.url
    img.alt = ''
    img.addEventListener('error', () => {
      img.remove()
      const message = document.createElement('span')
      message.className = 'cm-image-error'
      message.textContent = `画像を読めません: ${this.reference}`
      wrap.appendChild(message)
    })
    wrap.appendChild(img)
    return wrap
  }
}

const editorTheme = EditorView.theme({
  '&': {
    fontWeight: '400',
    fontSize: 'var(--font-size-md)',
    color: 'var(--color-text)',
    background: 'transparent',
  },
  // 既定のテーマが.cm-scrollerに等幅の書体と行の高さを当てるので、ここで上書きする
  '.cm-scroller': {
    fontFamily: 'var(--font-family-prose)',
    lineHeight: '1.9',
  },
  '&.cm-focused': { outline: 'none' },
  '.cm-content': {
    padding: '0',
    caretColor: 'var(--color-accent)',
  },
  '.cm-line': { padding: '0' },
  '.cm-cursor, .cm-dropCursor': {
    borderLeft: '2px solid var(--color-accent)',
  },
  '.cm-selectionBackground': { background: 'var(--color-selection)' },
  '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground': {
    background: 'var(--color-selection)',
  },
  '.cm-content ::selection': { background: 'var(--color-selection)' },
  '.cm-heading-1, .cm-heading-2, .cm-heading-3, .cm-heading-4, .cm-heading-5, .cm-heading-6':
    {
      fontFamily: 'var(--font-family)',
      fontWeight: 'var(--font-weight-strong)',
    },
  '.cm-heading-1': { fontSize: '1.625rem', lineHeight: '1.5' },
  '.cm-heading-2': { fontSize: '1.375rem', lineHeight: '1.6' },
  '.cm-heading-3': { fontSize: '1.125rem', lineHeight: '1.7' },
  '.cm-heading-4, .cm-heading-5, .cm-heading-6': { fontSize: '1rem' },
  '.cm-image': { margin: '0.5rem 0' },
  '.cm-image img': {
    display: 'block',
    maxWidth: '100%',
    height: 'auto',
  },
  '.cm-image-error': {
    fontFamily: 'var(--font-family)',
    fontSize: 'var(--font-size-xs)',
    color: 'var(--color-text-secondary)',
  },
})

// cursorは挿入した文字の先頭からの位置
function replaceSelection(view: EditorView, insert: string, cursor = insert.length) {
  const { from, to } = view.state.selection.main
  view.dispatch({
    changes: { from, to, insert },
    selection: { anchor: from + cursor },
    scrollIntoView: true,
    userEvent: 'input',
  })
  view.focus()
}

// 選んだ文字を`[文字](url)`にする。urlが空ならカーソルを`()`の中に置く
export function insertLink(view: EditorView, url = '') {
  const { from, to } = view.state.selection.main
  const text = view.state.sliceDoc(from, to)
  const insert = `[${text}](${url})`
  replaceSelection(view, insert, url ? insert.length : text.length + 3)
}

// 画像は記法のある行の下に出るので、1つずつ別の行に置く
export function insertImageRefs(view: EditorView, refs: string[]) {
  const { from, to } = view.state.selection.main
  const doc = view.state.doc
  const before = from > doc.lineAt(from).from ? '\n' : ''
  const after = to < doc.lineAt(to).to ? '\n' : ''
  replaceSelection(view, before + refs.join('\n') + after)
}

const urlPattern = /^https?:\/\/\S+$/

// 文字を選んでURLを貼ると`[文字](URL)`にする。文字が無ければ画像の貼り付けとして渡す(filesは空のことがある)。
// 表計算などからのコピーは文字と画像を両方持つので、文字があれば文字として貼る
function pasteHandler(onPasteImage: (files: File[]) => void): Extension {
  return EditorView.domEventHandlers({
    paste: (event, view) => {
      const text = event.clipboardData?.getData('text/plain') ?? ''
      if (!text) {
        event.preventDefault()
        onPasteImage(Array.from(event.clipboardData?.files ?? []))
        return true
      }
      const url = text.trim()
      const { main, ranges } = view.state.selection
      if (main.empty || ranges.length > 1 || !urlPattern.test(url)) return false
      event.preventDefault()
      insertLink(view, url)
      return true
    },
  })
}

export function editorExtensions(
  slug: string,
  onPasteImage: (files: File[]) => void,
): Extension[] {
  return [
    pasteHandler(onPasteImage),
    history(),
    keymap.of([...defaultKeymap, ...historyKeymap]),
    drawSelection(),
    EditorView.lineWrapping,
    EditorView.contentAttributes.of({ 'aria-label': '本文' }),
    markdown({ base: commonmarkLanguage, extensions: [GFM, urlTags] }),
    syntaxHighlighting(highlight),
    headingLine,
    imageField(slug),
    editorTheme,
  ]
}
