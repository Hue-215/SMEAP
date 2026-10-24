//! 保存画面で選ばれたファイルは、書き写す前に作られていて消す手段を持たない。そのため、キャッシュの
//! 中でzipを作り終えてから書き写す。書き写しに失敗したら、書きかけが残りうることを結果で返す。

use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::{DialogExt, FilePath};

use crate::settings::{self, SettingsState};
use crate::store::{Store, StoreError};

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExportOutcome {
    Saved,
    Cancelled,
    // Androidではstderrがlogcatに出ないので、理由は画面へ返して残す
    LeftIncomplete { reason: String },
}

#[tauri::command(async)]
pub fn export_zip(
    app: AppHandle,
    store: State<'_, Store>,
    settings: State<'_, SettingsState>,
    slug: String,
) -> Result<ExportOutcome, StoreError> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|error| StoreError::io(Path::new("app_cache_dir"), error))?;
    fs::create_dir_all(&cache_dir).map_err(|error| StoreError::io(&cache_dir, error))?;
    // dropすると消える。強制終了で残ったものは、キャッシュとしてOSが消すことがある
    let staged = tempfile::NamedTempFile::new_in(&cache_dir)
        .map_err(|error| StoreError::io(&cache_dir, error))?;
    store.write_zip(&slug, &settings::posts_dir_name(&settings), staged.path())?;

    let Some(chosen) = app
        .dialog()
        .file()
        .set_file_name(format!("{slug}.zip"))
        .add_filter("ZIP", &["zip"])
        .blocking_save_file()
    else {
        return Ok(ExportOutcome::Cancelled);
    };

    // Androidのfs側は、提供元がファイル記述子を返さないとpanicする
    let copied = panic::catch_unwind(AssertUnwindSafe(|| {
        let mut dest = open_chosen(&app, chosen)?;
        io::copy(&mut File::open(staged.path())?, &mut dest)?;
        sync(&dest)
    }))
    .unwrap_or_else(|_| Err(io::Error::other("a panic occurred while opening the destination file")));
    Ok(match copied {
        Ok(()) => ExportOutcome::Saved,
        Err(error) => ExportOutcome::LeftIncomplete {
            reason: error.to_string(),
        },
    })
}

fn open_chosen(app: &AppHandle, chosen: FilePath) -> io::Result<File> {
    match chosen {
        FilePath::Path(path) => File::create(path),
        FilePath::Url(url) => {
            use tauri_plugin_fs::{FsExt, OpenOptions};

            let mut options = OpenOptions::new();
            options.write(true).truncate(true);
            app.fs().open(url, options)
        }
    }
}

// 書き写した中身を記憶装置へ書き切らせる。提供元がパイプで受け取る場合は書き切る操作を持たない
// ので、それは失敗にしない
fn sync(out: &File) -> io::Result<()> {
    match out.sync_all() {
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::InvalidInput | ErrorKind::Unsupported
            ) =>
        {
            Ok(())
        }
        result => result,
    }
}
