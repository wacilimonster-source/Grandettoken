use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    str::FromStr,
    time::{Duration, Instant},
};
use tokenscope_core::gateway_proto::{
    error::{openai_error_value, parse_upstream_error_summary, redact_secrets},
    request::{apply_400_repairs, prepare_upstream_body, repairable_params_on_400},
    sources::{
        build_upstream_headers, host_of_url, AGENT_PLATFORM_PRESET_MODELS,
        GEMINI_AISTUDIO_MODELS_URL,
    },
    stream::{SseStreamNormalizer, StreamStats},
    thinking::normalize_non_stream_response,
    GatewaySourceKind,
};
use tokenscope_core::secrets;

use crate::gateway::{authorized, effective_models, lock, model_id, peer_allowed, Gateway, Published};

const MAX_GEMINI_MODEL_PAGES: usize = 5;
const MODELS_TIMEOUT_SECS: u64 = 25;
const CHAT_TIMEOUT_SECS: u64 = 180;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayDiagEntry {
    pub id: u64,
    pub timestamp: i64,
    pub channel_id: String,
    pub channel_alias: String,
    pub source: String,
    pub model: String,
    pub stream: bool,
    pub status: u16,
    pub latency_ms: u64,
    pub transforms: Vec<String>,
    pub thinking_chars: usize,
    pub content_chars: usize,
    pub finish_reason: Option<String>,
    pub error: Option<String>,
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
    pub requested_model: String,
    pub upstream_model: String,
    pub thinking: String,
    pub content: String,
    pub finish_reason: Option<String>,
    pub transforms: Vec<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub error: Option<String>,
}

pub fn build_router(g: Gateway) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/models/{*id}", get(get_model))
        .route("/v1/chat/completions", post(chat))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
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
    let settings = lock(&g.inner).settings.clone();
    if !peer_allowed(peer, &settings) {
        return json_error(StatusCode::FORBIDDEN, "仅允许本机或局域网访问", "gateway_error", None, None);
    }
    Json(json!({ "ok": true, "running": true })).into_response()
}

async fn list_models(
    State(g): State<Gateway>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let settings = lock(&g.inner).settings.clone();
    if !peer_allowed(peer, &settings) {
        return json_error(StatusCode::FORBIDDEN, "仅允许本机或局域网访问", "gateway_error", None, None);
    }
    if !authorized(&headers, &settings) {
        return json_error(StatusCode::UNAUTHORIZED, "网关密钥无效", "gateway_error", None, None);
    }
    let mut seen = BTreeSet::new();
    let mut data: Vec<Value> = Vec::new();
    for c in g.live_channels() {
        let kind = c.kind();
        for id in effective_models(&c) {
            let full = model_id(&c, &id);
            if seen.insert(full.clone()) {
                data.push(json!({
                    "id": full,
                    "object": "model",
                    "owned_by": kind.as_str(),
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
    let settings = lock(&g.inner).settings.clone();
    if !peer_allowed(peer, &settings) {
        return json_error(StatusCode::FORBIDDEN, "仅允许本机或局域网访问", "gateway_error", None, None);
    }
    if !authorized(&headers, &settings) {
        return json_error(StatusCode::UNAUTHORIZED, "网关密钥无效", "gateway_error", None, None);
    }
    for c in g.live_channels() {
        for id in effective_models(&c) {
            let full = model_id(&c, &id);
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
    let settings = lock(&g.inner).settings.clone();
    if !peer_allowed(peer, &settings) {
        return json_error(StatusCode::FORBIDDEN, "仅允许本机或局域网访问", "gateway_error", None, None);
    }
    if !authorized(&headers, &settings) {
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

    let resolved = match resolve_channel_and_model(&g, &requested_model) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };

    execute_chat_http(&g, resolved.0, &requested_model, &resolved.1, data).await
}

fn resolve_channel_and_model(
    g: &Gateway,
    requested_model: &str,
) -> Result<(Published, String), Response> {
    let mut matches = g.live_channels().into_iter().filter_map(|c| {
        effective_models(&c)
            .into_iter()
            .find(|id| model_id(&c, id) == requested_model)
            .map(|id| (c, id))
    });
    let first = match matches.next() {
        Some(v) => v,
        None => {
            return Err(json_error(
                StatusCode::NOT_FOUND,
                "模型不存在、已下架或密钥已更新，请刷新模型列表",
                "invalid_request_error",
                Some("model_not_found"),
                Some("model"),
            ))
        }
    };
    if matches.next().is_some() {
        return Err(json_error(
            StatusCode::CONFLICT,
            "多个渠道的别名和模型相同，请修改其中一个别名或调整可见模型",
            "invalid_request_error",
            Some("duplicate_model"),
            Some("model"),
        ));
    }
    Ok(first)
}

async fn load_upstream_key(channel: &Published) -> Result<Option<String>, String> {
    if channel.no_upstream_key && channel.kind() == GatewaySourceKind::OpenAiCompat {
        return Ok(None);
    }
    let vid = channel.vault_id.clone();
    if vid.trim().is_empty() {
        return Err("未关联密钥箱条目".into());
    }
    match tokio::task::spawn_blocking(move || secrets::vault_get(&vid)).await {
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
    let api_key = match load_upstream_key(channel).await {
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
    let hdrs = match build_upstream_headers(kind, api_key.as_deref(), &channel.custom_headers) {
        Ok(h) => h,
        Err(msg) => {
            return UpstreamReady::Err {
                status: StatusCode::BAD_REQUEST,
                message: msg,
                code: Some("invalid_upstream_header".into()),
                retry_after: None,
                transforms: vec![],
            };
        }
    };
    let client = match g.clients.client(channel.system_proxy) {
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
    let mut prepared = prepare_upstream_body(
        kind,
        &channel.base_url,
        upstream_model,
        channel.thought_policy,
        &mut data,
    );

    let url = format!("{}/chat/completions", channel.base_url.trim_end_matches('/'));
    let mut repairs = 0u8;
    loop {
        let req = match apply_headers(client.post(&url).timeout(Duration::from_secs(CHAT_TIMEOUT_SECS)), &hdrs) {
            Ok(b) => b.json(&data),
            Err(msg) => {
                return UpstreamReady::Err {
                    status: StatusCode::BAD_REQUEST,
                    message: msg,
                    code: Some("invalid_upstream_header".into()),
                    retry_after: None,
                    transforms: prepared.transforms,
                };
            }
        };
        let upstream = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                return UpstreamReady::Err {
                    status: StatusCode::BAD_GATEWAY,
                    message: redact_secrets(&format!("无法连接上游服务：{e}")),
                    code: Some("upstream_unreachable".into()),
                    retry_after: None,
                    transforms: prepared.transforms,
                };
            }
        };
        let status = upstream.status();
        if status.is_success() {
            return UpstreamReady::Ok {
                resp: upstream,
                transforms: prepared.transforms,
                requested_stream,
            };
        }

        let retry_after = upstream.headers().get(header::RETRY_AFTER).cloned();
        let err_body = upstream.text().await.unwrap_or_default();
        if status.as_u16() == 400 && repairs < 1 {
            if let Some(params) = repairable_params_on_400(kind, &err_body) {
                let removed = apply_400_repairs(&mut data, &params);
                if !removed.is_empty() {
                    for r in removed {
                        prepared.transforms.push(format!("repair_400:{r}"));
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
            transforms: prepared.transforms,
        };
    }
}

async fn execute_chat_http(
    g: &Gateway,
    channel: Published,
    requested_model: &str,
    upstream_model: &str,
    data: Value,
) -> Response {
    let started = Instant::now();
    match send_upstream_with_repair(g, &channel, upstream_model, data).await {
        UpstreamReady::Err {
            status,
            message,
            code,
            retry_after,
            transforms,
        } => {
            g.record_diag(GatewayDiagEntry {
                id: 0,
                timestamp: now_unix(),
                channel_id: channel.channel_id.clone(),
                channel_alias: channel.alias.clone(),
                source: channel.kind().as_str().to_string(),
                model: requested_model.to_string(),
                stream: false,
                status: status.as_u16(),
                latency_ms: started.elapsed().as_millis() as u64,
                transforms,
                thinking_chars: 0,
                content_chars: 0,
                finish_reason: None,
                error: Some(message.clone()),
            });
            let mut resp = json_error(status, &message, "upstream_error", code.as_deref(), None);
            if let Some(ra) = retry_after {
                resp.headers_mut().insert(header::RETRY_AFTER, ra);
            }
            resp
        }
        UpstreamReady::Ok {
            resp,
            transforms,
            requested_stream,
        } => {
            let ct = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            let is_sse = requested_stream || ct.contains("text/event-stream");
            if is_sse {
                build_sse_response(
                    g.clone(),
                    channel,
                    requested_model.to_string(),
                    transforms,
                    started,
                    resp,
                )
            } else {
                build_json_response(
                    g,
                    &channel,
                    requested_model,
                    transforms,
                    started,
                    resp,
                )
                .await
            }
        }
    }
}

fn build_sse_response(
    g: Gateway,
    channel: Published,
    requested_model: String,
    transforms: Vec<String>,
    started: Instant,
    upstream: reqwest::Response,
) -> Response {
    struct StreamState<S> {
        stream: S,
        normalizer: SseStreamNormalizer,
        pending: Vec<String>,
        finished: bool,
        g: Gateway,
        channel: Published,
        requested_model: String,
        transforms: Vec<String>,
        started: Instant,
    }

    let state = StreamState {
        stream: Box::pin(upstream.bytes_stream()),
        normalizer: SseStreamNormalizer::new(requested_model.clone(), channel.thinking_mode),
        pending: Vec::new(),
        finished: false,
        g,
        channel,
        requested_model,
        transforms,
        started,
    };

    let body_stream = futures::stream::unfold(state, |mut st| async move {
        loop {
            if !st.pending.is_empty() {
                let next_frame = st.pending.remove(0);
                return Some((Ok::<Bytes, std::io::Error>(Bytes::from(next_frame)), st));
            }
            if st.finished {
                return None;
            }
            match st.stream.next().await {
                Some(Ok(chunk)) => {
                    let frames = st.normalizer.push_bytes(&chunk);
                    st.pending.extend(frames);
                }
                Some(Err(e)) => {
                    st.finished = true;
                    let frames = st.normalizer.finish();
                    st.pending.extend(frames);
                    record_stream_diag(&st.g, &st.channel, &st.requested_model, &st.transforms, st.started, &st.normalizer.stats, Some(format!("流式传输中断：{e}")));
                }
                None => {
                    st.finished = true;
                    let frames = st.normalizer.finish();
                    st.pending.extend(frames);
                    let err_note = if st.normalizer.stats.saw_error && !st.normalizer.stats.completed_cleanly {
                        Some("上游流式未正常完成".to_string())
                    } else {
                        None
                    };
                    record_stream_diag(&st.g, &st.channel, &st.requested_model, &st.transforms, st.started, &st.normalizer.stats, err_note);
                }
            }
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

fn record_stream_diag(
    g: &Gateway,
    channel: &Published,
    requested_model: &str,
    transforms: &[String],
    started: Instant,
    stats: &StreamStats,
    err_note: Option<String>,
) {
    g.record_diag(GatewayDiagEntry {
        id: 0,
        timestamp: now_unix(),
        channel_id: channel.channel_id.clone(),
        channel_alias: channel.alias.clone(),
        source: channel.kind().as_str().to_string(),
        model: requested_model.to_string(),
        stream: true,
        status: if err_note.is_some() { 502 } else { 200 },
        latency_ms: started.elapsed().as_millis() as u64,
        transforms: transforms.to_vec(),
        thinking_chars: stats.thinking_chars,
        content_chars: stats.content_chars,
        finish_reason: stats.finish_reason.clone(),
        error: err_note.map(|e| redact_secrets(&e)),
    });
}

async fn build_json_response(
    g: &Gateway,
    channel: &Published,
    requested_model: &str,
    transforms: Vec<String>,
    started: Instant,
    upstream: reqwest::Response,
) -> Response {
    let status = upstream.status();
    let text = match upstream.text().await {
        Ok(t) => t,
        Err(e) => {
            let msg = redact_secrets(&format!("读取上游响应失败：{e}"));
            g.record_diag(GatewayDiagEntry {
                id: 0,
                timestamp: now_unix(),
                channel_id: channel.channel_id.clone(),
                channel_alias: channel.alias.clone(),
                source: channel.kind().as_str().to_string(),
                model: requested_model.to_string(),
                stream: false,
                status: 502,
                latency_ms: started.elapsed().as_millis() as u64,
                transforms,
                thinking_chars: 0,
                content_chars: 0,
                finish_reason: None,
                error: Some(msg.clone()),
            });
            return json_error(StatusCode::BAD_GATEWAY, &msg, "gateway_protocol", Some("gateway_protocol"), None);
        }
    };
    let mut val: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => {
            let short: String = redact_secrets(&text).chars().take(300).collect();
            let msg = format!("上游返回非 JSON 响应：{short}");
            g.record_diag(GatewayDiagEntry {
                id: 0,
                timestamp: now_unix(),
                channel_id: channel.channel_id.clone(),
                channel_alias: channel.alias.clone(),
                source: channel.kind().as_str().to_string(),
                model: requested_model.to_string(),
                stream: false,
                status: 502,
                latency_ms: started.elapsed().as_millis() as u64,
                transforms,
                thinking_chars: 0,
                content_chars: 0,
                finish_reason: None,
                error: Some(msg.clone()),
            });
            return json_error(StatusCode::BAD_GATEWAY, &msg, "gateway_protocol", Some("gateway_protocol"), None);
        }
    };

    let (thinking_chars, content_chars) =
        normalize_non_stream_response(&mut val, requested_model, channel.thinking_mode);
    let finish_reason = val
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("finish_reason"))
        .and_then(Value::as_str)
        .map(str::to_string);

    g.record_diag(GatewayDiagEntry {
        id: 0,
        timestamp: now_unix(),
        channel_id: channel.channel_id.clone(),
        channel_alias: channel.alias.clone(),
        source: channel.kind().as_str().to_string(),
        model: requested_model.to_string(),
        stream: false,
        status: status.as_u16(),
        latency_ms: started.elapsed().as_millis() as u64,
        transforms,
        thinking_chars,
        content_chars,
        finish_reason,
        error: None,
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
    let mut body = json!({
        "model": requested_model,
        "messages": [{ "role": "user", "content": prompt }],
        "stream": stream,
        "max_tokens": 512,
        "include_reasoning": include_reasoning,
    });
    if let Some(eff) = reasoning_effort.filter(|s| !s.trim().is_empty()) {
        body["reasoning_effort"] = Value::String(eff);
    }

    match send_upstream_with_repair(g, &channel, &upstream_model, body).await {
        UpstreamReady::Err {
            status,
            message,
            transforms,
            ..
        } => {
            g.record_diag(GatewayDiagEntry {
                id: 0,
                timestamp: now_unix(),
                channel_id: channel.channel_id.clone(),
                channel_alias: channel.alias.clone(),
                source: channel.kind().as_str().to_string(),
                model: requested_model.clone(),
                stream,
                status: status.as_u16(),
                latency_ms: started.elapsed().as_millis() as u64,
                transforms: transforms.clone(),
                thinking_chars: 0,
                content_chars: 0,
                finish_reason: None,
                error: Some(message.clone()),
            });
            ChatTestResult {
                ok: false,
                status: status.as_u16(),
                latency_ms: started.elapsed().as_millis() as u64,
                requested_model,
                upstream_model,
                thinking: String::new(),
                content: String::new(),
                finish_reason: None,
                transforms,
                prompt_tokens: None,
                completion_tokens: None,
                error: Some(message),
            }
        }
        UpstreamReady::Ok {
            resp,
            transforms,
            requested_stream,
        } => {
            let status = resp.status().as_u16();
            let ct = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            if requested_stream || ct.contains("text/event-stream") {
                let mut normalizer =
                    SseStreamNormalizer::new(requested_model.clone(), channel.thinking_mode);
                let mut thinking = String::new();
                let mut content = String::new();
                let mut error_msg: Option<String> = None;
                let mut frames_text = Vec::new();
                let mut bytes_stream = resp.bytes_stream();
                while let Some(item) = bytes_stream.next().await {
                    match item {
                        Ok(chunk) => frames_text.extend(normalizer.push_bytes(&chunk)),
                        Err(e) => {
                            error_msg = Some(redact_secrets(&format!("读取流失败：{e}")));
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
                let ok = error_msg.is_none() && normalizer.stats.completed_cleanly;
                record_stream_diag(
                    g,
                    &channel,
                    &requested_model,
                    &transforms,
                    started,
                    &normalizer.stats,
                    error_msg.clone(),
                );
                ChatTestResult {
                    ok,
                    status: if ok { status } else { 502 },
                    latency_ms: started.elapsed().as_millis() as u64,
                    requested_model,
                    upstream_model,
                    thinking,
                    content,
                    finish_reason: normalizer.stats.finish_reason,
                    transforms,
                    prompt_tokens: normalizer.stats.prompt_tokens,
                    completion_tokens: normalizer.stats.completion_tokens,
                    error: error_msg,
                }
            } else {
                let text = resp.text().await.unwrap_or_default();
                let mut val: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => {
                        let msg = "上游返回非 JSON 响应".to_string();
                        return ChatTestResult {
                            ok: false,
                            status: 502,
                            latency_ms: started.elapsed().as_millis() as u64,
                            requested_model,
                            upstream_model,
                            thinking: String::new(),
                            content: String::new(),
                            finish_reason: None,
                            transforms,
                            prompt_tokens: None,
                            completion_tokens: None,
                            error: Some(msg),
                        };
                    }
                };
                let (tc, cc) = normalize_non_stream_response(
                    &mut val,
                    &requested_model,
                    channel.thinking_mode,
                );
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
                let prompt_tokens = val.pointer("/usage/prompt_tokens").and_then(Value::as_u64);
                let completion_tokens = val.pointer("/usage/completion_tokens").and_then(Value::as_u64);
                g.record_diag(GatewayDiagEntry {
                    id: 0,
                    timestamp: now_unix(),
                    channel_id: channel.channel_id.clone(),
                    channel_alias: channel.alias.clone(),
                    source: channel.kind().as_str().to_string(),
                    model: requested_model.clone(),
                    stream: false,
                    status,
                    latency_ms: started.elapsed().as_millis() as u64,
                    transforms: transforms.clone(),
                    thinking_chars: tc,
                    content_chars: cc,
                    finish_reason: finish_reason.clone(),
                    error: None,
                });
                ChatTestResult {
                    ok: true,
                    status,
                    latency_ms: started.elapsed().as_millis() as u64,
                    requested_model,
                    upstream_model,
                    thinking,
                    content,
                    finish_reason,
                    transforms,
                    prompt_tokens,
                    completion_tokens,
                    error: None,
                }
            }
        }
    }
}

pub async fn fetch_channel_models(
    client: &reqwest::Client,
    kind: GatewaySourceKind,
    base_url: &str,
    api_key: Option<&str>,
    custom_headers: &BTreeMap<String, String>,
) -> Result<ModelFetchResult, String> {
    let host = host_of_url(base_url);
    match kind {
        GatewaySourceKind::AgentPlatform => {
            // 校验自定义头语法合法性
            let _ = build_upstream_headers(kind, api_key, custom_headers)?;
            let fetched = AGENT_PLATFORM_PRESET_MODELS
                .iter()
                .map(|s| s.to_string())
                .collect();
            Ok(ModelFetchResult {
                fetched,
                host,
                probe_note: Some("Agent Platform 端点不提供 /models 接口，已载入预置模型并支持手动增补".into()),
            })
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
                        .map_err(|e| redact_secrets(&format!("读取 Gemini 模型列表失败：{e}")))?;
                    let status = res.status();
                    if !status.is_success() {
                        let txt = res.text().await.unwrap_or_default();
                        let (summary, _) = parse_upstream_error_summary(status.as_u16(), &txt);
                        return Err(summary);
                    }
                    let body: Value = res.json().await.map_err(|_| "Gemini 返回格式异常".to_string())?;
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
    let hdrs = build_upstream_headers(kind, api_key, custom_headers)?;
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let req = apply_headers(
        client.get(&url).timeout(Duration::from_secs(MODELS_TIMEOUT_SECS)),
        &hdrs,
    )?;
    let res = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return Err(redact_secrets(&format!("读取 /models 失败：{e}")));
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
        let txt = res.text().await.unwrap_or_default();
        let (summary, _) = parse_upstream_error_summary(status.as_u16(), &txt);
        return Err(summary);
    }
    let body: Value = match res.json().await {
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
