mod clipboard;
mod commands;
#[cfg(target_os = "android")]
mod export;
mod image_protocol;
mod settings;
mod store;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // fsとdialogはRust側からだけ使う(src/export.rs・src/settings.rs)。画面に権限は与えない
    let builder =
        image_protocol::register(tauri::Builder::default()).plugin(tauri_plugin_dialog::init());
    #[cfg(target_os = "android")]
    let builder = builder.plugin(tauri_plugin_fs::init());
    builder
        .setup(|app| {
            let settings = settings::load(app.handle());
            let data_dir = app.path().app_data_dir()?;
            let backup_root = data_dir.join("backup");
            #[cfg(not(target_os = "android"))]
            let store = {
                let exe = std::env::current_exe()?;
                let exe_dir = exe.parent().ok_or("実行ファイルの親ディレクトリが無い")?;
                let default_dir = exe_dir.join("src/content/posts");
                // 設定のディレクトリが使えなくても起動は止めない
                match settings
                    .posts_dir
                    .map(|dir| store::Store::new(dir, backup_root.clone()))
                {
                    Some(Ok(store)) => store,
                    Some(Err(error)) => {
                        eprintln!("設定の記事のディレクトリを使えません: {error}");
                        store::Store::new(default_dir, backup_root)?
                    }
                    None => store::Store::new(default_dir, backup_root)?,
                }
            };
            #[cfg(target_os = "android")]
            let store = store::Store::new(data_dir.join("posts/src/content/posts"), backup_root)?;
            app.manage(store);
            #[cfg(target_os = "android")]
            app.manage(settings::SettingsState::new(settings));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::validate_slug,
            commands::list_articles,
            commands::create_article,
            commands::read_article,
            commands::save_article,
            commands::rename_article,
            commands::delete_article,
            commands::import_body_image,
            commands::import_hero_image,
            commands::remove_hero_image,
            commands::import_body_image_from_path,
            commands::import_clipboard_image,
            #[cfg(not(target_os = "android"))]
            settings::get_posts_dir,
            #[cfg(not(target_os = "android"))]
            settings::pick_posts_dir,
            #[cfg(target_os = "android")]
            export::export_zip,
            #[cfg(target_os = "android")]
            settings::get_posts_dir_name,
            #[cfg(target_os = "android")]
            settings::set_posts_dir_name,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
