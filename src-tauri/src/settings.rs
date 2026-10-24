//! 記事の保存先の設定と、それを変えるコマンド。app_config_dirの下のsettings.jsonに持つ。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "android")]
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

#[cfg(not(target_os = "android"))]
use crate::store::{self, Store};
use crate::store::{write_temp, StoreError};

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[cfg(not(target_os = "android"))]
    pub posts_dir: Option<PathBuf>,
    // zipの中の`src/content/`の下の名前。端末の中の置き場所は変えない
    #[cfg(target_os = "android")]
    posts_dir_name: Option<String>,
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

fn file_path(app: &AppHandle) -> Result<PathBuf, StoreError> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join("settings.json"))
        .map_err(|error| StoreError::io(Path::new("app_config_dir"), error))
}

// 読めなくても起動は止めず、既定の設定で始める
pub fn load(app: &AppHandle) -> Settings {
    let settings = read(app).unwrap_or_else(|error| {
        eprintln!("Failed to read settings: {error}");
        Settings::default()
    });
    // 検証を通らない名前が残っていたら、無かったものとして扱う
    #[cfg(target_os = "android")]
    let settings = match &settings.posts_dir_name {
        Some(name) if crate::store::validate_posts_dir_name(name).is_err() => {
            eprintln!("Invalid directory name in settings: {name}");
            Settings::default()
        }
        _ => settings,
    };
    settings
}

fn read(app: &AppHandle) -> Result<Settings, StoreError> {
    read_from(&file_path(app)?)
}

fn read_from(file: &Path) -> Result<Settings, StoreError> {
    let content = match fs::read_to_string(file) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(error) => return Err(StoreError::io(file, error)),
    };
    serde_json::from_str(&content).map_err(|error| StoreError::io(file, error))
}

fn save(app: &AppHandle, settings: &Settings) -> Result<(), StoreError> {
    save_to(&file_path(app)?, settings)
}

fn save_to(file: &Path, settings: &Settings) -> Result<(), StoreError> {
    let dir = file.parent().expect("settings.json should have a parent directory");
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
    save(
        &app,
        &Settings {
            posts_dir: Some(path.clone()),
        },
    )?;
    store.set_posts_dir(path.clone())?;
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
        .unwrap_or_else(|| "posts".to_string())
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
    let mut settings = settings.lock();
    let next = Settings {
        posts_dir_name: Some(name),
    };
    save(&app, &next)?;
    *settings = next;
    Ok(())
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn file_of(tmp: &TempDir) -> PathBuf {
        tmp.path().join("settings.json")
    }

    #[test]
    fn 保存して読み直すと同じ値() {
        let tmp = TempDir::new().unwrap();
        let file = file_of(&tmp);
        let settings = Settings {
            posts_dir: Some(PathBuf::from("/x/src/content/blog")),
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
}
