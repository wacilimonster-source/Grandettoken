/// 优惠列表每半小时刷新，复用连接池并把客户端初始化错误交给界面。
static PROMO_HTTP: std::sync::OnceLock<Result<reqwest::Client, String>> =
    std::sync::OnceLock::new();

fn promo_client() -> Result<&'static reqwest::Client, String> {
    PROMO_HTTP
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "优惠网络客户端初始化失败".into())
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// 官网公开优惠列表的某一页，由后端拼固定地址。页码只用于取更早的优惠。
#[tauri::command]
pub(crate) async fn fetch_promo_page(page: u32) -> Result<String, String> {
    const MAX_BYTES: usize = 2_000_000;
    if !(1..=100).contains(&page) {
        return Err("优惠页码超出范围".into());
    }
    let url = if page == 1 {
        "https://www.aipromonow.com/deals".to_string()
    } else {
        format!("https://www.aipromonow.com/deals?page={page}")
    };
    let client = promo_client()?;
    let mut response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| if e.is_timeout() { "连接优惠来源超时" } else { "无法连接优惠来源" })?;
    if !response.status().is_success() {
        return Err(format!("优惠来源返回 HTTP {}", response.status().as_u16()));
    }
    if response.content_length().unwrap_or(0) > MAX_BYTES as u64 {
        return Err("优惠页面内容过大".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "读取优惠页面失败")? {
        if chunk.len() > MAX_BYTES - bytes.len() {
            return Err("优惠页面内容过大".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| "优惠页面编码无效".into())
}

#[tauri::command]
pub(crate) async fn open_promo_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let parsed = reqwest::Url::parse(&url).map_err(|_| "地址无效")?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("www.aipromonow.com") || !parsed.path().starts_with("/promos/") {
        return Err("不允许打开这个地址".into());
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(url, None::<&str>).map_err(|e| format!("打开失败: {e}"))
}
