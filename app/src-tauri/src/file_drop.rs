use std::{
    collections::HashSet,
    fs::File,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::{ipc::Response, DragDropEvent, Manager, State, Window, WindowEvent};

const MAX_BYTES: u64 = 25 * 1024 * 1024;
const TOO_LARGE: &str = "文件大小不能超过 25 MB";
const UNREADABLE: &str = "无法读取拖入的文件";

#[derive(Default)]
pub struct DroppedFiles(Mutex<HashSet<PathBuf>>);

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
    }
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
