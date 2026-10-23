import { saveArticle, type ArticleFields } from './api'

// 仮の間隔(docs/spec.md)。入力が止まってからと、入力が続いていても保存するまで
const IDLE_DELAY = 15_000
const MAX_DELAY = 120_000

export interface SaveStatus {
  // この記事を開いてから最後に保存できた時刻
  savedAt: Date | null
  // 最後の保存に失敗したか
  failed: boolean
}

export const initialSaveStatus: SaveStatus = { savedAt: null, failed: false }

// 保存は記事をまたいで1つずつ順に行う
let queue: Promise<unknown> = Promise.resolve()

// 書いている途中の記事を読まないよう、記事を読む前に待つ
export function whenSaved(): Promise<void> {
  return queue.then(() => {})
}

// 開いている1つの記事の自動保存。変更のたびに番号を進め、保存した番号と比べて未保存かを見る。
export class AutoSaver {
  private readonly slug: string
  private read: () => ArticleFields = () => {
    throw new Error('attach()の前に保存した')
  }
  private onStatus: (status: SaveStatus) => void = () => {}
  private revision = 0
  private savedRevision = 0
  private savedAt: Date | null = null
  private idleTimer: number | undefined
  private maxTimer: number | undefined
  private paused = false
  // 画面から外したあとは、保存の予約も結果の知らせもしない
  private attached = false

  constructor(slug: string) {
    this.slug = slug
  }

  // readは保存する内容を返す。保存を頼まれたときに読む
  attach(read: () => ArticleFields, onStatus: (status: SaveStatus) => void) {
    this.read = read
    this.onStatus = onStatus
    this.attached = true
  }

  detach() {
    this.attached = false
    this.clearTimers()
  }

  isDirty() {
    return this.revision !== this.savedRevision
  }

  changed() {
    this.revision++
    this.schedule()
  }

  // 止めている間は、自分からは保存しない。save()は呼べる
  setPaused(paused: boolean) {
    this.paused = paused
    if (paused) this.clearTimers()
    else if (this.isDirty()) this.schedule()
  }

  // 止めていなければ、間隔を待たずに保存する
  flush() {
    if (!this.paused && this.isDirty()) void this.save()
  }

  // 失敗したらfalse。未保存の変更が無ければ、前の保存を待ってtrue
  save(): Promise<boolean> {
    this.clearTimers()
    const target = this.revision
    const fields = this.isDirty() ? this.read() : null
    const job = queue.then(() => this.write(target, fields))
    queue = job
    return job
  }

  private async write(target: number, fields: ArticleFields | null): Promise<boolean> {
    // 前に頼まれた保存が、後の変更まで含めて済ませていることがある
    if (fields === null || target <= this.savedRevision) return true
    try {
      await saveArticle(this.slug, fields)
    } catch (e) {
      console.error('記事を保存できませんでした', e)
      if (this.attached) {
        this.onStatus({ savedAt: this.savedAt, failed: true })
        // 入力が無くても、決まった間隔で保存し直す
        if (!this.paused) this.maxTimer ??= window.setTimeout(() => void this.save(), MAX_DELAY)
      }
      return false
    }
    this.savedRevision = target
    this.savedAt = new Date()
    if (this.attached) this.onStatus({ savedAt: this.savedAt, failed: false })
    return true
  }

  private schedule() {
    if (this.paused || !this.attached) return
    window.clearTimeout(this.idleTimer)
    this.idleTimer = window.setTimeout(() => void this.save(), IDLE_DELAY)
    this.maxTimer ??= window.setTimeout(() => void this.save(), MAX_DELAY)
  }

  private clearTimers() {
    window.clearTimeout(this.idleTimer)
    window.clearTimeout(this.maxTimer)
    this.idleTimer = undefined
    this.maxTimer = undefined
  }
}

export function formatSavedTime(date: Date): string {
  return date.toLocaleTimeString('ja-JP', { hour: '2-digit', minute: '2-digit' })
}
