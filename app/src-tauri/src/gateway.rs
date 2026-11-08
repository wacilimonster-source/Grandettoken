use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::Manager;
use tokenscope_core::{models, secrets, store::Store};

const CONFIG_KEY: &str = "local_gateway_v1";
const SECRET_KEY: &str = "local-gateway-access";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Published {
    vault_id: String,
    source: String,
    alias: String,
    host: String,
    models: Vec<String>,
    updated_at: i64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    port: u16,
    lan: bool,
    auth: bool,
    channels: Vec<Published>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            port: 18771,
            lan: false,
            auth: true,
            channels: vec![],
        }
    }
}
struct Inner {
    settings: Settings,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}
#[derive(Clone)]
pub struct Gateway {
    store: Arc<Mutex<Store>>,
    inner: Arc<Mutex<Inner>>,
    client: reqwest::Client,
    lifecycle: Arc<tokio::sync::Mutex<()>>,
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
impl Gateway {
    pub fn new(store: Arc<Mutex<Store>>) -> Self {
        let settings = lock(&store)
            .get_setting(CONFIG_KEY)
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        Self {
            store,
            inner: Arc::new(Mutex::new(Inner {
                settings,
                shutdown: None,
            })),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .build()
                .expect("HTTP client"),
            lifecycle: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
    fn save(&self, settings: &Settings) -> Result<(), String> {
        lock(&self.store).set_setting(
            CONFIG_KEY,
            &serde_json::to_string(settings).map_err(|e| e.to_string())?,
        )
    }
    pub fn remove_vault(&self, id: &str) -> Result<(), String> {
        let mut inner = lock(&self.inner);
        let mut next = inner.settings.clone();
        next.channels.retain(|c| c.vault_id != id);
        self.save(&next)?;
        inner.settings = next;
        Ok(())
    }
    pub fn invalidate_vault(&self, id: &str) -> Result<Option<(String, String)>, String> {
        let mut inner = lock(&self.inner);
        let mut next = inner.settings.clone();
        let pair = next
            .channels
            .iter_mut()
            .find(|c| c.vault_id == id)
            .map(|c| {
                c.updated_at = -1;
                (c.source.clone(), c.alias.clone())
            });
        if pair.is_some() {
            self.save(&next)?;
            inner.settings = next;
        }
        Ok(pair)
    }
    fn live_channels(&self) -> Vec<Published> {
        let channels = lock(&self.inner).settings.channels.clone();
        let vault = match lock(&self.store).vault_list() {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        channels
            .into_iter()
            .filter(|c| {
                vault
                    .iter()
                    .any(|v| v.id == c.vault_id && v.updated_at == c.updated_at)
            })
            .collect()
    }
}
fn token() -> Result<String, String> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).map_err(|e| e.to_string())?;
    Ok(format!(
        "gw_{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    ))
}
fn secret() -> Result<String, String> {
    match secrets::get(SECRET_KEY)? {
        Some(v) if !v.is_empty() => Ok(v),
        _ => {
            let s = token()?;
            secrets::set(SECRET_KEY, &s)?;
            Ok(s)
        }
    }
}
fn error(status: StatusCode, msg: &str) -> Response {
    (
        status,
        Json(json!({"error":{"message":msg,"type":"gateway_error"}})),
    )
        .into_response()
}
fn authorized(headers: &HeaderMap, settings: &Settings) -> bool {
    if headers.contains_key(header::ORIGIN) {
        return false;
    }
    if !settings.auth && !settings.lan {
        return true;
    }
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    let expected = match secrets::get(SECRET_KEY) {
        Ok(Some(s)) => s,
        _ => return false,
    };
    if provided.len() != expected.len() || provided.is_empty() {
        return false;
    }
    provided
        .bytes()
        .zip(expected.bytes())
        .fold(0u8, |a, (x, y)| a | (x ^ y))
        == 0
}
fn model_id(c: &Published, id: &str) -> String {
    format!("{}/{}/{}", c.source, c.alias, id)
}
fn peer_allowed(peer: SocketAddr, settings: &Settings) -> bool {
    match peer.ip() {
        IpAddr::V4(ip) => {
            ip.is_loopback() || (settings.lan && (ip.is_private() || ip.is_link_local()))
        }
        _ => false,
    }
}
async fn list(
    State(g): State<Gateway>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let settings = lock(&g.inner).settings.clone();
    if !peer_allowed(peer, &settings) {
        return error(StatusCode::FORBIDDEN, "仅允许本机或局域网访问");
    }
    if !authorized(&headers, &settings) {
        return error(StatusCode::UNAUTHORIZED, "网关密钥无效");
    }
    let data: Vec<Value> = g
        .live_channels()
        .iter()
        .flat_map(|c| {
            c.models
                .iter()
                .map(|id| json!({"id":model_id(c,id),"object":"model","owned_by":c.source}))
                .collect::<Vec<_>>()
        })
        .collect();
    Json(json!({"object":"list","data":data})).into_response()
}
async fn chat(
    State(g): State<Gateway>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let settings = lock(&g.inner).settings.clone();
    if !peer_allowed(peer, &settings) {
        return error(StatusCode::FORBIDDEN, "仅允许本机或局域网访问");
    }
    if !authorized(&headers, &settings) {
        return error(StatusCode::UNAUTHORIZED, "网关密钥无效");
    }
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
        .unwrap_or(false)
    {
        return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "需要 application/json");
    }
    let mut data: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return error(StatusCode::BAD_REQUEST, "请求必须是 JSON"),
    };
    let model = match data.get("model").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => return error(StatusCode::BAD_REQUEST, "缺少 model"),
    };
    let found = g.live_channels().into_iter().find_map(|c| {
        c.models
            .iter()
            .find(|id| model_id(&c, id) == model)
            .cloned()
            .map(|id| (c, id))
    });
    let (channel, id) = match found {
        Some(v) => v,
        None => {
            return error(
                StatusCode::NOT_FOUND,
                "模型不存在、已下架或密钥已更新，请刷新模型列表",
            )
        }
    };
    let vid = channel.vault_id.clone();
    let key = match tokio::task::spawn_blocking(move || secrets::vault_get(&vid)).await {
        Ok(Ok(Some(k))) => k,
        _ => return error(StatusCode::SERVICE_UNAVAILABLE, "上游密钥不可用"),
    };
    data["model"] = Value::String(id);
    let base = match channel.source.as_str() {
        "gemini" => "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
        "inkstone" => "https://discovery-api.intern-ai.org.cn/v1".to_string(),
        "hapi" => "https://ai.yuchuantest.com/v1".to_string(),
        _ => return error(StatusCode::NOT_FOUND, "渠道不受支持"),
    };
    let req = g
        .client
        .post(format!("{base}/chat/completions"))
        .bearer_auth(key)
        .json(&data);
    let upstream = match req.send().await {
        Ok(r) => r,
        Err(_) => return error(StatusCode::BAD_GATEWAY, "无法连接上游服务"),
    };
    let status = upstream.status();
    if !status.is_success() {
        return error(status, "上游服务拒绝请求，请检查密钥、模型权限或额度");
    }
    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    if let Some(v) = content_type {
        response.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    response
}
fn local_ips() -> Vec<Value> {
    let mut ips: Vec<Value> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| {
            let ip = iface.ip();
            match ip {
                IpAddr::V4(v) if !v.is_loopback() && !v.is_unspecified() && !v.is_link_local() => {
                    Some(json!({"address":v.to_string(),"name":iface.name}))
                }
                _ => None,
            }
        })
        .collect();
    ips.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());
    ips
}
#[tauri::command]
pub async fn gateway_status(app: tauri::AppHandle) -> Result<Value, String> {
    let g = app.state::<Gateway>();
    let vault = lock(&g.store).vault_list()?;
    let (settings, running) = {
        let mut inner = lock(&g.inner);
        let mut next = inner.settings.clone();
        next.channels
            .retain(|c| vault.iter().any(|v| v.id == c.vault_id));
        if next.channels.len() != inner.settings.channels.len() {
            g.save(&next)?;
            inner.settings = next;
        }
        (inner.settings.clone(), inner.shutdown.is_some())
    };
    let channels:Vec<Value>=settings.channels.iter().map(|c|{let v=vault.iter().find(|v|v.id==c.vault_id);json!({"vaultId":c.vault_id,"source":c.source,"alias":c.alias,"label":v.map(|v|v.label.clone()).unwrap_or_default(),"count":c.models.len(),"models":c.models.iter().map(|m|model_id(c,m)).collect::<Vec<_>>(),"stale":v.map(|v|v.updated_at != c.updated_at).unwrap_or(true)})}).collect();
    Ok(
        json!({"port":settings.port,"lan":settings.lan,"auth":settings.auth,"running":running,"lanIps":local_ips(),"channels":channels,"vault":vault}),
    )
}
#[tauri::command]
pub async fn gateway_start(
    app: tauri::AppHandle,
    port: u16,
    lan: bool,
    auth: bool,
) -> Result<Value, String> {
    let g = app.state::<Gateway>();
    let _guard = g.lifecycle.lock().await;
    if port == 0 {
        return Err("端口必须在 1 到 65535 之间".into());
    }
    if lan && !auth {
        return Err("局域网访问必须启用网关密钥".into());
    }
    if lock(&g.inner).shutdown.is_some() {
        return Err("请先停止网关再修改设置".into());
    }
    if auth {
        secret()?;
    }
    let addr = SocketAddr::new(
        IpAddr::V4(if lan {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        }),
        port,
    );
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("无法监听端口 {port}: {e}"))?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let mut inner = lock(&g.inner);
        let mut next = inner.settings.clone();
        next.port = port;
        next.lan = lan;
        next.auth = auth;
        g.save(&next)?;
        inner.settings = next;
        inner.shutdown = Some(tx);
    }
    let router = Router::new()
        .route("/v1/models", get(list))
        .route("/v1/chat/completions", post(chat))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .with_state(g.inner().clone());
    tauri::async_runtime::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = rx.await;
        })
        .await;
    });
    drop(_guard);
    gateway_status(app).await
}
#[tauri::command]
pub async fn gateway_stop(app: tauri::AppHandle) -> Result<(), String> {
    let g = app.state::<Gateway>();
    let _guard = g.lifecycle.lock().await;
    if let Some(tx) = lock(&g.inner).shutdown.take() {
        let _ = tx.send(());
    }
    Ok(())
}
#[tauri::command]
pub async fn gateway_secret(rotate: bool) -> Result<String, String> {
    if rotate {
        let k = token()?;
        secrets::set(SECRET_KEY, &k)?;
        Ok(k)
    } else {
        secret()
    }
}
#[tauri::command]
pub async fn gateway_publish(
    app: tauri::AppHandle,
    vault_id: String,
    source: String,
    alias: String,
) -> Result<(), String> {
    let g = app.state::<Gateway>();
    let alias = alias.trim().to_ascii_lowercase();
    if alias.is_empty()
        || alias.len() > 32
        || !alias
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
    {
        return Err("别名只能包含英文字母、数字、连字符和下划线（最多 32 字）".into());
    }
    if !["hapi", "gemini", "inkstone"].contains(&source.as_str()) {
        return Err("不支持的渠道".into());
    }
    let was_published = lock(&g.inner)
        .settings
        .channels
        .iter()
        .any(|c| c.vault_id == vault_id);
    let item = lock(&g.store)
        .vault_get(&vault_id)?
        .ok_or("密钥箱条目不存在")?;
    let vid = vault_id.clone();
    let key = tokio::task::spawn_blocking(move || secrets::vault_get(&vid))
        .await
        .map_err(|e| e.to_string())??
        .ok_or("密钥不存在")?;
    let (mut ids, host) = if source == "hapi" {
        models::fetch_source_ids(
            &g.client,
            models::find_source("hapi").ok_or("渠道不可用")?,
            Some(&key),
        )
        .await?
    } else {
        let url = if source == "gemini" {
            "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000"
        } else {
            "https://discovery-api.intern-ai.org.cn/v1/models"
        };
        let req = if source == "gemini" {
            g.client.get(url).header("x-goog-api-key", &key)
        } else {
            g.client.get(url).bearer_auth(&key)
        };
        let res = req
            .timeout(Duration::from_secs(25))
            .send()
            .await
            .map_err(|_| "读取模型列表失败，请检查网络")?;
        if !res.status().is_success() {
            return Err(format!("读取模型列表失败（HTTP {}）", res.status()));
        }
        let v: Value = res.json().await.map_err(|_| "模型列表格式无效")?;
        let ids = if source == "gemini" {
            if v.get("nextPageToken").is_some() {
                return Err("Gemini 模型列表有未读取的分页，请稍后重试".into());
            }
            v.get("models")
                .and_then(Value::as_array)
                .ok_or("Gemini 模型列表格式无效")?
                .iter()
                .filter(|m| {
                    m.get("supportedGenerationMethods")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().any(|x| x.as_str() == Some("generateContent")))
                        .unwrap_or(false)
                })
                .filter_map(|m| m.get("name").and_then(Value::as_str))
                .filter_map(|s| s.strip_prefix("models/"))
                .filter(|s| {
                    s.starts_with("gemini-")
                        && !s.contains("image")
                        && !s.contains("tts")
                        && !s.contains("audio")
                        && !s.contains("live")
                })
                .map(str::to_owned)
                .collect()
        } else {
            v.get("data")
                .and_then(Value::as_array)
                .ok_or("书生端砚模型列表格式无效")?
                .iter()
                .filter(|m| m.get("is_ready").and_then(Value::as_bool) != Some(false))
                .filter(|m| {
                    m.get("output_modalities")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .any(|x| x.get("type").and_then(Value::as_str) == Some("text"))
                        })
                        .unwrap_or(true)
                })
                .filter_map(|m| m.get("id").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        };
        (
            ids,
            if source == "gemini" {
                "generativelanguage.googleapis.com".into()
            } else {
                "discovery-api.intern-ai.org.cn".into()
            },
        )
    };
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Err("没有可上架的模型".into());
    }
    if lock(&g.store).vault_get(&vault_id)?.map(|v| v.updated_at) != Some(item.updated_at) {
        return Err("密钥已变化，请重试".into());
    }
    let check_id = vault_id.clone();
    let current = tokio::task::spawn_blocking(move || secrets::vault_get(&check_id))
        .await
        .map_err(|e| e.to_string())??;
    if current.as_deref() != Some(key.as_str()) {
        return Err("密钥已变化，请重试".into());
    }
    let mut inner = lock(&g.inner);
    let mut next = inner.settings.clone();
    if was_published && !next.channels.iter().any(|c| c.vault_id == vault_id) {
        return Err("密钥已下架，请重新上架".into());
    }
    if next
        .channels
        .iter()
        .any(|c| c.source == source && c.alias == alias && c.vault_id != vault_id)
    {
        return Err("同一渠道的别名已存在".into());
    }
    next.channels.retain(|c| c.vault_id != vault_id);
    next.channels.push(Published {
        vault_id,
        source,
        alias,
        host,
        models: ids,
        updated_at: item.updated_at,
    });
    g.save(&next)?;
    inner.settings = next;
    Ok(())
}
#[tauri::command]
pub async fn gateway_unpublish(app: tauri::AppHandle, vault_id: String) -> Result<(), String> {
    let g = app.state::<Gateway>();
    let mut inner = lock(&g.inner);
    let mut next = inner.settings.clone();
    next.channels.retain(|c| c.vault_id != vault_id);
    g.save(&next)?;
    inner.settings = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_are_scoped() {
        let c = Published {
            vault_id: "v".into(),
            source: "gemini".into(),
            alias: "main".into(),
            host: String::new(),
            models: vec![],
            updated_at: 1,
        };
        assert_eq!(
            model_id(&c, "gemini-2.5-flash"),
            "gemini/main/gemini-2.5-flash"
        );
    }
    #[test]
    fn local_no_auth_rejects_browser_origin() {
        let mut s = Settings::default();
        s.auth = false;
        let mut h = HeaderMap::new();
        assert!(authorized(&h, &s));
        h.insert(header::ORIGIN, "https://example.com".parse().unwrap());
        assert!(!authorized(&h, &s));
        s.lan = true;
        assert!(!authorized(&HeaderMap::new(), &s));
    }
    #[test]
    fn only_local_or_private_networks_are_allowed() {
        let mut s = Settings::default();
        assert!(peer_allowed("127.0.0.1:1234".parse().unwrap(), &s));
        assert!(!peer_allowed("192.168.1.10:1234".parse().unwrap(), &s));
        s.lan = true;
        assert!(peer_allowed("192.168.1.10:1234".parse().unwrap(), &s));
        assert!(!peer_allowed("8.8.8.8:1234".parse().unwrap(), &s));
    }
    #[test]
    fn generated_keys_are_distinct() {
        let a = token().unwrap();
        let b = token().unwrap();
        assert!(a.starts_with("gw_"));
        assert_ne!(a, b);
    }
    #[tokio::test]
    async fn http_list_respects_deletion_and_rejects_browser_origin() {
        let file = std::env::temp_dir().join(format!("gateway-test-{}.db", token().unwrap()));
        let store = Arc::new(Mutex::new(Store::open(&file).unwrap()));
        let item = tokenscope_core::store::VaultItem {
            id: "sample".into(),
            platform: "gemini".into(),
            label: "test".into(),
            hint: String::new(),
            note: String::new(),
            api_endpoint: String::new(),
            website: String::new(),
            created_at: 1,
            updated_at: 1,
        };
        lock(&store).vault_upsert(&item).unwrap();
        let g = Gateway::new(store.clone());
        {
            let mut inner = lock(&g.inner);
            inner.settings.auth = false;
            inner.settings.channels.push(Published {
                vault_id: "sample".into(),
                source: "gemini".into(),
                alias: "main".into(),
                host: String::new(),
                models: vec!["test-model".into()],
                updated_at: 1,
            });
        }
        let router = Router::new()
            .route("/v1/models", get(list))
            .route("/v1/chat/completions", post(chat))
            .with_state(g);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://{addr}/v1/models");
        let data: Value = client.get(&url).send().await.unwrap().json().await.unwrap();
        assert_eq!(data["data"][0]["id"], "gemini/main/test-model");
        assert_eq!(
            client
                .get(&url)
                .header("Origin", "https://example.com")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        lock(&store).vault_delete("sample").unwrap();
        let data: Value = client.get(&url).send().await.unwrap().json().await.unwrap();
        assert!(data["data"].as_array().unwrap().is_empty());
        task.abort();
        drop(store);
        let _ = std::fs::remove_file(file);
    }
}
