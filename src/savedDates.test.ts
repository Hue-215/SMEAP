import { describe, expect, it } from 'vitest'
import type { FrontmatterSettings, SavedDates, Warning } from './api'
import {
  DateEdits,
  applySavedDates,
  datesToSend,
  visibleWarnings,
  type DateValues,
} from './savedDates'

function values(pubDate: string | null, updatedDate: string | null): DateValues {
  return { pubDate, updatedDate }
}

describe('datesToSend', () => {
  it('手で変えていない日付は入らない', () => {
    const fields = values('2026-01-01', '2026-02-02')
    expect(datesToSend(fields, [])).toEqual({})
  })

  it('pubDateを変えたら入る', () => {
    const fields = values('2026-03-03', '2026-02-02')
    expect(datesToSend(fields, ['pubDate'])).toEqual({ pubDate: '2026-03-03' })
  })

  it('updatedDateを外したらnullが入る', () => {
    const fields = values('2026-01-01', null)
    expect(datesToSend(fields, ['updatedDate'])).toEqual({ updatedDate: null })
  })

  it('updatedDateを変えたら日付が入る', () => {
    const fields = values('2026-01-01', '2026-03-03')
    expect(datesToSend(fields, ['updatedDate'])).toEqual({ updatedDate: '2026-03-03' })
  })
})

describe('DateEdits', () => {
  it('pubDateを変えて保存できた後、本文だけの保存では日付を送らない', () => {
    const edits = new DateEdits()
    edits.edit('pubDate')
    const taken = edits.take()
    expect(taken).toEqual(['pubDate'])

    edits.saved(taken)
    expect(edits.take()).toEqual([])
  })

  it('updatedDateを変えて保存できた後も、次の保存では送らない', () => {
    const edits = new DateEdits()
    edits.edit('updatedDate')
    edits.edit('pubDate')
    const taken = edits.take()
    expect([...taken].sort()).toEqual(['pubDate', 'updatedDate'])

    edits.saved(taken)
    expect(edits.take()).toEqual([])
  })

  it('保存に失敗したら次の保存でも送る', () => {
    const edits = new DateEdits()
    edits.edit('pubDate')
    expect(edits.take()).toEqual(['pubDate'])

    // 保存に失敗したのでsavedを呼ばない
    expect(edits.take()).toEqual(['pubDate'])
  })

  it('takeの後に変えた日付は、保存できても次の保存で送る', () => {
    const edits = new DateEdits()
    edits.edit('updatedDate')
    const taken = edits.take()
    expect(edits.changedSince(taken, 'updatedDate')).toBe(false)

    edits.edit('updatedDate')
    expect(edits.changedSince(taken, 'updatedDate')).toBe(true)
    edits.saved(taken)
    expect(edits.take()).toEqual(['updatedDate'])
  })
})

describe('DateEdits(保存が重なるとき)', () => {
  it('前の保存の結果では、後のtakeまでに変えた日付を変えたものとして扱う', () => {
    const edits = new DateEdits()
    edits.edit('pubDate')
    const first = edits.take()
    edits.edit('pubDate')
    const second = edits.take()

    expect(edits.changedSince(first, 'pubDate')).toBe(true)
    edits.saved(first)
    expect(edits.changedSince(second, 'pubDate')).toBe(false)
    expect(edits.take()).toEqual(['pubDate'])
    edits.saved(second)
    expect(edits.take()).toEqual([])
  })
})

describe('applySavedDates', () => {
  it('保存の間に手で変えていない日付は結果の値になる', () => {
    const current = values('2026-01-01', null)
    const result: SavedDates = { pubDate: '2026-05-01', updatedDate: '2026-05-01' }
    expect(applySavedDates(current, result, [])).toEqual(result)
  })

  it('結果がnullの日付はnullになる', () => {
    const current = values('2026-01-01', '2026-03-03')
    const result: SavedDates = { pubDate: '2026-01-01', updatedDate: null }
    expect(applySavedDates(current, result, [])).toEqual(result)
  })

  it('保存の間に変えた日付は手の値のまま残る', () => {
    const current = values('2026-01-01', '2026-03-03')
    const result: SavedDates = { pubDate: '2026-05-01', updatedDate: '2026-05-01' }
    expect(applySavedDates(current, result, ['updatedDate'])).toEqual({
      pubDate: '2026-05-01',
      updatedDate: '2026-03-03',
    })
  })
})

describe('visibleWarnings', () => {
  const warnings: Warning[] = [
    { key: 'heroImage', message: 'heroImageを読めません', kept: true },
    { key: 'updatedDate', message: 'updatedDateを読めません', kept: true },
    { key: 'pubDate', message: 'pubDateを読めません', kept: true },
    { key: null, message: 'フロントマターがありません', kept: false },
  ]

  const enabled: FrontmatterSettings = { heroImage: true, updatedDate: true }

  it('どちらも有効ならすべての警告を出す', () => {
    expect(visibleWarnings(warnings, enabled)).toEqual(warnings)
  })

  it('heroImageを無効にするとheroImageの警告だけ除く', () => {
    expect(visibleWarnings(warnings, { ...enabled, heroImage: false })).toEqual([
      warnings[1],
      warnings[2],
      warnings[3],
    ])
  })

  it('updatedDateを無効にするとupdatedDateの警告だけ除く', () => {
    expect(visibleWarnings(warnings, { ...enabled, updatedDate: false })).toEqual([
      warnings[0],
      warnings[2],
      warnings[3],
    ])
  })

  it('どちらも無効にするとその2つの警告を除く', () => {
    expect(visibleWarnings(warnings, { heroImage: false, updatedDate: false })).toEqual([
      warnings[2],
      warnings[3],
    ])
  })
})
