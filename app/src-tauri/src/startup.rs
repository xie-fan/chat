use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tauri::{AppHandle, Manager, WebviewWindow};

static STARTUP_SHOWN: AtomicBool = AtomicBool::new(false);

#[tauri::command]
pub fn show_startup_window(window: WebviewWindow) {
    if !STARTUP_SHOWN.swap(true, Ordering::Relaxed) {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn schedule_fallback(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            show_startup_window(window);
        });
    }
}
