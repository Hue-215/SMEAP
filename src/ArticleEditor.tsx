import {
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type Ref,
  type RefObject,
} from 'react'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { EditorSelection, type EditorState, type StateEffect } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import {
  importBodyImage,
  importBodyImageFromPath,
  importClipboardImage,
  heroImageUrl,
  importHeroImage,
  removeHeroImage,
  type Article,
  type EditedFields,
  type FrontmatterSettings,
  type HeroImage,
  type SavedDates,
  type StoreError,
  type Warning,
} from './api'
import ArticleHeader, { type FieldsPatch } from './ArticleHeader'
import { AutoSaver, type SaveStatus } from './autoSave'
import Editor from './Editor'
import { insertImageRefs, insertLink } from './editor'
import { t } from './i18n'
import { isAndroid } from './platform'
import {
  DateEdits,
  applySavedDates,
  datesToSend,
  visibleWarnings,
  type DateKey,
} from './savedDates'
import {
  findMatches,
  findNextMatch,
  findPrevMatch,
  findStatus,
  nextMatch,
  selectedQuery,
  setSearchHighlight,
  type FindStatus,
  type Match,
} from './search'

export interface ArticleEditorHandle {
  isDirty: () => boolean
  // 失敗したらfalse
  save: () => Promise<boolean>
  insertLink: () => void
  insertImages: (files: File[]) => void
  // 今の選択の先頭を、findが探し始める位置として覚える。選択が1行に収まっていればその文字を返す
  beginFind: () => string | null
  find: (query: string) => void
  findNext: () => void
  findPrev: () => void
  endFind: () => void
  focusBody: () => void
}

interface ArticleEditorProps {
  // 開いたときの内容。記事を切り替えたら、keyで作り直す
  article: Article
  frontmatter: FrontmatterSettings
  // trueの間は決まった間隔での保存をしない
  paused: boolean
  // 削除予定の記事は入力できない
  readOnly: boolean
  onSaveStatus: (status: SaveStatus) => void
  onSaved: (fields: EditedFields, dates: SavedDates) => void
  // エラーとして出す文言
  onNotice: (text: string) => void
  // 本文・選択・検索の文字が変わり、進み具合が前と違うときに知らせる
  onFindStatus: (status: FindStatus) => void
  ref: Ref<ArticleEditorHandle>
}

type HeaderFields = {
  title: string | null
  description: string | null
  pubDate: string | null
  updatedDate: string | null
}
type HeaderKey = keyof HeaderFields

const DATE_KEYS: DateKey[] = ['pubDate', 'updatedDate']

// runは本文に入れる参照を返す。クリップボードに画像が無ければnull
interface ImageImport {
  label: string
  run: (slug: string) => Promise<string | null>
}

const fromFile = (file: File): ImageImport => ({
  label: file.name,
  run: async (slug) => importBodyImage(slug, new Uint8Array(await file.arrayBuffer())),
})

const fromPath = (path: string): ImageImport => ({
  label: path,
  run: (slug) => importBodyImageFromPath(slug, path),
})

const fromClipboard: ImageImport = { label: 'clipboard', run: importClipboardImage }

const isUnsupportedImage = (e: unknown) =>
  (e as StoreError | undefined)?.kind === 'unsupportedImage'

// 取り込めたものだけを入れる
async function importImages(
  slug: string,
  view: RefObject<EditorView | null>,
  onNotice: (text: string) => void,
  imports: ImageImport[],
) {
  const refs: string[] = []
  let unsupported = 0
  let failed = 0
  for (const image of imports) {
    try {
      const ref = await image.run(slug)
      if (ref !== null) refs.push(ref)
    } catch (e) {
      console.error('Failed to import the image', image.label, e)
      if (isUnsupportedImage(e)) unsupported++
      else failed++
    }
  }
  if (refs.length > 0) {
    // 取り込みの間に記事を切り替えるか削除予定にすると、編集欄はもう無いか入力できない。
    // 取り込んだ画像は参照されないまま残る
    if (view.current && !view.current.state.readOnly) insertImageRefs(view.current, refs)
    else console.error('Failed to insert the imported images into the body', slug, refs)
  }
  if (failed > 0) onNotice(t('article_editor.images_failed', { count: failed + unsupported }))
  else if (unsupported > 0) onNotice(t('article_editor.images_unsupported', { count: unsupported }))
}

// 開いている記事の入力欄と、その自動保存。
export default function ArticleEditor({
  article,
  frontmatter,
  paused,
  readOnly,
  onSaveStatus,
  onSaved,
  onNotice,
  onFindStatus,
  ref,
}: ArticleEditorProps) {
  const [fields, setFields] = useState<HeaderFields>(() => ({
    title: article.title,
    description: article.description,
    pubDate: article.pubDate,
    updatedDate: article.updatedDate,
  }))
  // サムネイルの表示。保存の入力には含まれないので、fieldsとは別に持つ
  const [hero, setHero] = useState<{ image: HeroImage | null; version: number }>({
    image: article.heroImage,
    version: 0,
  })
  // 値を変えた項目。そのkeyの警告は出さない
  const [edited, setEdited] = useState<Set<HeaderKey>>(() => new Set())
  // 保存のときに読む。stateは描き直すまで前の値なので、入力を受けたときにこちらも書き換える
  const latest = useRef(fields)
  const view = useRef<EditorView>(null)
  const callbacks = useRef({ onSaveStatus, onSaved, onNotice, onFindStatus })
  const readOnlyRef = useRef(readOnly)
  useEffect(() => {
    callbacks.current = { onSaveStatus, onSaved, onNotice, onFindStatus }
    readOnlyRef.current = readOnly
  })
  const [saver] = useState(() => new AutoSaver(article.slug))
  // 手で変えた日付と、保存を頼んだときに送った日付
  const [dateEdits] = useState(() => new DateEdits())
  const takes = useRef(new WeakMap<EditedFields, DateKey[]>())

  // 前の記事の保存の状態が見えないよう、描く前に知らせる
  useLayoutEffect(() => {
    saver.attach({
      read: () => {
        if (!view.current) throw new Error('The body editor is missing')
        const taken = dateEdits.take()
        const dates = datesToSend(latest.current, taken)
        const fields: EditedFields = {
          title: latest.current.title,
          description: latest.current.description,
          body: view.current.state.doc.toString(),
        }
        if (dates.pubDate != null) fields.pubDate = dates.pubDate
        if (dates.updatedDate !== undefined) fields.updatedDate = dates.updatedDate
        takes.current.set(fields, taken)
        return fields
      },
      onStatus: (status) => callbacks.current.onSaveStatus(status),
      onSaved: (fields, dates) => {
        const taken = takes.current.get(fields) ?? []
        takes.current.delete(fields)
        const changed = DATE_KEYS.filter((key) => dateEdits.changedSince(taken, key))
        dateEdits.saved(taken)
        latest.current = { ...latest.current, ...applySavedDates(latest.current, dates, changed) }
        setFields(latest.current)
        callbacks.current.onSaved(fields, dates)
      },
    })
    // Androidでは、裏へ回したアプリが知らせなく終了されることがある。確認のダイアログを出している間も保存する
    const onVisibilityChange = () => {
      if (document.hidden && saver.isDirty()) void saver.save()
    }
    document.addEventListener('visibilitychange', onVisibilityChange)
    return () => {
      document.removeEventListener('visibilitychange', onVisibilityChange)
      saver.detach()
    }
  }, [saver, dateEdits])

  useEffect(() => saver.setPaused(paused || readOnly), [saver, paused, readOnly])

  const slug = article.slug
  const runImports = (imports: ImageImport[]) =>
    void importImages(slug, view, (text) => callbacks.current.onNotice(text), imports)

  // 取り込みや外す操作が並んで走ると、mdと表示が別の画像を指すことがあるので、終わるまで次を受けない
  const [heroBusy, setHeroBusy] = useState(false)
  // runは新しいheroImageのファイル名を返す。failedはエラーから知らせる文言を作る。subjectはログに残す対象
  const changeHero = async (
    run: () => Promise<string | null>,
    failed: (e: unknown) => string,
    subject: string,
  ) => {
    setHeroBusy(true)
    try {
      // 前の画像を消すかはmdの本文が参照しているかで決めるので、本文の変更を先に保存する
      if (!(await saver.save())) throw new Error('Failed to save the article')
      const name = await run()
      setHero((prev) => ({
        image: name === null ? null : { name, shared: false },
        version: prev.version + 1,
      }))
    } catch (e) {
      console.error('Failed to change the hero image', subject, e)
      callbacks.current.onNotice(failed(e))
    } finally {
      setHeroBusy(false)
    }
  }

  const pickHeroImage = (file: File) =>
    void changeHero(
      async () => importHeroImage(slug, new Uint8Array(await file.arrayBuffer())),
      (e) =>
        t(
          isUnsupportedImage(e)
            ? 'article_editor.hero_unsupported'
            : 'article_editor.hero_import_failed',
        ),
      file.name,
    )

  const removeHero = () =>
    void changeHero(
      async () => {
        await removeHeroImage(slug)
        return null
      },
      () => t('article_editor.hero_remove_failed'),
      slug,
    )

  // LinuxのWebKitGTKはファイルマネージャーからのドロップを画面へファイルとして渡さないので、Tauriが受けたパスで取り込む
  useEffect(() => {
    if (isAndroid) return
    let disposed = false
    let unlisten: (() => void) | null = null
    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type !== 'drop' || !view.current || readOnlyRef.current) return
        const editor = view.current
        const x = event.payload.position.x / window.devicePixelRatio
        const y = event.payload.position.y / window.devicePixelRatio
        // 本文の外に落としたらカーソルの位置に入れる
        const target = document.elementFromPoint(x, y)
        const pos = target && editor.dom.contains(target) ? editor.posAtCoords({ x, y }) : null
        if (pos !== null) editor.dispatch({ selection: { anchor: pos } })
        void importImages(
          slug,
          view,
          (text) => callbacks.current.onNotice(text),
          event.payload.paths.map(fromPath),
        )
      })
      .then((fn) => {
        if (disposed) fn()
        else unlisten = fn
      })
      .catch((e) => console.error('Failed to handle the drop', e))
    return () => {
      disposed = true
      unlisten?.()
    }
  }, [slug])

  const findStart = useRef(0)
  // Appは前の記事の値を持っているので、初めの1回は値によらず知らせる(null)
  const lastFindStatus = useRef<FindStatus | null>(null)

  const reportFindStatus = (state: EditorState) => {
    const status = findStatus(state)
    const last = lastFindStatus.current
    if (last && status.current === last.current && status.total === last.total) return
    lastFindStatus.current = status
    callbacks.current.onFindStatus(status)
  }

  const selectMatch = (match: Match, effects: StateEffect<unknown>[] = []) => {
    view.current?.dispatch({
      selection: EditorSelection.single(match.from, match.to),
      // 上に浮かせた検索の箱に隠れないよう、余白を大きめにとる
      effects: [...effects, EditorView.scrollIntoView(match.from, { y: 'nearest', yMargin: 80 })],
    })
  }

  const find = (query: string) => {
    const v = view.current
    if (!v) return
    const match = nextMatch(findMatches(v.state, query), findStart.current)
    const effect = setSearchHighlight.of(query)
    // 一致が無くても文字を渡しておき、後の書き換えで一致ができたら下線を付ける
    if (match) selectMatch(match, [effect])
    else v.dispatch({ effects: effect })
  }

  const findNext = () => {
    const match = view.current && findNextMatch(view.current.state)
    if (match) selectMatch(match)
  }

  const findPrev = () => {
    const match = view.current && findPrevMatch(view.current.state)
    if (match) selectMatch(match)
  }

  useImperativeHandle(ref, () => ({
    isDirty: () => saver.isDirty(),
    save: () => saver.save(),
    insertLink: () => {
      if (view.current) insertLink(view.current)
    },
    insertImages: (files) => runImports(files.map(fromFile)),
    beginFind: () => {
      if (!view.current) return null
      findStart.current = view.current.state.selection.main.from
      return selectedQuery(view.current.state)
    },
    find,
    findNext,
    findPrev,
    endFind: () => view.current?.dispatch({ effects: setSearchHighlight.of('') }),
    focusBody: () => view.current?.focus(),
  }))

  const updateFields = (patch: FieldsPatch) => {
    const changes = Object.entries(patch) as Array<[HeaderKey, string | null]>
    for (const [key] of changes) {
      if (key === 'pubDate' || key === 'updatedDate') dateEdits.edit(key)
    }
    if (changes.some(([key, value]) => value !== article[key])) {
      setEdited((keys) => {
        const next = new Set(keys)
        for (const [key] of changes) next.add(key)
        return next
      })
    }
    latest.current = { ...latest.current, ...patch }
    setFields(latest.current)
    saver.changed()
  }

  const warnings = visibleWarnings(article.warnings, frontmatter).filter((warning: Warning) => {
    // heroImageの警告は、サムネイルを取り込むか外したら出さない(どちらもmdの行を書き直す)
    if (warning.key === 'heroImage' && hero.version > 0) return false
    return warning.key === null || !edited.has(warning.key as HeaderKey)
  })

  return (
    <>
      <ArticleHeader
        fields={fields}
        warnings={warnings}
        onChange={updateFields}
        frontmatter={frontmatter}
        hero={
          hero.image === null
            ? null
            : {
                name: hero.image.name,
                url: heroImageUrl(slug, hero.image, hero.version),
              }
        }
        heroBusy={heroBusy}
        onPickHeroImage={pickHeroImage}
        onRemoveHeroImage={removeHero}
        readOnly={readOnly}
      />
      <Editor
        slug={article.slug}
        initialDoc={article.body}
        viewRef={view}
        readOnly={readOnly}
        onChange={() => saver.changed()}
        onUpdate={reportFindStatus}
        onPasteImage={(files) =>
          runImports(files.length > 0 ? files.map(fromFile) : isAndroid ? [] : [fromClipboard])
        }
      />
    </>
  )
}
