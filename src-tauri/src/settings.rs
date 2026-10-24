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

#[derive(Debug, Default, Serialize, Deserialize)]
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
    read(app).unwrap_or_else(|error| {
        eprintln!("設定を読めませんでした: {error}");
        Settings::default()
    })
}

fn read(app: &AppHandle) -> Result<Settings, StoreError> {
    let file = file_path(app)?;
    let content = match fs::read_to_string(&file) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(error) => return Err(StoreError::io(&file, error)),
    };
    serde_json::from_str(&content).map_err(|error| StoreError::io(&file, error))
}

fn save(app: &AppHandle, settings: &Settings) -> Result<(), StoreError> {
    let file = file_path(app)?;
    let dir = file.parent().expect("settings.jsonの親がある");
    fs::create_dir_all(dir).map_err(|error| StoreError::io(dir, error))?;
    let json = serde_json::to_string(settings).map_err(|error| StoreError::io(&file, error))?;
    write_temp(dir, json.as_bytes())?
        .persist(&file)
        .map_err(|error| StoreError::io(&file, error.error))?;
    Ok(())
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
pub fn get_posts_dir(store: State<'_, Store>) -> String {
    store.posts_dir().display().to_string()
}

// キャンセルされたらNone
#[cfg(not(target_os = "android"))]
#[tauri::command(async)]
pub fn pick_posts_dir(
    app: AppHandle,
    store: State<'_, Store>,
) -> Result<Option<String>, StoreError> {
    use tauri_plugin_dialog::DialogExt;

    let current = store.posts_dir();
    let mut dialog = app.dialog().file();
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
    Ok(Some(path.display().to_string()))
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
    if !crate::store::is_filename(&name) || name.starts_with('.') {
        return Err(StoreError::InvalidPostsDirName { name });
    }
    let mut settings = settings.lock();
    let next = Settings {
        posts_dir_name: Some(name),
    };
    save(&app, &next)?;
    *settings = next;
    Ok(())
}
