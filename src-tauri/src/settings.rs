//! 記事の保存先の設定と、それを変えるコマンド。app_config_dirの下のsettings.jsonに持つ。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

#[cfg(not(target_os = "android"))]
use crate::store::Store;
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

pub struct SettingsState(Mutex<Settings>);

impl SettingsState {
    pub fn new(settings: Settings) -> Self {
        Self(Mutex::new(settings))
    }

    fn lock(&self) -> MutexGuard<'_, Settings> {
        match self.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
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
    let file = match file_path(app) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("設定を読めませんでした: {error}");
            return Settings::default();
        }
    };
    let content = match fs::read_to_string(&file) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Settings::default(),
        Err(error) => {
            eprintln!("設定を読めませんでした: {}", StoreError::io(&file, error));
            return Settings::default();
        }
    };
    serde_json::from_str(&content).unwrap_or_else(|error| {
        eprintln!("設定を読めませんでした: {}", StoreError::io(&file, error));
        Settings::default()
    })
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
    settings: State<'_, SettingsState>,
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
    store.set_posts_dir(path.clone())?;
    let mut settings = settings.lock();
    let previous = settings.posts_dir.replace(path.clone());
    if let Err(error) = save(&app, &settings) {
        settings.posts_dir = previous;
        store
            .set_posts_dir(current)
            .expect("前のディレクトリには戻せる");
        return Err(error);
    }
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
    let valid =
        !name.is_empty() && !name.contains('/') && !name.contains('\\') && !name.starts_with('.');
    if !valid {
        return Err(StoreError::InvalidPostsDirName { name });
    }
    let mut settings = settings.lock();
    let previous = settings.posts_dir_name.replace(name);
    let result = save(&app, &settings);
    if result.is_err() {
        settings.posts_dir_name = previous;
    }
    result
}
