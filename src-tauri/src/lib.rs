mod clipboard;
mod commands;
#[cfg(target_os = "android")]
mod export;
mod github;
mod image_protocol;
mod settings;
mod store;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTKの<input type="date">の書式は端末のロケール(LANG・LC_ALL)に従い、jaでもen_USでも
    // `2026-07-02`の形にはならない。ISO 8601になるen_CAを、GTKの初期化より前に設定する。
    // LC_ALLはプロセス全体に効くので、GTKのファイルダイアログなどの文言もen_CAになる
    #[cfg(target_os = "linux")]
    std::env::set_var("LC_ALL", "en_CA.UTF-8");
    // fsとdialogとopenerはRust側からだけ使う(src/export.rs・src/settings.rs・src/github/commands.rs)。
    // 画面に権限は与えない
    let builder = image_protocol::register(tauri::Builder::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init());
    #[cfg(target_os = "android")]
    let builder = builder.plugin(tauri_plugin_fs::init());
    builder
        .setup(|app| {
            let settings = settings::load(app.handle());
            let data_dir = app.path().app_data_dir()?;
            let backup_root = data_dir.join("backup");
            // 今までの保存先(GitHub連携より前の向き先)と、設定に保存先が無いときの既定
            #[cfg(not(target_os = "android"))]
            let (store, local_dir, default_dir) = {
                let exe = std::env::current_exe()?;
                let exe_dir = exe
                    .parent()
                    .ok_or("the executable has no parent directory")?;
                let default_dir = exe_dir.join("src/content/blog");
                // 設定のディレクトリが使えなくても起動は止めない
                match settings
                    .posts_dir
                    .clone()
                    .map(|dir| (store::Store::new(dir.clone(), backup_root.clone()), dir))
                {
                    Some((Ok(store), dir)) => (store, dir, default_dir),
                    other => {
                        if let Some((Err(error), _)) = other {
                            eprintln!("Failed to use the configured article directory: {error}");
                        }
                        let store = store::Store::new(default_dir.clone(), backup_root)?;
                        (store, default_dir.clone(), default_dir)
                    }
                }
            };
            #[cfg(target_os = "android")]
            let (store, local_dir, default_dir) = {
                let local_dir = data_dir.join("posts/src/content/blog");
                let store = store::Store::new(local_dir.clone(), backup_root)?;
                (store, local_dir.clone(), local_dir)
            };
            // 起動時の保存先。token.jsonが読めないときと向き先が使えないときは、今までの保存先で始める
            let (startup_dir, token_error) =
                github::config::startup_posts_dir(local_dir, &data_dir, &settings.github);
            if let Some(error) = token_error {
                eprintln!("Failed to read the GitHub token: {error}");
            }
            if let Err(error) = store.set_posts_dir(startup_dir) {
                eprintln!("Failed to use the GitHub article directory: {error}");
            }
            app.manage(store);
            app.manage(github::commands::GitHub::new(&data_dir, default_dir));
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
            github::commands::github_status,
            github::commands::github_start_login,
            github::commands::github_wait_login,
            github::commands::github_cancel_login,
            github::commands::github_logout,
            github::commands::github_list_repositories,
            github::commands::github_select_repository,
            github::commands::github_update_settings,
            github::commands::validate_branch_format,
            github::commands::validate_posts_path,
            github::commands::open_github_page,
            github::commands::github_sync,
            github::commands::github_fetch_images,
            github::commands::github_commit_preview,
            github::commands::github_commit_push,
            github::commands::github_pull,
            github::commands::github_article_states,
            github::commands::github_article_state,
            github::commands::github_discard_edit,
            github::commands::github_remove_published,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
