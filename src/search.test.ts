import { describe, expect, it } from 'vitest'
import { EditorSelection, EditorState } from '@codemirror/state'
import {
  currentIndex,
  findMatches,
  nextMatch,
  prevMatch,
  searchHighlight,
  selectedQuery,
  setSearchHighlight,
} from './search'

function createState(doc: string, selection?: EditorSelection): EditorState {
  return EditorState.create({ doc, selection })
}

describe('findMatches', () => {
  it('空の文字なら空の配列', () => {
    expect(findMatches(createState('うえお'), '')).toEqual([])
  })

  it('一致が無ければ空の配列', () => {
    expect(findMatches(createState('うえお'), 'かき')).toEqual([])
  })

  it('複数の一致を文書の順に返す', () => {
    expect(findMatches(createState('あいう\nえおあい'), 'あい')).toEqual([
      { from: 0, to: 2 },
      { from: 6, to: 8 },
    ])
  })

  it('大文字と小文字を区別しない', () => {
    expect(findMatches(createState('Astro ASTRO'), 'astro')).toEqual([
      { from: 0, to: 5 },
      { from: 6, to: 11 },
    ])
  })

  it('全角と半角を区別しない', () => {
    expect(findMatches(createState('Ａｓｔｒｏ astro'), 'ASTRO')).toEqual([
      { from: 0, to: 5 },
      { from: 6, to: 11 },
    ])
  })

  it('NFKDで長さの変わる文字は本文の側の範囲を返す', () => {
    expect(findMatches(createState('ｶﾞ'), 'ガ')).toEqual([{ from: 0, to: 2 }])
    expect(findMatches(createState('ガ'), 'ｶﾞ')).toEqual([{ from: 0, to: 1 }])
    expect(findMatches(createState('㍻'), '平成')).toEqual([{ from: 0, to: 1 }])
  })

  it('NFKDで分かれた文字の途中までの一致は外す', () => {
    expect(findMatches(createState('ガギ カ'), 'カ')).toEqual([{ from: 3, to: 4 }])
    expect(findMatches(createState('はばぱ'), 'は')).toEqual([{ from: 0, to: 1 }])
  })

  it('ひらがなとカタカナは区別する', () => {
    expect(findMatches(createState('キゴウ'), 'きごう')).toEqual([])
  })

  it('正規表現として解釈しない', () => {
    const state = createState('a.*b [a]\nc\\n')
    expect(findMatches(state, '.*')).toEqual([{ from: 1, to: 3 }])
    expect(findMatches(state, '[a]')).toEqual([{ from: 5, to: 8 }])
    expect(findMatches(state, '\\n')).toEqual([{ from: 10, to: 12 }])
  })

  it('サロゲートペアの後ろでも位置がずれない', () => {
    expect(findMatches(createState('😀あい'), 'あい')).toEqual([{ from: 2, to: 4 }])
  })

  it('一致は重ならない', () => {
    expect(findMatches(createState('aaa'), 'aa')).toEqual([{ from: 0, to: 2 }])
    expect(findMatches(createState('aaaa'), 'aa')).toEqual([
      { from: 0, to: 2 },
      { from: 2, to: 4 },
    ])
  })
})

describe('currentIndex', () => {
  const matches = findMatches(createState('うえおうえお'), 'うえお')

  it('選択が一致とちょうど同じならその番号', () => {
    expect(currentIndex(matches, EditorSelection.single(0, 3).main)).toBe(0)
    expect(currentIndex(matches, EditorSelection.single(3, 6).main)).toBe(1)
  })

  it('選択の向きが逆でも番号を返す', () => {
    expect(currentIndex(matches, EditorSelection.single(6, 3).main)).toBe(1)
  })

  it('一致の一部なら-1', () => {
    expect(currentIndex(matches, EditorSelection.single(0, 2).main)).toBe(-1)
  })

  it('一致より広いなら-1', () => {
    expect(currentIndex(matches, EditorSelection.single(0, 6).main)).toBe(-1)
  })

  it('空の選択なら-1', () => {
    expect(currentIndex(matches, EditorSelection.single(3).main)).toBe(-1)
  })

  it('一致が無ければ-1', () => {
    expect(currentIndex([], EditorSelection.single(0, 3).main)).toBe(-1)
  })
})

describe('nextMatch', () => {
  const matches = findMatches(createState('うえおうえおうえお'), 'うえお')

  it('posちょうどから始まる一致を返す', () => {
    expect(nextMatch(matches, 0)).toEqual({ from: 0, to: 3 })
    expect(nextMatch(matches, 6)).toEqual({ from: 6, to: 9 })
  })

  it('posが一致の途中ならその次の一致を返す', () => {
    expect(nextMatch(matches, 4)).toEqual({ from: 6, to: 9 })
  })

  it('後ろに無ければ先頭へ回る', () => {
    expect(nextMatch(matches, 9)).toEqual({ from: 0, to: 3 })
  })

  it('一致が1つならそれを返す', () => {
    const one = findMatches(createState('うえお'), 'うえお')
    expect(nextMatch(one, 3)).toEqual({ from: 0, to: 3 })
  })

  it('一致が無ければnull', () => {
    expect(nextMatch([], 0)).toBeNull()
  })

  it('選択のtoを渡すと選択中の一致の次へ進む', () => {
    const state = createState('うえおうえおうえお', EditorSelection.single(3, 6))
    const to = state.selection.main.to
    expect(nextMatch(matches, to)).toEqual({ from: 6, to: 9 })
  })
})

describe('prevMatch', () => {
  const matches = findMatches(createState('うえおうえおうえお'), 'うえお')

  it('posちょうどから始まる一致は返さない', () => {
    expect(prevMatch(matches, 6)).toEqual({ from: 3, to: 6 })
  })

  it('posが一致の途中ならその一致を返す', () => {
    expect(prevMatch(matches, 5)).toEqual({ from: 3, to: 6 })
  })

  it('前に無ければ末尾へ回る', () => {
    expect(prevMatch(matches, 0)).toEqual({ from: 6, to: 9 })
  })

  it('一致が1つならそれを返す', () => {
    const one = findMatches(createState('うえお'), 'うえお')
    expect(prevMatch(one, 0)).toEqual({ from: 0, to: 3 })
  })

  it('一致が無ければnull', () => {
    expect(prevMatch([], 0)).toBeNull()
  })

  it('選択のfromを渡すと選択中の一致の前へ戻る', () => {
    const state = createState('うえおうえおうえお', EditorSelection.single(3, 6))
    const from = state.selection.main.from
    expect(prevMatch(matches, from)).toEqual({ from: 0, to: 3 })
  })
})

describe('selectedQuery', () => {
  it('1行に収まる選択ならその文字', () => {
    expect(selectedQuery(createState('あいうえお', EditorSelection.single(1, 4)))).toBe('いうえ')
  })

  it('選択の向きが逆でもその文字', () => {
    expect(selectedQuery(createState('あいうえお', EditorSelection.single(4, 1)))).toBe('いうえ')
  })

  it('空の選択ならnull', () => {
    expect(selectedQuery(createState('あいうえお', EditorSelection.single(2)))).toBeNull()
  })

  it('改行をまたぐ選択ならnull', () => {
    expect(selectedQuery(createState('あいう\nえお', EditorSelection.single(2, 5)))).toBeNull()
  })

  it('選択が複数ならnull', () => {
    const selection = EditorSelection.create([
      EditorSelection.range(0, 2),
      EditorSelection.range(4, 5),
    ])
    // 有効にしないとEditorState.createが選択を1つにまとめる
    const state = EditorState.create({
      doc: 'あいうえお',
      selection,
      extensions: EditorState.allowMultipleSelections.of(true),
    })
    expect(selectedQuery(state)).toBeNull()
  })
})

describe('searchHighlight', () => {
  const doc = 'うえおうえおうえお'

  function highlightState(initialDoc: string): EditorState {
    return EditorState.create({ doc: initialDoc, extensions: [searchHighlight] })
  }

  function setQuery(state: EditorState, query: string): EditorState {
    return state.update({ effects: setSearchHighlight.of(query) }).state
  }

  // 範囲と合わせて、印がcm-searchMatchのものかも確かめる
  function highlightRanges(state: EditorState) {
    const ranges: Array<{ from: number; to: number }> = []
    const cursor = state.field(searchHighlight).iter()
    while (cursor.value) {
      expect(cursor.value.spec.class).toBe('cm-searchMatch')
      ranges.push({ from: cursor.from, to: cursor.to })
      cursor.next()
    }
    return ranges
  }

  it('初めは装飾が無い', () => {
    expect(highlightRanges(highlightState(doc))).toEqual([])
  })

  it('文字を渡すと一致に装飾が付く', () => {
    const searched = setQuery(highlightState(doc), 'うえお')
    expect(highlightRanges(searched)).toEqual(findMatches(searched, 'うえお'))
  })

  it('空の文字を渡すと消える', () => {
    const cleared = setQuery(setQuery(highlightState(doc), 'うえお'), '')
    expect(highlightRanges(cleared)).toEqual([])
  })

  it('本文を書き換えると新しい本文で付け直す', () => {
    const start = setQuery(highlightState('うえお'), 'うえお')
    expect(highlightRanges(start)).toEqual([{ from: 0, to: 3 }])

    // 一致を足す
    const added = start.update({ changes: { from: 3, insert: 'うえお' } }).state
    expect(highlightRanges(added)).toEqual([
      { from: 0, to: 3 },
      { from: 3, to: 6 },
    ])

    // 一致の前に文字を足して位置がずれる
    const shifted = added.update({ changes: { from: 0, insert: 'x' } }).state
    expect(highlightRanges(shifted)).toEqual([
      { from: 1, to: 4 },
      { from: 4, to: 7 },
    ])

    // 一致を消す。範囲ごと消すと装飾の位置の写し替えだけでも消えるので、同じ長さの別の文字に替える
    const removed = shifted.update({ changes: { from: 2, to: 3, insert: 'x' } }).state
    expect(highlightRanges(removed)).toEqual([{ from: 4, to: 7 }])
  })

  it('選択だけの変更では装飾を作り直さない', () => {
    const searched = setQuery(highlightState(doc), 'うえお')
    const before = searched.field(searchHighlight)
    const moved = searched.update({ selection: EditorSelection.single(3) }).state
    expect(moved.field(searchHighlight)).toBe(before)
  })
})
