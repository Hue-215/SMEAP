use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::token;
use super::GithubError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
    pub default_branch: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GithubSettings {
    pub repo: Option<RepoRef>,
    pub base_branch: Option<String>,
    pub branch_format: String,
    pub posts_path: String,
}

impl Default for GithubSettings {
    fn default() -> Self {
        Self {
            repo: None,
            base_branch: None,
            branch_format: "blog/{slug}".to_string(),
            posts_path: "src/content/blog".to_string(),
        }
    }
}

impl GithubSettings {
    // 検証を通らない項目だけ既定値に戻す
    pub fn sanitized(self) -> Self {
        let repo = self.repo.filter(|repo| {
            validate_repo_part(&repo.owner).is_ok()
                && validate_repo_part(&repo.name).is_ok()
                && check_ref_format(&repo.default_branch)
        });
        let base_branch = self.base_branch.filter(|branch| check_ref_format(branch));
        let branch_format = if validate_branch_format(&self.branch_format).is_ok() {
            self.branch_format
        } else {
            Self::default().branch_format
        };
        let posts_path = if validate_posts_path(&self.posts_path).is_ok() {
            self.posts_path
        } else {
            Self::default().posts_path
        };
        Self {
            repo,
            base_branch,
            branch_format,
            posts_path,
        }
    }
}

pub fn validate_branch_format(format: &str) -> Result<(), GithubError> {
    // {slug}をちょうど1つ含む
    if format.matches("{slug}").count() == 1
        && check_ref_format(&format.replace("{slug}", "first-post"))
    {
        Ok(())
    } else {
        Err(GithubError::InvalidBranchFormat)
    }
}

pub fn validate_posts_path(path: &str) -> Result<(), GithubError> {
    // /区切りの相対パスで、2つ上があること(画像は2つ上のassetsに入る)。
    // .で始まる部分・\・ASCIIの印字可能文字以外は断つ
    let ok = !path.is_empty()
        && path.split('/').count() >= 2
        && path.split('/').all(|component| {
            !component.is_empty()
                && !component.starts_with('.')
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() && byte != b'\\')
        });
    if ok {
        Ok(())
    } else {
        Err(GithubError::InvalidPostsPath)
    }
}

pub fn validate_repo_part(part: &str) -> Result<(), GithubError> {
    // 手元のパスとURLの両方に使う。.と..と空も断つ
    let ok = !part.is_empty()
        && part != "."
        && part != ".."
        && part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(GithubError::InvalidRepository)
    }
}

// Base branchとRepositoryのdefault branch
pub fn validate_branch_name(name: &str) -> Result<(), GithubError> {
    if check_ref_format(name) {
        Ok(())
    } else {
        Err(GithubError::InvalidBranchName)
    }
}

// git check-ref-format --branchと同じ決まり
fn check_ref_format(name: &str) -> bool {
    // -で始まる名前(オプションと紛れる)と、@だけの名前と、.で終わる名前
    if name.is_empty() || name == "@" || name.starts_with('-') || name.ends_with('.') {
        return false;
    }
    // 空の部分(先頭・末尾・連続の/)と、.で始まる・.lockで終わる部分
    name.split('/').all(|component| {
        !component.is_empty() && !component.starts_with('.') && !component.ends_with(".lock")
    }) && !name.contains("..")
        && !name.contains("@{")
        && !name.chars().any(|c| {
            c.is_ascii_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
}

// 画像の置き場所は2つ上のassets
// "src/content/blog" -> "src/assets"
pub fn assets_path(posts_path: &str) -> String {
    let mut components: Vec<&str> = posts_path.split('/').collect();
    components.truncate(components.len().saturating_sub(2));
    components.push("assets");
    components.join("/")
}

pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join("github/token.json")
}

// filesの下に記事を置き、同じgithub/{owner}/{repo}/の下にstate.json・synced・backupを置く(#64)
pub fn github_posts_dir(data_dir: &Path, repo: &RepoRef, posts_path: &str) -> PathBuf {
    data_dir
        .join("github")
        .join(&repo.owner)
        .join(&repo.name)
        .join("files")
        .join(posts_path)
}

pub fn choose_posts_dir(
    local: PathBuf,
    data_dir: &Path,
    settings: &GithubSettings,
    logged_in: bool,
) -> PathBuf {
    match (logged_in, &settings.repo) {
        (true, Some(repo)) => github_posts_dir(data_dir, repo, &settings.posts_path),
        _ => local,
    }
}

pub fn linked(data_dir: &Path, settings: &GithubSettings) -> bool {
    settings.repo.is_some()
        && token::read(&token_path(data_dir)).is_ok_and(|tokens| tokens.is_some())
}

// 起動時。トークンを読んでchoose_posts_dirする。読めなければログインしていない扱いにし、そのエラーも返す
pub fn startup_posts_dir(
    local: PathBuf,
    data_dir: &Path,
    settings: &GithubSettings,
) -> (PathBuf, Option<GithubError>) {
    // Repositoryを選んでいなければ向き先は替わらないので、資格情報の保存先に問い合わせない
    if settings.repo.is_none() {
        return (local, None);
    }
    match token::read(&token_path(data_dir)) {
        Ok(Some(_)) => (choose_posts_dir(local, data_dir, settings, true), None),
        Ok(None) => (local, None),
        Err(error) => (local, Some(error)),
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::github::token;

    fn repo() -> RepoRef {
        RepoRef {
            owner: "owner".to_string(),
            name: "blog".to_string(),
            default_branch: "main".to_string(),
        }
    }

    fn settings_with_repo() -> GithubSettings {
        GithubSettings {
            repo: Some(repo()),
            ..GithubSettings::default()
        }
    }

    fn token_set() -> token::TokenSet {
        token::TokenSet {
            access_token: "ACCESS".to_string(),
            expires_at: 100,
            refresh_token: "REFRESH".to_string(),
            refresh_token_expires_at: 200,
        }
    }

    #[test]
    fn GitHubの設定の既定値() {
        assert_eq!(
            GithubSettings::default(),
            GithubSettings {
                repo: None,
                base_branch: None,
                branch_format: "blog/{slug}".to_string(),
                posts_path: "src/content/blog".to_string(),
            }
        );
    }

    #[test]
    fn branch名の形式は決まった形を通す() {
        for ok in [
            "blog/{slug}",
            "{slug}",
            "post/{slug}/draft",
            "feature/blog/{slug}",
            "blog-{slug}",
        ] {
            assert!(validate_branch_format(ok).is_ok(), "{ok} は通すはず");
        }
    }

    #[test]
    fn branch名の形式はgitのブランチ名に使えない形を断る() {
        for ng in [
            // {slug}の数
            "blog/",
            "{slug}/{slug}",
            // スラッシュの位置
            "/blog/{slug}",
            "blog/{slug}/",
            "blog//{slug}",
            // ドットで始まる・終わる部分と連続のドット
            "blog/.x/{slug}",
            "blog/{slug}.lock",
            "blog.lock/{slug}",
            "blog..x/{slug}",
            "blog/{slug}.",
            // 使えない文字
            "bl og/{slug}",
            "blog~/{slug}",
            "blog^{slug}",
            "blog:{slug}",
            "blog?{slug}",
            "blog*{slug}",
            "blog[{slug}",
            "blog\\{slug}",
            "blog@{/{slug}",
            "blog/{slug}\u{1}",
        ] {
            assert!(
                matches!(
                    validate_branch_format(ng),
                    Err(GithubError::InvalidBranchFormat)
                ),
                "{ng} は断るはず"
            );
        }
        // -で始まる名前は、コマンドのオプションと誤認されるので断つ
        assert!(validate_branch_format("-{slug}").is_err());
    }

    #[test]
    fn 記事のディレクトリは決まった形を通す() {
        for ok in ["src/content/blog", "src/content/posts", "content/blog"] {
            assert!(validate_posts_path(ok).is_ok(), "{ok} は通すはず");
        }
    }

    #[test]
    fn 記事のディレクトリは使えない形を断る() {
        for ng in [
            "",
            "blog",
            "/src/content/blog",
            "src/content/blog/",
            "src//blog",
            "src/./blog",
            "src/../blog",
            "src\\content\\blog",
            ".git/content/blog",
            "src/.git/blog",
            "src/content/blo\u{1}g",
        ] {
            assert!(
                matches!(validate_posts_path(ng), Err(GithubError::InvalidPostsPath)),
                "{ng} は断るはず"
            );
        }
    }

    #[test]
    fn 画像の置き場所は2つ上のassets() {
        assert_eq!(assets_path("src/content/blog"), "src/assets");
        assert_eq!(assets_path("content/blog"), "assets");
    }

    #[test]
    fn branch名はgitで使える名前を通す() {
        for ok in ["main", "develop", "release/1.0", "v1+build", "日本語"] {
            assert!(validate_branch_name(ok).is_ok(), "{ok} は通すはず");
        }
        for ng in ["", "-main", "a..b", "a b", "a:b", "main.lock", "@"] {
            assert!(validate_branch_name(ng).is_err(), "{ng} は断るはず");
        }
    }

    #[test]
    fn repositoryのownerとnameは使える文字だけ通す() {
        for ok in ["owner", "my-blog", "blog_dev", "a.b"] {
            assert!(validate_repo_part(ok).is_ok(), "{ok} は通すはず");
        }
        for ng in ["", ".", "..", "a/b", "a b", "a^b"] {
            assert!(
                matches!(validate_repo_part(ng), Err(GithubError::InvalidRepository)),
                "{ng} は断るはず"
            );
        }
    }

    #[test]
    fn token_jsonはデータ領域のgithubの下() {
        assert_eq!(
            token_path(Path::new("/data")),
            PathBuf::from("/data/github/token.json")
        );
    }

    #[test]
    fn GitHub用の置き場所はデータ領域のgithubの下() {
        assert_eq!(
            github_posts_dir(Path::new("/data"), &repo(), "src/content/blog"),
            PathBuf::from("/data/github/owner/blog/files/src/content/blog")
        );
    }

    #[test]
    fn 保存先の向き先はログインとrepositoryの選択で決める() {
        let local = PathBuf::from("/local/src/content/blog");
        let data_dir = Path::new("/data");
        // ログインしていない
        assert_eq!(
            choose_posts_dir(local.clone(), data_dir, &settings_with_repo(), false),
            local
        );
        // ログインしていてRepositoryを選んでいない
        assert_eq!(
            choose_posts_dir(local.clone(), data_dir, &GithubSettings::default(), true),
            local
        );
        // ログインしていてRepositoryを選んでいる
        assert_eq!(
            choose_posts_dir(local, data_dir, &settings_with_repo(), true),
            github_posts_dir(data_dir, &repo(), "src/content/blog")
        );
    }

    #[test]
    fn 起動時はtoken_jsonがあればGitHub用の置き場所で始める() {
        let tmp = TempDir::new().unwrap();
        let local = tmp.path().join("local/src/content/blog");
        token::write(&token_path(tmp.path()), &token_set()).unwrap();
        let (posts_dir, error) = startup_posts_dir(local, tmp.path(), &settings_with_repo());
        assert_eq!(
            posts_dir,
            github_posts_dir(tmp.path(), &repo(), "src/content/blog")
        );
        assert!(error.is_none());
    }

    #[test]
    fn 起動時はtoken_jsonが無ければ今までの保存先で始める() {
        let tmp = TempDir::new().unwrap();
        let local = tmp.path().join("local/src/content/blog");
        let (posts_dir, error) =
            startup_posts_dir(local.clone(), tmp.path(), &settings_with_repo());
        assert_eq!(posts_dir, local);
        assert!(error.is_none());
    }

    #[test]
    fn 起動時はtoken_jsonが壊れていても今までの保存先で始める() {
        let tmp = TempDir::new().unwrap();
        let local = tmp.path().join("local/src/content/blog");
        let path = token_path(tmp.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "@").unwrap();
        let (posts_dir, error) =
            startup_posts_dir(local.clone(), tmp.path(), &settings_with_repo());
        assert_eq!(posts_dir, local);
        // 画面で知らせるために返す
        assert!(error.is_some());
    }

    #[test]
    fn linkedはrepositoryとtoken_jsonの両方あるときtrue() {
        let tmp = TempDir::new().unwrap();
        // token.jsonが無ければfalse
        assert!(!linked(tmp.path(), &settings_with_repo()));
        token::write(&token_path(tmp.path()), &token_set()).unwrap();
        // repositoryが無ければfalse
        assert!(!linked(tmp.path(), &GithubSettings::default()));
        assert!(linked(tmp.path(), &settings_with_repo()));
        // token.jsonが壊れていればfalse
        fs::write(token_path(tmp.path()), "@").unwrap();
        assert!(!linked(tmp.path(), &settings_with_repo()));
    }
}
