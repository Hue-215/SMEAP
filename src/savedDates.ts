import type { FrontmatterSettings, SavedDates, Warning } from './api'

export type DateKey = 'pubDate' | 'updatedDate'

export type DateValues = { pubDate: string | null; updatedDate: string | null }

// 手で変えた日付を覚える。保存は重なることがあるので、takeごとにその時点の番号を持つ
export class DateEdits {
  // 手で変えて、まだ保存できていない日付
  private edited = new Set<DateKey>()
  private editedAt = new Map<DateKey, number>()
  private sequence = 0
  private takenAt = new WeakMap<readonly DateKey[], number>()

  edit(key: DateKey): void {
    this.edited.add(key)
    this.editedAt.set(key, ++this.sequence)
  }

  // 保存を頼むときに送る日付
  take(): DateKey[] {
    const keys = [...this.edited]
    this.takenAt.set(keys, this.sequence)
    return keys
  }

  saved(taken: readonly DateKey[]): void {
    for (const key of taken) {
      if (!this.changedSince(taken, key)) this.edited.delete(key)
    }
  }

  changedSince(taken: readonly DateKey[], key: DateKey): boolean {
    return (this.editedAt.get(key) ?? 0) > (this.takenAt.get(taken) ?? 0)
  }
}

// takeで返った日付だけを保存の入力に入れる。updatedDateを外したときはnull
export function datesToSend(fields: DateValues, taken: readonly DateKey[]): Partial<DateValues> {
  const sent: Partial<DateValues> = {}
  if (taken.includes('pubDate')) sent.pubDate = fields.pubDate
  if (taken.includes('updatedDate')) sent.updatedDate = fields.updatedDate
  return sent
}

// 保存を頼んでから返るまでに手で変えていない日付だけ、結果の値にする
export function applySavedDates(
  current: DateValues,
  result: SavedDates,
  changed: readonly DateKey[],
): DateValues {
  return {
    pubDate: changed.includes('pubDate') ? current.pubDate : result.pubDate,
    updatedDate: changed.includes('updatedDate') ? current.updatedDate : result.updatedDate,
  }
}

// 無効にした項目の警告は出さない
export function visibleWarnings(warnings: Warning[], frontmatter: FrontmatterSettings): Warning[] {
  return warnings.filter((warning) => {
    if (warning.key === 'heroImage' && !frontmatter.heroImage) return false
    if (warning.key === 'updatedDate' && !frontmatter.updatedDate) return false
    return true
  })
}
