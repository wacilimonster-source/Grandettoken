//! 从两端的正式版清单读取可分享的安装包直链。

use serde_json::Value;
use tokenscope_core::release_links::{parse_link, RELEASES};
use std::time::Duration;

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
