import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react'
import {
  UnsavedArticleError,
  githubAssetsPath,
  githubLogout,
  githubSelectRepository,
  githubStatus,
  githubUpdateSettings,
  repoFullName,
  validateBranchFormat,
  validatePostsPath,
  type GithubError,
  type GithubRepoRef,
  type GithubSettings,
  type GithubStatus,
  type GithubWaitLogin,
  type FrontmatterSettings,
  type PostsDir,
  type StoreError,
} from './api'
import Dialog from './Dialog'
import GithubLoginDialog from './GithubLoginDialog'
import GithubRepoDialog from './GithubRepoDialog'
import Icon from './Icon'
import { githubErrorText } from './githubError'
import { t } from './i18n'
import { isAndroid } from './platform'
import type { License } from 'virtual:licenses'

interface SettingsProps {
  narrow: boolean
  // 読めていない間はnull
  postsDir: PostsDir | null
  // 読めていなければnull。切り替えさせない
  frontmatter: FrontmatterSettings | null
  onChangeFrontmatter: (frontmatter: FrontmatterSettings) => Promise<void>
  // デスクトップだけ
  onPickPostsDir: () => Promise<void>
  // Androidだけ
  onChangePostsDirName: (name: string) => Promise<void>
  // 保存先が替わるGitHubの操作。開いている記事を保存してから呼び、記事を閉じて読み直すのはAppが行う
  onGithubSwitch: (command: () => Promise<void>) => Promise<void>
  // ログインの待ち。待つ前に開いている記事を保存することと、loggedInで返った後の読み直しはAppが行う
  onGithubLoginWait: () => Promise<GithubWaitLogin>
  onClose: () => void
}

interface ShownText {
  title: string
  text: string
}

// GitHubの節でまとめて保存する3つの値
interface GithubFieldsValues {
  baseBranch: string
  branchFormat: string
  postsPath: string
}

// 編集画面の上に重ねる設定画面。ライセンスの全文は、広い窓では一覧の行の下に開き、狭い窓では
// 別のページに出す。
export default function Settings({
  narrow,
  postsDir,
  frontmatter,
  onChangeFrontmatter,
  onPickPostsDir,
  onChangePostsDirName,
  onGithubSwitch,
  onGithubLoginWait,
  onClose,
}: SettingsProps) {
  const [page, setPage] = useState<'top' | 'list'>('top')
  const [shown, setShown] = useState<ShownText | null>(null)
  // GitHubの節の状態。読めている間だけ節を出す
  const [github, setGithub] = useState<GithubStatus | null>(null)
  const [githubFailure, setGithubFailure] = useState<string | null>(null)
  const [loginDialog, setLoginDialog] = useState(false)
  const [repoDialog, setRepoDialog] = useState(false)
  const [logoutBusy, setLogoutBusy] = useState(false)
  const [frontmatterBusy, setFrontmatterBusy] = useState(false)
  const [frontmatterFailure, setFrontmatterFailure] = useState<string | null>(null)
  const headingId = useId()
  const backButton = useRef<HTMLButtonElement>(null)
  const body = useRef<HTMLDivElement>(null)
  // 全文から戻ったときに、一覧の位置へ戻す
  const listScroll = useRef(0)

  const reloadGithub = (): Promise<GithubStatus | null> => {
    return githubStatus()
      .then(async (status) => {
        // ログインが切れていたら、記事を保存してから今までの保存先へ戻す
        if (status.loginExpired) {
          await onGithubSwitch(githubLogout)
          status = await githubStatus()
        }
        setGithub(status)
        setGithubFailure(null)
        return status
      })
      .catch((e) => {
        console.error('Failed to read the GitHub status', e)
        setGithub(null)
        setGithubFailure(githubErrorText(e))
        return null
      })
  }

  // 開いたときに一度だけ読む。読む処理はpropsを使うので、ref経由で最新のものを呼ぶ
  const reloadGithubRef = useRef(reloadGithub)
  useEffect(() => {
    reloadGithubRef.current = reloadGithub
  })
  useEffect(() => {
    void reloadGithubRef.current()
  }, [])

  // 連携しているか。ログインしていてRepositoryを選んでいること(Rust側のconfig::linkedと同じ)
  const linked = github !== null && github.loggedIn && github.settings.repo !== null
  const showPostsLocation = isAndroid || !linked

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
    ? [shown.title, t('settings.back_to_licenses')]
    : page === 'list'
      ? [t('settings.licenses'), t('settings.back_to_settings')]
      : [t('settings.title'), t('settings.back_to_editor')]

  const logout = async () => {
    if (logoutBusy) return
    setLogoutBusy(true)
    setGithubFailure(null)
    try {
      await onGithubSwitch(githubLogout)
      await reloadGithub()
    } catch (e) {
      console.error('Failed to log out from GitHub', e)
      setGithubFailure(githubErrorText(e))
    } finally {
      setLogoutBusy(false)
    }
  }

  // ダイアログを閉じた後に承認が通っても、表示をログインした状態に合わせる
  const waitLogin = async () => {
    const result = await onGithubLoginWait()
    if (result.outcome === 'loggedIn') void reloadGithub()
    return result
  }

  // ログインできた後。Repositoryを選んでいなければ、続けて選ばせる
  const loginDone = async () => {
    setLoginDialog(false)
    const status = await reloadGithub()
    if (status !== null && status.settings.repo === null) setRepoDialog(true)
  }

  // 選べたらダイアログを閉じて読み直す。失敗の文言はダイアログに出す
  const selectRepo = async (repo: GithubRepoRef): Promise<string | null> => {
    try {
      await onGithubSwitch(() => githubSelectRepository(repo))
      setRepoDialog(false)
      await reloadGithub()
      return null
    } catch (e) {
      console.error('Failed to select the repository', e)
      return githubErrorText(e)
    }
  }

  // 3つの欄をまとめて保存する。記事のディレクトリが変わるときは保存先も替わるので、
  // 開いている記事の保存と読み直しをAppが行う
  const saveGithubSettings = async (values: GithubFieldsValues, dirChanged: boolean) => {
    const command = () =>
      githubUpdateSettings(
        values.baseBranch === '' ? null : values.baseBranch,
        values.branchFormat,
        values.postsPath,
      )
    if (dirChanged) await onGithubSwitch(command)
    else await command()
  }

  const toggleFrontmatter = async (key: keyof FrontmatterSettings) => {
    if (frontmatterBusy || frontmatter === null) return
    setFrontmatterBusy(true)
    setFrontmatterFailure(null)
    try {
      await onChangeFrontmatter({ ...frontmatter, [key]: !frontmatter[key] })
    } catch (e) {
      console.error('Failed to save the frontmatter settings', e)
      setFrontmatterFailure(t('common.change_failed'))
    } finally {
      setFrontmatterBusy(false)
    }
  }

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
                {github?.tokenUnreadable && (
                  <p className="settings-github-warning">{t('settings.github_token_unreadable')}</p>
                )}
                <h2 className="settings-h2">{t('settings.github')}</h2>
                {github !== null && (
                  <div className="settings-rows">
                    {github.loggedIn ? (
                      <>
                        <div className="settings-row">
                          <span className="settings-row-main">
                            <span>{t('settings.github_account')}</span>
                            <span className="settings-note">
                              {github.login ?? t('settings.github_account_unreadable')}
                            </span>
                          </span>
                          <button
                            type="button"
                            className="settings-row-action"
                            onClick={() => void logout()}
                            disabled={logoutBusy}
                          >
                            {t('settings.github_logout')}
                          </button>
                        </div>
                        <div className="settings-row">
                          <span className="settings-row-main">
                            <span>{t('settings.github_repo')}</span>
                            <span className="settings-note">
                              {github.settings.repo === null
                                ? t('settings.github_repo_none')
                                : repoFullName(github.settings.repo)}
                            </span>
                          </span>
                          <button
                            type="button"
                            className="settings-row-action"
                            onClick={() => setRepoDialog(true)}
                          >
                            {github.settings.repo === null
                              ? t('settings.github_repo_pick')
                              : t('settings.github_repo_change')}
                          </button>
                        </div>
                        {github.settings.repo !== null && (
                          <GithubFields
                            key={repoFullName(github.settings.repo)}
                            initial={github.settings}
                            onSave={saveGithubSettings}
                          />
                        )}
                      </>
                    ) : (
                      <div className="settings-row">
                        <span className="settings-row-main">
                          <span>{t('settings.github_not_logged_in')}</span>
                          <span className="settings-note">{t('settings.github_login_note')}</span>
                        </span>
                        <button
                          type="button"
                          className="settings-row-action primary"
                          onClick={() => setLoginDialog(true)}
                        >
                          {t('settings.github_login')}
                        </button>
                      </div>
                    )}
                  </div>
                )}
                {githubFailure !== null && <p className="settings-failure">{githubFailure}</p>}
                {showPostsLocation && (
                  <>
                    <h2 className="settings-h2">{t('settings.posts_location')}</h2>
                    {isAndroid ? (
                      postsDir?.kind === 'android' && (
                        <PostsDirNameRow initial={postsDir.name} onChange={onChangePostsDirName} />
                      )
                    ) : (
                      <PostsDirRow postsDir={postsDir} onPick={onPickPostsDir} />
                    )}
                  </>
                )}
                <h2 className="settings-h2">{t('settings.frontmatter')}</h2>
                <div className="settings-rows">
                  {(
                    [
                      ['heroImage', 'settings.frontmatter_hero_image'],
                      ['updatedDate', 'settings.frontmatter_updated_date'],
                    ] as const
                  ).map(([key, label]) => (
                    <label key={key} className="settings-row">
                      <span className="settings-row-main">{t(label)}</span>
                      <input
                        type="checkbox"
                        className="settings-check"
                        checked={frontmatter?.[key] ?? true}
                        disabled={frontmatterBusy || frontmatter === null}
                        onChange={() => void toggleFrontmatter(key)}
                      />
                    </label>
                  ))}
                </div>
                {frontmatterFailure !== null && (
                  <p className="settings-failure">{frontmatterFailure}</p>
                )}
                <h2 className="settings-h2">{t('settings.about')}</h2>
                <div className="settings-rows">
                  <div className="settings-row">
                    <span className="settings-row-name">SMEAP</span>
                    <span className="settings-note">MIT License</span>
                  </div>
                  <button type="button" className="settings-row" onClick={() => setPage('list')}>
                    <span className="settings-row-main">
                      {t('settings.licenses')}
                      <span className="settings-note">{t('settings.licenses_note')}</span>
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
      {loginDialog && (
        <GithubLoginDialog
          onWait={waitLogin}
          onDone={() => void loginDone()}
          onCancel={() => setLoginDialog(false)}
        />
      )}
      {repoDialog && (
        <GithubRepoDialog
          current={github?.settings.repo ?? null}
          onSelect={selectRepo}
          onCancel={() => setRepoDialog(false)}
        />
      )}
    </Dialog>
  )
}

function PostsDirRow({
  postsDir,
  onPick,
}: {
  postsDir: PostsDir | null
  onPick: () => Promise<void>
}) {
  const [busy, setBusy] = useState(false)
  const [failure, setFailure] = useState<string | null>(null)
  const dirs = postsDir?.kind === 'desktop' ? postsDir : null

  const pick = async () => {
    setBusy(true)
    setFailure(null)
    try {
      await onPick()
    } catch (e) {
      console.error('Failed to change the article directory', e)
      setFailure(
        (e as StoreError | undefined)?.kind === 'invalidPostsDir'
          ? t('settings.invalid_posts_dir')
          : e instanceof UnsavedArticleError
            ? t('settings.unsaved_article')
            : t('common.change_failed'),
      )
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <div className="settings-rows">
        <button type="button" className="settings-row" onClick={pick} disabled={busy}>
          <span className="settings-row-main">
            {t('settings.posts_dir')}
            <span className="settings-note posts-dir-note">
              {dirs && (
                <>
                  <span>{t('settings.posts_dir_posts', { path: dirs.postsDir })}</span>
                  <span>
                    {t('settings.posts_dir_assets', { path: `${dirs.assetsDir}/{slug}/` })}
                  </span>
                </>
              )}
            </span>
          </span>
          <Icon name="chevron_right" />
        </button>
      </div>
      {failure && <p className="settings-failure">{failure}</p>}
    </>
  )
}

function PostsDirNameRow({
  initial,
  onChange,
}: {
  initial: string
  onChange: (name: string) => Promise<void>
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
      await onChange(name)
      setValue(name)
    } catch (e) {
      console.error('Failed to change the article directory name', e)
      setFailure(
        (e as StoreError | undefined)?.kind === 'invalidPostsDirName'
          ? t('settings.invalid_posts_dir_name')
          : t('common.change_failed'),
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
            <label htmlFor={inputId}>{t('settings.posts_dir_name')}</label>
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

type Check = 'ok' | 'invalid' | 'failed'

const isGithubError = (e: unknown, kind: GithubError['kind']) =>
  typeof e === 'object' && e !== null && (e as { kind?: unknown }).kind === kind

// 使えない値ならinvalid、確かめること自体に失敗したらfailed
const checkOf = (e: unknown, kind: GithubError['kind']): Check =>
  isGithubError(e, kind) ? 'invalid' : 'failed'

// 入力のたびに確かめ、最後の応答だけを出す。nullは確かめている間、failedは確かめられなかった
function useCheck(validate: (value: string) => Promise<void>, kind: GithubError['kind']) {
  const [check, setCheck] = useState<Check | null>('ok')
  const request = useRef(0)
  const run = (value: string) => {
    const id = ++request.current
    setCheck(null)
    validate(value).then(
      () => {
        if (id === request.current) setCheck('ok')
      },
      (e) => {
        if (id === request.current) setCheck(checkOf(e, kind))
      },
    )
  }
  return [check, run, setCheck] as const
}

// 文言の{slug}は差し込む値ではなく、形式に入れる文字そのもの
function branchFormatMessage(count: number, check: Check | null, format: string): string | null {
  if (count !== 1) return t('settings.github_branch_format_count', { slug: '{slug}' })
  switch (check) {
    case 'ok':
      return t('settings.github_branch_format_example', {
        name: format.replace('{slug}', 'first-post'),
      })
    case 'invalid':
      return t('settings.github_branch_format_invalid')
    case 'failed':
      return t('settings.github_check_failed')
    default:
      return null
  }
}

// GitHubの節の、Base branch以下の3つの入力欄。欄から外れたときとEnterで、3つをまとめて保存する
function GithubFields({
  initial,
  onSave,
}: {
  initial: GithubSettings
  onSave: (values: GithubFieldsValues, dirChanged: boolean) => Promise<void>
}) {
  const initialValues: GithubFieldsValues = {
    baseBranch: initial.baseBranch ?? '',
    branchFormat: initial.branchFormat,
    postsPath: initial.postsPath,
  }
  const [baseBranch, setBaseBranch] = useState(initialValues.baseBranch)
  const [format, setFormat] = useState(initialValues.branchFormat)
  const [postsPath, setPostsPath] = useState(initialValues.postsPath)
  // 保存できた値。替わったかは、ここと比べて分かる
  const [saved, setSaved] = useState(initialValues)
  const [formatCheck, checkFormat, setFormatCheck] = useCheck(
    validateBranchFormat,
    'invalidBranchFormat',
  )
  const [pathCheck, checkPostsPath, setPathCheck] = useCheck(validatePostsPath, 'invalidPostsPath')
  // Base branchは入力中には確かめず、保存のときに断られたら出す
  const [baseInvalid, setBaseInvalid] = useState(false)
  const [busy, setBusy] = useState(false)
  const [failure, setFailure] = useState<string | null>(null)
  const baseId = useId()
  const baseHintId = useId()
  const formatId = useId()
  const formatHintId = useId()
  const pathId = useId()
  const pathHintId = useId()

  const commit = async () => {
    if (busy) return
    const next: GithubFieldsValues = {
      baseBranch: baseBranch.trim(),
      branchFormat: format,
      postsPath,
    }
    const dirChanged = next.postsPath !== saved.postsPath
    const changed =
      dirChanged || next.baseBranch !== saved.baseBranch || next.branchFormat !== saved.branchFormat
    if (!changed) return
    setBusy(true)
    setFailure(null)
    try {
      // 入力中の確かめが終わる前に欄を離れても保存できるよう、ここで確かめ直す
      try {
        await validateBranchFormat(next.branchFormat)
        await validatePostsPath(next.postsPath)
      } catch (e) {
        if (isGithubError(e, 'invalidBranchFormat')) setFormatCheck('invalid')
        else if (isGithubError(e, 'invalidPostsPath')) setPathCheck('invalid')
        else throw e
        return
      }
      await onSave(next, dirChanged)
      setSaved(next)
    } catch (e) {
      if (isGithubError(e, 'invalidBranchName')) {
        setBaseInvalid(true)
        return
      }
      console.error('Failed to save the GitHub settings', e)
      setFailure(githubErrorText(e))
    } finally {
      setBusy(false)
    }
  }

  const formatCount = format.split('{slug}').length - 1
  const formatMessage = branchFormatMessage(formatCount, formatCheck, format)
  const formatInvalid = formatCount !== 1 || formatCheck === 'invalid'
  const pathInvalid = pathCheck === 'invalid'

  return (
    <>
      <div className="settings-field">
        <label htmlFor={baseId}>{t('settings.github_base_branch')}</label>
        <FieldInput
          id={baseId}
          describedBy={baseHintId}
          value={baseBranch}
          invalid={baseInvalid}
          busy={busy}
          placeholder={
            initial.repo === null
              ? undefined
              : t('settings.github_base_branch_placeholder', { name: initial.repo.defaultBranch })
          }
          onCommit={() => void commit()}
          onChange={(value) => {
            setBaseBranch(value)
            setBaseInvalid(false)
            setFailure(null)
          }}
        />
        <p
          id={baseHintId}
          aria-live="polite"
          className={baseInvalid ? 'settings-hint error' : 'settings-hint'}
        >
          {baseInvalid
            ? t('settings.github_base_branch_invalid')
            : t('settings.github_base_branch_note')}
        </p>
      </div>
      <div className="settings-field">
        <label htmlFor={formatId}>{t('settings.github_branch_format')}</label>
        <FieldInput
          id={formatId}
          describedBy={formatMessage === null ? undefined : formatHintId}
          value={format}
          invalid={formatInvalid}
          busy={busy}
          onCommit={() => void commit()}
          onChange={(value) => {
            setFormat(value)
            setFailure(null)
            checkFormat(value)
          }}
        />
        {formatMessage !== null && (
          <p
            id={formatHintId}
            aria-live="polite"
            className={formatCheck === 'ok' ? 'settings-hint' : 'settings-hint error'}
          >
            {formatMessage}
          </p>
        )}
      </div>
      <div className="settings-field">
        <label htmlFor={pathId}>{t('settings.github_posts_path')}</label>
        <FieldInput
          id={pathId}
          describedBy={pathCheck === null ? undefined : pathHintId}
          value={postsPath}
          invalid={pathInvalid}
          busy={busy}
          onCommit={() => void commit()}
          onChange={(value) => {
            setPostsPath(value)
            setFailure(null)
            checkPostsPath(value)
          }}
        />
        {pathCheck !== null && (
          <p
            id={pathHintId}
            aria-live="polite"
            className={pathCheck === 'ok' ? 'settings-hint' : 'settings-hint error'}
          >
            {pathCheck === 'ok' ? (
              <>
                <span>{t('settings.posts_dir_posts', { path: `${postsPath}/{slug}.md` })}</span>
                <span>
                  {t('settings.posts_dir_assets', {
                    path: `${githubAssetsPath(postsPath)}/{slug}/`,
                  })}
                </span>
              </>
            ) : pathInvalid ? (
              t('settings.github_posts_path_invalid')
            ) : (
              t('settings.github_check_failed')
            )}
          </p>
        )}
      </div>
      {failure !== null && (
        <p role="alert" className="settings-failure">
          {failure}
        </p>
      )}
    </>
  )
}

// GitHubの節の3つの入力欄の共通の部分
function FieldInput({
  id,
  describedBy,
  value,
  invalid,
  busy,
  placeholder,
  onCommit,
  onChange,
}: {
  id: string
  describedBy?: string
  value: string
  invalid: boolean
  busy: boolean
  placeholder?: string
  onCommit: () => void
  onChange: (value: string) => void
}) {
  return (
    <input
      id={id}
      type="text"
      className={invalid ? 'settings-input error' : 'settings-input'}
      value={value}
      placeholder={placeholder}
      disabled={busy}
      aria-invalid={invalid}
      aria-describedby={describedBy}
      onKeyDown={(e) => {
        if (e.key === 'Enter') onCommit()
      }}
      onBlur={onCommit}
      onChange={(e) => onChange(e.target.value)}
      autoCapitalize="none"
      autoCorrect="off"
      spellCheck={false}
    />
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
        console.error('Failed to load the license list', e)
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

  if (licenses === 'failed')
    return <p className="editor-message">{t('settings.licenses_load_failed')}</p>
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
          aria-label={t('settings.licenses_filter')}
          placeholder={t('settings.licenses_filter')}
          onChange={(e) => setFilter(e.target.value)}
        />
      </label>
      <p className="settings-count settings-note">
        {t('settings.licenses_count', { count: rows.length })}
      </p>
      <div className="settings-rows">
        {rows.map((license) => {
          const key = `${license.name}@${license.version}`
          const title = duplicated.has(license.name)
            ? `${license.name} ${license.version}`
            : license.name
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
                  name={
                    narrow ? 'chevron_right' : open ? 'keyboard_arrow_up' : 'keyboard_arrow_down'
                  }
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
