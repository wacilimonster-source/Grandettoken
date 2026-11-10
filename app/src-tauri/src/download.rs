//! 从两端的正式版清单读取可分享的安装包直链。

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::Value;
use std::time::Duration;

const RELEASES: &str = "https://github.com/wacilimonster-source/Grandettoken/releases";

/// 清单解析用的 HTTP 客户端:**进程内复用一份**。参数与错误文案逐字不变,
/// 只是不再每次点「下载」都重建连接池与 TLS 上下文。
fn release_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|e| format!("创建下载连接失败: {e}"))
        })
        .as_ref()
        .map_err(|e| e.clone())
}

#[tauri::command]
pub async fn download_link(platform: String) -> Result<String, String> {
    let path = match platform.as_str() {
        "windows" => "latest/download/latest.json",
        "android" => "download/android-stable/android-latest.json",
        _ => return Err("不支持的下载平台".into()),
    };
    let client = release_client()?;
    let manifest = client
        .get(format!("{RELEASES}/{path}"))
        .send()
        .await
        .map_err(|e| format!("获取正式版下载链接失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("正式版下载暂不可用: {e}"))?
        .json::<Value>()
        .await
        .map_err(|e| format!("读取下载清单失败: {e}"))?;
    parse_link(&platform, manifest)
}

fn parse_link(platform: &str, manifest: Value) -> Result<String, String> {
    let invalid = || "下载清单中没有有效的正式版安装包直链".to_string();
    let (version, url, expected) = match platform {
        "windows" => {
            let version = manifest["version"].as_str().ok_or_else(invalid)?;
            let url = manifest["platforms"]["windows-x86_64"]["url"]
                .as_str()
                .ok_or_else(invalid)?;
            let expected =
                format!("{RELEASES}/download/v{version}/Grandettoken_{version}_x64-setup.exe");
            (version.to_string(), url.to_string(), expected)
        }
        "android" => {
            // Android 使用独立的 stable 指针，payload 为 base64url 编码的 JSON。
            let bytes = URL_SAFE_NO_PAD
                .decode(manifest["payload"].as_str().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            let payload: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            if payload["channel"] != "stable"
                || payload["applicationId"] != "com.wacil.grandettoken.sync"
            {
                return Err(invalid());
            }
            let version = payload["versionName"].as_str().ok_or_else(invalid)?;
            let artifact = payload["artifacts"]
                .as_array()
                .ok_or_else(invalid)?
                .iter()
                .find(|a| a["abi"] == "universal")
                .ok_or_else(invalid)?;
            let url = artifact["downloadUrl"].as_str().ok_or_else(invalid)?;
            let expected = format!(
                "{RELEASES}/download/android-v{version}/Grandettoken-android-v{version}.apk"
            );
            (version.to_string(), url.to_string(), expected)
        }
        _ => return Err("不支持的下载平台".into()),
    };
    if version.is_empty()
        || !version.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || url != expected
    {
        return Err(invalid());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn android_manifest(url: &str) -> Value {
        let payload = json!({
            "channel": "stable", "applicationId": "com.wacil.grandettoken.sync",
            "versionName": "0.1.8",
            "artifacts": [{ "abi": "universal", "downloadUrl": url }]
        });
        json!({ "payload": URL_SAFE_NO_PAD.encode(payload.to_string()) })
    }

    #[test]
    fn windows_uses_manifest_version() {
        let url = format!("{RELEASES}/download/v0.3.10/Grandettoken_0.3.10_x64-setup.exe");
        let manifest = json!({ "version": "0.3.10", "platforms": {
            "windows-x86_64": { "url": url }
        }});
        assert_eq!(parse_link("windows", manifest).unwrap(), url);
    }

    #[test]
    fn android_uses_its_own_stable_payload() {
        let url = format!("{RELEASES}/download/android-v0.1.8/Grandettoken-android-v0.1.8.apk");
        assert_eq!(parse_link("android", android_manifest(&url)).unwrap(), url);
    }

    #[test]
    fn rejects_missing_or_malformed_payload() {
        assert!(parse_link("windows", json!({})).is_err());
        assert!(parse_link("android", json!({ "payload": "!invalid" })).is_err());
        assert!(parse_link(
            "android",
            json!({ "payload": URL_SAFE_NO_PAD.encode("{}") })
        )
        .is_err());
    }

    #[test]
    fn rejects_unexpected_download_destinations() {
        assert!(parse_link("android", android_manifest("https://example.com/app.apk")).is_err());
        let wrong_version =
            format!("{RELEASES}/download/android-v0.1.7/Grandettoken-android-v0.1.7.apk");
        assert!(parse_link("android", android_manifest(&wrong_version)).is_err());
    }
}
