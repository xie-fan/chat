use tauri::AppHandle;

#[cfg(desktop)]
use tauri::{Emitter, Manager};

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    conv_id: String,
    session: u64,
}

#[cfg(target_os = "android")]
struct AndroidNotifications(tauri::plugin::PluginHandle<tauri::Wry>);

#[cfg(target_os = "android")]
pub fn init() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri::Manager;
    tauri::plugin::Builder::new("chat_notifications")
        .setup(|app, api| {
            let handle = api.register_android_plugin("com.chat.app", "ChatNotificationsPlugin")?;
            app.manage(AndroidNotifications(handle));
            Ok(())
        })
        .build()
}

#[tauri::command]
pub async fn notification_permission(
    app: AppHandle,
    request: Option<bool>,
) -> Result<&'static str, String> {
    let request = request.unwrap_or(false);
    #[cfg(target_os = "android")]
    {
        use tauri::Manager;
        let command = if request {
            "requestPermission"
        } else {
            "permission"
        };
        let state: String = app
            .state::<AndroidNotifications>()
            .0
            .run_mobile_plugin_async(command, ())
            .await
            .map_err(|e| e.to_string())?;
        Ok(match state.as_str() {
            "granted" => "granted",
            "denied" => "denied",
            _ => "default",
        })
    }
    #[cfg(target_os = "macos")]
    {
        use mac_usernotifications::{get_notification_settings, request_auth, AuthorizationStatus};
        let _ = app;
        let settings = get_notification_settings()
            .await
            .map_err(|e| e.to_string())?;
        Ok(match settings.authorization_status {
            AuthorizationStatus::Authorized
            | AuthorizationStatus::Provisional
            | AuthorizationStatus::Ephemeral => "granted",
            AuthorizationStatus::Denied => "denied",
            AuthorizationStatus::NotDetermined if request => {
                if request_auth().await.map_err(|e| e.to_string())? {
                    "granted"
                } else {
                    "denied"
                }
            }
            AuthorizationStatus::NotDetermined => "default",
            AuthorizationStatus::Unknown => "unavailable",
        })
    }
    #[cfg(target_os = "windows")]
    {
        use windows::UI::Notifications::{NotificationSetting, ToastNotificationManager};
        let _ = request;
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(
            &windows::core::HSTRING::from(windows_app_id(&app)),
        )
        .map_err(|e| e.to_string())?;
        let setting = notifier.Setting().map_err(|e| e.to_string())?;
        Ok(if setting == NotificationSetting::Enabled {
            "granted"
        } else {
            "denied"
        })
    }
    #[cfg(target_os = "linux")]
    {
        let _ = (app, request);
        Ok("granted")
    }
    #[cfg(not(any(
        target_os = "android",
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    )))]
    {
        let _ = (app, request);
        Ok("unavailable")
    }
}

#[tauri::command]
pub async fn show_chat_notification(
    app: AppHandle,
    title: String,
    body: String,
    target: Option<Target>,
) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        use tauri::Manager;
        app.state::<AndroidNotifications>()
            .0
            .run_mobile_plugin_async(
                "show",
                serde_json::json!({
                    "title": title, "body": body, "convId": target.map(|t| t.conv_id),
                }),
            )
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(desktop)]
    {
        show_desktop(app, title, body, target).await.map(|()| true)
    }
    #[cfg(all(not(desktop), not(target_os = "android")))]
    {
        let _ = (app, title, body, target);
        Ok(false)
    }
}

#[cfg(desktop)]
fn activate(app: &AppHandle, target: Option<Target>) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        crate::tray::reveal(&handle);
        if let Some(target) = target {
            if let Some(window) = handle.get_webview_window("main") {
                let _ = window.emit("chat-notification-open", target);
            }
        }
    });
}

#[cfg(target_os = "linux")]
pub async fn show_desktop(
    app: AppHandle,
    title: String,
    body: String,
    target: Option<Target>,
) -> Result<(), String> {
    let notification = notify_rust::Notification::new()
        .appname("Chat")
        .summary(&title)
        .body(&body)
        .action("default", "打开 Chat")
        .show_async()
        .await
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn(async move {
        notification
            .wait_for_action_async(|response| {
                if matches!(response, notify_rust::NotificationResponse::Default) {
                    activate(&app, target);
                }
            })
            .await;
    });
    Ok(())
}

#[cfg(target_os = "macos")]
pub async fn show_desktop(
    app: AppHandle,
    title: String,
    body: String,
    target: Option<Target>,
) -> Result<(), String> {
    let notification = mac_usernotifications::Notification::new()
        .title(title)
        .message(body)
        .timeout(std::time::Duration::from_secs(24 * 60 * 60))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn(async move {
        if let Ok(response) = notification.response().await {
            if response.is_default_action() {
                activate(&app, target);
            }
        }
    });
    Ok(())
}

#[cfg(target_os = "windows")]
fn windows_app_id(app: &AppHandle) -> &str {
    use tauri_winrt_notification::Toast;
    let is_unbundled = tauri::utils::platform::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        .is_some_and(|dir| dir.ends_with("target/debug") || dir.ends_with("target/release"));
    if is_unbundled {
        Toast::POWERSHELL_APP_ID
    } else {
        &app.config().identifier
    }
}

#[cfg(target_os = "windows")]
pub async fn show_desktop(
    app: AppHandle,
    title: String,
    body: String,
    target: Option<Target>,
) -> Result<(), String> {
    if notification_permission(app.clone(), None).await? != "granted" {
        return Err("Notifications are disabled".into());
    }
    let app_id = windows_app_id(&app).to_owned();
    tauri_winrt_notification::Toast::new(&app_id)
        .title(&title)
        .text1(&body)
        .on_activated(move |_| {
            activate(&app, target.clone());
            Ok(())
        })
        .show()
        .map_err(|e| e.to_string())
}
