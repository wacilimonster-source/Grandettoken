use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
mod responses;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
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
    responses::from_chat_request,
    stream::StreamStats,
    thinking::{normalize_non_stream_response, summarize_non_stream_tools},
    GatewaySourceKind, OpenAiWire,
};
use tokenscope_core::secrets;

use crate::gateway::{authorized, effective_models, lock, model_id, peer_allowed, Gateway, Published};

const MAX_GEMINI_MODEL_PAGES: usize = 5;
const MODELS_TIMEOUT_SECS: u64 = 25;
const CHAT_TIMEOUT_SECS: u64 = 300;

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
    #[serde(default)]
    pub first_byte_ms: Option<u64>,
    #[serde(default)]
    pub frame_count: usize,
    pub transforms: Vec<String>,
    pub thinking_chars: usize,
    pub content_chars: usize,
    #[serde(default)]
    pub tool_call_count: usize,
    #[serde(default)]
    pub tool_call_chars: usize,
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    #[serde(default)]
    pub diag_note: Option<String>,
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
    pub first_byte_ms: Option<u64>,
    pub frame_count: usize,
    pub requested_model: String,
    pub upstream_model: String,
    pub thinking: String,
    pub content: String,
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
        // 兼容未在 custom_url 末尾填写 `/v1` 的客户端（例如直接填 http://host:18771）
        .route("/models", get(list_models))
        .route("/models/{*id}", get(get_model))
        .route("/chat/completions", post(chat))
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

    execute_inference_http(&g, resolved.0, &requested_model, &resolved.1, data).await
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
        include_usage: bool,
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
    let include_usage = data.pointer("/stream_options/include_usage") == Some(&Value::Bool(true));
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
                message,
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
                    message: msg,
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
                    message: redact_secrets(&msg),
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
            };
        }

        let retry_after = upstream.headers().get(header::RETRY_AFTER).cloned();
        let err_body = upstream.text().await.unwrap_or_default();
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
    if thinking_chars == 0 && content_chars == 0 && tool_call_count == 0 {
        return Some(match finish_reason {
            Some(fr) => format!("上游已结束（finish_reason={fr}，{frame_count} 帧），未返回文本或工具调用"),
            None if frame_count == 0 => "上游未返回任何有效数据帧".to_string(),
            None => format!("上游返回 {frame_count} 帧，但未解析出文本或工具调用"),
        });
    }
    None
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
    g.record_diag(GatewayDiagEntry {
        id: 0,
        timestamp: now_unix(),
        channel_id: channel.channel_id.clone(),
        channel_alias: channel.alias.clone(),
        source: channel.kind().as_str().to_string(),
        model: requested_model.to_string(),
        stream,
        status,
        latency_ms: started.elapsed().as_millis() as u64,
        first_byte_ms: None,
        frame_count: 0,
        transforms,
        thinking_chars: 0,
        content_chars: 0,
        tool_call_count: 0,
        tool_call_chars: 0,
        finish_reason: None,
        prompt_tokens: None,
        completion_tokens: None,
        diag_note: None,
        error: Some(redact_secrets(message)),
    });
}

async fn execute_inference_http(
    g: &Gateway,
    channel: Published,
    requested_model: &str,
    upstream_model: &str,
    data: Value,
) -> Response {
    let started = Instant::now();
    let stream = data.get("stream").and_then(Value::as_bool).unwrap_or(false);
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
                    include_usage,
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
    include_usage: bool,
) -> Response {
    struct StreamState<S> {
        stream: S,
        normalizer: responses::ChatSse,
        pending: VecDeque<String>,
        finished: bool,
        first_byte_ms: Option<u64>,
        g: Gateway,
        channel: Published,
        requested_model: String,
        transforms: Vec<String>,
        started: Instant,
    }

    let state = StreamState {
        stream: Box::pin(upstream.bytes_stream()),
        normalizer: responses::ChatSse::new(&requested_model, channel.thinking_mode, channel.wire, include_usage),
        pending: VecDeque::new(),
        finished: false,
        first_byte_ms: None,
        g,
        channel,
        requested_model,
        transforms,
        started,
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
                    if st.first_byte_ms.is_none() && !chunk.is_empty() {
                        st.first_byte_ms = Some(st.started.elapsed().as_millis() as u64);
                    }
                    st.pending.extend(st.normalizer.push_bytes(&chunk));
                    if !st.normalizer.is_terminal() {
                        continue;
                    }
                    None
                }
                Some(Err(e)) => Some(format!("流式传输中断：{e}")),
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
            record_stream_diag(
                &st.g,
                &st.channel,
                &st.requested_model,
                &st.transforms,
                st.started,
                st.first_byte_ms,
                stats,
                error,
            );
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
        stats.tool_call_count,
        stats.frame_count,
        stats.finish_reason.as_deref(),
        stats.diagnostic_note.as_deref(),
        error.as_deref(),
    );
    g.record_diag(GatewayDiagEntry {
        id: 0,
        timestamp: now_unix(),
        channel_id: channel.channel_id.clone(),
        channel_alias: channel.alias.clone(),
        source: channel.kind().as_str().to_string(),
        model: requested_model.to_string(),
        stream: true,
        status,
        latency_ms: started.elapsed().as_millis() as u64,
        first_byte_ms,
        frame_count: stats.frame_count,
        transforms: transforms.to_vec(),
        thinking_chars: stats.thinking_chars,
        content_chars: stats.content_chars,
        tool_call_count: stats.tool_call_count,
        tool_call_chars: stats.tool_call_chars,
        finish_reason: stats.finish_reason.clone(),
        prompt_tokens: stats.prompt_tokens,
        completion_tokens: stats.completion_tokens,
        diag_note,
        error,
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
    let mut val = match responses::read_chat_json(upstream, channel.wire, requested_model).await {
        Ok(value) => value,
        Err(msg) => {
            record_error_diag(g, channel, requested_model, false, 502, started, transforms, &msg);
            return json_error(StatusCode::BAD_GATEWAY, &msg, "gateway_protocol", Some("gateway_protocol"), None);
        }
    };

    let (thinking_chars, content_chars) =
        normalize_non_stream_response(&mut val, requested_model, channel.thinking_mode);
    let (tool_call_count, tool_call_chars) = summarize_non_stream_tools(&val);
    let finish_reason = val
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("finish_reason"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let prompt_tokens = val.pointer("/usage/prompt_tokens").and_then(Value::as_u64);
    let completion_tokens = val.pointer("/usage/completion_tokens").and_then(Value::as_u64);
    let frame_count = val
        .get("choices")
        .and_then(Value::as_array)
        .map(|a| usize::from(!a.is_empty()))
        .unwrap_or(0);
    let diag_note = infer_zero_output_note(
        status.as_u16(),
        thinking_chars,
        content_chars,
        tool_call_count,
        frame_count,
        finish_reason.as_deref(),
        None,
        None,
    );

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
        first_byte_ms: None,
        frame_count,
        transforms,
        thinking_chars,
        content_chars,
        tool_call_count,
        tool_call_chars,
        finish_reason,
        prompt_tokens,
        completion_tokens,
        diag_note,
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
        } => {
            let status = resp.status().as_u16();
            let ct = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            if requested_stream || ct.contains("text/event-stream") {
                let mut normalizer = responses::ChatSse::new(
                    &requested_model, channel.thinking_mode, channel.wire, include_usage,
                );
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
                            if normalizer.is_terminal() {
                                break;
                            }
                        }
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
                    stats.tool_call_count,
                    stats.frame_count,
                    stats.finish_reason.as_deref(),
                    stats.diagnostic_note.as_deref(),
                    final_error.as_deref(),
                );
                record_stream_diag(
                    g,
                    &channel,
                    &requested_model,
                    &transforms,
                    started,
                    first_byte_ms,
                    stats,
                    final_error.clone(),
                );
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
                let mut val = match responses::read_chat_json(resp, channel.wire, &requested_model).await {
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
                let frame_count = val
                    .get("choices")
                    .and_then(Value::as_array)
                    .map(|a| usize::from(!a.is_empty()))
                    .unwrap_or(0);
                let diag_note = infer_zero_output_note(
                    status,
                    tc,
                    cc,
                    tool_call_count,
                    frame_count,
                    finish_reason.as_deref(),
                    None,
                    None,
                );
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
                    first_byte_ms,
                    frame_count,
                    transforms: transforms.clone(),
                    thinking_chars: tc,
                    content_chars: cc,
                    tool_call_count,
                    tool_call_chars,
                    finish_reason: finish_reason.clone(),
                    prompt_tokens,
                    completion_tokens,
                    diag_note: diag_note.clone(),
                    error: None,
                });
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
        build_upstream_headers(kind, api_key, &channel.custom_headers)?;
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
mod tests {
    use super::*;

    #[test]
    fn upstream_send_timeout_maps_to_504_instead_of_unreachable() {
        let (status, code, msg) = classify_upstream_send_error(true, "operation timed out");
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(code, "upstream_timeout");
        assert!(msg.contains("等待上游响应超时"));

        let (status2, code2, msg2) = classify_upstream_send_error(false, "tcp connect error");
        assert_eq!(status2, StatusCode::BAD_GATEWAY);
        assert_eq!(code2, "upstream_unreachable");
        assert!(msg2.contains("无法连接上游服务"));
    }
}
