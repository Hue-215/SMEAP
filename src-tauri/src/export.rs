//! 保存画面で選ばれたファイルは、書き写す前に作られていて消す手段を持たない。そのため、キャッシュの
//! 中でzipを作り終えてから書き写す。書き写しに失敗したら、書きかけが残りうることを結果で返す。

use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::{DialogExt, FilePath};

use crate::store::{Store, StoreError};

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExportOutcome {
    Saved,
    Cancelled,
    LeftIncomplete,
}

#[tauri::command(async)]
pub fn export_zip(
    app: AppHandle,
    store: State<'_, Store>,
    slug: String,
) -> Result<ExportOutcome, StoreError> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|error| StoreError::io(Path::new("app_cache_dir"), error))?;
    fs::create_dir_all(&cache_dir).map_err(|error| StoreError::io(&cache_dir, error))?;
    // 落とすと消える。強制終了で残ったものは、キャッシュとしてOSが消す
    let staged = tempfile::NamedTempFile::new_in(&cache_dir)
        .map_err(|error| StoreError::io(&cache_dir, error))?;
    store.write_zip(&slug, staged.path())?;

    let Some(chosen) = app
        .dialog()
        .file()
        .set_file_name(format!("{slug}.zip"))
        .add_filter("ZIP", &["zip"])
        .blocking_save_file()
    else {
        return Ok(ExportOutcome::Cancelled);
    };

    // 開く手順がpanicしても(Androidのfs側は、提供元がファイル記述子を返さないとpanicする)
    // 失敗として返す
    let copied = panic::catch_unwind(AssertUnwindSafe(|| {
        let mut dest = open_chosen(&app, chosen)?;
        io::copy(&mut File::open(staged.path())?, &mut dest)?;
        sync(&dest)
    }))
    .unwrap_or_else(|_| Err(io::Error::other("保存先のファイルを開くのがpanicした")));
    if let Err(error) = copied {
        eprintln!("選ばれたファイルへ書き写せませんでした: {error}");
        return Ok(ExportOutcome::LeftIncomplete);
    }
    Ok(ExportOutcome::Saved)
}

// 保存画面が返したものを、中身を置き換えて書く形で開く。パスで届くものと`content://`のURIで
// 届くもの(URIは`tauri-plugin-fs`で開く)の両方がある
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
