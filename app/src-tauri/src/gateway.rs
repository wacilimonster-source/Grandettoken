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
use tokenscope_core::{models, proxy::ProxyClients, secrets, store::Store};

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
    #[serde(default)]
    allowed_models: Option<Vec<String>>,
    #[serde(default = "default_system_proxy")]
    system_proxy: bool,
}
fn default_system_proxy() -> bool { true }
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
            lan: true,
            auth: true,
            channels: vec![],
        }
    }
}
struct Inner {
    settings: Settings,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    startup_error: Option<String>,
}
#[derive(Clone)]
pub struct Gateway {
    store: Arc<Mutex<Store>>,
    inner: Arc<Mutex<Inner>>,
    clients: Arc<ProxyClients>,
    lifecycle: Arc<tokio::sync::Mutex<()>>,
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Gateway {
    pub fn new(store: Arc<Mutex<Store>>) -> Result<Self, String> {
        let settings = lock(&store)
            .get_setting(CONFIG_KEY)
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        // TLS 后端初始化失败这类罕见故障不再 expect 硬退(报告 L16):失败交还调用方,
        // 走 startup_error 通道,余额监控与签到照常,只是本地网关这一项起不来。
        let clients = ProxyClients::new()?;
        Ok(Self {
            store,
            inner: Arc::new(Mutex::new(Inner {
                settings,
                shutdown: None,
                startup_error: None,
            })),
            clients: Arc::new(clients),
            lifecycle: Arc::new(tokio::sync::Mutex::new(())),
        })
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
            .map(|mut c| { if let Some(ref allowed) = c.allowed_models { c.models.retain(|m| allowed.contains(m)); } c })
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
        // 无鉴权本机模式:只认 127.0.0.1 / localhost / ::1 的 Host(报告 L15)。
        // 浏览器同源 GET 不带 Origin,DNS rebinding 后攻击页对攻击域名的请求
        // Host 是攻击者的域名 —— 校验 Host 直接免疫 rebinding 读走 /v1/models。
        let Some(h) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
            return false;
        };
        // IPv6 字面量带方括号,端口在 ] 之后;其余情况端口在最后一个冒号之后
        let host = if let Some(rest) = h.strip_prefix('[') {
            rest.split(']').next().unwrap_or("")
        } else {
            h.rsplit_once(':').map(|(a, _)| a).unwrap_or(h)
        };
        return matches!(host, "127.0.0.1" | "localhost" | "::1");
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
    format!("{}/{}", c.alias, id)
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
    let mut matches = g.live_channels().into_iter().filter_map(|c| {
        c.models.iter().find(|id| model_id(&c, id) == model).cloned().map(|id| (c, id))
    });
    let (channel, id) = match matches.next() {
        Some(v) => v,
        None => return error(StatusCode::NOT_FOUND, "模型不存在、已下架或密钥已更新，请刷新模型列表"),
    };
    if matches.next().is_some() {
        return error(StatusCode::CONFLICT, "多个密钥的别名和模型相同，请下架并使用不同别名重新上架");
    }
    let vid = channel.vault_id.clone();
    let key = match tokio::task::spawn_blocking(move || secrets::vault_get(&vid)).await {
        Ok(Ok(Some(k))) => k,
        _ => return error(StatusCode::SERVICE_UNAVAILABLE, "上游密钥不可用"),
    };
    data["model"] = Value::String(id.clone());
    let is_gemini = channel.source == "gemini";
    let base = match channel.source.as_str() {
        "gemini" => "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
        "inkstone" => "https://discovery-api.intern-ai.org.cn/v1".to_string(),
        "hapi" => "https://ai.yuchuantest.com/v1".to_string(),
        _ => return error(StatusCode::NOT_FOUND, "渠道不受支持"),
    };
    let client = match g.clients.client(channel.system_proxy) {
        Ok(client) => client,
        Err(_) => return error(StatusCode::BAD_GATEWAY, "无法初始化上游代理连接"),
    };
    if is_gemini {
        sanitize_gemini_body(&mut data, &id);
    }
    // 修复重试上限 1 次,与 eatgemini 的 maxParamRepairsPerRequest 对齐;
    // 修复必须发生在任何字节转发给客户端之前(400 在响应头阶段就能拿到),所以安全。
    let mut repairs = 0u8;
    let upstream = loop {
        let req = client
            .post(format!("{base}/chat/completions"))
            .bearer_auth(key.clone())
            .json(&data);
        let upstream = match req.send().await {
            Ok(r) => r,
            Err(_) => return error(StatusCode::BAD_GATEWAY, "无法连接上游服务"),
        };
        let status = upstream.status();
        if !status.is_success() {
            // 上游 4xx/5xx 的详情原样透传(截断),别替换成一句固定文案(报告 L15):
            // 「密钥失效」与「额度耗尽」的处置完全不同,拿不到 body 就得去抓包才能排查。
            let retry_after = upstream.headers().get(header::RETRY_AFTER).cloned();
            let body = upstream.text().await.unwrap_or_default();
            if is_gemini && status.as_u16() == 400 && repairs < 1 {
                if let Some(params) = gemini_repairable_params(&body) {
                    if let Some(obj) = data.as_object_mut() {
                        for p in &params {
                            obj.remove(p);
                        }
                    }
                    repairs += 1;
                    continue;
                }
            }
            let short: String = body.chars().take(400).collect();
            let mut resp = error(
                status,
                &format!(
                    "上游返回 {} · {}",
                    status.as_u16(),
                    if short.is_empty() { "上游服务拒绝请求，请检查密钥、模型权限或额度" } else { &short }
                ),
            );
            if let Some(ra) = retry_after {
                resp.headers_mut().insert(header::RETRY_AFTER, ra);
            }
            return resp;
        }
        break upstream;
    };
    let status = upstream.status();
    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    if let Some(v) = content_type {
        response.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    response
}
fn advertised_lan_ip(ip: Ipv4Addr) -> bool {
    ip.is_private()
}
// ───────────── Gemini 兼容层参数治理(与 eatgemini 同款)─────────────
// eatgemini 对「Gemini 渠道 400」的两道防线,这里照搬:
// 1) 发送前清理(sanitize_gemini_body):兼容层只认 OpenAI 标准字段的子集,
//    客户端(SillyTavern 等)自带的 seed/logit_bias/logprobs、Gemini 3 系
//    不吃的采样参数与惩罚项,透传过去就是 400 —— 干脆不发。
// 2) 400 修复重试(gemini_repairable_params):错误体点名可修参数或报
//    "penalty" 时剥掉重试一次;不可修的 400 原样透传,不吞错误。
/// 输出 token 上限。eatgemini 对 Gemini 各系统一按 65536 封顶,超限直接 400。
const GEMINI_OUTPUT_LIMIT: u64 = 65536;
/// 400 时可以剥掉重试的参数(eatgemini classify.dart 的 _repairableParams)。
const GEMINI_REPAIRABLE: [&str; 10] = [
    "temperature",
    "top_p",
    "top_k",
    "max_tokens",
    "max_completion_tokens",
    "stop",
    "reasoning_effort",
    "seed",
    "frequency_penalty",
    "presence_penalty",
];
/// 发送前清理请求体(eatgemini router_engine._buildBody 的剥离清单)。
fn sanitize_gemini_body(data: &mut Value, model: &str) {
    let Some(obj) = data.as_object_mut() else { return };
    // 兼容层未收录/明确不支持的字段:一律不透传(未知字段不发、seed 永远不发)
    for k in [
        "seed",
        "logit_bias",
        "logprobs",
        "top_logprobs",
        "n",
        "user",
        "metadata",
        "parallel_tool_calls",
    ] {
        obj.remove(k);
    }
    // stream_options 只在流式请求里有意义(OpenAI 语义:非流式带上会被拒)
    if obj.get("stream") != Some(&Value::Bool(true)) {
        obj.remove("stream_options");
    }
    // max_completion_tokens → max_tokens:兼容层统一吃后者(eatgemini 只发 max_tokens)
    if let Some(v) = obj.remove("max_completion_tokens") {
        obj.entry("max_tokens".to_string()).or_insert(v);
    }
    // 输出上限压到封顶值(eatgemini clamp:max_tokens),避免「must be at most」400
    if obj.get("max_tokens").and_then(Value::as_u64).unwrap_or(0) > GEMINI_OUTPUT_LIMIT {
        obj.insert("max_tokens".to_string(), Value::from(GEMINI_OUTPUT_LIMIT));
    }
    // reasoning_effort 只在 2.5/3 系有效,档位按模型家族放行
    // (eatgemini:3 系 flash = low/medium/high,flash-lite = low/high,其余剥离)
    if model.starts_with("gemini-2.5") || model.starts_with("gemini-3") {
        let efforts: &[&str] = if model.starts_with("gemini-3") && model.contains("flash-lite") {
            &["low", "high"]
        } else {
            &["low", "medium", "high"]
        };
        let bad = obj
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .map(|v| !efforts.contains(&v))
            .unwrap_or(false);
        if bad {
            obj.remove("reasoning_effort");
        }
    } else {
        obj.remove("reasoning_effort");
    }
    // Gemini 3 系不吃采样参数与惩罚项(实测 "Penalty is not enabled for this model"
    // 等 400;eatgemini 对 3.x 的 stripParams = temperature/top_p/top_k)
    if model.starts_with("gemini-3") {
        for k in [
            "temperature",
            "top_p",
            "top_k",
            "frequency_penalty",
            "presence_penalty",
        ] {
            obj.remove(k);
        }
    }
}
/// 从 Gemini 兼容层的 400 错误体里找出可修复参数( eatgemini classify.dart 同款):
/// 兼容层报 OpenAI 风格错误时带 param 字段;惩罚项的报错不带 param
/// ("Penalty is not enabled for this model"),用 message 关键词一次剥两个。
/// 找不到可修的东西返回 None,调用方原样透传给客户端。
fn gemini_repairable_params(err_body: &str) -> Option<Vec<String>> {
    let v: Value = serde_json::from_str(err_body).ok()?;
    let err = v.get("error")?;
    if let Some(p) = err.get("param").and_then(Value::as_str) {
        if GEMINI_REPAIRABLE.contains(&p) {
            return Some(vec![p.to_string()]);
        }
    }
    let msg = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if msg.contains("penalty") {
        return Some(vec![
            "frequency_penalty".to_string(),
            "presence_penalty".to_string(),
        ]);
    }
    None
}
fn local_ips() -> Vec<Value> {
    let mut ips: Vec<Value> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| {
            let ip = iface.ip();
            match ip {
                IpAddr::V4(v) if advertised_lan_ip(v) => {
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
    let (settings, running, startup_error) = {
        let mut inner = lock(&g.inner);
        let mut next = inner.settings.clone();
        next.channels
            .retain(|c| vault.iter().any(|v| v.id == c.vault_id));
        if next.channels.len() != inner.settings.channels.len() {
            g.save(&next)?;
            inner.settings = next;
        }
        (inner.settings.clone(), inner.shutdown.is_some(), inner.startup_error.clone())
    };
    let channels:Vec<Value>=settings.channels.iter().map(|c|{let v=vault.iter().find(|v|v.id==c.vault_id);json!({"vaultId":c.vault_id,"source":c.source,"alias":c.alias,"label":v.map(|v|v.label.clone()).unwrap_or_default(),"platform":v.map(|v|v.platform.clone()).unwrap_or_default(),"availableModels":c.models,"allowedModels":c.allowed_models,"systemProxy":c.system_proxy,"count":c.models.len(),"models":c.models.iter().map(|m|model_id(c,m)).collect::<Vec<_>>(),"stale":v.map(|v|v.updated_at != c.updated_at).unwrap_or(true)})}).collect();
    Ok(
        json!({"port":settings.port,"lan":settings.lan,"auth":settings.auth,"running":running,"startupError":startup_error,"lanIps":local_ips(),"channels":channels,"vault":vault}),
    )
}
pub async fn autostart(app: tauri::AppHandle) {
    let settings = lock(&app.state::<Gateway>().inner).settings.clone();
    if let Err(e) = gateway_start(app.clone(), settings.port, settings.lan, settings.auth).await {
        lock(&app.state::<Gateway>().inner).startup_error = Some(e);
    }
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
        inner.startup_error = None;
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
    system_proxy: Option<bool>,
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
    let previous = lock(&g.inner).settings.channels.iter().find(|c| c.vault_id == vault_id).cloned();
    let was_published = previous.is_some();
    let use_proxy = system_proxy.or_else(|| previous.as_ref().map(|c| c.system_proxy)).unwrap_or(false);
    let item = lock(&g.store)
        .vault_get(&vault_id)?
        .ok_or("密钥箱条目不存在")?;
    let vid = vault_id.clone();
    let key = tokio::task::spawn_blocking(move || secrets::vault_get(&vid))
        .await
        .map_err(|e| e.to_string())??
        .ok_or("密钥不存在")?;
    let client = g.clients.client(use_proxy)?;
    let (mut ids, host) = if source == "hapi" {
        models::fetch_source_ids(
            &client,
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
            client.get(url).header("x-goog-api-key", &key)
        } else {
            client.get(url).bearer_auth(&key)
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
        .any(|c| c.alias == alias && c.vault_id != vault_id)
    {
        return Err("别名已被其他密钥使用".into());
    }
    let existing = next.channels.iter().find(|c| c.vault_id == vault_id);
    let allowed_models = existing.and_then(|c| c.allowed_models.clone());
    let saved_proxy = if system_proxy.is_some() { use_proxy } else { existing.map(|c| c.system_proxy).unwrap_or(use_proxy) };
    next.channels.retain(|c| c.vault_id != vault_id);
    next.channels.push(Published {
        vault_id,
        source,
        alias,
        host,
        models: ids,
        allowed_models,
        system_proxy: saved_proxy,
        updated_at: item.updated_at,
    });
    g.save(&next)?;
    inner.settings = next;
    Ok(())
}
#[tauri::command]
pub async fn gateway_set_proxy(app: tauri::AppHandle, vault_id: String, system_proxy: bool) -> Result<(), String> {
    let g = app.state::<Gateway>();
    let mut inner = lock(&g.inner);
    let mut next = inner.settings.clone();
    let c = next.channels.iter_mut().find(|c| c.vault_id == vault_id).ok_or("密钥未上架")?;
    c.system_proxy = system_proxy;
    g.save(&next)?;
    inner.settings = next;
    Ok(())
}
#[tauri::command]
pub async fn gateway_set_models(app: tauri::AppHandle, vault_id: String, models: Vec<String>) -> Result<(), String> {
    let g = app.state::<Gateway>();
    let mut inner = lock(&g.inner);
    let mut next = inner.settings.clone();
    let c = next.channels.iter_mut().find(|c| c.vault_id == vault_id).ok_or("密钥未上架")?;
    if models.iter().any(|m| !c.models.contains(m)) { return Err("模型不在当前列表中，请刷新后重试".into()); }
    c.allowed_models = Some(models);
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
    fn legacy_groups_keep_system_proxy_and_proxy_choice_persists() {
        let legacy = r#"{"vaultId":"v","source":"gemini","alias":"a","host":"h","models":[],"updatedAt":1}"#;
        let mut group: Published = serde_json::from_str(legacy).unwrap();
        assert!(group.system_proxy);
        group.system_proxy = false;
        let restored: Published = serde_json::from_str(&serde_json::to_string(&group).unwrap()).unwrap();
        assert!(!restored.system_proxy);
    }
    #[test]
    fn identifiers_are_scoped() {
        let c = Published {
            vault_id: "v".into(),
            source: "gemini".into(),
            alias: "main".into(),
            host: String::new(),
            models: vec![],
            updated_at: 1,
            allowed_models: None,
            system_proxy: false,
        };
        assert_eq!(
            model_id(&c, "gemini-2.5-flash"),
            "main/gemini-2.5-flash"
        );
    }
    #[test]
    fn local_no_auth_rejects_browser_origin() {
        let mut s = Settings::default();
        s.auth = false;
        s.lan = false;
        let mut h = HeaderMap::new();
        // 无鉴权本机模式现在还要求 Host 是本机(L15,DNS rebinding 免疫):
        // 没 Host 不放行;本机 Host 放行;攻击者域名不放行;Origin 一律不放行。
        assert!(!authorized(&h, &s));
        h.insert(header::HOST, "127.0.0.1:18771".parse().unwrap());
        assert!(authorized(&h, &s));
        *h.get_mut(header::HOST).unwrap() = "attacker.example:18771".parse().unwrap();
        assert!(!authorized(&h, &s));
        *h.get_mut(header::HOST).unwrap() = "localhost:18771".parse().unwrap();
        assert!(authorized(&h, &s));
        h.insert(header::ORIGIN, "https://example.com".parse().unwrap());
        assert!(!authorized(&h, &s));
        s.lan = true;
        assert!(!authorized(&HeaderMap::new(), &s));
    }
    #[test]
    fn only_local_or_private_networks_are_allowed() {
        let mut s = Settings::default();
        s.lan = false;
        assert!(peer_allowed("127.0.0.1:1234".parse().unwrap(), &s));
        assert!(!peer_allowed("192.168.1.10:1234".parse().unwrap(), &s));
        s.lan = true;
        assert!(peer_allowed("192.168.1.10:1234".parse().unwrap(), &s));
        assert!(!peer_allowed("8.8.8.8:1234".parse().unwrap(), &s));
    }
    #[test]
    fn advertised_lan_addresses_are_accepted_private_ipv4() {
        assert!(advertised_lan_ip("192.168.1.10".parse().unwrap()));
        assert!(advertised_lan_ip("10.0.0.2".parse().unwrap()));
        assert!(!advertised_lan_ip("100.69.220.4".parse().unwrap()));
        assert!(!advertised_lan_ip("8.8.8.8".parse().unwrap()));
        assert!(!advertised_lan_ip("127.0.0.1".parse().unwrap()));
    }
    #[test]
    fn generated_keys_are_distinct() {
        let a = token().unwrap();
        let b = token().unwrap();
        assert!(a.starts_with("gw_"));
        assert_ne!(a, b);
    }
    #[test]
    fn gemini_sanitize_strips_unsupported_fields() {
        // SillyTavern 典型载荷:兼容层不收的字段透传即 400,清完必须全没了
        let mut body: Value = serde_json::from_str(
            r#"{"model":"gemini-2.5-flash","messages":[],"stream":true,
                "seed":1,"logit_bias":{"x":1},"logprobs":true,"top_logprobs":3,
                "n":1,"user":"u","metadata":{},"stream_options":{"include_usage":true}}"#,
        )
        .unwrap();
        sanitize_gemini_body(&mut body, "gemini-2.5-flash");
        let obj = body.as_object().unwrap();
        for k in ["seed", "logit_bias", "logprobs", "top_logprobs", "n", "user", "metadata"] {
            assert!(!obj.contains_key(k), "{k} 应被剥离");
        }
        assert!(obj.contains_key("stream_options")); // 流式请求保留
        assert_eq!(obj["model"], "gemini-2.5-flash"); // 非目标字段不动
    }
    #[test]
    fn gemini_sanitize_drops_stream_options_when_not_streaming() {
        let mut body: Value = serde_json::from_str(
            r#"{"stream":false,"stream_options":{"include_usage":true}}"#,
        )
        .unwrap();
        sanitize_gemini_body(&mut body, "gemini-2.5-flash");
        assert!(body.as_object().unwrap().get("stream_options").is_none());
    }
    #[test]
    fn gemini_sanitize_maps_completion_tokens_and_clamps() {
        let mut body: Value =
            serde_json::from_str(r#"{"max_completion_tokens":900000}"#).unwrap();
        sanitize_gemini_body(&mut body, "gemini-2.5-flash");
        let obj = body.as_object().unwrap();
        assert_eq!(obj["max_tokens"], 65536);
        assert!(!obj.contains_key("max_completion_tokens"));
        // 不超限的值不动
        let mut body: Value = serde_json::from_str(r#"{"max_tokens":1024}"#).unwrap();
        sanitize_gemini_body(&mut body, "gemini-2.5-flash");
        assert_eq!(body["max_tokens"], 1024);
    }
    #[test]
    fn gemini_sanitize_strips_sampling_for_gemini3_only() {
        // 3 系:采样参数 + 惩罚项 + 采样外的 reasoning 档位全剥
        let mut body: Value = serde_json::from_str(
            r#"{"temperature":0.7,"top_p":0.9,"top_k":40,
                "frequency_penalty":0.5,"presence_penalty":0.5,"reasoning_effort":"minimal"}"#,
        )
        .unwrap();
        sanitize_gemini_body(&mut body, "gemini-3-flash-preview");
        let obj = body.as_object().unwrap();
        for k in ["temperature", "top_p", "top_k", "frequency_penalty", "presence_penalty"] {
            assert!(!obj.contains_key(k), "3 系 {k} 应被剥离");
        }
        assert!(!obj.contains_key("reasoning_effort")); // minimal 不在 3 系放行列表
        // 放行的档位保留
        let mut body: Value = serde_json::from_str(r#"{"reasoning_effort":"low"}"#).unwrap();
        sanitize_gemini_body(&mut body, "gemini-3-flash-preview");
        assert_eq!(body["reasoning_effort"], "low");
        // flash-lite 只放 low/high
        let mut body: Value = serde_json::from_str(r#"{"reasoning_effort":"medium"}"#).unwrap();
        sanitize_gemini_body(&mut body, "gemini-3.5-flash-lite");
        assert!(!body.as_object().unwrap().contains_key("reasoning_effort"));
        // 非 3 系保留采样参数;非 2.5/3 系整个剥离 reasoning_effort
        let mut body: Value = serde_json::from_str(
            r#"{"temperature":0.7,"top_p":0.9,"frequency_penalty":0.5,"reasoning_effort":"low"}"#,
        )
        .unwrap();
        sanitize_gemini_body(&mut body, "gemini-2.5-flash");
        let obj = body.as_object().unwrap();
        assert!(obj.contains_key("temperature") && obj.contains_key("frequency_penalty"));
        assert_eq!(obj["reasoning_effort"], "low");
        let mut body: Value = serde_json::from_str(r#"{"temperature":0.7,"reasoning_effort":"low"}"#).unwrap();
        sanitize_gemini_body(&mut body, "gemini-2.0-flash");
        let obj = body.as_object().unwrap();
        assert!(obj.contains_key("temperature"));
        assert!(!obj.contains_key("reasoning_effort"));
    }
    #[test]
    fn gemini_repair_reads_param_and_penalty_message() {
        // OpenAI 风格错误带 param:按名修复
        let p = gemini_repairable_params(
            r#"{"error":{"message":"Invalid value","type":"invalid_request_error","param":"temperature"}}"#,
        );
        assert_eq!(p.unwrap(), vec!["temperature"]);
        // 惩罚项报错不带 param(实测文案),按 message 一次剥两个
        let p = gemini_repairable_params(
            r#"{"error":{"code":400,"message":"Penalty is not enabled for this model","status":"INVALID_ARGUMENT"}}"#,
        );
        assert_eq!(
            p.unwrap(),
            vec!["frequency_penalty", "presence_penalty"]
        );
        // 不可修的 400 返回 None(原样透传)
        assert!(gemini_repairable_params(r#"{"error":{"message":"API key not valid"}}"#).is_none());
        assert!(gemini_repairable_params("not json").is_none());
        // param 字段不在可修名单里也不修
        let p = gemini_repairable_params(r#"{"error":{"message":"bad","param":"messages"}}"#);
        assert!(p.is_none());
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
        let g = Gateway::new(store.clone()).unwrap();
        {
            let mut inner = lock(&g.inner);
            inner.settings.auth = false;
            inner.settings.lan = false;
            inner.settings.channels.push(Published {
                vault_id: "sample".into(),
                source: "gemini".into(),
                alias: "main".into(),
                host: String::new(),
                models: vec!["test-model".into()],
                updated_at: 1,
                allowed_models: None,
                system_proxy: false,
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
        assert_eq!(data["data"][0]["id"], "main/test-model");
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
