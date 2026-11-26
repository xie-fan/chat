//! 在线更新（仅桌面）：更新地址跟着用户填的服务器走，只能运行时设置。
//! 服务器可能是明文 HTTP（局域网），安全性靠安装包签名（tauri.conf.json 里的 pubkey）。
use tauri_plugin_updater::UpdaterExt;
use url::Url;

const DOWNLOAD_PATH: &str = "/v1/client/download/";

/// 检查并安装；没有更新返回 false。Windows 下安装器启动后本进程会退出。
#[tauri::command]
pub async fn install_update(app: tauri::AppHandle, server: String) -> Result<bool, String> {
    let server = Url::parse(server.trim()).map_err(|e| format!("服务器地址无效：{e}"))?;
    if !matches!(server.scheme(), "https" | "http")
        || server.host_str().is_none()
        || !server.username().is_empty()
        || server.password().is_some()
        || server.query().is_some()
        || server.fragment().is_some()
    {
        return Err("服务器地址无效".into());
    }
    let url = format!(
        "{}/v1/client/update/{{{{target}}}}/{{{{arch}}}}/{{{{current_version}}}}?bundle={{{{bundle_type}}}}",
        server.as_str().trim_end_matches('/')
    );
    let endpoint = url.parse().map_err(|e| format!("服务器地址无效：{e}"))?;
    let update = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?;
    let Some(mut update) = update else {
        return Ok(false);
    };
    update.download_url = server_download_url(server, &update.download_url)?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
}

/// 老版本服务端可能仍返回 public_url。安装包始终由本次检查更新的服务器提供。
fn server_download_url(mut server: Url, advertised: &Url) -> Result<Url, String> {
    let asset = advertised
        .path()
        .rsplit_once(DOWNLOAD_PATH)
        .map(|(_, asset)| asset)
        .filter(|asset| !asset.is_empty() && !asset.contains('/'))
        .ok_or_else(|| "更新安装包地址无效".to_string())?;
    server.set_path(&format!(
        "{}{DOWNLOAD_PATH}{asset}",
        server.path().trim_end_matches('/')
    ));
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_download_uses_the_configured_server_and_prefix() {
        let server = Url::parse("https://lan.example:8443/chat/").unwrap();
        let advertised =
            Url::parse("https://public.example/v1/client/download/Chat%20Setup.exe").unwrap();
        assert_eq!(
            server_download_url(server, &advertised).unwrap().as_str(),
            "https://lan.example:8443/chat/v1/client/download/Chat%20Setup.exe"
        );
    }
}
