// 托盘、关窗隐藏、ACP 本地宿主仅桌面
#[cfg(desktop)]
mod acp_host;
#[cfg(desktop)]
mod tray;
#[cfg(desktop)]
mod updater;
#[cfg(desktop)]
mod startup;
// MM4：附件直存下载目录（桌面走 reqwest，Android 走 Kotlin MediaStore 插件）
mod download;
// AS2：Android 应用内 APK 安装（缓存 + FileProvider + 系统安装器，与附件下载分离）
mod apk_installer;
mod notifications;
#[cfg(target_os = "linux")]
mod file_drop;
#[cfg(target_os = "linux")]
mod restore_drop;

#[cfg(desktop)]
pub fn maybe_run_cli() -> bool {
    acp_host::maybe_run_cli()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "windows")]
    if std::env::args().any(|arg| arg.to_ascii_lowercase().starts_with("chat://")) {
        use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};
        unsafe {
            let _ = AllowSetForegroundWindow(ASFW_ANY);
        }
    }
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(download::init())
        .plugin(apk_installer::init());
    #[cfg(target_os = "android")]
    let builder = builder.plugin(notifications::init());
    #[cfg(target_os = "windows")]
    let builder = builder
        .manage(notifications::WindowsActivation::default())
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if !notifications::handle_windows_args(app, args) {
                tray::reveal(app);
            }
        }));
    #[cfg(desktop)]
    let builder = builder
        .manage(acp_host::AcpHostState::new())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ));
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    #[cfg(target_os = "linux")]
    let builder = builder
        .manage(file_drop::DroppedFiles::default())
        .manage(file_drop::DroppedLinks::default())
        .manage(restore_drop::SelectedBackup::default())
        .on_window_event(file_drop::on_window_event);
    #[cfg(desktop)]
    let builder = builder
        .invoke_handler(tauri::generate_handler![
            notifications::notification_permission,
            notifications::show_chat_notification,
            notifications::notification_ready,
            download::download_to_downloads,
            updater::install_update,
            #[cfg(target_os = "linux")]
            file_drop::read_dropped_file,
            #[cfg(target_os = "linux")]
            file_drop::take_dropped_link,
            #[cfg(target_os = "linux")]
            restore_drop::select_dropped_backup,
            #[cfg(target_os = "linux")]
            restore_drop::restore_dropped_backup,
            startup::show_startup_window,
            acp_host::acp_host_start,
            acp_host::acp_host_ticket,
            acp_host::acp_host_stop,
            acp_host::acp_host_confirm_exec,
            acp_host::acp_host_local_policy,
            acp_host::acp_host_set_local_allow_all
        ])
        .setup(|app| {
            tray::install(app.handle())?;
            #[cfg(target_os = "windows")]
            notifications::handle_windows_args(app.handle(), std::env::args().collect());
            #[cfg(target_os = "linux")]
            file_drop::listen_for_link_uris(app.handle())?;
            startup::schedule_fallback(app.handle());
            Ok(())
        })
        .on_window_event(tray::on_window_event);
    #[cfg(mobile)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        #[cfg(target_os = "android")]
        notifications::listen_for_android_notification_open,
        notifications::notification_permission,
        notifications::show_chat_notification,
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
