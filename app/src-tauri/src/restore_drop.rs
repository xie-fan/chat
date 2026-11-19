use std::{path::PathBuf, sync::Mutex};

use reqwest::multipart::{Form, Part};
use tauri::{ipc::Channel, State, Window};
use tokio::io::AsyncReadExt;
use url::Url;
use uuid::Uuid;

use crate::file_drop::{open_regular_file, DroppedFiles};

const UNREADABLE: &str = "无法读取拖入的备份文件";

#[derive(Default)]
pub struct SelectedBackup(Mutex<Option<(Uuid, PathBuf)>>);

#[tauri::command]
pub async fn select_dropped_backup(
    window: Window,
    dropped: State<'_, DroppedFiles>,
    selected: State<'_, SelectedBackup>,
    path: PathBuf,
) -> Result<Uuid, String> {
    if !dropped.take(&window, &path) || !path.is_file() {
        return Err(UNREADABLE.into());
    }
    let id = Uuid::new_v4();
    *selected.0.lock().unwrap() = Some((id, path));
    Ok(id)
}

#[tauri::command]
pub async fn restore_dropped_backup(
    window: Window,
    selected: State<'_, SelectedBackup>,
    id: Uuid,
    server: String,
    access: String,
    progress: Channel<u8>,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err(UNREADABLE.into());
    }
    let path = selected
        .0
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(key, _)| *key == id)
        .map(|(_, path)| path.clone())
        .ok_or(UNREADABLE)?;
    let endpoint = restore_url(&server).ok_or("服务器地址无效")?;
    let (file, len) = tauri::async_runtime::spawn_blocking(move || open_regular_file(&path))
        .await
        .map_err(|_| UNREADABLE)??;
    let chunks = futures_util::stream::try_unfold(
        (tokio::fs::File::from_std(file).take(len), 0u64, progress),
        move |(mut file, sent, progress)| async move {
            let mut chunk = vec![0; 64 * 1024];
            let read = file.read(&mut chunk).await?;
            if read == 0 {
                if sent == len {
                    return Ok(None);
                }
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
            }
            chunk.truncate(read);
            let next = sent + read as u64;
            let percent = |bytes| (u128::from(bytes) * 100 / u128::from(len)) as u8;
            if percent(sent) != percent(next) {
                let _ = progress.send(percent(next));
            }
            Ok(Some((chunk, (file, next, progress))))
        },
    );
    let part = Part::stream_with_length(reqwest::Body::wrap_stream(chunks), len)
        .file_name("backup.tar.gz");
    let form = Form::new().part("file", part).text("confirm", "RESTORE");
    let _ = rustls::crypto::ring::default_provider().install_default();
    let response = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "网络错误")?
        .post(endpoint)
        .bearer_auth(access)
        .multipart(form)
        .send()
        .await
        .map_err(|_| "网络错误")?;
    if response.status().is_success() {
        return Ok(());
    }
    let failure = response.json::<serde_json::Value>().await.ok();
    Err(failure
        .as_ref()
        .and_then(|body| body["error"].as_str())
        .unwrap_or("恢复失败")
        .to_string())
}

fn restore_url(server: &str) -> Option<Url> {
    let base = Url::parse(server).ok()?;
    if !matches!(base.scheme(), "http" | "https")
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return None;
    }
    Url::parse(&format!(
        "{}/v1/admin/restore",
        base.as_str().trim_end_matches('/')
    ))
    .ok()
}

#[cfg(test)]
mod tests {
    use super::restore_url;

    #[test]
    fn restore_upload_stays_on_the_configured_http_server_and_prefix() {
        let url = restore_url("https://chat.example.org/prefix/").unwrap();
        assert_eq!(
            url.as_str(),
            "https://chat.example.org/prefix/v1/admin/restore"
        );
        for invalid in [
            "file:///backup",
            "https://user@chat.example.org",
            "https://chat.example.org/#x",
        ] {
            assert!(restore_url(invalid).is_none());
        }
    }
}
