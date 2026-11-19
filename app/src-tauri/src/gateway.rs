use tokenscope_core::gateway::http::{dedupe_nonempty,fetch_channel_models,fetch_configured_models,now_unix,run_test_chat};
pub use tokenscope_core::gateway::{all_channel_models,effective_models,lock,model_id,Gateway,Published};
use tokenscope_core::gateway::http::GatewayDiagEntry;
use serde::Deserialize;
use serde_json::{json,Value};
use std::{collections::BTreeMap,net::{IpAddr,Ipv4Addr}};
use tauri::{Manager,Emitter};
use tokenscope_core::{gateway_proto::{error::redact_secrets,normalize_alias,sources::{host_of_url,resolve_source_defaults,AGENT_PLATFORM_DEFAULT_BASE_URL,AGENT_PLATFORM_PRESET_MODELS,OPENAI_PRESETS},GatewaySourceKind,OpenAiWire,ThinkingMode,ThoughtRequestPolicy},secrets};
const SECRET_KEY:&str=tokenscope_core::gateway::SECRET_KEY;
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

fn get_gateway(app: &tauri::AppHandle) -> Result<tauri::State<'_, Gateway>, String> {
    app.try_state::<Gateway>().ok_or_else(|| "本地网关未初始化，请检查启动错误并恢复配置后重启".into())
}

pub fn report_sync_error(app: &tauri::AppHandle, error: String) {
    if let Some(g) = app.try_state::<Gateway>() {
        let revision = g.begin_sync("import");
        let state = g.finish_sync(revision, Err(error));
        let _ = app.emit("gateway-sync-status", state);
    } else {
        let _ = app.emit("gateway-sync-status", tokenscope_core::gateway::SyncState { pending:false, error:Some(redact_secrets(&error)), revision:0 });
    }
}

pub fn spawn_vault_sync(app: &tauri::AppHandle, vault_id: String) {
    if vault_id.trim().is_empty() { return; }
    let Some(g) = app.try_state::<Gateway>() else { return; };
    let g = g.inner().clone();
    let revision = g.begin_sync(&vault_id);
    let _ = app.emit("gateway-sync-status", lock(&g.inner).sync_state.clone());
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = g.sync_vault(&vault_id).await;
        let state = g.finish_sync(revision, result);
        let _ = handle.emit("gateway-sync-status", state);
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
    pub wire: Option<OpenAiWire>,
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
    pub wire: Option<OpenAiWire>,
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
    let g = get_gateway(&app)?;
    let vault = lock(&g.store).vault_list()?;
    let (settings, running, startup_error, diag_count, sync_state) = {
        let mut inner = lock(&g.inner);
        let mut next = inner.settings.clone();
        let before = next.channels.len();
        next.channels
            .retain(|c| c.no_upstream_key || vault.iter().any(|v| v.id == c.vault_id));
        if next.channels.len() != before {
            g.commit_settings(&mut inner, next)?;
        }
        (
            inner.settings.clone(),
            inner.shutdown.is_some(),
            inner.startup_error.clone(),
            inner.diagnostics.len(),
            inner.sync_state.clone(),
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
                "wire": c.wire,
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
        "syncState": sync_state,
    }))
}

pub async fn autostart(app: tauri::AppHandle) {
    let Some(g) = app.try_state::<Gateway>() else { return; };
    let settings = lock(&g.inner).settings.clone();
    if let Err(e) = gateway_start(app.clone(), settings.port, settings.lan, settings.auth).await {
        lock(&g.inner).startup_error = Some(e);
    }
}

#[tauri::command]
pub async fn gateway_start(app: tauri::AppHandle, port:u16, lan:bool, auth:bool) -> Result<Value,String> {
    get_gateway(&app)?.start(port, lan, auth).await?;
    gateway_status(app).await
}

#[tauri::command]
pub async fn gateway_stop(app: tauri::AppHandle) -> Result<(),String> {
    get_gateway(&app)?.stop().await
}

#[tauri::command]
pub async fn gateway_restart(app: tauri::AppHandle, port:u16, lan:bool, auth:bool) -> Result<Value,String> {
    get_gateway(&app)?.restart(port, lan, auth).await?;
    gateway_status(app).await
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
    let g = get_gateway(&app)?;
    let alias = normalize_alias(&input.alias)?;
    let no_upstream_key = input.no_upstream_key.unwrap_or(false);
    let channel_id = match input.channel_id.filter(|s| !s.trim().is_empty()) {
        Some(id) => id,
        None => new_channel_id()?,
    };
    let (existing, revision) = {
        let inner = lock(&g.inner);
        let existing = inner.settings.channels.iter().find(|c| c.channel_id == channel_id).cloned();
        (existing, inner.config_revision)
    };
    let wire = input.wire.or_else(|| existing.as_ref().map(|c| c.wire)).unwrap_or_default();
    let (kind, template, base_url) = resolve_source_defaults(
        &input.source,
        input.template.as_deref().unwrap_or(""),
        wire.base_url_input(input.base_url.as_deref().unwrap_or("")),
    )?;
    if no_upstream_key && kind != GatewaySourceKind::OpenAiCompat {
        return Err("仅 OpenAI 兼容端点支持免上游密钥模式".into());
    }
    wire.validate_source(kind)?;
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

    // 先捕获版本，再在锁外读凭据；最终提交复核版本，探测失败不写端点。
    let vault_updated_at = if no_upstream_key { None } else {
        Some(lock(&g.store).vault_get(&input.vault_id)?.ok_or("密钥箱条目不存在")?.updated_at)
    };
    let key = read_key_for_channel(kind, &input.vault_id, no_upstream_key).await?;
    let save_endpoint = input.save_endpoint_to_vault.unwrap_or(false)
        && kind == GatewaySourceKind::OpenAiCompat && !no_upstream_key;
    let endpoint_update = if save_endpoint {
        vault_updated_at.map(|old| (base_url.clone(), now_unix().max(old.saturating_add(1))))
    } else { None };
    let updated_at = endpoint_update.as_ref().map(|(_, at)| *at)
        .or(vault_updated_at).unwrap_or_else(now_unix);
    let vault_id = input.vault_id.clone();

    let mut channel = Published {
        channel_id: channel_id.clone(),
        vault_id: if no_upstream_key { String::new() } else { input.vault_id },
        no_upstream_key,
        source: kind.as_str().to_string(),
        wire,
        template,
        base_url,
        alias,
        host: String::new(),
        models: Vec::new(),
        manual_models,
        updated_at,
        allowed_models: existing.as_ref().and_then(|c| c.allowed_models.clone()),
        system_proxy: use_proxy,
        custom_headers,
        thinking_mode,
        thought_policy,
        last_status: "ok".into(),
        last_error: None,
        last_checked_at: Some(now_unix()),
    };
    let client = g.clients.client(use_proxy)?;
    match fetch_configured_models(&client, &channel, key.as_deref()).await {
        Ok(res) => {
            channel.models = res.fetched;
            channel.host = res.host;
            channel.last_error = res.probe_note;
        }
        Err(err) if channel.manual_models.is_empty() || wire == OpenAiWire::Responses => return Err(err),
        Err(err) => {
            channel.host = host_of_url(&channel.base_url);
            channel.last_status = "error".into();
            channel.last_error = Some(redact_secrets(&err));
        }
    }
    let all = all_channel_models(&channel);
    if wire == OpenAiWire::Responses && all.is_empty() {
        return Err("未取得 Responses 模型，请填写手动模型列表".into());
    }
    if let Some(ref mut allowed) = channel.allowed_models {
        allowed.retain(|m| all.contains(m));
    }

    {
        let mut inner = lock(&g.inner);
        if inner.config_revision != revision { return Err("渠道配置已变化，请重新上架".into()); }
        let mut next = inner.settings.clone();
        check_alias_unique(&next.channels, &channel_id, &channel.alias)?;
        next.channels.retain(|c| c.channel_id != channel_id);
        next.channels.push(channel);
        g.commit_publish(&mut inner, revision, next,
            vault_updated_at.map(|at| (vault_id.as_str(), at)),
            endpoint_update.as_ref().map(|(url, at)| (url.as_str(), *at)))?;
    }
    gateway_status(app).await
}

#[tauri::command]
pub async fn gateway_update_channel(
    app: tauri::AppHandle,
    input: UpdateChannelInput,
) -> Result<Value, String> {
    let g = get_gateway(&app)?;
    let (mut channel, revision) = {
        let inner = lock(&g.inner);
        let channel = inner.settings.channels.iter().find(|c| c.channel_id == input.channel_id)
            .cloned().ok_or("渠道不存在或已下架")?;
        (channel, inner.config_revision)
    };

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
    if let Some(wire) = input.wire {
        wire.validate_source(channel.kind())?;
        channel.wire = wire;
    }
    if input.template.is_some() || input.base_url.is_some() || input.wire.is_some() {
        let tpl = input.template.unwrap_or_else(|| channel.template.clone());
        let url_raw = input.base_url.unwrap_or_else(|| channel.base_url.clone());
        let (kind, next_tpl, next_url) = resolve_source_defaults(
            &channel.source, &tpl, channel.wire.base_url_input(&url_raw),
        )?;
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
    let need_refresh;
    {
        let mut inner = lock(&g.inner);
        if inner.config_revision != revision { return Err("渠道配置已变化，请重新保存".into()); }
        let mut next = inner.settings.clone();
        check_alias_unique(&next.channels, &channel.channel_id, &channel.alias)?;
        let slot = next.channels.iter_mut().find(|c| c.channel_id == channel.channel_id)
            .ok_or("渠道已被移除")?;
        // 刷新不改用户配置修订号；保留同锁读取的最新派生结果，而非保存早前 clone。
        channel.merge_current_probe(slot);
        if channel.wire == OpenAiWire::Responses && !channel.manual_models.is_empty() {
            channel.models.clear();
            if let Some(allowed) = channel.allowed_models.as_mut() {
                allowed.retain(|m| channel.manual_models.contains(m));
            }
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
        need_refresh = input.refresh_models.unwrap_or(false)
            || (channel.wire == OpenAiWire::Responses && all_channel_models(&channel).is_empty());
        *slot = channel;
        g.commit_settings(&mut inner, next)?;
    }
    if need_refresh {
        if let Err(error) = g.refresh_channel(&input.channel_id).await {
            report_sync_error(&app, format!("渠道设置已保存，但模型同步失败：{error}"));
        }
    }
    gateway_status(app).await
}

#[tauri::command]
pub async fn gateway_refresh_channel(
    app: tauri::AppHandle,
    channel_id: String,
) -> Result<Value, String> {
    let g = get_gateway(&app)?;
    if !g.refresh_channel(&channel_id).await? { return Err("渠道配置已变化，请重新刷新".into()); }
    gateway_status(app).await
}

#[tauri::command]
pub async fn gateway_unpublish(
    app: tauri::AppHandle,
    channel_id: String,
) -> Result<(), String> {
    let g = get_gateway(&app)?;
    let mut inner = lock(&g.inner);
    let mut next = inner.settings.clone();
    next.channels
        .retain(|c| c.channel_id != channel_id && c.vault_id != channel_id);
    g.commit_settings(&mut inner, next)?;
    Ok(())
}

#[tauri::command]
pub async fn gateway_test_channel(
    app: tauri::AppHandle,
    channel_id: String,
) -> Result<Value, String> {
    let g = get_gateway(&app)?;
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
    if channel.kind() == GatewaySourceKind::AgentPlatform || channel.wire == OpenAiWire::Responses {
        let res = run_test_chat(
            &g,
            channel.clone(),
            test_model,
            "Reply with OK only.".into(),
            false,
            false,
            (channel.kind() == GatewaySourceKind::AgentPlatform).then(|| "none".into()),
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
    let g = get_gateway(&app)?;
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
    let stream = input.stream.unwrap_or(true);
    let include_reasoning = input.include_reasoning.unwrap_or(true);
    let reasoning_effort = input.reasoning_effort;
    let gw = g.inner().clone();
    let handle = tokio::spawn(async move {
        run_test_chat(
            &gw,
            channel,
            stripped,
            prompt,
            stream,
            include_reasoning,
            reasoning_effort,
        )
        .await
    });
    let test_id = {
        let mut inner = lock(&g.inner);
        if let Some((_, prev)) = inner.test_chat_abort.take() {
            prev.abort();
        }
        let id = inner.next_test_id;
        inner.next_test_id += 1;
        inner.test_chat_abort = Some((id, handle.abort_handle()));
        id
    };
    let joined = handle.await;
    {
        let mut inner = lock(&g.inner);
        if inner.test_chat_abort.as_ref().is_some_and(|(id, _)| *id == test_id) {
            inner.test_chat_abort = None;
        }
    }
    match joined {
        Ok(res) => serde_json::to_value(res).map_err(|e| e.to_string()),
        Err(e) if e.is_cancelled() => Err("已取消本次联调测试".into()),
        Err(e) => Err(format!("联调测试任务异常结束：{e}")),
    }
}

#[tauri::command]
pub async fn gateway_cancel_test_chat(app: tauri::AppHandle) -> Result<bool, String> {
    let g = get_gateway(&app)?;
    let prev = lock(&g.inner).test_chat_abort.take();
    if let Some((_, abort)) = prev {
        abort.abort();
        return Ok(true);
    }
    Ok(false)
}

#[tauri::command]
pub async fn gateway_diagnostics(app: tauri::AppHandle) -> Result<Vec<GatewayDiagEntry>, String> {
    let g = get_gateway(&app)?;
    let inner = lock(&g.inner);
    Ok(inner.diagnostics.iter().rev().cloned().collect())
}

#[tauri::command]
pub async fn gateway_clear_diagnostics(app: tauri::AppHandle) -> Result<(), String> {
    let g = get_gateway(&app)?;
    lock(&g.inner).diagnostics.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advertised_lan_addresses_are_accepted_private_ipv4() {
        assert!(advertised_lan_ip("192.168.1.10".parse().unwrap()));
        assert!(advertised_lan_ip("10.0.0.2".parse().unwrap()));
        assert!(!advertised_lan_ip("100.69.220.4".parse().unwrap()));
        assert!(!advertised_lan_ip("8.8.8.8".parse().unwrap()));
        assert!(!advertised_lan_ip("127.0.0.1".parse().unwrap()));
    }
}
