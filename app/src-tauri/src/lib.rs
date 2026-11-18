// 托盘、关窗隐藏、ACP 本地宿主仅桌面
#[cfg(desktop)]
mod acp_host;
#[cfg(desktop)]
mod tray;
#[cfg(desktop)]
mod updater;
// MM4：附件直存下载目录（桌面走 reqwest，Android 走 Kotlin MediaStore 插件）
mod download;
// AS2：Android 应用内 APK 安装（缓存 + FileProvider + 系统安装器，与附件下载分离）
mod apk_installer;

#[cfg(desktop)]
pub fn maybe_run_cli() -> bool {
    acp_host::maybe_run_cli()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(download::init())
        .plugin(apk_installer::init());
    #[cfg(desktop)]
    let builder = builder
        .manage(acp_host::AcpHostState::new())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ));
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    #[cfg(desktop)]
    let builder = builder
        .invoke_handler(tauri::generate_handler![
            download::download_to_downloads,
            updater::install_update,
            acp_host::acp_host_start,
            acp_host::acp_host_ticket,
            acp_host::acp_host_stop
        ])
        .setup(|app| {
            tray::install(app.handle())?;
            Ok(())
        })
        .on_window_event(tray::on_window_event);
    #[cfg(mobile)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        download::download_to_downloads,
        apk_installer::download_update_apk,
        apk_installer::apk_download_progress,
        apk_installer::check_install_permission,
        apk_installer::open_install_permission_settings,
        apk_installer::install_update_apk
    ]);
    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
