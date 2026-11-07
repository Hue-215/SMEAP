//! 記事の保存先の設定と、それを変えるコマンド。app_config_dir(Windowsはapp_local_data_dir)の下のsettings.jsonに持つ。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::github::config::GithubSettings;
use crate::github::GithubError;
#[cfg(target_os = "android")]
use crate::store::Store;
#[cfg(not(target_os = "android"))]
use crate::store::{self, Store};
use crate::store::{write_temp, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FrontmatterSettings {
    pub hero_image: bool,
    pub updated_date: bool,
}

impl Default for FrontmatterSettings {
    fn default() -> Self {
        Self {
            hero_image: true,
            updated_date: true,
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[cfg(not(target_os = "android"))]
    pub posts_dir: Option<PathBuf>,
    // zipの中の`src/content/`の下の名前。端末の中の置き場所は変えない
    #[cfg(target_os = "android")]
    posts_dir_name: Option<String>,
    #[serde(default)]
    pub github: GithubSettings,
    #[serde(default)]
    pub frontmatter: FrontmatterSettings,
    #[serde(default = "default_remove_unused_images")]
    pub remove_unused_images: bool,
}

fn default_remove_unused_images() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            #[cfg(not(target_os = "android"))]
            posts_dir: None,
            #[cfg(target_os = "android")]
            posts_dir_name: None,
            github: GithubSettings::default(),
            frontmatter: FrontmatterSettings::default(),
            remove_unused_images: default_remove_unused_images(),
        }
    }
}

// デスクトップの設定はStoreが持つので、Androidだけ
#[cfg(target_os = "android")]
pub struct SettingsState(Mutex<Settings>);

#[cfg(target_os = "android")]
impl SettingsState {
    pub fn new(settings: Settings) -> Self {
        Self(Mutex::new(settings))
    }

    fn lock(&self) -> MutexGuard<'_, Settings> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// Windowsのapp_config_dirはRoamingで、ほかの端末へ同期されることがある。記事の保存先は端末ごとの絶対パスなので置かない
pub(crate) fn file_path(app: &AppHandle) -> Result<PathBuf, StoreError> {
    #[cfg(windows)]
    let (dir, name) = (app.path().app_local_data_dir(), "app_local_data_dir");
    #[cfg(not(windows))]
    let (dir, name) = (app.path().app_config_dir(), "app_config_dir");
    dir.map(|dir| dir.join("settings.json"))
        .map_err(|error| StoreError::io(Path::new(name), error))
}

// Windowsのapp_data_dirはRoamingなので、バックアップや取得した記事・トークンを置かない。ほかのOSでは同じ場所
pub(crate) fn data_dir(app: &AppHandle) -> Result<PathBuf, StoreError> {
    app.path()
        .app_local_data_dir()
        .map_err(|error| StoreError::io(Path::new("app_local_data_dir"), error))
}

// 読めなくても起動は止めず、既定の設定で始める。ただし画像の片付けは、無効にしていた人の画像を移さないよう止める
pub fn load(app: &AppHandle) -> Settings {
    let settings = match file_path(app) {
        Ok(file) => load_from(&file),
        Err(error) => fallback(error),
    };
    // 検証を通らない名前が残っていたら、無かったものとして扱う(ほかの項目は残す)
    #[cfg(target_os = "android")]
    let settings = {
        let mut settings = settings;
        if let Some(name) = &settings.posts_dir_name {
            if crate::store::validate_posts_dir_name(name).is_err() {
                eprintln!("Invalid directory name in settings: {name}");
                settings.posts_dir_name = None;
            }
        }
        settings
    };
    settings
}

fn load_from(file: &Path) -> Settings {
    read_from(file).unwrap_or_else(fallback)
}

fn fallback(error: StoreError) -> Settings {
    eprintln!("Failed to read settings: {error}");
    Settings {
        remove_unused_images: false,
        ..Settings::default()
    }
}

pub(crate) fn read_from(file: &Path) -> Result<Settings, StoreError> {
    let content = match fs::read_to_string(file) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(error) => return Err(StoreError::io(file, error)),
    };
    let mut settings: Settings =
        serde_json::from_str(&content).map_err(|error| StoreError::io(file, error))?;
    let github = std::mem::take(&mut settings.github);
    settings.github = github.clone().sanitized();
    if settings.github != github {
        eprintln!("Reset invalid GitHub settings to the defaults");
    }
    Ok(settings)
}

static CHANGES: Mutex<()> = Mutex::new(());

// 設定の読み直しから保存、保存層の向き先の切り替えまでを1つずつにする。コマンドが並行して呼ばれると、
// 一方の変更が消えたり、settings.jsonと向き先が食い違ったりするため。token.jsonのロックより先に取る
pub(crate) fn lock_changes() -> MutexGuard<'static, ()> {
    CHANGES.lock().unwrap_or_else(PoisonError::into_inner)
}

fn save_to(file: &Path, settings: &Settings) -> Result<(), StoreError> {
    let dir = file
        .parent()
        .expect("settings.json should have a parent directory");
    fs::create_dir_all(dir).map_err(|error| StoreError::io(dir, error))?;
    let json = serde_json::to_string(settings).map_err(|error| StoreError::io(file, error))?;
    write_temp(dir, json.as_bytes())?
        .persist(file)
        .map_err(|error| StoreError::io(file, error.error))?;
    Ok(())
}

#[cfg(not(target_os = "android"))]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostsLocation {
    pub posts_dir: String,
    pub assets_dir: String,
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
pub fn get_posts_dir(store: State<'_, Store>) -> PostsLocation {
    PostsLocation {
        posts_dir: store.posts_dir().display().to_string(),
        assets_dir: store.assets_base().display().to_string(),
    }
}

// キャンセルされたらNone
#[cfg(not(target_os = "android"))]
#[tauri::command(async)]
pub fn pick_posts_dir(
    app: AppHandle,
    window: tauri::WebviewWindow,
    store: State<'_, Store>,
) -> Result<Option<PostsLocation>, StoreError> {
    use tauri_plugin_dialog::DialogExt;

    let current = store.posts_dir();
    let mut dialog = app.dialog().file();
    dialog = dialog.set_parent(&window);
    if current.is_dir() {
        dialog = dialog.set_directory(&current);
    }
    let Some(chosen) = dialog.blocking_pick_folder() else {
        return Ok(None);
    };
    let path = chosen
        .into_path()
        .map_err(|error| StoreError::InvalidRequest {
            reason: error.to_string(),
        })?;
    store::check_posts_dir(&path)?;
    let _changes = lock_changes();
    // ほかの項目を消さないよう、読み直してから1項目だけ替える
    let settings = update(&file_path(&app)?, |settings| {
        settings.posts_dir = Some(path.clone());
    })?;
    // GitHubと連携している間は、GitHub用の置き場所へ向いたままにする
    if !crate::github::config::linked(&data_dir(&app)?, &settings.github) {
        store.set_posts_dir(path.clone())?;
    }
    Ok(Some(PostsLocation {
        posts_dir: path.display().to_string(),
        assets_dir: store.assets_base().display().to_string(),
    }))
}

#[cfg(target_os = "android")]
pub fn posts_dir_name(settings: &SettingsState) -> String {
    settings
        .lock()
        .posts_dir_name
        .clone()
        .unwrap_or_else(|| "blog".to_string())
}

#[cfg(target_os = "android")]
#[tauri::command]
pub fn get_posts_dir_name(settings: State<'_, SettingsState>) -> String {
    posts_dir_name(&settings)
}

#[cfg(target_os = "android")]
#[tauri::command]
pub fn set_posts_dir_name(
    app: AppHandle,
    settings: State<'_, SettingsState>,
    name: String,
) -> Result<(), StoreError> {
    crate::store::validate_posts_dir_name(&name)?;
    let _changes = lock_changes();
    let mut state = settings.lock();
    // ほかの項目を消さないよう、読み直してから1項目だけ替える
    let next = update(&file_path(&app)?, |settings| {
        settings.posts_dir_name = Some(name.clone());
    })?;
    *state = next;
    Ok(())
}

pub(crate) fn update(
    file: &Path,
    change: impl FnOnce(&mut Settings),
) -> Result<Settings, StoreError> {
    let mut settings = read_from(file)?;
    change(&mut settings);
    save_to(file, &settings)?;
    Ok(settings)
}

#[tauri::command(async)]
pub fn get_frontmatter_settings(app: AppHandle) -> Result<FrontmatterSettings, StoreError> {
    Ok(read_from(&file_path(&app)?)?.frontmatter)
}

#[tauri::command(async)]
pub fn set_frontmatter_settings(
    app: AppHandle,
    frontmatter: FrontmatterSettings,
) -> Result<(), StoreError> {
    let _changes = lock_changes();
    // ほかの項目を消さないよう、読み直してから1項目だけ替える
    update(&file_path(&app)?, |settings| {
        settings.frontmatter = frontmatter;
    })?;
    Ok(())
}

// 起動時に設定を読めなかったときは無効で動いているので、ファイルではなくStoreの値を返す
#[tauri::command]
pub fn get_remove_unused_images(store: State<'_, Store>) -> bool {
    store.remove_unused_images()
}

#[tauri::command(async)]
pub fn set_remove_unused_images(
    app: AppHandle,
    store: State<'_, Store>,
    enabled: bool,
) -> Result<(), StoreError> {
    let _changes = lock_changes();
    // ほかの項目を消さないよう、読み直してから1項目だけ替える
    update(&file_path(&app)?, |settings| {
        settings.remove_unused_images = enabled;
    })?;
    store.set_remove_unused_images(enabled);
    Ok(())
}

// 向き先を確かめ、設定を保存できたら保存層の向き先を替える
pub(crate) fn switch_posts_dir(
    store: &Store,
    file: &Path,
    settings: &Settings,
    posts_dir: PathBuf,
) -> Result<(), StoreError> {
    // 向き先が使えないときは、設定を保存せず、替えもしない
    crate::store::check_posts_dir(&posts_dir)?;
    save_to(file, settings)?;
    store.set_posts_dir(posts_dir)?;
    Ok(())
}

// token.jsonを消し、向き先をlocalに戻す
pub(crate) fn logout(store: &Store, data_dir: &Path, local: PathBuf) -> Result<(), StoreError> {
    match crate::github::token::delete(&crate::github::config::token_path(data_dir)) {
        Ok(()) => {}
        Err(GithubError::Io { path, reason }) => return Err(StoreError::Io { path, reason }),
        Err(error) => return Err(StoreError::io(data_dir, error)),
    }
    store.set_posts_dir(local)?;
    Ok(())
}

#[cfg(all(test, not(target_os = "android")))]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    use crate::github::config::{github_posts_dir, token_path, RepoRef};
    use crate::github::token::{self, TokenSet};

    fn file_of(tmp: &TempDir) -> PathBuf {
        tmp.path().join("settings.json")
    }

    fn repo() -> RepoRef {
        RepoRef {
            owner: "owner".to_string(),
            name: "blog".to_string(),
            default_branch: "main".to_string(),
        }
    }

    fn token_set() -> TokenSet {
        TokenSet {
            access_token: "ACCESS".to_string(),
            expires_at: 100,
            refresh_token: "REFRESH".to_string(),
            refresh_token_expires_at: 200,
        }
    }

    #[test]
    fn 保存して読み直すと同じ値() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        let settings = Settings {
            posts_dir: Some(PathBuf::from("/x/src/content/blog")),
            ..Default::default()
        };
        save_to(&file, &settings).unwrap();
        assert_eq!(read_from(&file).unwrap(), settings);
    }

    #[test]
    fn 固定のjsonを読める() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        fs::write(&file, r#"{"postsDir":"/x/src/content/blog"}"#).unwrap();
        assert_eq!(
            read_from(&file).unwrap(),
            Settings {
                posts_dir: Some(PathBuf::from("/x/src/content/blog")),
                ..Default::default()
            }
        );
    }

    #[test]
    fn ファイルが無ければ既定値() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(read_from(&file_of(&tmp)).unwrap(), Settings::default());
    }

    #[test]
    fn 壊れたjsonは失敗する() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        fs::write(&file, "@").unwrap();
        assert!(read_from(&file).is_err());
    }

    #[test]
    fn GitHubの設定を含めて保存し読み直すと同じ値() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        let settings = Settings {
            posts_dir: Some(PathBuf::from("/x/src/content/blog")),
            github: GithubSettings {
                repo: Some(repo()),
                base_branch: Some("main".to_string()),
                branch_format: "posts/{slug}".to_string(),
                posts_path: "src/content/posts".to_string(),
            },
            frontmatter: FrontmatterSettings::default(),
            ..Settings::default()
        };
        save_to(&file, &settings).unwrap();
        assert_eq!(read_from(&file).unwrap(), settings);
    }

    #[test]
    fn 記事の保存先を変えてもGitHubの設定が残る() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        let settings = Settings {
            posts_dir: Some(PathBuf::from("/a")),
            github: GithubSettings {
                repo: Some(repo()),
                ..GithubSettings::default()
            },
            ..Settings::default()
        };
        save_to(&file, &settings).unwrap();
        update(&file, |settings| {
            settings.posts_dir = Some(PathBuf::from("/b"))
        })
        .unwrap();
        let read = read_from(&file).unwrap();
        assert_eq!(read.posts_dir, Some(PathBuf::from("/b")));
        assert_eq!(read.github, settings.github);
    }

    #[test]
    fn GitHubの設定を変えても記事の保存先が残る() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        let settings = Settings {
            posts_dir: Some(PathBuf::from("/a")),
            github: GithubSettings {
                repo: Some(repo()),
                ..GithubSettings::default()
            },
            ..Settings::default()
        };
        save_to(&file, &settings).unwrap();
        update(&file, |settings| {
            settings.github.branch_format = "posts/{slug}".to_string()
        })
        .unwrap();
        let read = read_from(&file).unwrap();
        assert_eq!(read.github.branch_format, "posts/{slug}");
        assert_eq!(read.posts_dir, Some(PathBuf::from("/a")));
    }

    // 1項目だけ不正にしたGitHubの設定を書いて読む。ほかの項目は既定値と違う有効な値にしておく
    fn read_with_one_invalid(field: &str, value: serde_json::Value) -> Settings {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        let mut github = serde_json::json!({
            "repo": { "owner": "owner", "name": "blog", "defaultBranch": "main" },
            "baseBranch": "develop",
            "branchFormat": "posts/{slug}",
            "postsPath": "src/content/posts"
        });
        github[field] = value;
        let json = serde_json::json!({ "postsDir": "/a/src/content/blog", "github": github });
        fs::write(&file, json.to_string()).unwrap();
        read_from(&file).unwrap()
    }

    #[test]
    fn 検証を通らないGitHubの設定はその項目だけ既定値で読む() {
        let read = read_with_one_invalid(
            "repo",
            serde_json::json!({ "owner": "o/", "name": "blog", "defaultBranch": "main" }),
        );
        assert_eq!(read.posts_dir, Some(PathBuf::from("/a/src/content/blog")));
        assert_eq!(read.github.repo, None);
        assert_eq!(read.github.base_branch.as_deref(), Some("develop"));
        assert_eq!(read.github.branch_format, "posts/{slug}");
        assert_eq!(read.github.posts_path, "src/content/posts");

        let read = read_with_one_invalid("branchFormat", serde_json::json!("{slug}/{slug}"));
        assert_eq!(read.github.repo, Some(repo()));
        assert_eq!(read.github.branch_format, "blog/{slug}");
        assert_eq!(read.github.posts_path, "src/content/posts");

        let read = read_with_one_invalid("postsPath", serde_json::json!("../blog"));
        assert_eq!(read.github.repo, Some(repo()));
        assert_eq!(read.github.branch_format, "posts/{slug}");
        assert_eq!(read.github.posts_path, "src/content/blog");

        // 不正なbaseBranch
        let read = read_with_one_invalid("baseBranch", serde_json::json!("a..b"));
        assert_eq!(read.github.repo, Some(repo()));
        assert_eq!(read.github.base_branch, None);

        // defaultBranchが不正なrepository
        let read = read_with_one_invalid(
            "repo",
            serde_json::json!({ "owner": "owner", "name": "blog", "defaultBranch": "a..b" }),
        );
        assert_eq!(read.github.repo, None);

        // ownerかnameが..のrepository
        let read = read_with_one_invalid(
            "repo",
            serde_json::json!({ "owner": "..", "name": "blog", "defaultBranch": "main" }),
        );
        assert_eq!(read.github.repo, None);
        let read = read_with_one_invalid(
            "repo",
            serde_json::json!({ "owner": "owner", "name": "..", "defaultBranch": "main" }),
        );
        assert_eq!(read.github.repo, None);
    }

    #[test]
    fn switch_posts_dirで渡した設定を保存してから向き先を替える() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        let before = tmp.path().join("a/src/content/blog");
        let store = store::Store::new(before.clone(), tmp.path().join("backup")).unwrap();
        // 前の向き先のファイルは消えない
        fs::create_dir_all(&before).unwrap();
        fs::write(before.join("kept.md"), "本文").unwrap();
        save_to(
            &file,
            &Settings {
                posts_dir: Some(before.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        // Repositoryを選んだときの形。記事の保存先(posts_dir)の設定は変えない
        let settings = Settings {
            posts_dir: Some(before.clone()),
            github: GithubSettings {
                repo: Some(repo()),
                ..GithubSettings::default()
            },
            frontmatter: FrontmatterSettings::default(),
            ..Settings::default()
        };
        let after = github_posts_dir(&tmp.path().join("data"), &repo(), "src/content/blog");
        switch_posts_dir(&store, &file, &settings, after.clone()).unwrap();
        assert_eq!(store.posts_dir(), after);
        assert_eq!(read_from(&file).unwrap(), settings);
        assert!(before.join("kept.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn switch_posts_dirで設定の保存に失敗したら向き先は替えない() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        let before = tmp.path().join("a/src/content/blog");
        let store = store::Store::new(before.clone(), tmp.path().join("backup")).unwrap();
        let settings = Settings {
            posts_dir: Some(before.clone()),
            ..Default::default()
        };
        save_to(&file, &settings).unwrap();
        let Some(read_only) = crate::github::fake::read_only_dir(tmp.path()) else {
            return;
        };
        let result = switch_posts_dir(
            &store,
            &file,
            &settings,
            tmp.path().join("b/src/content/blog"),
        );
        drop(read_only);
        assert!(result.is_err());
        assert_eq!(store.posts_dir(), before);
    }

    #[test]
    fn switch_posts_dirで向き先が使えないときは設定も保存せず向き先も替えない() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        let before = tmp.path().join("a/src/content/blog");
        let store = store::Store::new(before.clone(), tmp.path().join("backup")).unwrap();
        let settings = Settings {
            posts_dir: Some(before.clone()),
            ..Default::default()
        };
        let result = switch_posts_dir(&store, &file, &settings, PathBuf::from("/a"));
        assert!(result.is_err());
        assert_eq!(store.posts_dir(), before);
        assert!(!file.exists());
    }

    #[test]
    fn logoutするとtoken_jsonを消して向き先を戻す() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let local = tmp.path().join("local/src/content/blog");
        // GitHub用の置き場所を向いた状態から始める。そこのファイルは残る
        let github_dir = github_posts_dir(&data_dir, &repo(), "src/content/blog");
        let store = store::Store::new(github_dir.clone(), tmp.path().join("backup")).unwrap();
        fs::create_dir_all(&github_dir).unwrap();
        fs::write(github_dir.join("kept.md"), "本文").unwrap();
        let path = token_path(&data_dir);
        token::write(&path, &token_set()).unwrap();
        logout(&store, &data_dir, local.clone()).unwrap();
        assert!(!path.exists());
        assert_eq!(store.posts_dir(), local);
        assert!(github_dir.join("kept.md").exists());
    }

    #[test]
    fn frontmatterの無い設定はどちらも有効で保存して読み直すと同じ値() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        fs::write(&file, r#"{"postsDir":"/x/src/content/blog"}"#).unwrap();
        assert_eq!(
            read_from(&file).unwrap().frontmatter,
            FrontmatterSettings {
                hero_image: true,
                updated_date: true,
            }
        );

        let settings = Settings {
            posts_dir: Some(PathBuf::from("/x/src/content/blog")),
            github: GithubSettings::default(),
            frontmatter: FrontmatterSettings {
                hero_image: false,
                updated_date: true,
            },
            ..Settings::default()
        };
        save_to(&file, &settings).unwrap();
        assert_eq!(read_from(&file).unwrap(), settings);

        // 一部の項目だけがあれば、残りは有効
        fs::write(&file, r#"{"frontmatter":{"heroImage":false}}"#).unwrap();
        assert_eq!(
            read_from(&file).unwrap().frontmatter,
            FrontmatterSettings {
                hero_image: false,
                updated_date: true,
            }
        );
    }

    #[test]
    fn removeUnusedImagesの無い設定は有効でfalseで保存して読み直すと無効() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        fs::write(&file, r#"{"postsDir":"/x/src/content/blog"}"#).unwrap();
        assert!(read_from(&file).unwrap().remove_unused_images);

        let settings = Settings {
            posts_dir: Some(PathBuf::from("/x/src/content/blog")),
            remove_unused_images: false,
            ..Settings::default()
        };
        save_to(&file, &settings).unwrap();
        assert!(!read_from(&file).unwrap().remove_unused_images);
    }

    #[test]
    fn 起動時に設定を読めなければ片付けは無効で無ければ有効() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        assert!(load_from(&file).remove_unused_images);
        fs::write(&file, "@").unwrap();
        assert!(!load_from(&file).remove_unused_images);
    }
}
