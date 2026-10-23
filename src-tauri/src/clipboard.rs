//! LinuxのWebKitGTKは、貼り付けた画像を画面側のclipboardDataに入れないので、GTKのクリップボードから読む。

use tauri::AppHandle;

use crate::store::StoreError;

#[cfg(target_os = "linux")]
pub fn read_image_png(app: &AppHandle) -> Result<Option<Vec<u8>>, StoreError> {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();
    // GTKのクリップボードはメインスレッドからしか触れない
    app.run_on_main_thread(move || {
        let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
        clipboard.request_image(move |_, pixbuf| {
            let png = pixbuf.map(|pixbuf| {
                pixbuf
                    .save_to_bufferv("png", &[])
                    .map_err(|error| error.to_string())
            });
            let _ = tx.send(png.transpose());
        });
    })
    .map_err(clipboard_error)?;
    rx.recv().map_err(clipboard_error)?.map_err(clipboard_error)
}

// Androidでは画面側のclipboardDataで受ける
#[cfg(not(target_os = "linux"))]
pub fn read_image_png(_app: &AppHandle) -> Result<Option<Vec<u8>>, StoreError> {
    Ok(None)
}

#[cfg(target_os = "linux")]
fn clipboard_error(error: impl std::fmt::Display) -> StoreError {
    StoreError::Clipboard {
        reason: error.to_string(),
    }
}
