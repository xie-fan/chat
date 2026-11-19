// 桌面托盘。关窗口只是隐藏，继续收消息通知；只有菜单「退出」才结束进程。
// Android 不编译本模块。

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Window, WindowEvent};

static QUITTING: AtomicBool = AtomicBool::new(false);
static TRAY_HINT_SHOWN: AtomicBool = AtomicBool::new(false);

const TRAY_HINT: &str = "Chat 已最小化到托盘，会继续接收消息";

fn box_err(err: impl std::error::Error + Send + Sync + 'static) -> Box<dyn std::error::Error> {
    Box::new(err)
}

pub fn install(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    {
        use ksni::TrayMethods;
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let tray = LinuxTray(app.clone());
            match tray.spawn().await {
                Ok(handle) => {
                    app.manage(handle);
                }
                Err(_) => {
                    let handle = app.clone();
                    let _ = app.run_on_main_thread(move || {
                        let _ = install_standard(&handle);
                    });
                }
            }
        });
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    install_standard(app)
}

fn install_standard(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>).map_err(box_err)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>).map_err(box_err)?;
    let menu = Menu::with_items(app, &[&show, &quit]).map_err(box_err)?;
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Chat")
        .menu(&menu)
        .show_menu_on_left_click(cfg!(target_os = "linux"))
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => reveal(app),
            "quit" => request_quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if click_shows_window(&event) {
                reveal(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    // build 会把图标登记进应用资源表，这里丢掉局部变量不会卸掉托盘。
    builder.build(app).map_err(box_err)?;
    Ok(())
}

fn click_shows_window(event: &TrayIconEvent) -> bool {
    matches!(
        event,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } | TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        }
    )
}

pub(crate) fn reveal(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

#[cfg(target_os = "linux")]
struct LinuxTray(AppHandle);

#[cfg(target_os = "linux")]
impl LinuxTray {
    fn on_main(&self, action: fn(&AppHandle)) {
        let app = self.0.clone();
        let _ = self.0.run_on_main_thread(move || action(&app));
    }
}

#[cfg(target_os = "linux")]
impl ksni::Tray for LinuxTray {
    fn id(&self) -> String {
        self.0.config().identifier.clone()
    }

    fn title(&self) -> String {
        "Chat".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let Some(icon) = self.0.default_window_icon() else {
            return Vec::new();
        };
        let mut data = icon.rgba().to_vec();
        for pixel in data.as_chunks_mut::<4>().0 {
            pixel.rotate_right(1);
        }
        vec![ksni::Icon {
            width: icon.width() as i32,
            height: icon.height() as i32,
            data,
        }]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.on_main(reveal);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "显示窗口".into(),
                activate: Box::new(|tray: &mut Self| tray.on_main(reveal)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "退出".into(),
                activate: Box::new(|tray: &mut Self| tray.on_main(request_quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// QUITTING 让随后的 CloseRequested 真正关掉窗口。
fn request_quit(app: &AppHandle) {
    if !QUITTING.swap(true, Ordering::SeqCst) {
        app.exit(0);
    }
}

pub fn on_window_event(window: &Window, event: &WindowEvent) {
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    if QUITTING.load(Ordering::SeqCst) {
        return;
    }
    api.prevent_close();
    let _ = window.hide();
    if TRAY_HINT_SHOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        let _ =
            crate::notifications::show_desktop(app, "Chat".into(), TRAY_HINT.into(), None).await;
    });
}
