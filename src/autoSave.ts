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

export interface AutoSaverHost {
  // 保存する内容。保存を頼まれたときに読む
  read: () => ArticleFields
  onStatus: (status: SaveStatus) => void
  onSaved: (fields: ArticleFields) => void
}

// 開いている1つの記事の自動保存。変更のたびに番号を進め、保存した番号と比べて未保存かを見る。
export class AutoSaver {
  private readonly slug: string
  // 画面から外したあとは、保存の予約も結果の知らせもしない
  private host: AutoSaverHost | null = null
  private status = initialSaveStatus
  private revision = 0
  private savedRevision = 0
  private idleTimer: number | undefined
  private maxTimer: number | undefined
  private paused = false

  constructor(slug: string) {
    this.slug = slug
  }

  attach(host: AutoSaverHost) {
    this.host = host
    host.onStatus(this.status)
  }

  detach() {
    this.host = null
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
    if (!this.isDirty()) return whenSaved().then(() => true)
    if (!this.host) return Promise.resolve(false)
    const target = this.revision
    const fields = this.host.read()
    const job = queue.then(() => this.write(target, fields))
    queue = job
    return job
  }

  private async write(target: number, fields: ArticleFields): Promise<boolean> {
    // 前に頼まれた保存が、後の変更まで含めて済ませていることがある
    if (target <= this.savedRevision) return true
    try {
      await saveArticle(this.slug, fields)
    } catch (e) {
      console.error('記事を保存できませんでした', e)
      this.setStatus({ savedAt: this.status.savedAt, failed: true })
      // 入力が無くても、決まった間隔で保存し直す
      if (this.host && !this.paused) {
        this.maxTimer ??= window.setTimeout(() => void this.save(), MAX_DELAY)
      }
      return false
    }
    this.savedRevision = target
    this.setStatus({ savedAt: new Date(), failed: false })
    this.host?.onSaved(fields)
    return true
  }

  private setStatus(status: SaveStatus) {
    this.status = status
    this.host?.onStatus(status)
  }

  private schedule() {
    if (this.paused || !this.host) return
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
