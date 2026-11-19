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
        use mac_usernotifications::{get_notification_settings, AuthorizationStatus};
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
                if request_mac_auth().await? {
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
        let setting = match notifier.Setting() {
            Ok(setting) => setting,
            Err(error) if error.code() == windows::core::HRESULT(0x80070490u32 as i32) => {
                NotificationSetting::Enabled
            }
            Err(error) => return Err(error.to_string()),
        };
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
    android_account: Option<String>,
    android_message_id: Option<String>,
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
                    "account": android_account,
                    "messageId": android_message_id,
                }),
            )
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(desktop)]
    {
        let _ = (android_account, android_message_id);
        show_desktop(app, title, body, target).await.map(|()| true)
    }
    #[cfg(all(not(desktop), not(target_os = "android")))]
    {
        let _ = (app, title, body, target, android_account, android_message_id);
        Ok(false)
    }
}

#[cfg(target_os = "android")]
#[tauri::command]
pub async fn android_push_registration(
    app: AppHandle,
    account: Option<String>,
    server: Option<String>,
    enabled: bool,
) -> Result<Option<serde_json::Value>, String> {
    use tauri::Manager;
    app.state::<AndroidNotifications>()
        .0
        .run_mobile_plugin_async(
            "androidPushRegistration",
            serde_json::json!({ "account": account, "server": server, "enabled": enabled }),
        )
        .await
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
#[tauri::command]
pub async fn listen_for_android_notification_open(
    app: AppHandle,
    account: String,
    channel: tauri::ipc::Channel<String>,
) -> Result<(), String> {
    use tauri::Manager;
    app.state::<AndroidNotifications>()
        .0
        .run_mobile_plugin_async(
            "listenForNotificationOpen",
            serde_json::json!({ "account": account, "channel": channel }),
        )
        .await
        .map_err(|e| e.to_string())
}

#[cfg(desktop)]
fn activate(app: &AppHandle, target: Option<Target>) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        #[cfg(target_os = "linux")]
        crate::tray::reveal_notification(&handle);
        #[cfg(not(target_os = "linux"))]
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

// mac-usernotifications 只申请 Alert|Sound；不申请 Badge 时 Dock 角标不显示。
#[cfg(target_os = "macos")]
async fn request_mac_auth() -> Result<bool, String> {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::NSError;
    use objc2_user_notifications::{UNAuthorizationOptions, UNUserNotificationCenter};
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = std::sync::Mutex::new(Some(tx));
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert
                | UNAuthorizationOptions::Sound
                | UNAuthorizationOptions::Badge,
            &RcBlock::new(move |granted: Bool, _: *mut NSError| {
                if let Some(tx) = tx.lock().ok().and_then(|mut tx| tx.take()) {
                    let _ = tx.send(granted.as_bool());
                }
            }),
        );
    rx.await.map_err(|e| e.to_string())
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
#[derive(Default)]
pub struct WindowsActivation(std::sync::Mutex<(bool, Option<Option<Target>>)>);

#[cfg(target_os = "windows")]
fn activation_url(target: Option<&Target>) -> String {
    let mut url = url::Url::parse("chat://open").unwrap();
    if let Some(target) = target {
        url.query_pairs_mut()
            .append_pair("conv", &target.conv_id)
            .append_pair("session", &target.session.to_string());
    }
    url.into()
}

#[cfg(target_os = "windows")]
fn parse_activation_url(raw: &str) -> Option<Option<Target>> {
    let url = url::Url::parse(raw).ok()?;
    if url.scheme() != "chat" || url.host_str() != Some("open")
        || !matches!(url.path(), "" | "/") || url.port().is_some()
        || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some()
    {
        return None;
    }
    let mut conv = None;
    let mut session = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "conv" if conv.is_none() => conv = Some(uuid::Uuid::parse_str(&value).ok()?.to_string()),
            "session" if session.is_none() => session = Some(value.parse::<u64>().ok()?),
            _ => return None,
        }
    }
    match (conv, session) {
        (None, None) => Some(None),
        (Some(conv_id), Some(session)) => Some(Some(Target { conv_id, session })),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
pub fn handle_windows_args(app: &AppHandle, args: Vec<String>) -> bool {
    for arg in args.into_iter().skip(1) {
        if let Some(target) = parse_activation_url(&arg) {
            let state = app.state::<WindowsActivation>();
            let mut state = state.0.lock().unwrap();
            if state.0 {
                drop(state);
                activate(app, target);
            } else {
                state.1 = Some(target);
                crate::tray::reveal(app);
            }
            return true;
        }
    }
    false
}

#[cfg(desktop)]
#[tauri::command]
pub fn notification_ready(app: AppHandle, session: u64) {
    #[cfg(target_os = "windows")]
    {
        let state = app.state::<WindowsActivation>();
        let mut state = state.0.lock().unwrap();
        state.0 = true;
        let pending = state.1.take();
        drop(state);
        if let Some(target) = pending {
            activate(&app, target.map(|mut target| { target.session = session; target }));
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (app, session);
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
    use windows::{core::HSTRING, Data::Xml::Dom::XmlDocument, UI::Notifications::{ToastNotification, ToastNotificationManager}};
    let show = || -> windows::core::Result<()> {
        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from("<toast activationType=\"protocol\"><visual><binding template=\"ToastGeneric\"><text/><text/></binding></visual></toast>"))?;
        xml.DocumentElement()?.SetAttribute(&HSTRING::from("launch"), &HSTRING::from(activation_url(target.as_ref())))?;
        let texts = xml.GetElementsByTagName(&HSTRING::from("text"))?;
        texts.Item(0)?.AppendChild(&xml.CreateTextNode(&HSTRING::from(title))?)?;
        texts.Item(1)?.AppendChild(&xml.CreateTextNode(&HSTRING::from(body))?)?;
        let toast = ToastNotification::CreateToastNotification(&xml)?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(windows_app_id(&app)))?.Show(&toast)
    };
    show().map_err(|e| e.to_string())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn activation_urls_round_trip() {
        let target = Target { conv_id: "6ae4331a-e649-4f6a-95e5-8422dfe4ba80".into(), session: 42 };
        let parsed = parse_activation_url(&activation_url(Some(&target))).unwrap().unwrap();
        assert_eq!(parsed.conv_id, target.conv_id);
        assert_eq!(parsed.session, target.session);
        assert!(matches!(parse_activation_url(&activation_url(None)), Some(None)));
    }

    #[test]
    fn rejects_invalid_activation_urls() {
        for raw in ["", "https://open", "chat://other", "chat://open/path", "chat://open?conv=bad&session=1", "chat://open?session=1", "chat://open?session=x", "chat://open?session=1&session=2", "chat://open?extra=1", "chat://open#fragment", "chat://user@open"] {
            assert!(parse_activation_url(raw).is_none(), "{raw}");
        }
    }
}
