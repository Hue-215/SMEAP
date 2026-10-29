import { useEffect, useId, useRef, useState } from 'react'
import { githubListRepositories, openGithubPage, repoFullName, type GithubRepoRef } from './api'
import Dialog from './Dialog'
import Icon from './Icon'
import { githubErrorText } from './githubError'
import { t } from './i18n'

// ownerとnameだけで比べる(defaultBranchは問わない)
const sameRepo = (a: GithubRepoRef, b: GithubRepoRef) => a.owner === b.owner && a.name === b.name

interface GithubRepoDialogProps {
  // 初めに選んでおくrepository
  current: GithubRepoRef | null
  // 選ぶ。開いている記事の保存と向き先の替えはAppが行う。失敗の文言を返す
  onSelect: (repo: GithubRepoRef) => Promise<string | null>
  onCancel: () => void
}

// トークンで触れられるrepositoryの一覧から選ぶダイアログ
export default function GithubRepoDialog({ current, onSelect, onCancel }: GithubRepoDialogProps) {
  // 一覧。nullは読んでいる間、'failed'は読めなかった
  const [repos, setRepos] = useState<GithubRepoRef[] | 'failed' | null>(null)
  const [failure, setFailure] = useState<string | null>(null)
  const [picked, setPicked] = useState<GithubRepoRef | null>(current)
  const [busy, setBusy] = useState(false)
  // 選んでいる最中にフォーカスが戻っても読み直さない。effectを付け直さないようrefで見る
  const busyRef = useRef(false)
  // 実行中の読み込みより後の応答だけを出す
  const request = useRef(0)
  const headingId = useId()

  useEffect(() => {
    const load = () => {
      const id = ++request.current
      setRepos(null)
      setFailure(null)
      githubListRepositories()
        .then((list) => {
          if (id !== request.current) return
          setRepos(list)
        })
        .catch((e) => {
          if (id !== request.current) return
          console.error('Failed to list the repositories', e)
          setRepos('failed')
          setFailure(githubErrorText(e))
        })
    }
    load()
    // Appのインストールのページから戻ったときに読み直す
    const onFocus = () => {
      if (!busyRef.current) load()
    }
    window.addEventListener('focus', onFocus)
    return () => window.removeEventListener('focus', onFocus)
  }, [])

  const openInstall = () => {
    openGithubPage('install').catch((e) => {
      console.error('Failed to open the GitHub page', e)
      setFailure(t('common.open_browser_failed'))
    })
  }

  // 処理中に閉じると、選んだ後の操作が残る
  const cancel = () => {
    if (!busy) onCancel()
  }

  const pick = async () => {
    if (busy || selected === null) return
    busyRef.current = true
    setBusy(true)
    setFailure(null)
    let text: string | null = null
    try {
      text = await onSelect(selected)
    } catch (e) {
      console.error('Failed to select the repository', e)
      text = t('common.change_failed')
    }
    busyRef.current = false
    setBusy(false)
    // 選べたら閉じるのは親が行う
    if (text !== null) setFailure(text)
  }

  const list = repos === null || repos === 'failed' ? null : repos
  const hasRepos = list !== null && list.length > 0
  const emptyList = list !== null && list.length === 0
  // 一覧の中で選んでいるrepository。今のrepositoryが一覧に無い(Appを外した)ときは、どれも選んでいない扱いにする
  const selected =
    picked === null || list === null ? null : (list.find((repo) => sameRepo(repo, picked)) ?? null)

  return (
    <Dialog labelledBy={headingId} onCancel={cancel}>
      <div className="dialog-form">
        <h2 id={headingId} className="dialog-title">
          {t('github_repo.title')}
        </h2>
        {hasRepos && (
          <div role="radiogroup" aria-labelledby={headingId} className="github-repo-list">
            {list.map((repo) => {
              const name = repoFullName(repo)
              return (
                <label key={name} className="github-repo-option">
                  <input
                    type="radio"
                    name="github-repo"
                    checked={picked !== null && sameRepo(repo, picked)}
                    onChange={() => setPicked(repo)}
                  />
                  <span>{name}</span>
                </label>
              )
            })}
          </div>
        )}
        {(failure !== null || emptyList) && (
          <div className="dialog-body">
            {emptyList && <p>{t('github_repo.empty')}</p>}
            {failure !== null && (
              <p role="alert" className="confirm-dialog-error">
                {failure}
              </p>
            )}
          </div>
        )}
        <div className="dialog-buttons">
          <button type="button" className="left github-repo-add" onClick={openInstall}>
            <Icon name="open_in_new" />
            {t('github_repo.add')}
          </button>
          <button type="button" onClick={cancel} disabled={busy}>
            {t('common.cancel')}
          </button>
          {hasRepos && (
            <button
              type="button"
              className="primary"
              onClick={() => void pick()}
              disabled={busy || selected === null}
            >
              {t('github_repo.pick')}
            </button>
          )}
        </div>
      </div>
    </Dialog>
  )
}
