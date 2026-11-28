#[cfg(test)]
use axum::body::Body;
use axum::{
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
mod anthropic;
mod dispatch;
mod responses;
mod agent_ua;
mod agent_models;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(test)]
use std::collections::VecDeque;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    str::FromStr,
    time::{Duration, Instant},
};
use crate::gateway_proto::{
    error::{openai_error_value, parse_upstream_error_summary, redact_known, redact_secrets},
    request::{apply_400_repairs, prepare_upstream_body, repairable_params_on_400},
    sources::{
        build_upstream_headers, host_of_url, GEMINI_AISTUDIO_MODELS_URL,
    },
    responses::from_chat_request,
    stream::{extract_usage_details, StreamStats},
    thinking::{normalize_non_stream_response, summarize_non_stream_tools},
    GatewaySourceKind, OpenAiWire,
};

use crate::gateway::{
    effective_models, lock, model_id, peer_allowed,
    routing::{group_is_live, group_model_id},
    Gateway, Published,
};

const MAX_GEMINI_MODEL_PAGES: usize = 5;
const MODELS_TIMEOUT_SECS: u64 = 25;
const CHAT_TIMEOUT_SECS: u64 = 300;

pub(crate) use self::agent_ua::REQ_AGENT;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayDiagEntry {
    pub id: u64,
    pub timestamp: i64,
    pub channel_id: String,
    pub channel_alias: String,
    pub source: String,
    pub model: String,
    #[serde(default)]
    pub upstream_model: Option<String>,
    #[serde(default)]
    pub route_group: Option<String>,
    #[serde(default)]
    pub failover_count: u8,
    #[serde(default)]
    pub masked_count: usize,
    #[serde(default)]
    pub protocol: String,
    pub stream: bool,
    pub status: u16,
    pub latency_ms: u64,
    #[serde(default)]
    pub first_byte_ms: Option<u64>,
    #[serde(default)]
    pub frame_count: usize,
    pub transforms: Vec<String>,
    pub thinking_chars: usize,
    pub content_chars: usize,
    #[serde(default)]
    pub refusal_chars: usize,
    #[serde(default)]
    pub tool_call_count: usize,
    #[serde(default)]
    pub tool_call_chars: usize,
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub cached_prompt_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    #[serde(default)]
    pub estimated_cost_cny: Option<f64>,
    #[serde(default)]
    pub diag_note: Option<String>,
    pub error: Option<String>,
    /// 入站 User-Agent 归因（仅展示，不作鉴权；UA 可伪造）
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFetchResult {
    pub fetched: Vec<String>,
    pub host: String,
    pub probe_note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTestResult {
    pub ok: bool,
    pub status: u16,
    pub latency_ms: u64,
    pub first_byte_ms: Option<u64>,
    pub frame_count: usize,
    pub requested_model: String,
    pub upstream_model: String,
    pub thinking: String,
    pub content: String,
    pub refusal_chars: usize,
    pub tool_call_count: usize,
    pub tool_call_chars: usize,
    pub finish_reason: Option<String>,
    pub transforms: Vec<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub diag_note: Option<String>,
    pub error: Option<String>,
}

pub fn build_router(g: Gateway) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/models/{*id}", get(get_model))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/messages", post(anthropic::messages))
        .route("/v1/messages/count_tokens", post(anthropic::count_tokens))
        // 兼容未在 custom_url 末尾填写 `/v1` 的客户端（例如直接填 http://host:18771）
        .route("/models", get(list_models))
        .route("/models/{*id}", get(get_model))
        .route("/chat/completions", post(chat))
        .route("/messages", post(anthropic::messages))
        .route("/messages/count_tokens", post(anthropic::count_tokens))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .layer(axum::middleware::from_fn(self::agent_ua::agent_layer))
        .with_state(g)
}

pub fn json_error(
    status: StatusCode,
    msg: &str,
    err_type: &str,
    code: Option<&str>,
    param: Option<&str>,
) -> Response {
    (status, Json(openai_error_value(msg, err_type, code, param))).into_response()
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn healthz(
    State(g): State<Gateway>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if headers.contains_key(header::ORIGIN) {
        return json_error(StatusCode::UNAUTHORIZED, "禁止浏览器跨域访问", "gateway_error", None, None);
    }
    if !peer_allowed(peer, &lock(&g.inner).settings) {
        return json_error(StatusCode::FORBIDDEN, "仅允许本机或局域网访问", "gateway_error", None, None);
    }
    Json(json!({ "ok": true, "running": true })).into_response()
}

async fn list_models(
    State(g): State<Gateway>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let settings = g.access_settings();
    if !peer_allowed(peer, &settings) {
        return json_error(StatusCode::FORBIDDEN, "仅允许本机或局域网访问", "gateway_error", None, None);
    }
    if !g.is_authorized(&headers, &settings).await {
        return json_error(StatusCode::UNAUTHORIZED, "网关密钥无效", "gateway_error", None, None);
    }
    let mut seen = BTreeSet::new();
    let mut data: Vec<Value> = Vec::new();
    let live = match g.live_channels() {
        Ok(v) => v,
        Err(e) => return dispatch::DispatchError::vault(e).into_openai_response(),
    };
    for c in &live {
        let kind = c.kind();
        for id in effective_models(c) {
            let full = model_id(c, &id);
            if seen.insert(full.clone()) {
                data.push(json!({
                    "id": full,
                    "object": "model",
                    "owned_by": kind.as_str(),
                }));
            }
        }
    }
    for gr in &lock(&g.inner).settings.groups {
        if group_is_live(gr, &live) {
            let full = group_model_id(&gr.id);
            if seen.insert(full.clone()) {
                data.push(json!({
                    "id": full,
                    "object": "model",
                    "owned_by": "routing_group",
                }));
            }
        }
    }
    Json(json!({ "object": "list", "data": data })).into_response()
}

async fn get_model(
    State(g): State<Gateway>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(target): Path<String>,
) -> Response {
    let settings = g.access_settings();
    if !peer_allowed(peer, &settings) {
        return json_error(StatusCode::FORBIDDEN, "仅允许本机或局域网访问", "gateway_error", None, None);
    }
    if !g.is_authorized(&headers, &settings).await {
        return json_error(StatusCode::UNAUTHORIZED, "网关密钥无效", "gateway_error", None, None);
    }
    let live = match g.live_channels() {
        Ok(v) => v,
        Err(e) => return dispatch::DispatchError::vault(e).into_openai_response(),
    };
    for c in &live {
        for id in effective_models(c) {
            let full = model_id(c, &id);
            if full == target {
                return Json(json!({
                    "id": full,
                    "object": "model",
                    "owned_by": c.kind().as_str(),
                }))
                .into_response();
            }
        }
    }
    for gr in &lock(&g.inner).settings.groups {
        if group_is_live(gr, &live) && group_model_id(&gr.id) == target {
            return Json(json!({
                "id": target,
                "object": "model",
                "owned_by": "routing_group",
            }))
            .into_response();
        }
    }
    json_error(
        StatusCode::NOT_FOUND,
        "模型不存在或已下架",
        "invalid_request_error",
        Some("model_not_found"),
        Some("model"),
    )
}

async fn chat(
    State(g): State<Gateway>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let settings = g.access_settings();
    if !peer_allowed(peer, &settings) {
        return json_error(StatusCode::FORBIDDEN, "仅允许本机或局域网访问", "gateway_error", None, None);
    }
    if !g.is_authorized(&headers, &settings).await {
        return json_error(StatusCode::UNAUTHORIZED, "网关密钥无效", "gateway_error", None, None);
    }
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
        .unwrap_or(false);
    if !is_json {
        return json_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "需要 application/json", "invalid_request_error", None, None);
    }
    let data: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "请求必须是 JSON", "invalid_request_error", None, None),
    };
    let requested_model = match data.get("model").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "缺少 model",
                "invalid_request_error",
                Some("missing_model"),
                Some("model"),
            )
        }
    };

    dispatch::handle_openai_chat(&g, &headers, data, &requested_model).await
}

async fn load_upstream_key(g: &Gateway, channel: &Published) -> Result<Option<String>, String> {
    if channel.no_upstream_key && channel.kind() == GatewaySourceKind::OpenAiCompat {
        return Ok(None);
    }
    let vid = channel.vault_id.clone();
    if vid.trim().is_empty() {
        return Err("未关联密钥箱条目".into());
    }
    let credentials = g.credentials.clone();
    match tokio::task::spawn_blocking(move || credentials.get(&vid)).await {
        Ok(Ok(Some(k))) if !k.trim().is_empty() => Ok(Some(k)),
        _ => Err("上游密钥不可用（请在密钥箱确认已填写密钥）".into()),
    }
}

fn apply_headers(
    mut builder: reqwest::RequestBuilder,
    hdrs: &[(String, String)],
) -> Result<reqwest::RequestBuilder, String> {
    for (k, v) in hdrs {
        let name = HeaderName::from_str(k).map_err(|_| format!("请求头名称无效：{k}"))?;
        let val = HeaderValue::from_str(v).map_err(|_| format!("请求头 {k} 包含非法字符"))?;
        builder = builder.header(name, val);
    }
    Ok(builder)
}

enum UpstreamReady {
    Ok {
        resp: reqwest::Response,
        transforms: Vec<String>,
        requested_stream: bool,
        include_usage: bool,
        expected_choices: Option<u64>,
        sensitive: Vec<String>,
    },
    Err {
        status: StatusCode,
        message: String,
        code: Option<String>,
        retry_after: Option<HeaderValue>,
        transforms: Vec<String>,
    },
}

async fn send_upstream_with_repair(
    g: &Gateway,
    channel: &Published,
    upstream_model: &str,
    mut data: Value,
) -> UpstreamReady {
    let kind = channel.kind();
    let api_key = match load_upstream_key(g, channel).await {
        Ok(k) => k,
        Err(msg) => {
            return UpstreamReady::Err {
                status: StatusCode::SERVICE_UNAVAILABLE,
                message: msg,
                code: Some("missing_upstream_key".into()),
                retry_after: None,
                transforms: vec![],
            };
        }
    };
    let sensitive = responses::sensitive_values(api_key.as_deref(), &channel.custom_headers);
    let hdrs = match build_upstream_headers(kind, api_key.as_deref(), &channel.custom_headers) {
        Ok(h) => h,
        Err(msg) => {
            return UpstreamReady::Err {
                status: StatusCode::BAD_REQUEST,
                message: redact_known(&msg, &sensitive),
                code: Some("invalid_upstream_header".into()),
                retry_after: None,
                transforms: vec![],
            };
        }
    };
    let client = match g.clients.client_async(channel.system_proxy).await {
        Ok(c) => c,
        Err(_) => {
            return UpstreamReady::Err {
                status: StatusCode::BAD_GATEWAY,
                message: "无法初始化上游代理连接".into(),
                code: Some("proxy_init_failed".into()),
                retry_after: None,
                transforms: vec![],
            };
        }
    };

    data["model"] = Value::String(upstream_model.to_string());
    let requested_stream = data.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let include_usage = data.pointer("/stream_options/include_usage") == Some(&Value::Bool(true));
    let expected_choices = data.get("n").and_then(Value::as_u64);
    let wire = channel.wire;
    let mut transforms = match wire {
        OpenAiWire::ChatCompletions => prepare_upstream_body(
            kind, &channel.base_url, upstream_model, channel.thought_policy, &mut data,
        ).transforms,
        OpenAiWire::Responses => match from_chat_request(&data) {
            Ok((converted, transforms)) => {
                data = converted;
                transforms
            }
            Err(message) => return UpstreamReady::Err {
                status: StatusCode::BAD_REQUEST,
                message: redact_known(&message, &sensitive),
                code: Some("responses_conversion".into()),
                retry_after: None,
                transforms: vec![],
            },
        },
    };

    let url = format!("{}{}", channel.base_url.trim_end_matches('/'), wire.endpoint());
    let mut repairs = 0u8;
    loop {
        let req = match apply_headers(client.post(&url).timeout(Duration::from_secs(CHAT_TIMEOUT_SECS)), &hdrs) {
            Ok(b) => b.json(&data),
            Err(msg) => {
                return UpstreamReady::Err {
                    status: StatusCode::BAD_REQUEST,
                    message: redact_known(&msg, &sensitive),
                    code: Some("invalid_upstream_header".into()),
                    retry_after: None,
                    transforms,
                };
            }
        };
        let upstream = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                let (status, code, msg) = classify_upstream_send_error(e.is_timeout(), &e.to_string());
                return UpstreamReady::Err {
                    status,
                    message: redact_known(&msg, &sensitive),
                    code: Some(code.into()),
                    retry_after: None,
                    transforms,
                };
            }
        };
        let status = upstream.status();
        if status.is_success() {
            return UpstreamReady::Ok {
                resp: upstream,
                transforms,
                requested_stream,
                include_usage,
                expected_choices,
                sensitive,
            };
        }

        let retry_after = upstream.headers().get(header::RETRY_AFTER).cloned()
            .and_then(|v| v.to_str().ok().and_then(|s| HeaderValue::from_str(&redact_known(s, &sensitive)).ok()));
        let err_body = match responses::read_error_body(upstream, &sensitive).await {
            Ok(body) => body,
            Err(message) => return UpstreamReady::Err {
                status: StatusCode::BAD_GATEWAY, message,
                code: Some("gateway_protocol".into()), retry_after: None, transforms,
            },
        };
        if status == StatusCode::BAD_REQUEST && repairs < 1 {
            if let Some(mut params) = repairable_params_on_400(kind, &err_body) {
                if wire == OpenAiWire::Responses {
                    params.retain(|p| matches!(p.as_str(), "temperature" | "top_p"));
                }
                let removed = apply_400_repairs(&mut data, &params);
                if !removed.is_empty() {
                    for r in removed {
                        transforms.push(format!("repair_400:{r}"));
                    }
                    repairs += 1;
                    continue;
                }
            }
        }
        let (msg, code) = parse_upstream_error_summary(status.as_u16(), &err_body);
        return UpstreamReady::Err {
            status,
            message: msg,
            code,
            retry_after,
            transforms,
        };
    }
}

fn infer_zero_output_note(
    status: u16,
    thinking_chars: usize,
    content_chars: usize,
    refusal_chars: usize,
    tool_call_count: usize,
    frame_count: usize,
    finish_reason: Option<&str>,
    stats_note: Option<&str>,
    error: Option<&str>,
) -> Option<String> {
    if let Some(note) = stats_note {
        return Some(redact_secrets(note));
    }
    if error.is_some() || status >= 400 {
        return None;
    }
    if thinking_chars == 0 && content_chars == 0 && refusal_chars == 0 && tool_call_count == 0 {
        return Some(match finish_reason {
            Some(fr) => format!("上游已结束（finish_reason={fr}，{frame_count} 帧），未返回文本或工具调用"),
            None if frame_count == 0 => "上游未返回任何有效数据帧".to_string(),
            None => format!("上游返回 {frame_count} 帧，但未解析出文本或工具调用"),
        });
    }
    None
}

pub(super) fn record_error_diag_ctx(
    g: &Gateway,
    channel: &Published,
    requested_model: &str,
    upstream_model: Option<String>,
    route_group: Option<String>,
    failover_count: u8,
    masked_count: usize,
    protocol: &str,
    stream: bool,
    status: u16,
    started: Instant,
    transforms: Vec<String>,
    message: &str,
) {
    g.record_diag(GatewayDiagEntry {
        id: 0,
        agent: None,
        timestamp: now_unix(),
        channel_id: channel.channel_id.clone(),
        channel_alias: channel.alias.clone(),
        source: channel.kind().as_str().to_string(),
        model: requested_model.to_string(),
        upstream_model,
        route_group,
        failover_count,
        masked_count,
        protocol: protocol.to_string(),
        stream,
        status,
        latency_ms: started.elapsed().as_millis() as u64,
        first_byte_ms: None,
        frame_count: 0,
        transforms,
        thinking_chars: 0,
        content_chars: 0,
        refusal_chars: 0,
        tool_call_count: 0,
        tool_call_chars: 0,
        finish_reason: None,
        prompt_tokens: None,
        cached_prompt_tokens: None,
        cache_creation_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        estimated_cost_cny: None,
        diag_note: None,
        error: Some(redact_secrets(message)),
    });
}

fn record_error_diag(
    g: &Gateway,
    channel: &Published,
    requested_model: &str,
    stream: bool,
    status: u16,
    started: Instant,
    transforms: Vec<String>,
    message: &str,
) {
    record_error_diag_ctx(
        g,
        channel,
        requested_model,
        None,
        None,
        0,
        0,
        "openai_chat",
        stream,
        status,
        started,
        transforms,
        message,
    );
}

#[cfg(test)]
async fn execute_inference_http(
    g: &Gateway,
    channel: Published,
    requested_model: &str,
    upstream_model: &str,
    data: Value,
) -> Response {
    let started = Instant::now();
    let stream = data.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let mut guard = responses::DiagnosticGuard::new(g, &channel, requested_model, stream, started);
    match send_upstream_with_repair(g, &channel, upstream_model, data).await {
        UpstreamReady::Err {
            status,
            message,
            code,
            retry_after,
            transforms,
        } => {
            record_error_diag(
                g,
                &channel,
                requested_model,
                stream,
                status.as_u16(),
                started,
                transforms,
                &message,
            );
            guard.disarm();
            let error_type = if code.as_deref() == Some("responses_conversion") {
                "invalid_request_error"
            } else {
                "upstream_error"
            };
            let mut resp = json_error(status, &message, error_type, code.as_deref(), None);
            if let Some(ra) = retry_after {
                resp.headers_mut().insert(header::RETRY_AFTER, ra);
            }
            resp
        }
        UpstreamReady::Ok {
            resp,
            transforms,
            requested_stream,
            include_usage,
            expected_choices,
            sensitive,
        } => {
            let ct = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            let is_sse = requested_stream || ct.contains("text/event-stream");
            guard.update(&transforms, is_sse);
            if is_sse {
                build_sse_response(
                    channel,
                    requested_model.to_string(),
                    resp,
                    include_usage,
                    expected_choices,
                    sensitive,
                    guard,
                )
            } else {
                let response = build_json_response(
                    g, &channel, requested_model, transforms, started, resp, &sensitive,
                ).await;
                guard.disarm();
                response
            }
        }
    }
}

#[cfg(test)]
fn build_sse_response(
    channel: Published,
    requested_model: String,
    upstream: reqwest::Response,
    include_usage: bool,
    expected_choices: Option<u64>,
    sensitive: Vec<String>,
    guard: responses::DiagnosticGuard,
) -> Response {
    struct StreamState<S> {
        stream: S,
        normalizer: responses::ChatSse,
        pending: VecDeque<String>,
        finished: bool,
        sensitive: Vec<String>,
        guard: responses::DiagnosticGuard,
    }

    let state = StreamState {
        stream: Box::pin(upstream.bytes_stream()),
        normalizer: responses::ChatSse::new(&requested_model, channel.thinking_mode, channel.wire, include_usage)
            .with_expected_choices(expected_choices).with_sensitive_values(sensitive.clone()),
        pending: VecDeque::new(),
        finished: false,
        sensitive,
        guard,
    };

    let body_stream = futures::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(next_frame) = st.pending.pop_front() {
                return Some((Ok::<Bytes, std::io::Error>(Bytes::from(next_frame)), st));
            }
            if st.finished {
                return None;
            }
            let transport_error = match st.stream.next().await {
                Some(Ok(chunk)) => {
                    st.pending.extend(st.normalizer.push_bytes(&chunk));
                    st.guard.observe(st.normalizer.stats(), !chunk.is_empty());
                    if !st.normalizer.is_terminal() {
                        continue;
                    }
                    None
                }
                Some(Err(e)) => Some(redact_known(&format!("流式传输中断：{e}"), &st.sensitive)),
                None => None,
            };
            st.finished = true;
            st.pending.extend(st.normalizer.finish());
            let stats = st.normalizer.stats();
            let error = transport_error.or_else(|| {
                stats.saw_error.then(|| {
                    stats
                        .diagnostic_note
                        .clone()
                        .unwrap_or_else(|| "上游流式未正常完成".into())
                })
            });
            st.guard.finish_stream(stats, error);
        }
    });

    let mut response = Response::new(Body::from_stream(body_stream));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

pub(super) fn record_stream_diag_with_ctx(
    g: &Gateway,
    agent: Option<String>,
    channel: &Published,
    requested_model: &str,
    upstream_model: Option<String>,
    route_group: Option<String>,
    failover_count: u8,
    masked_count: usize,
    protocol: &str,
    transforms: &[String],
    started: Instant,
    first_byte_ms: Option<u64>,
    stats: &StreamStats,
    err_note: Option<String>,
) {
    let status = if err_note.is_some() { 502 } else { 200 };
    let error = err_note.map(|e| redact_secrets(&e));
    let diag_note = infer_zero_output_note(
        status,
        stats.thinking_chars,
        stats.content_chars,
        stats.refusal_chars,
        stats.tool_call_count,
        stats.frame_count,
        stats.finish_reason.as_deref(),
        stats.diagnostic_note.as_deref(),
        error.as_deref(),
    );
    g.record_diag(GatewayDiagEntry {
        id: 0,
        agent,
        timestamp: now_unix(),
        channel_id: channel.channel_id.clone(),
        channel_alias: channel.alias.clone(),
        source: channel.kind().as_str().to_string(),
        model: requested_model.to_string(),
        upstream_model,
        route_group,
        failover_count,
        masked_count,
        protocol: protocol.to_string(),
        stream: true,
        status,
        latency_ms: started.elapsed().as_millis() as u64,
        first_byte_ms,
        frame_count: stats.frame_count,
        transforms: transforms.to_vec(),
        thinking_chars: stats.thinking_chars,
        content_chars: stats.content_chars,
        refusal_chars: stats.refusal_chars,
        tool_call_count: stats.tool_call_count,
        tool_call_chars: stats.tool_call_chars,
        finish_reason: stats.finish_reason.clone(),
        prompt_tokens: stats.prompt_tokens,
        cached_prompt_tokens: stats.cached_prompt_tokens,
        cache_creation_tokens: stats.cache_creation_tokens,
        completion_tokens: stats.completion_tokens,
        reasoning_tokens: stats.reasoning_tokens,
        estimated_cost_cny: None,
        diag_note,
        error,
    });
}

#[cfg(test)]
async fn build_json_response(
    g: &Gateway,
    channel: &Published,
    requested_model: &str,
    transforms: Vec<String>,
    started: Instant,
    upstream: reqwest::Response,
    sensitive: &[String],
) -> Response {
    let status = upstream.status();
    let mut val = match responses::read_chat_json(upstream, channel.wire, requested_model, sensitive).await {
        Ok(value) => value,
        Err(msg) => {
            record_error_diag(g, channel, requested_model, false, 502, started, transforms, &msg);
            return json_error(StatusCode::BAD_GATEWAY, &msg, "gateway_protocol", Some("gateway_protocol"), None);
        }
    };

    let (thinking_chars, content_chars) =
        normalize_non_stream_response(&mut val, requested_model, channel.thinking_mode);
    let (tool_call_count, tool_call_chars) = summarize_non_stream_tools(&val);
    let refusal_chars = responses::refusal_chars(&val);
    let finish_reason = val
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("finish_reason"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let usage = extract_usage_details(&val["usage"]);
    let frame_count = val
        .get("choices")
        .and_then(Value::as_array)
        .map(|a| usize::from(!a.is_empty()))
        .unwrap_or(0);
    let diag_note = infer_zero_output_note(
        status.as_u16(),
        thinking_chars,
        content_chars,
        refusal_chars,
        tool_call_count,
        frame_count,
        finish_reason.as_deref(),
        None,
        None,
    );

    g.record_diag(GatewayDiagEntry {
        id: 0,
        agent: None,
        timestamp: now_unix(),
        channel_id: channel.channel_id.clone(),
        channel_alias: channel.alias.clone(),
        source: channel.kind().as_str().to_string(),
        model: requested_model.to_string(),
        protocol: "openai_chat".into(),
        stream: false,
        status: status.as_u16(),
        latency_ms: started.elapsed().as_millis() as u64,
        first_byte_ms: None,
        frame_count,
        transforms,
        thinking_chars,
        content_chars,
        refusal_chars,
        tool_call_count,
        tool_call_chars,
        finish_reason,
        prompt_tokens: usage.prompt_tokens,
        cached_prompt_tokens: usage.cached_prompt_tokens,
        cache_creation_tokens: usage.cache_creation_tokens,
        completion_tokens: usage.completion_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        diag_note,
        error: None,
        ..Default::default()
    });

    (status, Json(val)).into_response()
}

pub async fn run_test_chat(
    g: &Gateway,
    channel: Published,
    upstream_model: String,
    prompt: String,
    stream: bool,
    include_reasoning: bool,
    reasoning_effort: Option<String>,
) -> ChatTestResult {
    let started = Instant::now();
    let requested_model = model_id(&channel, &upstream_model);
    let mut guard = responses::DiagnosticGuard::new(g, &channel, &requested_model, stream, started);
    let mut body = json!({
        "messages": [{ "role": "user", "content": prompt }],
        "stream": stream,
        "max_tokens": 512,
        "include_reasoning": include_reasoning,
    });
    if let Some(eff) = reasoning_effort.filter(|s| !s.trim().is_empty()) {
        body["reasoning_effort"] = Value::String(eff);
    }
    if stream && channel.wire == OpenAiWire::Responses {
        body["stream_options"] = json!({"include_usage":true});
    }

    match send_upstream_with_repair(g, &channel, &upstream_model, body).await {
        UpstreamReady::Err {
            status,
            message,
            transforms,
            ..
        } => {
            record_error_diag(
                g,
                &channel,
                &requested_model,
                stream,
                status.as_u16(),
                started,
                transforms.clone(),
                &message,
            );
            guard.disarm();
            ChatTestResult {
                ok: false,
                status: status.as_u16(),
                latency_ms: started.elapsed().as_millis() as u64,
                first_byte_ms: None,
                frame_count: 0,
                requested_model,
                upstream_model,
                thinking: String::new(),
                content: String::new(),
                refusal_chars: 0,
                tool_call_count: 0,
                tool_call_chars: 0,
                finish_reason: None,
                transforms,
                prompt_tokens: None,
                completion_tokens: None,
                diag_note: None,
                error: Some(message),
            }
        }
        UpstreamReady::Ok {
            resp,
            transforms,
            requested_stream,
            include_usage,
            expected_choices,
            sensitive,
        } => {
            let status = resp.status().as_u16();
            let ct = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            guard.update(&transforms, requested_stream || ct.contains("text/event-stream"));
            if requested_stream || ct.contains("text/event-stream") {
                let mut normalizer = responses::ChatSse::new(
                    &requested_model, channel.thinking_mode, channel.wire, include_usage,
                ).with_expected_choices(expected_choices).with_sensitive_values(sensitive.clone());
                let mut thinking = String::new();
                let mut content = String::new();
                let mut error_msg: Option<String> = None;
                let mut first_byte_ms: Option<u64> = None;
                let mut frames_text = Vec::new();
                let mut bytes_stream = resp.bytes_stream();
                while let Some(item) = bytes_stream.next().await {
                    match item {
                        Ok(chunk) => {
                            if first_byte_ms.is_none() && !chunk.is_empty() {
                                first_byte_ms = Some(started.elapsed().as_millis() as u64);
                            }
                            frames_text.extend(normalizer.push_bytes(&chunk));
                            guard.observe(normalizer.stats(), !chunk.is_empty());
                            if normalizer.is_terminal() {
                                break;
                            }
                        }
                        Err(e) => {
                            error_msg = Some(redact_known(&format!("读取流失败：{e}"), &sensitive));
                            break;
                        }
                    }
                }
                frames_text.extend(normalizer.finish());
                for frame in frames_text {
                    if let Some(json_str) = frame.strip_prefix("data: ") {
                        let payload = json_str.trim();
                        if payload == "[DONE]" {
                            continue;
                        }
                        if let Ok(v) = serde_json::from_str::<Value>(payload) {
                            if let Some(msg) = v
                                .pointer("/error/message")
                                .and_then(Value::as_str)
                            {
                                error_msg = Some(msg.to_string());
                            }
                            if let Some(delta) = v.pointer("/choices/0/delta") {
                                if let Some(rc) = delta.get("reasoning_content").and_then(Value::as_str) {
                                    thinking.push_str(rc);
                                }
                                if let Some(c) = delta.get("content").and_then(Value::as_str) {
                                    content.push_str(c);
                                }
                            }
                        }
                    }
                }
                let stats = normalizer.stats();
                let ok = error_msg.is_none() && stats.completed_cleanly;
                let final_error = error_msg.or_else(|| {
                    (!ok).then(|| {
                        stats
                            .diagnostic_note
                            .clone()
                            .unwrap_or_else(|| "上游流式未正常完成".into())
                    })
                });
                let diag_note = infer_zero_output_note(
                    if ok { status } else { 502 },
                    stats.thinking_chars,
                    stats.content_chars,
                    stats.refusal_chars,
                    stats.tool_call_count,
                    stats.frame_count,
                    stats.finish_reason.as_deref(),
                    stats.diagnostic_note.as_deref(),
                    final_error.as_deref(),
                );
                guard.finish_stream(stats, final_error.clone());
                ChatTestResult {
                    ok,
                    status: if ok { status } else { 502 },
                    latency_ms: started.elapsed().as_millis() as u64,
                    first_byte_ms,
                    frame_count: stats.frame_count,
                    requested_model,
                    upstream_model,
                    thinking,
                    content,
                    refusal_chars: stats.refusal_chars,
                    tool_call_count: stats.tool_call_count,
                    tool_call_chars: stats.tool_call_chars,
                    finish_reason: stats.finish_reason.clone(),
                    transforms,
                    prompt_tokens: stats.prompt_tokens,
                    completion_tokens: stats.completion_tokens,
                    diag_note,
                    error: final_error,
                }
            } else {
                let first_byte_ms = Some(started.elapsed().as_millis() as u64);
                let mut val = match responses::read_chat_json(resp, channel.wire, &requested_model, &sensitive).await {
                    Ok(v) => v,
                    Err(msg) => {
                        record_error_diag(
                            g,
                            &channel,
                            &requested_model,
                            false,
                            502,
                            started,
                            transforms.clone(),
                            &msg,
                        );
                        guard.disarm();
                        return ChatTestResult {
                            ok: false,
                            status: 502,
                            latency_ms: started.elapsed().as_millis() as u64,
                            first_byte_ms,
                            frame_count: 0,
                            requested_model,
                            upstream_model,
                            thinking: String::new(),
                            content: String::new(),
                            refusal_chars: 0,
                            tool_call_count: 0,
                            tool_call_chars: 0,
                            finish_reason: None,
                            transforms,
                            prompt_tokens: None,
                            completion_tokens: None,
                            diag_note: None,
                            error: Some(msg),
                        };
                    }
                };
                let (tc, cc) = normalize_non_stream_response(
                    &mut val,
                    &requested_model,
                    channel.thinking_mode,
                );
                let (tool_call_count, tool_call_chars) = summarize_non_stream_tools(&val);
                let refusal_chars = responses::refusal_chars(&val);
                let msg = val.pointer("/choices/0/message").cloned().unwrap_or(Value::Null);
                let thinking = msg
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let content = msg
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let finish_reason = val
                    .pointer("/choices/0/finish_reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let usage = extract_usage_details(&val["usage"]);
                let prompt_tokens = usage.prompt_tokens;
                let completion_tokens = usage.completion_tokens;
                let frame_count = val
                    .get("choices")
                    .and_then(Value::as_array)
                    .map(|a| usize::from(!a.is_empty()))
                    .unwrap_or(0);
                let diag_note = infer_zero_output_note(
                    status,
                    tc,
                    cc,
                    refusal_chars,
                    tool_call_count,
                    frame_count,
                    finish_reason.as_deref(),
                    None,
                    None,
                );
                g.record_diag(GatewayDiagEntry {
                    id: 0,
                    agent: None,
                    timestamp: now_unix(),
                    channel_id: channel.channel_id.clone(),
                    channel_alias: channel.alias.clone(),
                    source: channel.kind().as_str().to_string(),
                    model: requested_model.clone(),
                    upstream_model: Some(upstream_model.clone()),
                    protocol: "openai_chat".into(),
                    stream: false,
                    status,
                    latency_ms: started.elapsed().as_millis() as u64,
                    first_byte_ms,
                    frame_count,
                    transforms: transforms.clone(),
                    thinking_chars: tc,
                    content_chars: cc,
                    refusal_chars,
                    tool_call_count,
                    tool_call_chars,
                    finish_reason: finish_reason.clone(),
                    prompt_tokens,
                    cached_prompt_tokens: usage.cached_prompt_tokens,
                    cache_creation_tokens: usage.cache_creation_tokens,
                    completion_tokens,
                    reasoning_tokens: usage.reasoning_tokens,
                    diag_note: diag_note.clone(),
                    error: None,
                    ..Default::default()
                });
                guard.disarm();
                ChatTestResult {
                    ok: true,
                    status,
                    latency_ms: started.elapsed().as_millis() as u64,
                    first_byte_ms,
                    frame_count,
                    requested_model,
                    upstream_model,
                    thinking,
                    content,
                    refusal_chars,
                    tool_call_count,
                    tool_call_chars,
                    finish_reason,
                    transforms,
                    prompt_tokens,
                    completion_tokens,
                    diag_note,
                    error: None,
                }
            }
        }
    }
}

pub async fn fetch_configured_models(
    client: &reqwest::Client,
    channel: &Published,
    api_key: Option<&str>,
) -> Result<ModelFetchResult, String> {
    let kind = channel.kind();
    if channel.wire == OpenAiWire::Responses && !channel.manual_models.is_empty() {
        let sensitive = responses::sensitive_values(api_key, &channel.custom_headers);
        build_upstream_headers(kind, api_key, &channel.custom_headers)
            .map_err(|e| redact_known(&e, &sensitive))?;
        return Ok(ModelFetchResult {
            fetched: vec![],
            host: host_of_url(&channel.base_url),
            probe_note: Some("使用手动模型，请通过「测连通」验证 Responses 接口".into()),
        });
    }
    fetch_channel_models(client, kind, &channel.base_url, api_key, &channel.custom_headers).await
}

pub async fn fetch_channel_models(
    client: &reqwest::Client,
    kind: GatewaySourceKind,
    base_url: &str,
    api_key: Option<&str>,
    custom_headers: &BTreeMap<String, String>,
) -> Result<ModelFetchResult, String> {
    let host = host_of_url(base_url);
    let sensitive = responses::sensitive_values(api_key, custom_headers);
    match kind {
        GatewaySourceKind::AgentPlatform => {
            agent_models::fetch_agent_platform_models(
                client,
                base_url,
                api_key,
                custom_headers,
                host,
                &sensitive,
            )
            .await
        }
        GatewaySourceKind::GeminiAiStudio => {
            let is_official = host == "generativelanguage.googleapis.com";
            if is_official {
                let key = api_key
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "Gemini AI Studio 密钥为空".to_string())?;
                let mut ids = Vec::new();
                let mut page_token: Option<String> = None;
                for _ in 0..MAX_GEMINI_MODEL_PAGES {
                    let url = match &page_token {
                        Some(tok) => format!("{GEMINI_AISTUDIO_MODELS_URL}&pageToken={tok}"),
                        None => GEMINI_AISTUDIO_MODELS_URL.to_string(),
                    };
                    let res = client
                        .get(&url)
                        .header("x-goog-api-key", key)
                        .timeout(Duration::from_secs(MODELS_TIMEOUT_SECS))
                        .send()
                        .await
                        .map_err(|e| redact_known(&format!("读取 Gemini 模型列表失败：{e}"), &sensitive))?;
                    let status = res.status();
                    if !status.is_success() {
                        let txt = responses::read_error_body(res, &sensitive).await?;
                        let (summary, _) = parse_upstream_error_summary(status.as_u16(), &txt);
                        return Err(summary);
                    }
                    let bytes = responses::read_body_capped(res, &sensitive).await?;
                    let body: Value = serde_json::from_slice(&bytes).map_err(|_| "Gemini 返回格式异常".to_string())?;
                    for item in body.get("models").and_then(Value::as_array).into_iter().flatten() {
                        let supports_gen = item
                            .get("supportedGenerationMethods")
                            .and_then(Value::as_array)
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(Value::as_str)
                                    .any(|m| m == "generateContent")
                            })
                            .unwrap_or(false);
                        if supports_gen {
                            if let Some(name) = item.get("name").and_then(Value::as_str) {
                                ids.push(name.strip_prefix("models/").unwrap_or(name).to_string());
                            }
                        }
                    }
                    page_token = body
                        .get("nextPageToken")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    if page_token.is_none() {
                        break;
                    }
                }
                Ok(ModelFetchResult {
                    fetched: dedupe_nonempty(ids),
                    host,
                    probe_note: None,
                })
            } else {
                fetch_openai_models(client, kind, base_url, api_key, custom_headers, host).await
            }
        }
        GatewaySourceKind::OpenAiCompat => {
            fetch_openai_models(client, kind, base_url, api_key, custom_headers, host).await
        }
    }
}

async fn fetch_openai_models(
    client: &reqwest::Client,
    kind: GatewaySourceKind,
    base_url: &str,
    api_key: Option<&str>,
    custom_headers: &BTreeMap<String, String>,
    host: String,
) -> Result<ModelFetchResult, String> {
    let sensitive = responses::sensitive_values(api_key, custom_headers);
    let hdrs = build_upstream_headers(kind, api_key, custom_headers)
        .map_err(|e| redact_known(&e, &sensitive))?;
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let req = apply_headers(
        client.get(&url).timeout(Duration::from_secs(MODELS_TIMEOUT_SECS)),
        &hdrs,
    ).map_err(|e| redact_known(&e, &sensitive))?;
    let res = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return Err(redact_known(&format!("读取 /models 失败：{e}"), &sensitive));
        }
    };
    let status = res.status();
    if matches!(status.as_u16(), 404 | 405 | 501) {
        return Ok(ModelFetchResult {
            fetched: vec![],
            host,
            probe_note: Some(format!("上游未提供 /models（HTTP {}），请使用手动模型列表", status.as_u16())),
        });
    }
    if !status.is_success() {
        let txt = responses::read_error_body(res, &sensitive).await?;
        let (summary, _) = parse_upstream_error_summary(status.as_u16(), &txt);
        return Err(summary);
    }
    let bytes = responses::read_body_capped(res, &sensitive).await?;
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            return Ok(ModelFetchResult {
                fetched: vec![],
                host,
                probe_note: Some("上游 /models 返回非标准 JSON，请使用手动模型列表".into()),
            })
        }
    };
    let mut ids = Vec::new();
    let arr = body
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| body.as_array())
        .or_else(|| body.get("models").and_then(Value::as_array));
    for item in arr.into_iter().flatten() {
        if let Some(s) = item.as_str() {
            ids.push(s.to_string());
        } else if let Some(id) = item
            .get("id")
            .or_else(|| item.get("name"))
            .and_then(Value::as_str)
        {
            ids.push(id.strip_prefix("models/").unwrap_or(id).to_string());
        }
    }
    Ok(ModelFetchResult {
        fetched: dedupe_nonempty(ids),
        host,
        probe_note: None,
    })
}

pub fn dedupe_nonempty(list: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for raw in list {
        let t = raw.trim().to_string();
        if !t.is_empty() && seen.insert(t.clone()) {
            out.push(t);
        }
    }
    out
}

/// 区分连接失败(502 upstream_unreachable)与首字/响应等待超时(504 upstream_timeout),
/// 避免慢思考模型等待超过阈值时被误报成网络连不上上游。
fn classify_upstream_send_error(is_timeout: bool, err: &str) -> (StatusCode, &'static str, String) {
    if is_timeout {
        (
            StatusCode::GATEWAY_TIMEOUT,
            "upstream_timeout",
            format!("等待上游响应超时（模型首字耗时过长或网络卡顿）：{err}"),
        )
    } else {
        (
            StatusCode::BAD_GATEWAY,
            "upstream_unreachable",
            format!("无法连接上游服务：{err}"),
        )
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod magpie_tests;

