import { SearchQuery } from '@codemirror/search'
import { EditorState, StateEffect, StateField, type SelectionRange } from '@codemirror/state'
import { Decoration, EditorView, type DecorationSet } from '@codemirror/view'

// 記事内検索の照合と下線。DOMに触れないので、テストはnodeで動く

export interface Match {
  from: number
  to: number
}

// currentは0始まりで、選択がどの一致とも同じでなければ-1
export interface FindStatus {
  current: number
  total: number
}

const normalize = (text: string) => text.normalize('NFKD').toLowerCase()

// 一致の直後が濁点などの結合する文字なら、その文字の途中までの一致になる(「ｶﾞ」「か゛」の「か」)
const combining = /^[\p{M}ﾞﾟ]/u

// @codemirror/searchの照合はNFKDで正規化するので、「ガ」の中の「カ」のように正規化で
// 分かれた文字の途中までの一致は、testで範囲ごと比べて外す
function queryFor(search: string): SearchQuery {
  return new SearchQuery({
    search,
    literal: true,
    caseSensitive: false,
    test: (match, state, _from, to) =>
      normalize(match) === normalize(search) && !combining.test(state.sliceDoc(to, to + 1)),
  })
}

export function findMatches(state: EditorState, query: string): Match[] {
  if (!query) return []
  const matches: Match[] = []
  const cursor = queryFor(query).getCursor(state)
  for (let r = cursor.next(); !r.done; r = cursor.next()) {
    matches.push({ from: r.value.from, to: r.value.to })
  }
  return matches
}

export function currentIndex(matches: Match[], selection: SelectionRange): number {
  return matches.findIndex((match) => match.from === selection.from && match.to === selection.to)
}

export function nextMatch(matches: Match[], pos: number): Match | null {
  return matches.find((match) => match.from >= pos) ?? matches[0] ?? null
}

export function prevMatch(matches: Match[], pos: number): Match | null {
  for (let i = matches.length - 1; i >= 0; i--) {
    if (matches[i].from < pos) return matches[i]
  }
  return matches[matches.length - 1] ?? null
}

export function selectedQuery(state: EditorState): string | null {
  const { main, ranges } = state.selection
  if (main.empty || ranges.length > 1) return null
  return state.doc.lineAt(main.from).from === state.doc.lineAt(main.to).from
    ? state.sliceDoc(main.from, main.to)
    : null
}

export const setSearchHighlight = StateEffect.define<string>()

const searchQuery = StateField.define<string>({
  create: () => '',
  update: (value, tr) => {
    for (const effect of tr.effects) if (effect.is(setSearchHighlight)) return effect.value
    return value
  },
})

// setSearchHighlightで渡した今の検索の文字
export function searchText(state: EditorState): string {
  return state.field(searchQuery, false) ?? ''
}

export function findStatus(state: EditorState): FindStatus {
  const matches = findMatches(state, searchText(state))
  return { current: currentIndex(matches, state.selection.main), total: matches.length }
}

// 今の選択の後ろの一致。選択中の一致そのものは飛ばす
export function findNextMatch(state: EditorState): Match | null {
  return nextMatch(findMatches(state, searchText(state)), state.selection.main.to)
}

export function findPrevMatch(state: EditorState): Match | null {
  return prevMatch(findMatches(state, searchText(state)), state.selection.main.from)
}

function markMatches(state: EditorState, query: string): DecorationSet {
  const mark = Decoration.mark({ class: 'cm-searchMatch' })
  return Decoration.set(
    findMatches(state, query).map((match) => mark.range(match.from, match.to)),
    true,
  )
}

export const searchHighlight = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update: (decorations, tr) => {
    let query = tr.startState.field(searchQuery)
    let rebuild = tr.docChanged
    for (const effect of tr.effects) {
      if (effect.is(setSearchHighlight)) {
        query = effect.value
        rebuild = true
      }
    }
    if (!rebuild) return decorations
    return query ? markMatches(tr.state, query) : Decoration.none
  },
  // searchHighlightだけをextensionsに入れたstateでも、検索の文字が読めるようにする
  provide: (field) => [searchQuery, EditorView.decorations.from(field)],
})
