use crate::gateway_http::{
    build_router, dedupe_nonempty, fetch_channel_models, now_unix, run_test_chat,
    GatewayDiagEntry,
};
use axum::http::{header, HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::Manager;
use tokenscope_core::{
    gateway_proto::{
        error::redact_secrets,
        normalize_alias,
        sources::{
            host_of_url, resolve_source_defaults, AGENT_PLATFORM_DEFAULT_BASE_URL,
            AGENT_PLATFORM_PRESET_MODELS, OPENAI_PRESETS,
        },
        GatewaySourceKind, ThinkingMode, ThoughtRequestPolicy,
    },
    proxy::ProxyClients,
    secrets,
    store::Store,
};

const CONFIG_KEY: &str = "local_gateway_v1";
const SECRET_KEY: &str = "local-gateway-access";
const MAX_DIAG_ENTRIES: usize = 50;

fn default_system_proxy() -> bool {
    true
}

fn default_last_status() -> String {
    "unknown".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Published {
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub vault_id: String,
    #[serde(default)]
    pub no_upstream_key: bool,
    pub source: String,
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub base_url: String,
    pub alias: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub manual_models: Vec<String>,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    #[serde(default = "default_system_proxy")]
    pub system_proxy: bool,
    #[serde(default)]
    pub custom_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub thinking_mode: ThinkingMode,
    #[serde(default)]
    pub thought_policy: ThoughtRequestPolicy,
    #[serde(default = "default_last_status")]
    pub last_status: String,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_checked_at: Option<i64>,
}

impl Published {
    pub fn kind(&self) -> GatewaySourceKind {
        GatewaySourceKind::parse(&self.source).unwrap_or(GatewaySourceKind::OpenAiCompat)
    }

    pub fn normalize_in_place(&mut self, fallback_id: &str) -> bool {
        let mut changed = false;
        if self.channel_id.trim().is_empty() {
            self.channel_id = if !self.vault_id.trim().is_empty() {
                self.vault_id.clone()
            } else {
                fallback_id.to_string()
            };
            changed = true;
        }
        if let Ok((kind, tpl, url)) =
            resolve_source_defaults(&self.source, &self.template, &self.base_url)
        {
            if self.source != kind.as_str() {
                self.source = kind.as_str().to_string();
                changed = true;
            }
            if self.template != tpl {
                self.template = tpl;
                changed = true;
            }
            if self.base_url != url {
                self.base_url = url;
                changed = true;
            }
        }
        let h = host_of_url(&self.base_url);
        if !h.is_empty() && self.host != h {
            self.host = h;
            changed = true;
        }
        changed
    }
}

pub fn all_channel_models(c: &Published) -> Vec<String> {
    let mut combined = c.models.clone();
    combined.extend(c.manual_models.clone());
    dedupe_nonempty(combined)
}

pub fn effective_models(c: &Published) -> Vec<String> {
    let mut all = all_channel_models(c);
    if let Some(ref allowed) = c.allowed_models {
        all.retain(|m| allowed.contains(m));
    }
    all
}

pub fn model_id(c: &Published, id: &str) -> String {
    format!("{}/{}", c.alias, id)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub port: u16,
    pub lan: bool,
    pub auth: bool,
    pub channels: Vec<Published>,
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

pub struct Inner {
    pub settings: Settings,
    pub shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    pub startup_error: Option<String>,
    pub diagnostics: VecDeque<GatewayDiagEntry>,
    pub next_diag_id: u64,
}

#[derive(Clone)]
pub struct Gateway {
    pub store: Arc<Mutex<Store>>,
    pub inner: Arc<Mutex<Inner>>,
    pub clients: Arc<ProxyClients>,
    pub lifecycle: Arc<tokio::sync::Mutex<()>>,
}

pub fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Gateway {
    pub fn new(store: Arc<Mutex<Store>>) -> Result<Self, String> {
        let mut settings: Settings = lock(&store)
            .get_setting(CONFIG_KEY)
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        let mut migrated = false;
        for (i, ch) in settings.channels.iter_mut().enumerate() {
            if ch.normalize_in_place(&format!("legacy_{i}")) {
                migrated = true;
            }
        }
        let clients = ProxyClients::new()?;
        let gw = Self {
            store,
            inner: Arc::new(Mutex::new(Inner {
                settings,
                shutdown: None,
                startup_error: None,
                diagnostics: VecDeque::with_capacity(MAX_DIAG_ENTRIES),
                next_diag_id: 1,
            })),
            clients: Arc::new(clients),
            lifecycle: Arc::new(tokio::sync::Mutex::new(())),
        };
        if migrated {
            let current = lock(&gw.inner).settings.clone();
            let _ = gw.save(&current);
        }
        Ok(gw)
    }

    pub fn save(&self, settings: &Settings) -> Result<(), String> {
        lock(&self.store).set_setting(
            CONFIG_KEY,
            &serde_json::to_string(settings).map_err(|e| e.to_string())?,
        )
    }

    pub fn remove_vault(&self, id: &str) -> Result<(), String> {
        let mut inner = lock(&self.inner);
        let mut next = inner.settings.clone();
        next.channels.retain(|c| c.no_upstream_key || c.vault_id != id);
        self.save(&next)?;
        inner.settings = next;
        Ok(())
    }

    pub fn record_diag(&self, mut entry: GatewayDiagEntry) {
        let mut inner = lock(&self.inner);
        entry.id = inner.next_diag_id;
        inner.next_diag_id += 1;
        if inner.diagnostics.len() >= MAX_DIAG_ENTRIES {
            inner.diagnostics.pop_front();
        }
        inner.diagnostics.push_back(entry);
    }

    pub fn live_channels(&self) -> Vec<Published> {
        let channels = lock(&self.inner).settings.channels.clone();
        let vault = match lock(&self.store).vault_list() {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        channels
            .into_iter()
            .filter(|c| {
                (c.no_upstream_key && c.kind() == GatewaySourceKind::OpenAiCompat)
                    // 上游请求按 vault_id 实时读密钥箱最新凭据；更新密钥后异步刷模型期间
                    // 不得把渠道从 /v1/models 里临时抹掉（否则外部客户端拉到空模型列表）。
                    || vault.iter().any(|v| v.id == c.vault_id)
            })
            .collect()
    }
}

pub fn token() -> Result<String, String> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).map_err(|e| e.to_string())?;
    Ok(format!(
        "gw_{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    ))
}

fn new_channel_id() -> Result<String, String> {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).map_err(|e| e.to_string())?;
    Ok(format!(
        "ch_{}",
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

pub fn authorized(headers: &HeaderMap, settings: &Settings) -> bool {
    if headers.contains_key(header::ORIGIN) {
        return false;
    }
    if !settings.auth && !settings.lan {
        let Some(h) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
            return false;
        };
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
        .and_then(|s| {
            let trimmed = s.trim();
            let (scheme, token) = trimmed.split_once(' ')?;
            if scheme.eq_ignore_ascii_case("bearer") {
                Some(token.trim())
            } else {
                None
            }
        })
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
        })
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

pub fn peer_allowed(peer: SocketAddr, settings: &Settings) -> bool {
    let allow_v4 = |ip: Ipv4Addr| {
        ip.is_loopback() || (settings.lan && (ip.is_private() || ip.is_link_local()))
    };
    match peer.ip() {
        IpAddr::V4(ip) => allow_v4(ip),
        IpAddr::V6(ip) => ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(allow_v4),
    }
}

fn advertised_lan_ip(ip: Ipv4Addr) -> bool {
    ip.is_private()
}

fn local_ips() -> Vec<Value> {
    let mut ips: Vec<Value> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| match iface.ip() {
            IpAddr::V4(v) if advertised_lan_ip(v) => {
                Some(json!({ "address": v.to_string(), "name": iface.name }))
            }
            _ => None,
        })
        .collect();
    ips.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());
    ips
}

fn check_alias_unique(channels: &[Published], self_id: &str, alias: &str) -> Result<(), String> {
    if channels
        .iter()
        .any(|c| c.channel_id != self_id && c.alias.eq_ignore_ascii_case(alias))
    {
        return Err(format!("渠道别名 {alias} 已被其他渠道使用，请换一个别名"));
    }
    Ok(())
}

async fn read_key_for_channel(
    kind: GatewaySourceKind,
    vault_id: &str,
    no_upstream_key: bool,
) -> Result<Option<String>, String> {
    if no_upstream_key {
        if kind != GatewaySourceKind::OpenAiCompat {
            return Err("只有 OpenAI 兼容端点允许免上游密钥模式".into());
        }
        return Ok(None);
    }
    let vid = vault_id.trim().to_string();
    if vid.is_empty() {
        return Err("请选择关联的密钥箱条目".into());
    }
    let k = tokio::task::spawn_blocking(move || secrets::vault_get(&vid))
        .await
        .map_err(|e| e.to_string())??
        .ok_or_else(|| "所选密钥箱条目缺少有效密钥".to_string())?;
    if k.trim().is_empty() {
        return Err("所选密钥箱条目的密钥为空".into());
    }
    Ok(Some(k))
}

async fn refresh_single_channel(g: &Gateway, mut c: Published) -> Published {
    let item = if !c.no_upstream_key {
        lock(&g.store).vault_get(&c.vault_id).ok().flatten()
    } else {
        None
    };
    let kind = c.kind();
    let key_res = read_key_for_channel(kind, &c.vault_id, c.no_upstream_key).await;
    let client_res = g.clients.client(c.system_proxy);
    let now = now_unix();
    match (key_res, client_res) {
        (Ok(key), Ok(client)) => {
            match fetch_channel_models(&client, kind, &c.base_url, key.as_deref(), &c.custom_headers)
                .await
            {
                Ok(res) => {
                    c.models = res.fetched;
                    if !res.host.is_empty() {
                        c.host = res.host;
                    }
                    let all = all_channel_models(&c);
                    if let Some(ref mut allowed) = c.allowed_models {
                        allowed.retain(|m| all.contains(m));
                    }
                    c.last_status = "ok".into();
                    c.last_error = res.probe_note;
                }
                Err(e) => {
                    c.last_status = "error".into();
                    c.last_error = Some(redact_secrets(&e));
                }
            }
        }
        (Err(e), _) | (_, Err(e)) => {
            c.last_status = "error".into();
            c.last_error = Some(redact_secrets(&e));
        }
    }
    c.last_checked_at = Some(now);
    if let Some(v) = item {
        c.updated_at = v.updated_at;
    } else if c.no_upstream_key {
        c.updated_at = now;
    }
    c
}

/// 当密钥箱或渠道监控页面更新凭据后，自动同步所有关联该 `vault_id` 的网关渠道。
pub async fn sync_vault_channels(app: tauri::AppHandle, vault_id: String) {
    let g = app.state::<Gateway>();
    let targets: Vec<Published> = {
        let inner = lock(&g.inner);
        inner
            .settings
            .channels
            .iter()
            .filter(|c| !c.no_upstream_key && c.vault_id == vault_id)
            .cloned()
            .collect()
    };
    if targets.is_empty() {
        return;
    }
    let mut updated_map: BTreeMap<String, Published> = BTreeMap::new();
    for ch in targets {
        let cid = ch.channel_id.clone();
        let next = refresh_single_channel(&g, ch).await;
        updated_map.insert(cid, next);
    }
    let mut inner = lock(&g.inner);
    let mut next_settings = inner.settings.clone();
    for ch in next_settings.channels.iter_mut() {
        if let Some(u) = updated_map.remove(&ch.channel_id) {
            *ch = u;
        }
    }
    if g.save(&next_settings).is_ok() {
        inner.settings = next_settings;
    }
}

pub fn spawn_vault_sync(app: &tauri::AppHandle, vault_id: String) {
    if vault_id.trim().is_empty() {
        return;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        sync_vault_channels(handle, vault_id).await;
    });
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishChannelInput {
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub vault_id: String,
    #[serde(default)]
    pub no_upstream_key: Option<bool>,
    pub source: String,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub save_endpoint_to_vault: Option<bool>,
    pub alias: String,
    #[serde(default)]
    pub system_proxy: Option<bool>,
    #[serde(default)]
    pub manual_models: Option<Vec<String>>,
    #[serde(default)]
    pub custom_headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub thinking_mode: Option<String>,
    #[serde(default)]
    pub thought_policy: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateChannelInput {
    pub channel_id: String,
    #[serde(default)]
    pub vault_id: Option<String>,
    #[serde(default)]
    pub no_upstream_key: Option<bool>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub system_proxy: Option<bool>,
    #[serde(default)]
    pub manual_models: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_models: Option<Option<Vec<String>>>,
    #[serde(default)]
    pub custom_headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub thinking_mode: Option<String>,
    #[serde(default)]
    pub thought_policy: Option<String>,
    #[serde(default)]
    pub refresh_models: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestChatInput {
    pub channel_id: String,
    pub model: String,
    pub prompt: String,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub include_reasoning: Option<bool>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[tauri::command]
pub async fn gateway_status(app: tauri::AppHandle) -> Result<Value, String> {
    let g = app.state::<Gateway>();
    let vault = lock(&g.store).vault_list()?;
    let (settings, running, startup_error, diag_count) = {
        let mut inner = lock(&g.inner);
        let mut next = inner.settings.clone();
        let before = next.channels.len();
        next.channels
            .retain(|c| c.no_upstream_key || vault.iter().any(|v| v.id == c.vault_id));
        if next.channels.len() != before {
            g.save(&next)?;
            inner.settings = next;
        }
        (
            inner.settings.clone(),
            inner.shutdown.is_some(),
            inner.startup_error.clone(),
            inner.diagnostics.len(),
        )
    };
    let channels: Vec<Value> = settings
        .channels
        .iter()
        .map(|c| {
            let v = vault.iter().find(|v| v.id == c.vault_id);
            let avail = all_channel_models(c);
            let eff = effective_models(c);
            let stale = if c.no_upstream_key {
                false
            } else {
                v.map(|v| v.updated_at != c.updated_at).unwrap_or(true)
            };
            json!({
                "channelId": c.channel_id,
                "vaultId": c.vault_id,
                "noUpstreamKey": c.no_upstream_key,
                "source": c.kind().as_str(),
                "template": c.template,
                "baseUrl": c.base_url,
                "alias": c.alias,
                "host": c.host,
                "label": v.map(|v| v.label.clone()).unwrap_or_else(|| {
                    if c.no_upstream_key { "本地/免密服务".into() } else { String::new() }
                }),
                "platform": v.map(|v| v.platform.clone()).unwrap_or_default(),
                "fetchedModels": c.models,
                "manualModels": c.manual_models,
                "availableModels": avail,
                "allowedModels": c.allowed_models,
                "systemProxy": c.system_proxy,
                "customHeaders": c.custom_headers,
                "thinkingMode": c.thinking_mode,
                "thoughtPolicy": c.thought_policy,
                "count": eff.len(),
                "totalCount": avail.len(),
                "models": eff.iter().map(|m| model_id(c, m)).collect::<Vec<_>>(),
                "stale": stale,
                "lastStatus": c.last_status,
                "lastError": c.last_error,
                "lastCheckedAt": c.last_checked_at,
            })
        })
        .collect();
    Ok(json!({
        "port": settings.port,
        "lan": settings.lan,
        "auth": settings.auth,
        "running": running,
        "startupError": startup_error,
        "lanIps": local_ips(),
        "presets": OPENAI_PRESETS,
        "agentPlatformDefaults": {
            "baseUrl": AGENT_PLATFORM_DEFAULT_BASE_URL,
            "models": AGENT_PLATFORM_PRESET_MODELS,
        },
        "channels": channels,
        "vault": vault,
        "diagCount": diag_count,
    }))
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
        return Err("网关正在运行，请使用应用并重启或先停止网关".into());
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
    let router = build_router(g.inner().clone());
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
pub async fn gateway_restart(
    app: tauri::AppHandle,
    port: u16,
    lan: bool,
    auth: bool,
) -> Result<Value, String> {
    gateway_stop(app.clone()).await?;
    tokio::time::sleep(Duration::from_millis(120)).await;
    gateway_start(app, port, lan, auth).await
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
    input: PublishChannelInput,
) -> Result<Value, String> {
    let g = app.state::<Gateway>();
    let alias = normalize_alias(&input.alias)?;
    let (kind, template, base_url) = resolve_source_defaults(
        &input.source,
        input.template.as_deref().unwrap_or(""),
        input.base_url.as_deref().unwrap_or(""),
    )?;
    let no_upstream_key = input.no_upstream_key.unwrap_or(false);
    if no_upstream_key && kind != GatewaySourceKind::OpenAiCompat {
        return Err("仅 OpenAI 兼容端点支持免上游密钥模式".into());
    }

    let channel_id = match input.channel_id.filter(|s| !s.trim().is_empty()) {
        Some(id) => id,
        None => new_channel_id()?,
    };
    let existing = lock(&g.inner)
        .settings
        .channels
        .iter()
        .find(|c| c.channel_id == channel_id)
        .cloned();
    {
        let inner = lock(&g.inner);
        check_alias_unique(&inner.settings.channels, &channel_id, &alias)?;
    }

    let use_proxy = input
        .system_proxy
        .or_else(|| existing.as_ref().map(|c| c.system_proxy))
        .unwrap_or(true);
    let thinking_mode = input
        .thinking_mode
        .as_deref()
        .map(ThinkingMode::parse)
        .or_else(|| existing.as_ref().map(|c| c.thinking_mode))
        .unwrap_or_default();
    let thought_policy = input
        .thought_policy
        .as_deref()
        .map(ThoughtRequestPolicy::parse)
        .or_else(|| existing.as_ref().map(|c| c.thought_policy))
        .unwrap_or_default();
    let custom_headers = input
        .custom_headers
        .or_else(|| existing.as_ref().map(|c| c.custom_headers.clone()))
        .unwrap_or_default();
    let manual_models = input
        .manual_models
        .map(dedupe_nonempty)
        .or_else(|| existing.as_ref().map(|c| c.manual_models.clone()))
        .unwrap_or_default();

    let key = read_key_for_channel(kind, &input.vault_id, no_upstream_key).await?;
    let mut updated_at = now_unix();
    if !no_upstream_key {
        let mut item = lock(&g.store)
            .vault_get(&input.vault_id)?
            .ok_or("密钥箱条目不存在")?;
        if input.save_endpoint_to_vault.unwrap_or(false) && kind == GatewaySourceKind::OpenAiCompat {
            item.api_endpoint = base_url.clone();
            item.updated_at = now_unix();
            lock(&g.store).vault_upsert(&item)?;
        }
        updated_at = item.updated_at;
    }

    let client = g.clients.client(use_proxy)?;
    let fetch_res = fetch_channel_models(
        &client,
        kind,
        &base_url,
        key.as_deref(),
        &custom_headers,
    )
    .await;

    let (fetched_models, host, last_status, last_error) = match fetch_res {
        Ok(res) => (res.fetched, res.host, "ok".to_string(), res.probe_note),
        Err(err) => {
            if manual_models.is_empty() {
                return Err(err);
            }
            (vec![], host_of_url(&base_url), "error".to_string(), Some(redact_secrets(&err)))
        }
    };

    let mut channel = Published {
        channel_id: channel_id.clone(),
        vault_id: if no_upstream_key { String::new() } else { input.vault_id },
        no_upstream_key,
        source: kind.as_str().to_string(),
        template,
        base_url,
        alias,
        host,
        models: fetched_models,
        manual_models,
        updated_at,
        allowed_models: existing.as_ref().and_then(|c| c.allowed_models.clone()),
        system_proxy: use_proxy,
        custom_headers,
        thinking_mode,
        thought_policy,
        last_status,
        last_error,
        last_checked_at: Some(now_unix()),
    };
    let all = all_channel_models(&channel);
    if let Some(ref mut allowed) = channel.allowed_models {
        allowed.retain(|m| all.contains(m));
    }

    {
        let mut inner = lock(&g.inner);
        let mut next = inner.settings.clone();
        next.channels.retain(|c| c.channel_id != channel_id);
        next.channels.push(channel);
        g.save(&next)?;
        inner.settings = next;
    }
    gateway_status(app).await
}

#[tauri::command]
pub async fn gateway_update_channel(
    app: tauri::AppHandle,
    input: UpdateChannelInput,
) -> Result<Value, String> {
    let g = app.state::<Gateway>();
    let mut channel = lock(&g.inner)
        .settings
        .channels
        .iter()
        .find(|c| c.channel_id == input.channel_id)
        .cloned()
        .ok_or("渠道不存在或已下架")?;

    if let Some(ref alias_raw) = input.alias {
        let alias = normalize_alias(alias_raw)?;
        let inner = lock(&g.inner);
        check_alias_unique(&inner.settings.channels, &channel.channel_id, &alias)?;
        channel.alias = alias;
    }
    if let Some(no_key) = input.no_upstream_key {
        if no_key && channel.kind() != GatewaySourceKind::OpenAiCompat {
            return Err("仅 OpenAI 兼容端点支持免上游密钥模式".into());
        }
        channel.no_upstream_key = no_key;
        if no_key {
            channel.vault_id.clear();
        }
    }
    if let Some(ref vid) = input.vault_id {
        if !channel.no_upstream_key {
            let item = lock(&g.store)
                .vault_get(vid)?
                .ok_or("所选密钥箱条目不存在")?;
            channel.vault_id = vid.clone();
            channel.updated_at = item.updated_at;
        }
    }
    if input.template.is_some() || input.base_url.is_some() {
        let tpl = input.template.unwrap_or_else(|| channel.template.clone());
        let url_raw = input.base_url.unwrap_or_else(|| channel.base_url.clone());
        let (kind, next_tpl, next_url) = resolve_source_defaults(&channel.source, &tpl, &url_raw)?;
        channel.source = kind.as_str().to_string();
        channel.template = next_tpl;
        channel.base_url = next_url;
        channel.host = host_of_url(&channel.base_url);
    }
    if let Some(proxy) = input.system_proxy {
        channel.system_proxy = proxy;
    }
    if let Some(manual) = input.manual_models {
        channel.manual_models = dedupe_nonempty(manual);
    }
    if let Some(hdrs) = input.custom_headers {
        channel.custom_headers = hdrs;
    }
    if let Some(ref mode) = input.thinking_mode {
        channel.thinking_mode = ThinkingMode::parse(mode);
    }
    if let Some(ref pol) = input.thought_policy {
        channel.thought_policy = ThoughtRequestPolicy::parse(pol);
    }
    if let Some(opt_allowed) = input.allowed_models {
        let avail = all_channel_models(&channel);
        if let Some(ref list) = opt_allowed {
            if list.iter().any(|m| !avail.contains(m)) {
                return Err("存在不在可用模型列表中的勾选模型，请刷新后重试".into());
            }
        }
        channel.allowed_models = opt_allowed;
    }

    if input.refresh_models.unwrap_or(false) {
        channel = refresh_single_channel(&g, channel).await;
    }

    {
        let mut inner = lock(&g.inner);
        let mut next = inner.settings.clone();
        check_alias_unique(&next.channels, &channel.channel_id, &channel.alias)?;
        if let Some(slot) = next.channels.iter_mut().find(|c| c.channel_id == channel.channel_id) {
            *slot = channel;
        } else {
            return Err("渠道已被移除".into());
        }
        g.save(&next)?;
        inner.settings = next;
    }
    gateway_status(app).await
}

#[tauri::command]
pub async fn gateway_refresh_channel(
    app: tauri::AppHandle,
    channel_id: String,
) -> Result<Value, String> {
    let g = app.state::<Gateway>();
    let target = lock(&g.inner)
        .settings
        .channels
        .iter()
        .find(|c| c.channel_id == channel_id || c.vault_id == channel_id)
        .cloned()
        .ok_or("渠道不存在")?;
    let updated = refresh_single_channel(&g, target).await;
    {
        let mut inner = lock(&g.inner);
        let mut next = inner.settings.clone();
        if let Some(slot) = next.channels.iter_mut().find(|c| c.channel_id == updated.channel_id) {
            *slot = updated;
        }
        g.save(&next)?;
        inner.settings = next;
    }
    gateway_status(app).await
}

#[tauri::command]
pub async fn gateway_unpublish(
    app: tauri::AppHandle,
    channel_id: String,
) -> Result<(), String> {
    let g = app.state::<Gateway>();
    let mut inner = lock(&g.inner);
    let mut next = inner.settings.clone();
    next.channels
        .retain(|c| c.channel_id != channel_id && c.vault_id != channel_id);
    g.save(&next)?;
    inner.settings = next;
    Ok(())
}

#[tauri::command]
pub async fn gateway_test_channel(
    app: tauri::AppHandle,
    channel_id: String,
) -> Result<Value, String> {
    let g = app.state::<Gateway>();
    let channel = lock(&g.inner)
        .settings
        .channels
        .iter()
        .find(|c| c.channel_id == channel_id)
        .cloned()
        .ok_or("渠道不存在")?;
    let eff = effective_models(&channel);
    let test_model = eff
        .first()
        .cloned()
        .or_else(|| all_channel_models(&channel).into_iter().next())
        .ok_or("当前渠道没有任何可用模型，请先刷新或填写手动模型")?;

    let started = std::time::Instant::now();
    if channel.kind() == GatewaySourceKind::AgentPlatform {
        let res = run_test_chat(
            &g,
            channel.clone(),
            test_model,
            "Reply with OK only.".into(),
            false,
            false,
            Some("none".into()),
        )
        .await;
        return Ok(json!({
            "ok": res.ok,
            "mode": "chat_ping",
            "status": res.status,
            "latencyMs": res.latency_ms,
            "model": res.requested_model,
            "error": res.error,
        }));
    }

    let key = read_key_for_channel(channel.kind(), &channel.vault_id, channel.no_upstream_key).await?;
    let client = g.clients.client(channel.system_proxy)?;
    let res = fetch_channel_models(
        &client,
        channel.kind(),
        &channel.base_url,
        key.as_deref(),
        &channel.custom_headers,
    )
    .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match res {
        Ok(ok_res) => Ok(json!({
            "ok": true,
            "mode": "models_probe",
            "latencyMs": latency_ms,
            "fetchedCount": ok_res.fetched.len(),
            "note": ok_res.probe_note,
        })),
        Err(e) => Ok(json!({
            "ok": false,
            "mode": "models_probe",
            "latencyMs": latency_ms,
            "error": redact_secrets(&e),
        })),
    }
}

#[tauri::command]
pub async fn gateway_test_chat(
    app: tauri::AppHandle,
    input: TestChatInput,
) -> Result<Value, String> {
    let g = app.state::<Gateway>();
    let channel = lock(&g.inner)
        .settings
        .channels
        .iter()
        .find(|c| c.channel_id == input.channel_id)
        .cloned()
        .ok_or("渠道不存在")?;
    let model = input.model.trim();
    let stripped = model
        .strip_prefix(&format!("{}/", channel.alias))
        .unwrap_or(model)
        .to_string();
    if stripped.is_empty() {
        return Err("请选择测试模型".into());
    }
    let prompt = if input.prompt.trim().is_empty() {
        "请用一句中文打个招呼。".to_string()
    } else {
        input.prompt
    };
    let res = run_test_chat(
        &g,
        channel,
        stripped,
        prompt,
        input.stream.unwrap_or(true),
        input.include_reasoning.unwrap_or(true),
        input.reasoning_effort,
    )
    .await;
    serde_json::to_value(res).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn gateway_diagnostics(app: tauri::AppHandle) -> Result<Vec<GatewayDiagEntry>, String> {
    let g = app.state::<Gateway>();
    let inner = lock(&g.inner);
    Ok(inner.diagnostics.iter().rev().cloned().collect())
}

#[tauri::command]
pub async fn gateway_clear_diagnostics(app: tauri::AppHandle) -> Result<(), String> {
    let g = app.state::<Gateway>();
    lock(&g.inner).diagnostics.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Router};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn legacy_groups_keep_system_proxy_and_migrate_sources() {
        let legacy = r#"{"vaultId":"v","source":"gemini","alias":"a","host":"h","models":[],"updatedAt":1}"#;
        let mut group: Published = serde_json::from_str(legacy).unwrap();
        assert!(group.system_proxy);
        assert!(group.normalize_in_place("legacy_0"));
        assert_eq!(group.channel_id, "v");
        assert_eq!(group.source, "gemini_aistudio");
        assert!(group
            .base_url
            .contains("generativelanguage.googleapis.com/v1beta/openai"));

        group.system_proxy = false;
        let restored: Published =
            serde_json::from_str(&serde_json::to_string(&group).unwrap()).unwrap();
        assert!(!restored.system_proxy);
    }

    #[test]
    fn local_no_auth_rejects_browser_origin() {
        let mut s = Settings::default();
        s.auth = false;
        s.lan = false;
        let mut h = HeaderMap::new();
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
        assert!(peer_allowed("[::1]:1234".parse().unwrap(), &s));
        assert!(peer_allowed("[::ffff:127.0.0.1]:1234".parse().unwrap(), &s));
        assert!(!peer_allowed("192.168.1.10:1234".parse().unwrap(), &s));
        assert!(!peer_allowed("[::ffff:192.168.1.10]:1234".parse().unwrap(), &s));
        s.lan = true;
        assert!(peer_allowed("192.168.1.10:1234".parse().unwrap(), &s));
        assert!(peer_allowed("[::ffff:192.168.1.10]:1234".parse().unwrap(), &s));
        assert!(!peer_allowed("8.8.8.8:1234".parse().unwrap(), &s));
    }

    #[test]
    fn live_channels_survive_vault_timestamp_bump_until_deleted() {
        let file = std::env::temp_dir().join(format!("gw-live-{}.db", token().unwrap()));
        let store = Arc::new(Mutex::new(Store::open(&file).unwrap()));
        lock(&store)
            .vault_upsert(&tokenscope_core::store::VaultItem {
                id: "v_1".into(),
                platform: "openai".into(),
                label: "k1".into(),
                hint: "sk-***".into(),
                note: String::new(),
                api_endpoint: "https://api.example.com/v1".into(),
                website: String::new(),
                created_at: 10,
                updated_at: 20, // 已更新，比已发布渠道的 updated_at=10 更新
            })
            .unwrap();
        let g = Gateway::new(store.clone()).unwrap();
        lock(&g.inner).settings.channels.push(Published {
            channel_id: "ch_1".into(),
            vault_id: "v_1".into(),
            no_upstream_key: false,
            source: "openai_compat".into(),
            template: "custom".into(),
            base_url: "https://api.example.com/v1".into(),
            alias: "demo".into(),
            host: "api.example.com".into(),
            models: vec!["gpt-4o".into()],
            manual_models: vec![],
            updated_at: 10,
            allowed_models: None,
            system_proxy: true,
            custom_headers: BTreeMap::new(),
            thinking_mode: ThinkingMode::Auto,
            thought_policy: ThoughtRequestPolicy::FollowClient,
            last_status: "ok".into(),
            last_error: None,
            last_checked_at: Some(10),
        });
        assert_eq!(g.live_channels().len(), 1);
        lock(&store).vault_delete("v_1").unwrap();
        assert!(g.live_channels().is_empty());
        drop(store);
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn advertised_lan_addresses_are_accepted_private_ipv4() {
        assert!(advertised_lan_ip("192.168.1.10".parse().unwrap()));
        assert!(advertised_lan_ip("10.0.0.2".parse().unwrap()));
        assert!(!advertised_lan_ip("100.69.220.4".parse().unwrap()));
        assert!(!advertised_lan_ip("8.8.8.8".parse().unwrap()));
        assert!(!advertised_lan_ip("127.0.0.1".parse().unwrap()));
    }

    #[tokio::test]
    async fn http_router_lists_models_and_normalizes_streaming_think_tags() {
        // 1. 启动一个本地模拟 OpenAI 上游（无外部网络依赖）
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream_router = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                let sse = concat!(
                    "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"upstream-m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"<think>先想\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"upstream-m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"一想</think>\\n\\n最终答案\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"upstream-m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                );
                ([(header::CONTENT_TYPE, "text/event-stream")], sse)
            }),
        );
        let up_task = tokio::spawn(async move {
            axum::serve(upstream_listener, upstream_router).await.unwrap();
        });

        // 2. 启动 Gateway（免上游密钥的本地兼容渠道 + 无网关认证的本机测试模式）
        let file = std::env::temp_dir().join(format!("gw-test-{}.db", token().unwrap()));
        let store = Arc::new(Mutex::new(Store::open(&file).unwrap()));
        let g = Gateway::new(store.clone()).unwrap();
        {
            let mut inner = lock(&g.inner);
            inner.settings.auth = false;
            inner.settings.lan = false;
            inner.settings.channels.push(Published {
                channel_id: "ch_local".into(),
                vault_id: String::new(),
                no_upstream_key: true,
                source: "openai_compat".into(),
                template: "custom".into(),
                base_url: format!("http://{upstream_addr}/v1"),
                alias: "local".into(),
                host: "127.0.0.1".into(),
                models: vec!["qwen-think".into()],
                manual_models: vec!["custom-m".into()],
                updated_at: 1,
                allowed_models: None,
                system_proxy: false,
                custom_headers: BTreeMap::new(),
                thinking_mode: ThinkingMode::Auto,
                thought_policy: ThoughtRequestPolicy::FollowClient,
                last_status: "ok".into(),
                last_error: None,
                last_checked_at: Some(1),
            });
        }
        let gw_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gw_addr = gw_listener.local_addr().unwrap();
        let router = build_router(g);
        let gw_task = tokio::spawn(async move {
            axum::serve(
                gw_listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        // /v1/models 同时包含自动拉取和手动模型
        let list_val: Value = client
            .get(format!("http://{gw_addr}/v1/models"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let ids: Vec<&str> = list_val["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["id"].as_str())
            .collect();
        assert!(ids.contains(&"local/qwen-think"));
        assert!(ids.contains(&"local/custom-m"));

        // 未带 /v1 前缀的 /models 也必须兼容，避免 custom_url 漏写 /v1 返回 404
        let root_models: Value = client
            .get(format!("http://{gw_addr}/models"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let root_ids: Vec<&str> = root_models["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["id"].as_str())
            .collect();
        assert_eq!(root_ids, ids);

        // /v1/chat/completions 流式归一化输出标准 reasoning_content 与 content
        let sse_text = client
            .post(format!("http://{gw_addr}/v1/chat/completions"))
            .json(&json!({
                "model": "local/qwen-think",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true
            }))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(sse_text.contains("\"model\":\"local/qwen-think\""));
        assert!(sse_text.contains("\"reasoning_content\""));
        assert!(sse_text.contains("最终答案"));
        assert!(sse_text.contains("data: [DONE]"));

        // 验证原始 TCP 上浏览器 Origin 仍被拒绝
        let mut sock = tokio::net::TcpStream::connect(gw_addr).await.unwrap();
        sock.write_all(
            format!(
                "GET /v1/models HTTP/1.1\r\nHost: {gw_addr}\r\nOrigin: https://evil.test\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut buf = String::new();
        sock.read_to_string(&mut buf).await.unwrap();
        assert!(buf.starts_with("HTTP/1.1 401"));

        gw_task.abort();
        up_task.abort();
        drop(store);
        let _ = std::fs::remove_file(file);
    }
}
