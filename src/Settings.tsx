import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { setPostsDirName, type StoreError } from './api'
import Dialog from './Dialog'
import Icon from './Icon'
import { isAndroid } from './platform'
import type { License } from 'virtual:licenses'

interface SettingsProps {
  narrow: boolean
  // 読み込む前はnull
  postsDir: string | null
  // デスクトップだけ
  onPickPostsDir: () => Promise<void>
  // Androidだけ
  onPostsDirNameChange: (name: string) => void
  onClose: () => void
}

interface ShownText {
  title: string
  text: string
}

// 編集画面の上に重ねる設定画面。ライセンスの全文は、広い窓では一覧の行の下に開き、狭い窓では
// 別のページに出す。
export default function Settings({
  narrow,
  postsDir,
  onPickPostsDir,
  onPostsDirNameChange,
  onClose,
}: SettingsProps) {
  const [page, setPage] = useState<'top' | 'list'>('top')
  const [shown, setShown] = useState<ShownText | null>(null)
  const headingId = useId()
  const backButton = useRef<HTMLButtonElement>(null)
  const body = useRef<HTMLDivElement>(null)
  // 全文から戻ったときに、一覧の位置へ戻す
  const listScroll = useRef(0)

  useLayoutEffect(() => {
    if (body.current) body.current.scrollTop = page === 'list' && !shown ? listScroll.current : 0
  }, [page, shown])
  // 押したボタンがページごと消えるので、フォーカスを戻るボタンへ移す
  useEffect(() => {
    backButton.current?.focus()
  }, [page, shown])

  const show = (text: ShownText) => {
    listScroll.current = body.current?.scrollTop ?? 0
    setShown(text)
  }

  // 1段上のページへ(Esc・Androidの「戻る」も)。トップからは編集画面に戻る
  const back = () => {
    if (shown) setShown(null)
    else if (page === 'list') {
      listScroll.current = 0
      setPage('top')
    } else onClose()
  }

  const [heading, backLabel] = shown
    ? [shown.title, '一覧に戻る']
    : page === 'list'
      ? ['オープンソースライセンス', '設定に戻る']
      : ['設定', '編集画面に戻る']

  return (
    <Dialog labelledBy={headingId} onCancel={back} className="settings-dialog">
      <div className="settings">
        <div className="topbar">
          <button
            ref={backButton}
            type="button"
            className="icon-button"
            aria-label={backLabel}
            onClick={back}
          >
            <Icon name="arrow_back" />
          </button>
          <h1 id={headingId} className="settings-title">
            {heading}
          </h1>
        </div>
        <div ref={body} className="settings-body">
          <div className="settings-content">
            {page === 'top' ? (
              <>
                <h2 className="settings-h2">記事の保存先</h2>
                {isAndroid ? (
                  postsDir !== null && (
                    <PostsDirNameRow initial={postsDir} onChange={onPostsDirNameChange} />
                  )
                ) : (
                  <PostsDirRow postsDir={postsDir} onPick={onPickPostsDir} />
                )}
                <h2 className="settings-h2">このアプリについて</h2>
                <div className="settings-rows">
                  <div className="settings-row">
                    <span className="settings-row-name">Astro-Notepad</span>
                    <span className="settings-note">MIT License</span>
                  </div>
                  <button type="button" className="settings-row" onClick={() => setPage('list')}>
                    <span className="settings-row-main">
                      オープンソースライセンス
                      <span className="settings-note">使っているライブラリ・フォントとライセンス文</span>
                    </span>
                    <Icon name="chevron_right" />
                  </button>
                </div>
              </>
            ) : (
              <>
                {/* 全文から戻ったときに絞り込みを残すため、隠すだけにする */}
                <div hidden={shown !== null}>
                  <LicenseList narrow={narrow} onShow={show} />
                </div>
                {shown && <pre className="settings-text">{shown.text}</pre>}
              </>
            )}
          </div>
        </div>
      </div>
    </Dialog>
  )
}

function PostsDirRow({
  postsDir,
  onPick,
}: {
  postsDir: string | null
  onPick: () => Promise<void>
}) {
  const [busy, setBusy] = useState(false)
  const [failed, setFailed] = useState(false)

  const pick = async () => {
    setBusy(true)
    setFailed(false)
    try {
      await onPick()
    } catch (e) {
      console.error('記事のディレクトリを変えられませんでした', e)
      setFailed(true)
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <div className="settings-rows">
        <button type="button" className="settings-row" onClick={pick} disabled={busy}>
          <span className="settings-row-main">
            記事のディレクトリ
            <span className="settings-note">{postsDir}</span>
          </span>
          <Icon name="chevron_right" />
        </button>
      </div>
      {failed && <p className="settings-failure">変更できませんでした</p>}
    </>
  )
}

function PostsDirNameRow({
  initial,
  onChange,
}: {
  initial: string
  onChange: (name: string) => void
}) {
  const [value, setValue] = useState(initial)
  const [busy, setBusy] = useState(false)
  const [failure, setFailure] = useState<string | null>(null)
  const inputId = useId()

  const commit = async () => {
    const name = value.trim()
    if (busy || name === initial) return
    setBusy(true)
    setFailure(null)
    try {
      await setPostsDirName(name)
      setValue(name)
      onChange(name)
    } catch (e) {
      console.error('記事のディレクトリ名を変えられませんでした', e)
      setFailure(
        (e as StoreError | undefined)?.kind === 'invalidPostsDirName'
          ? 'ディレクトリ名に使えません'
          : '変更できませんでした',
      )
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <div className="settings-rows">
        <div className="settings-row">
          <span className="settings-row-main">
            <label htmlFor={inputId}>記事のディレクトリ名</label>
            <span className="settings-note posts-dir-name">
              <span>src/content/</span>
              <input
                id={inputId}
                type="text"
                value={value}
                disabled={busy}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') void commit()
                }}
                onBlur={() => void commit()}
                onChange={(e) => {
                  setValue(e.target.value)
                  setFailure(null)
                }}
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
              />
            </span>
          </span>
        </div>
      </div>
      {failure && <p className="settings-failure">{failure}</p>}
    </>
  )
}

function LicenseList({ narrow, onShow }: { narrow: boolean; onShow: (shown: ShownText) => void }) {
  const [licenses, setLicenses] = useState<License[] | 'failed' | null>(null)
  const [filter, setFilter] = useState('')
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set())

  // ライセンス文は数百件あるので、別のチャンクに分けて一覧を開いたときに読む
  useEffect(() => {
    import('virtual:licenses')
      .then((mod) => setLicenses(mod.default))
      .catch((e) => {
        console.error('ライセンスの一覧を読めませんでした', e)
        setLicenses('failed')
      })
  }, [])

  // 同じ名前が2つ以上あれば、版を添えて区別する
  const duplicated = useMemo(() => {
    const seen = new Set<string>()
    const duplicated = new Set<string>()
    if (Array.isArray(licenses)) {
      for (const { name } of licenses) (seen.has(name) ? duplicated : seen).add(name)
    }
    return duplicated
  }, [licenses])

  if (licenses === 'failed') return <p className="editor-message">ライセンスの一覧を読めませんでした</p>
  if (licenses === null) return null

  const query = filter.toLowerCase()
  const rows = licenses.filter((l) => l.name.toLowerCase().includes(query))

  const toggle = (key: string) =>
    setExpanded((prev) => {
      const next = new Set(prev)
      if (!next.delete(key)) next.add(key)
      return next
    })

  return (
    <>
      <label className="settings-filter">
        <Icon name="search" />
        <input
          value={filter}
          aria-label="パッケージ名で絞り込む"
          placeholder="パッケージ名で絞り込む"
          onChange={(e) => setFilter(e.target.value)}
        />
      </label>
      <p className="settings-count settings-note">{rows.length}件</p>
      <div className="settings-rows">
        {rows.map((license) => {
          const key = `${license.name}@${license.version}`
          const title = duplicated.has(license.name) ? `${license.name} ${license.version}` : license.name
          const open = !narrow && expanded.has(key)
          return (
            <div key={key}>
              <button
                type="button"
                className="settings-row"
                aria-expanded={narrow ? undefined : open}
                onClick={() => (narrow ? onShow({ title, text: license.text }) : toggle(key))}
              >
                <span className="settings-row-name">{title}</span>
                <span className="settings-note">{license.license}</span>
                <Icon
                  name={narrow ? 'chevron_right' : open ? 'keyboard_arrow_up' : 'keyboard_arrow_down'}
                />
              </button>
              {open && <pre className="settings-text">{license.text}</pre>}
            </div>
          )
        })}
      </div>
    </>
  )
}
