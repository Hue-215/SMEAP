import { Channel, invoke } from '@tauri-apps/api/core'
import type { GithubArticleState, GithubArticleStates, GithubSkipped } from './githubList'
import { customSchemeOrigin } from './platform'

// Rust側のエラー。種類によってpath・reasonを持つ
export interface StoreError {
  kind: string
  path?: string
  reason?: string
}

export interface ArticleSummary {
  slug: string
  title: string
  pubDate: string | null
  warning: boolean
}

// slugの分かる、読めない記事
export interface BrokenArticle {
  slug: string
  path: string
  reason: string
}

// 記事に結び付かない失敗
export interface UnreadableEntry {
  path: string
  reason: string
}

export interface ArticleList {
  articles: ArticleSummary[]
  broken: BrokenArticle[]
  unreadable: UnreadableEntry[]
}

// 読み取りの警告。keyは関係する既知の項目、keptは元の行をextraに残したとき
export interface Warning {
  key: string | null
  message: string
  kept: boolean
}

// sharedがtrueならassetsの直下の画像
export interface HeroImage {
  // ファイル名だけ。本文の画像のような参照ではない
  name: string
  shared: boolean
}

export interface Article {
  slug: string
  title: string | null
  description: string | null
  pubDate: string | null
  updatedDate: string | null
  heroImage: HeroImage | null
  body: string
  warnings: Warning[]
}

// heroImageはimportHeroImageが書き換えるので、保存では渡さない。
// 日付は手で変えたものだけを送る。送らなければ保存されている値のまま。updatedDateのnullは外す
export type EditedFields = Omit<
  Article,
  'slug' | 'warnings' | 'heroImage' | 'pubDate' | 'updatedDate'
> & {
  pubDate?: string
  updatedDate?: string | null
}

// Rust側のSavedDates(src-tauri/src/store.rs)とそろえる
export interface SavedDates {
  pubDate: string | null
  updatedDate: string | null
}

// Rust側のFrontmatterSettings(src-tauri/src/settings.rs)とそろえる
export interface FrontmatterSettings {
  heroImage: boolean
  updatedDate: boolean
}

// Rust側のFrontmatterSettingsのDefaultとそろえる
export const DEFAULT_FRONTMATTER: FrontmatterSettings = { heroImage: true, updatedDate: true }

export function listArticles(): Promise<ArticleList> {
  return invoke<ArticleList>('list_articles')
}

export function readArticle(slug: string): Promise<Article> {
  return invoke<Article>('read_article', { slug })
}

export function saveArticle(slug: string, fields: EditedFields): Promise<SavedDates> {
  return invoke<SavedDates>('save_article', { slug, fields })
}

export function createArticle(slug: string): Promise<Article> {
  return invoke<Article>('create_article', { slug })
}

export function renameArticle(oldSlug: string, newSlug: string): Promise<Article> {
  return invoke<Article>('rename_article', { old: oldSlug, new: newSlug })
}

export function deleteArticle(slug: string): Promise<void> {
  return invoke('delete_article', { slug })
}

// 画像は生のバイト列で送り、slugはヘッダーで渡す(src-tauri/src/commands.rs)。Androidでは数値の配列のJSONになる
export function importBodyImage(slug: string, data: Uint8Array): Promise<string> {
  return invoke<string>('import_body_image', data, { headers: { slug } })
}

export function importHeroImage(slug: string, data: Uint8Array): Promise<string> {
  return invoke<string>('import_hero_image', data, { headers: { slug } })
}

export function importBodyImageFromPath(slug: string, path: string): Promise<string> {
  return invoke<string>('import_body_image_from_path', { slug, path })
}

export function removeHeroImage(slug: string): Promise<void> {
  return invoke('remove_hero_image', { slug })
}

// クリップボードに画像が無ければnull。Linuxだけで読める
export function importClipboardImage(slug: string): Promise<string | null> {
  return invoke<string | null>('import_clipboard_image', { slug })
}

// Rust側のPostsLocation(src-tauri/src/settings.rs)。画像の置き場所は`{slug}`を付ける前の根
export interface PostsLocation {
  postsDir: string
  assetsDir: string
}

// Androidはzipの中の`src/content/`の下の名前
export type PostsDir = ({ kind: 'desktop' } & PostsLocation) | { kind: 'android'; name: string }

export class UnsavedArticleError extends Error {}

// デスクトップだけ呼べる
export function getPostsDir(): Promise<PostsLocation> {
  return invoke<PostsLocation>('get_posts_dir')
}

// 選ばせて変える。キャンセルされたらnull。デスクトップだけ呼べる
export function pickPostsDir(): Promise<PostsLocation | null> {
  return invoke<PostsLocation | null>('pick_posts_dir')
}

// Androidだけ呼べる。zipの中の`src/content/`の下の名前
export function getPostsDirName(): Promise<string> {
  return invoke<string>('get_posts_dir_name')
}

// Androidだけ呼べる
export function setPostsDirName(name: string): Promise<void> {
  return invoke('set_posts_dir_name', { name })
}

export function getFrontmatterSettings(): Promise<FrontmatterSettings> {
  return invoke<FrontmatterSettings>('get_frontmatter_settings')
}

export function setFrontmatterSettings(frontmatter: FrontmatterSettings): Promise<void> {
  return invoke('set_frontmatter_settings', { frontmatter })
}

export function getRemoveUnusedImages(): Promise<boolean> {
  return invoke<boolean>('get_remove_unused_images')
}

export function setRemoveUnusedImages(enabled: boolean): Promise<void> {
  return invoke('set_remove_unused_images', { enabled })
}

// Rust側のExportOutcome(src-tauri/src/export.rs)とそろえる
export type ExportOutcome =
  { kind: 'saved' } | { kind: 'cancelled' } | { kind: 'leftIncomplete'; reason: string }

export function exportZip(slug: string): Promise<ExportOutcome> {
  return invoke<ExportOutcome>('export_zip', { slug })
}

// 入力中の案内に使う。Rust側のvalidate_slug(src-tauri/src/store.rs)とそろえる
export function isValidSlug(slug: string): boolean {
  return /^[a-z0-9-]+$/.test(slug)
}

const articleImageOrigin = customSchemeOrigin('article-image', navigator.userAgent)

// Rust側の独自プロトコル(src-tauri/src/image_protocol.rs)のURL。
// referenceは本文に書いた参照のまま渡し、記事の画像かどうかはRust側が判断する。
export function articleImageUrl(slug: string, reference: string): string {
  return `${articleImageOrigin}/${encodeURIComponent(slug)}/${encodeURIComponent(reference)}`
}

// 記事ファイルから画像を指すときの相対パス。Rust側のSHARED_ASSETS_REFとassets_ref(src-tauri/src/store.rs)とそろえる
const sharedAssetsRef = '../../assets/'
const assetsRef = (slug: string) => `${sharedAssetsRef}${slug}/`

// 差し替えても同じ名前のことがあるので、versionを変えて読み直させる。独自プロトコルはクエリを見ない
export function heroImageUrl(slug: string, hero: HeroImage, version: number): string {
  const reference = (hero.shared ? sharedAssetsRef : assetsRef(slug)) + hero.name
  return `${articleImageUrl(slug, reference)}?v=${version}`
}

// GitHub連携。Rust側はsrc-tauri/src/github/の下

// Rust側のRepoRef(src-tauri/src/github/config.rs)とそろえる
export interface GithubRepoRef {
  owner: string
  name: string
  defaultBranch: string
}

export function repoFullName(repo: GithubRepoRef): string {
  return `${repo.owner}/${repo.name}`
}

// Rust側のGithubSettings(src-tauri/src/github/config.rs)とそろえる
export interface GithubSettings {
  repo: GithubRepoRef | null
  baseBranch: string | null
  branchFormat: string
  postsPath: string
}

// Rust側のGithubStatus(src-tauri/src/github/commands.rs)とそろえる
export interface GithubStatus {
  loggedIn: boolean
  // 通信できなければ、ログインしていてもnull
  login: string | null
  settings: GithubSettings
  tokenUnreadable: boolean
  // ログインし直しが必要になった。画面が記事を保存してからgithubLogoutで保存先を戻す
  loginExpired: boolean
}

// Rust側のStartLogin(src-tauri/src/github/commands.rs)とそろえる
export interface GithubStartLogin {
  userCode: string
  verificationUri: string
  expiresIn: number
}

// Rust側のWaitLogin(src-tauri/src/github/commands.rs)とそろえる
export interface GithubWaitLogin {
  outcome: 'loggedIn' | 'expired' | 'denied' | 'cancelled'
  login: string | null
}

// Rust側のGithubError(src-tauri/src/github/mod.rs)とそろえる。画面はkindで文言を選ぶ
export type GithubError =
  | { kind: 'loginRequired' }
  | { kind: 'rateLimited'; retryAt: number | null }
  | { kind: 'status'; status: number }
  | { kind: 'redirect' }
  | { kind: 'urlNotAllowed' }
  | { kind: 'network'; reason: string }
  | { kind: 'invalidResponse'; reason: string }
  | { kind: 'invalidBranchFormat' }
  | { kind: 'invalidBranchName' }
  | { kind: 'invalidPostsPath' }
  | { kind: 'invalidRepository' }
  | { kind: 'io'; path: string; reason: string }
  | { kind: 'baseBranchNotFound' }
  | { kind: 'invalidLayout'; path: string }
  | { kind: 'stateUnreadable' }
  | { kind: 'syncBusy' }
  | { kind: 'settingsChanged' }
  | { kind: 'notLinked' }
  | { kind: 'nothingToCommit' }
  | { kind: 'conflict'; head: string; md: boolean; images: string[] }
  | { kind: 'branchExists'; branch: string }
  | { kind: 'branchGone' }
  | { kind: 'pushRetryExceeded' }
  | { kind: 'articleNotFound' }
  | { kind: 'branchMoved' }
  | { kind: 'slugReserved'; slug: string }
  | { kind: 'slugInUse'; slug: string }
  | { kind: 'notPublished' }
  | { kind: 'outOfSync' }
  | { kind: 'conflicted' }

export type GithubDeleting = 'delete' | 'unpublish'

export function githubStatus(): Promise<GithubStatus> {
  return invoke<GithubStatus>('github_status')
}

export function githubStartLogin(): Promise<GithubStartLogin> {
  return invoke<GithubStartLogin>('github_start_login')
}

// 承認・期限切れ・拒否・取り消しまで返らない。やめるにはgithubCancelLoginを呼ぶ
export function githubWaitLogin(): Promise<GithubWaitLogin> {
  return invoke<GithubWaitLogin>('github_wait_login')
}

export function githubCancelLogin(): Promise<void> {
  return invoke('github_cancel_login')
}

export function githubLogout(): Promise<void> {
  return invoke('github_logout')
}

export function githubListRepositories(): Promise<GithubRepoRef[]> {
  return invoke<GithubRepoRef[]>('github_list_repositories')
}

export function githubSelectRepository(repo: GithubRepoRef): Promise<void> {
  return invoke('github_select_repository', { repo })
}

export function githubUpdateSettings(
  baseBranch: string | null,
  branchFormat: string,
  postsPath: string,
): Promise<void> {
  return invoke('github_update_settings', { baseBranch, branchFormat, postsPath })
}

// 入力中の案内に使う。Rust側のvalidate_branch_format(src-tauri/src/github/config.rs)とそろえる
export function validateBranchFormat(format: string): Promise<void> {
  return invoke('validate_branch_format', { format })
}

export function validatePostsPath(path: string): Promise<void> {
  return invoke('validate_posts_path', { path })
}

// deviceは認可のページ、installはAppのインストールのページ
export function openGithubPage(page: 'device' | 'install'): Promise<void> {
  return invoke('open_github_page', { page })
}

// Rust側のSyncResult(src-tauri/src/github/sync.rs)とそろえる
export interface GithubSyncResult {
  fetchedAt: number
  replaced: string[]
  removed: string[]
  skipped: GithubSkipped[]
  deferred: string[]
}

// protectの記事は置き換えも削除もせず、そうなるはずだったらdeferredに入れて返す
export function githubSync(protect: string | null): Promise<GithubSyncResult> {
  return invoke<GithubSyncResult>('github_sync', { protect })
}

// 取れなかった画像の件数。連携していなければ0
export function githubFetchImages(slug: string): Promise<number> {
  return invoke<number>('github_fetch_images', { slug })
}

// Rust側のCommitKind・CommitPreview(src-tauri/src/github/push.rs)とそろえる
export type CommitKind = 'add' | 'fix' | 'update'

export interface CommitPreview {
  title: string
  mdName: string
  // slugを変えた記事だけ入る
  oldMdName: string | null
  oldBranch: string | null
  added: number
  removed: number
  imagesAdded: number
  imagesRemoved: number
  kind: CommitKind
}

// Rust側のPushResult(src-tauri/src/github/push.rs)とそろえる
export interface PushResult {
  branch: string
  commit: string
  // 消せなかった古いbranch
  keptBranch: string | null
}

// Rust側のPushProgress(src-tauri/src/github/push.rs)とそろえる
export type PushProgress =
  | { step: 'checking' }
  | { step: 'uploading'; current: number; total: number }
  | { step: 'committing' }
  | { step: 'pushing' }

export function githubCommitPreview(slug: string): Promise<CommitPreview> {
  return invoke<CommitPreview>('github_commit_preview', { slug })
}

// overwriteは、Conflictで「Commit」を選んだときのリモートの先頭
export function githubCommitPush(
  slug: string,
  message: string,
  overwrite: string | null,
  onProgress: (progress: PushProgress) => void,
): Promise<PushResult> {
  const channel = new Channel<PushProgress>()
  channel.onmessage = onProgress
  return invoke<PushResult>('github_commit_push', {
    slug,
    message,
    overwrite,
    onProgress: channel,
  })
}

export function githubPull(slug: string, branch: string | null): Promise<void> {
  return invoke('github_pull', { slug, branch })
}

// 手元の変更を捨て、公開済みの内容に戻す
export function githubDiscardEdit(slug: string): Promise<void> {
  return invoke('github_discard_edit', { slug })
}

// 公開済みの記事を削除・非公開にする。手元のファイルは替えない
export function githubRemovePublished(
  slug: string,
  kind: GithubDeleting,
  onProgress: (progress: PushProgress) => void,
): Promise<void> {
  const channel = new Channel<PushProgress>()
  channel.onmessage = onProgress
  return invoke('github_remove_published', { slug, kind, onProgress: channel })
}

// 連携していなければnull
export function githubArticleStates(): Promise<GithubArticleStates | null> {
  return invoke<GithubArticleStates | null>('github_article_states')
}

export function githubArticleState(slug: string): Promise<GithubArticleState | null> {
  return invoke<GithubArticleState | null>('github_article_state', { slug })
}

// 画像は記事のディレクトリの2つ上のassetsに入る。Rust側のassets_path(src-tauri/src/github/config.rs)とそろえる
export function githubAssetsPath(postsPath: string): string {
  const components = postsPath.split('/')
  return [...components.slice(0, -2), 'assets'].join('/')
}
