use axum::http::HeaderMap;
use serde::{Deserialize,Serialize};
use std::{collections::{BTreeMap,VecDeque},net::{IpAddr,Ipv4Addr,SocketAddr},sync::{Arc,Mutex}};
use crate::{gateway_proto::{error::redact_secrets,masking::{MaskingSettings,StickyMaskStore},sources::{host_of_url,resolve_source_defaults},GatewaySourceKind,OpenAiWire,ThinkingMode,ThoughtRequestPolicy},proxy::ProxyClients,secrets,store::Store,store_gateway::GatewayUsageRow};
use self::http::{dedupe_nonempty,GatewayDiagEntry};
pub mod http;
mod access;
pub use access::authorized_with;
mod projection;
pub use projection::VaultMeta;
mod ledger;
pub mod lifecycle;
mod refresh;
pub mod routing;
#[cfg(test)]
mod tests;
pub(crate) const CONFIG_KEY:&str="local_gateway_v1";
pub const SECRET_KEY:&str="local-gateway-access";
const MAX_DIAG_ENTRIES:usize=50;
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
    pub wire: OpenAiWire,
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

fn sensitive_header(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase().replace('_', "-");
    matches!(name.as_str(), "authorization" | "proxy-authorization" | "cookie" | "set-cookie")
        || ["api-key", "apikey", "token", "secret", "password"].iter().any(|suffix| name.ends_with(suffix))
}

impl Published {
    pub fn check_custom_headers(&self) -> Result<(), String> {
        if self.custom_headers.keys().any(|name| sensitive_header(name)) {
            return Err("自定义请求头含认证信息，请改用密钥箱".into());
        }
        Ok(())
    }

    pub fn merge_current_probe(&mut self, current: &Published) {
        self.models = current.models.clone();
        self.last_status = current.last_status.clone();
        self.last_error = current.last_error.clone();
        self.last_checked_at = current.last_checked_at;
        if self.vault_id == current.vault_id && self.no_upstream_key == current.no_upstream_key {
            self.updated_at = current.updated_at;
        }
        if self.base_url == current.base_url { self.host = current.host.clone(); }
    }

    fn live_with(&self, vault: &std::collections::HashMap<String, projection::VaultMeta>) -> bool {
        (self.no_upstream_key && self.kind() == GatewaySourceKind::OpenAiCompat) || vault.contains_key(&self.vault_id)
    }

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
            resolve_source_defaults(&self.source, &self.template, self.wire.base_url_input(&self.base_url))
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
    #[serde(default)]
    pub groups: Vec<routing::RouteGroup>,
    #[serde(default)]
    pub masking: MaskingSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            port: 18771,
            lan: true,
            auth: true,
            channels: vec![],
            groups: vec![],
            masking: MaskingSettings::default(),
        }
    }
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncState {
    pub pending: bool,
    pub error: Option<String>,
    pub revision: u64,
}

fn update_sync_state(inner: &mut Inner) {
    inner.sync_state.revision = inner.sync_state.revision.wrapping_add(1);
    inner.sync_state.pending = inner.sync_tasks.values().any(|task| task.1);
    let errors: Vec<_> = inner.sync_tasks.values().filter_map(|task| task.2.as_deref()).collect();
    inner.sync_state.error = (!errors.is_empty()).then(|| errors.join("；"));
}

pub struct Inner {
    pub settings: Settings,
    pub shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    pub listener_released: Option<tokio::sync::oneshot::Receiver<()>>,
    pub run_id: u64,
    pub startup_error: Option<String>,
    pub diagnostics: VecDeque<GatewayDiagEntry>,
    pub next_diag_id: u64,
    pub test_chat_abort: Option<(u64, tokio::task::AbortHandle)>,
    pub next_test_id: u64,
    pub config_revision: u64,
    pub refresh_sequences: BTreeMap<String, u64>,
    pub sync_state: SyncState,
    pub sync_tasks: BTreeMap<String, (u64, bool, Option<String>)>,
    pub next_sync_id: u64,
    pub routing: routing::RoutingRuntime,
    pub mask_store: StickyMaskStore,
}

#[derive(Clone)]
pub struct Gateway {
    pub store: Arc<Mutex<Store>>,
    pub inner: Arc<Mutex<Inner>>,
    pub clients: Arc<ProxyClients>,
    pub lifecycle: Arc<tokio::sync::Mutex<()>>,
    pub credentials: Arc<dyn crate::vault::transaction::CredentialStore>,
    pub access: Arc<dyn crate::vault::transaction::CredentialStore>,
    access_token_sha256: Arc<Mutex<Option<[u8; 32]>>>,
    projections: Arc<projection::Projections>,
    ledger: Arc<ledger::Ledger>,
    draining_runs: Arc<Mutex<Vec<tokio::sync::oneshot::Receiver<()>>>>,
}

pub fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Gateway {
    pub fn new(store: Arc<Mutex<Store>>) -> Result<Self, String> {
        Self::with_credentials(store, Arc::new(crate::vault::transaction::NativeVault), Arc::new(NativeAccess))
    }

    pub fn with_credentials(store: Arc<Mutex<Store>>, credentials: Arc<dyn crate::vault::transaction::CredentialStore>, access: Arc<dyn crate::vault::transaction::CredentialStore>) -> Result<Self, String> {
        let mut settings: Settings = match lock(&store).get_setting(CONFIG_KEY)? {
            Some(raw) => serde_json::from_str(&raw).map_err(|_| "本地网关配置损坏，已保留原记录；请恢复配置后重启".to_string())?,
            None => Settings::default(),
        };
        let mut migrated = false;
        let mut removed_headers = false;
        for (i, ch) in settings.channels.iter_mut().enumerate() {
            if ch.check_custom_headers().is_err() {
                ch.custom_headers.retain(|name, _| !sensitive_header(name));
                removed_headers = true;
                migrated = true;
            }
            if ch.normalize_in_place(&format!("legacy_{i}")) {
                migrated = true;
            }
        }
        let clients = ProxyClients::new()?;
        let projections = Arc::new(projection::Projections::new(&lock(&store)));
        let gw = Self {
            store,
            inner: Arc::new(Mutex::new(Inner {
                settings,
                shutdown: None,
                listener_released: None,
                run_id: 0,
                startup_error: removed_headers.then(|| "已清理旧渠道的明文认证头，请在密钥箱配置上游密钥".into()),
                diagnostics: VecDeque::with_capacity(MAX_DIAG_ENTRIES),
                next_diag_id: 1,
                test_chat_abort: None,
                next_test_id: 1,
                config_revision: 1,
                refresh_sequences: BTreeMap::new(),
                sync_state: SyncState::default(),
                sync_tasks: BTreeMap::new(),
                next_sync_id: 1,
                routing: routing::RoutingRuntime::default(),
                mask_store: StickyMaskStore::default(),
            })),
            clients: Arc::new(clients),
            lifecycle: Arc::new(tokio::sync::Mutex::new(())),
            credentials,
            access,
            access_token_sha256: Arc::new(Mutex::new(None)),
            projections,
            ledger: Arc::default(),
            draining_runs: Arc::default(),
        };
        if migrated {
            let current = lock(&gw.inner).settings.clone();
            gw.save(&current).map_err(|e| format!("网关配置迁移保存失败，原记录已保留：{e}"))?;
        }
        Ok(gw)
    }

    pub fn save(&self, settings: &Settings) -> Result<(), String> {
        for channel in &settings.channels { channel.check_custom_headers()?; }
        lock(&self.store).set_setting(
            CONFIG_KEY,
            &serde_json::to_string(settings).map_err(|e| e.to_string())?,
        )
    }

    pub fn commit_publish(&self, inner: &mut Inner, expected_revision: u64, next: Settings, vault_version: Option<(&str, i64)>, endpoint: Option<(&str, i64)>) -> Result<(), String> {
        if inner.config_revision != expected_revision { return Err("渠道配置已变化，请重新上架".into()); }
        for channel in &next.channels { channel.check_custom_headers()?; }
        lock(&self.store).vault_metadata_and_setting(CONFIG_KEY, &serde_json::to_string(&next).map_err(|e| e.to_string())?, vault_version, endpoint)?;
        inner.config_revision = inner.config_revision.wrapping_add(1);
        inner.settings = next;
        Ok(())
    }

    pub fn commit_settings(&self, inner: &mut Inner, next: Settings) -> Result<(), String> {
        self.save(&next)?;
        inner.config_revision = inner.config_revision.wrapping_add(1);
        inner.settings = next;
        Ok(())
    }

    pub fn invalidate_vault(&self, id: &str) {
        let mut inner = lock(&self.inner);
        // 新渠道尚未提交时也可能持有此凭据的上架票据。
        if !id.is_empty() {
            inner.config_revision = inner.config_revision.wrapping_add(1);
        }
    }

    pub fn begin_sync(&self, target: &str) -> u64 {
        let mut inner = lock(&self.inner);
        let id = inner.next_sync_id;
        inner.next_sync_id = id.wrapping_add(1);
        inner.sync_tasks.insert(target.into(), (id, true, None));
        update_sync_state(&mut inner);
        id
    }

    pub fn finish_sync(&self, id: u64, result: Result<(), String>) -> SyncState {
        let mut inner = lock(&self.inner);
        if let Some(task) = inner.sync_tasks.values_mut().find(|task| task.0 == id) {
            task.1 = false;
            task.2 = result.err().map(|e| redact_secrets(&e));
            update_sync_state(&mut inner);
        }
        inner.sync_state.clone()
    }

    pub fn remove_vault(&self, id: &str) -> Result<(), String> {
        let mut inner = lock(&self.inner);
        let mut next = inner.settings.clone();
        next.channels.retain(|c| c.no_upstream_key || c.vault_id != id);
        self.commit_settings(&mut inner, next)?;
        Ok(())
    }

    pub fn record_diag(&self, mut entry: GatewayDiagEntry) {
        // 流式响应在 body 轮询时已离开 middleware 的 task-local 作用域，
        // 由 guard 建时取好并显式传入；这里只补没给的那些。
        if entry.agent.is_none() {
            entry.agent = self::http::REQ_AGENT.try_with(|a| *a).ok().flatten().map(String::from);
        }
        let log_line = (entry.status >= 400
            || entry.error.is_some()
            || entry.diag_note.is_some()
            || (entry.thinking_chars == 0
                && entry.content_chars == 0
                && entry.refusal_chars == 0
                && entry.tool_call_count == 0))
            .then(|| {
                let mut parts = vec![
                    format!("HTTP {}", entry.status),
                    format!("{}ms", entry.latency_ms),
                    if entry.stream { "SSE" } else { "JSON" }.to_string(),
                    format!(
                        "think={}/content={}/tools={}({})",
                        entry.thinking_chars,
                        entry.content_chars,
                        entry.tool_call_count,
                        entry.tool_call_chars
                    ),
                ];
                if let Some(ttft) = entry.first_byte_ms {
                    parts.push(format!("ttft={ttft}ms"));
                }
                if entry.frame_count > 0 {
                    parts.push(format!("frames={}", entry.frame_count));
                }
                if let Some(ref fr) = entry.finish_reason {
                    parts.push(format!("finish={fr}"));
                }
                if let Some(ref note) = entry.diag_note {
                    parts.push(format!("note={note}"));
                }
                if let Some(ref err) = entry.error {
                    parts.push(format!("err={err}"));
                }
                (entry.model.clone(), redact_secrets(&parts.join(" · ")))
            });

        let (template, base_url, upstream_model_name) = {
            let inner = lock(&self.inner);
            let ch = inner.settings.channels.iter().find(|c| c.channel_id == entry.channel_id);
            let tpl = ch.map(|c| c.template.clone()).unwrap_or_default();
            let url = ch.map(|c| c.base_url.clone()).unwrap_or_default();
            let um = entry
                .upstream_model
                .clone()
                .unwrap_or_else(|| {
                    entry
                        .model
                        .split_once('/')
                        .map(|(_, m)| m.to_string())
                        .unwrap_or_else(|| entry.model.clone())
                });
            (tpl, url, um)
        };
        if entry.estimated_cost_cny.is_none() && entry.status < 400 {
            entry.estimated_cost_cny = self.channel_cost(
                &template,
                &base_url,
                &upstream_model_name,
                entry.prompt_tokens,
                entry.cached_prompt_tokens,
                entry.completion_tokens,
            );
        }
        let ledger_row = GatewayUsageRow {
            ts: entry.timestamp,
            channel_id: entry.channel_id.clone(),
            channel_alias: entry.channel_alias.clone(),
            route_group: entry.route_group.clone(),
            model: entry.model.clone(),
            protocol: if entry.protocol.is_empty() { "openai_chat".into() } else { entry.protocol.clone() },
            stream: entry.stream,
            status: entry.status,
            latency_ms: entry.latency_ms,
            first_byte_ms: entry.first_byte_ms,
            prompt_tokens: entry.prompt_tokens,
            cached_prompt_tokens: entry.cached_prompt_tokens,
            cache_creation_tokens: entry.cache_creation_tokens,
            completion_tokens: entry.completion_tokens,
            reasoning_tokens: entry.reasoning_tokens,
            failover_count: entry.failover_count,
            masked_count: entry.masked_count,
            estimated_cost_cny: entry.estimated_cost_cny,
            error_code: (entry.status >= 400).then(|| format!("http_{}", entry.status)),
            agent: entry.agent.clone(),
        };

        {
            let mut inner = lock(&self.inner);
            entry.id = inner.next_diag_id;
            inner.next_diag_id += 1;
            if inner.diagnostics.len() >= MAX_DIAG_ENTRIES {
                inner.diagnostics.pop_front();
            }
            inner.diagnostics.push_back(entry);
        }
        self.ledger.push(&self.store, ledger_row);
        if let Some((model, msg)) = log_line {
            crate::diaglog::write_keyed("gateway", &model, &msg);
        }
    }

    pub fn update_channel_signals(&self, signals: BTreeMap<String, routing::ChannelSignal>) {
        lock(&self.inner).routing.signals = signals;
    }

    pub fn cooldown_snapshots(&self, now: i64) -> Vec<routing::CooldownSnapshot> {
        let mut inner = lock(&self.inner);
        inner.routing.clear_expired(now);
        inner.routing.snapshots(now)
    }

    pub fn live_channels(&self) -> Result<Vec<Published>, String> {
        let vault = self.vault_index()?;
        Ok(lock(&self.inner).settings.channels.iter()
            .filter(|c| c.live_with(&vault))
            .cloned().collect())
    }

}

struct NativeAccess;
impl crate::vault::transaction::CredentialStore for NativeAccess {
    fn get(&self, id: &str) -> Result<Option<String>, String> { secrets::get(id) }
    fn set(&self, id: &str, value: &str) -> Result<(), String> { secrets::set(id, value) }
    fn delete(&self, id: &str) -> Result<(), String> { secrets::delete(id) }
}

pub fn authorized(headers: &HeaderMap, settings: &Settings) -> bool {
    authorized_with(headers, settings, &NativeAccess)
}


pub fn advertised_lan_ip(ip: Ipv4Addr) -> bool {
    ip.is_private()
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


