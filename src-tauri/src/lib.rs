mod commands;
mod image_protocol;
mod store;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    image_protocol::register(tauri::Builder::default())
        .setup(|app| {
            #[cfg(not(target_os = "android"))]
            let store = {
                let exe = std::env::current_exe()?;
                let root = exe.parent().ok_or("実行ファイルの親ディレクトリが無い")?;
                store::Store::new(root.to_path_buf(), root.join(".backup"))
            };
            #[cfg(target_os = "android")]
            let store = {
                let base = app.path().app_data_dir()?;
                store::Store::new(base.join("posts"), base.join("backup"))
            };
            app.manage(store);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::validate_slug,
            commands::list_articles,
            commands::create_article,
            commands::read_article,
            commands::save_article,
            commands::rename_article,
            commands::import_body_image,
            commands::import_hero_image,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
