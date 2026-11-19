use gtk::prelude::WidgetExt;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::{ipc::Response, AppHandle, DragDropEvent, Manager, State, Window, WindowEvent};

const MAX_BYTES: u64 = 25 * 1024 * 1024;
const TOO_LARGE: &str = "文件大小不能超过 25 MB";
const UNREADABLE: &str = "无法读取拖入的文件";

#[derive(Default)]
pub struct DroppedFiles(Mutex<HashSet<PathBuf>>);

#[derive(Default)]
pub struct DroppedLinks {
    pending: Mutex<HashMap<String, String>>,
    dropped: Mutex<HashMap<String, String>>,
}

pub fn listen_for_link_uris(app: &AppHandle) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    let app = app.clone();
    window.with_webview(move |webview| {
        webview
            .inner()
            .connect_drag_data_received(move |_, _, _, _, data, info, _| {
                // Wry's URI-list GTK target is 2; it percent-decodes these URIs into paths.
                if info == 2 {
                    let links = data
                        .uris()
                        .into_iter()
                        .filter_map(|uri| link_pair(&uri))
                        .collect();
                    *app.state::<DroppedLinks>().pending.lock().unwrap() = links;
                }
            });
    })
}

fn link_pair(uri: &str) -> Option<(String, String)> {
    let url = url::Url::parse(uri).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let decoded = percent_encoding::percent_decode_str(uri)
        .decode_utf8_lossy()
        .into_owned();
    Some((decoded, uri.to_owned()))
}

impl DroppedFiles {
    pub fn take(&self, window: &Window, path: &Path) -> bool {
        window.label() == "main" && self.0.lock().unwrap().remove(path)
    }
}

pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if window.label() != "main" {
        return;
    }
    if let WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) = event {
        let state = window.state::<DroppedFiles>();
        *state.0.lock().unwrap() = paths.iter().filter(|p| p.is_absolute()).cloned().collect();
        let links = window.state::<DroppedLinks>();
        let mut pending = std::mem::take(&mut *links.pending.lock().unwrap());
        *links.dropped.lock().unwrap() = paths
            .iter()
            .filter_map(|path| {
                let decoded = path.to_str()?;
                pending
                    .remove(decoded)
                    .map(|original| (decoded.to_owned(), original))
            })
            .collect();
    } else if let WindowEvent::DragDrop(DragDropEvent::Leave) = event {
        window
            .state::<DroppedLinks>()
            .pending
            .lock()
            .unwrap()
            .clear();
    }
}

#[tauri::command]
pub fn take_dropped_link(
    window: Window,
    links: State<'_, DroppedLinks>,
    path: String,
) -> Option<String> {
    if window.label() != "main" {
        return None;
    }
    links.dropped.lock().unwrap().remove(&path)
}

#[tauri::command]
pub async fn read_dropped_file(
    window: Window,
    dropped: State<'_, DroppedFiles>,
    path: PathBuf,
) -> Result<Response, String> {
    if !dropped.take(&window, &path) {
        return Err(UNREADABLE.into());
    }
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        let bytes = read_file(&path)?;
        let (kind, _) = gio::content_type_guess(Some(&path), &bytes[..bytes.len().min(4096)]);
        let mime = gio::content_type_get_mime_type(&kind);
        let mut response = mime.as_deref().unwrap_or("").as_bytes().to_vec();
        response.push(0);
        response.extend(bytes);
        Ok::<_, &str>(response)
    })
    .await
    .map_err(|_| UNREADABLE)??;
    Ok(Response::new(bytes))
}

pub fn open_regular_file(path: &Path) -> Result<(File, u64), &'static str> {
    if !path.is_file() {
        return Err(UNREADABLE);
    }
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| UNREADABLE)?;
    let metadata = file.metadata().map_err(|_| UNREADABLE)?;
    if !metadata.is_file() {
        return Err(UNREADABLE);
    }
    Ok((file, metadata.len()))
}

fn read_file(path: &Path) -> Result<Vec<u8>, &'static str> {
    let (file, len) = open_regular_file(path)?;
    if len > MAX_BYTES {
        return Err(TOO_LARGE);
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UNREADABLE)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(TOO_LARGE);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_escaped_url_delimiters_separately_from_the_wry_path() {
        let raw = "https://example.org/?q=one%26two%23three#section";
        let (decoded, original) = link_pair(raw).unwrap();
        assert_eq!(decoded, "https://example.org/?q=one&two#three#section");
        assert_eq!(original, raw);
        assert!(link_pair("file:///tmp/backup.tar.gz").is_none());
    }

    #[test]
    fn reads_regular_files_and_rejects_oversized_files() {
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::write(&path, b"file").unwrap();
        let small = read_file(&path);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        let large = read_file(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(small.unwrap(), b"file");
        assert_eq!(large.unwrap_err(), TOO_LARGE);
    }
}
