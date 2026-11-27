//! Anthropic `/v1/messages` 与 `/v1/messages/count_tokens` HTTP 入口。

use super::dispatch::{execute_dispatched, DispatchedSuccess};
use super::responses::{ChatSse, DiagnosticGuard};
use crate::gateway::{peer_allowed, Gateway};
use crate::gateway_proto::{
    anthropic::{
        anthropic_error_value, anthropic_to_openai_request, estimate_anthropic_input_tokens,
        openai_to_anthropic_response, AnthropicSseTranslator,
    },
    error::redact_known,
    masking::SseFrameUnmasker,
};
use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures::StreamExt;
use serde_json::{json, Value};
use std::{collections::VecDeque, net::SocketAddr};

fn anthropic_json_error(status: StatusCode, err_type: &str, message: &str) -> Response {
    (status, Json(anthropic_error_value(err_type, message))).into_response()
}

async fn check_auth(
    g: &Gateway,
    peer: SocketAddr,
    headers: &HeaderMap,
) -> Result<(), Response> {
    let settings = g.access_settings();
    if !peer_allowed(peer, &settings) {
        return Err(anthropic_json_error(
            StatusCode::FORBIDDEN,
            "permission_error",
            "仅允许本机或局域网访问",
        ));
    }
    if !g.is_authorized(headers, &settings).await {
        return Err(anthropic_json_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "网关密钥无效",
        ));
    }
    Ok(())
}

fn parse_json_body(headers: &HeaderMap, body: &Bytes) -> Result<Value, Response> {
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
        return Err(anthropic_json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "invalid_request_error",
            "需要 application/json",
        ));
    }
    serde_json::from_slice(body).map_err(|_| {
        anthropic_json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "请求必须是 JSON",
        )
    })
}

fn new_msg_id() -> String {
    let mut b = [0u8; 8];
    let _ = getrandom::getrandom(&mut b);
    format!(
        "msg_{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

pub(super) async fn count_tokens(
    State(g): State<Gateway>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(resp) = check_auth(&g, peer, &headers).await {
        return resp;
    }
    let data = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let input_tokens = estimate_anthropic_input_tokens(&data);
    Json(json!({ "input_tokens": input_tokens })).into_response()
}

pub(super) async fn messages(
    State(g): State<Gateway>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(resp) = check_auth(&g, peer, &headers).await {
        return resp;
    }
    let data = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let (openai_req, meta) = match anthropic_to_openai_request(&data) {
        Ok(v) => v,
        Err(msg) => {
            return anthropic_json_error(StatusCode::BAD_REQUEST, "invalid_request_error", &msg);
        }
    };

    let msg_id = new_msg_id();
    match execute_dispatched(&g, &headers, openai_req, &meta.requested_model, "anthropic_messages")
        .await
    {
        Err(err) => {
            let err_type = match err.status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "authentication_error",
                StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
                StatusCode::NOT_FOUND => "not_found_error",
                StatusCode::BAD_REQUEST => "invalid_request_error",
                _ => "api_error",
            };
            let mut resp = anthropic_json_error(err.status, err_type, &err.message);
            if let Some(ra) = err.retry_after {
                resp.headers_mut().insert(header::RETRY_AFTER, ra);
            }
            resp
        }
        Ok(DispatchedSuccess::Json {
            value,
            status,
            channel,
            route_group,
            failover_count,
        }) => match openai_to_anthropic_response(&value, &meta.requested_model, &msg_id) {
            Ok(anthropic_body) => {
                let mut resp = (status, Json(anthropic_body)).into_response();
                if let Ok(v) = HeaderValue::from_str(&channel.alias) {
                    resp.headers_mut()
                        .insert(HeaderName::from_static("x-gateway-upstream-channel"), v);
                }
                if let Some(rg) = route_group {
                    if let Ok(v) = HeaderValue::from_str(&rg) {
                        resp.headers_mut()
                            .insert(HeaderName::from_static("x-gateway-route-group"), v);
                    }
                }
                if failover_count > 0 {
                    if let Ok(v) = HeaderValue::from_str(&failover_count.to_string()) {
                        resp.headers_mut()
                            .insert(HeaderName::from_static("x-gateway-failovers"), v);
                    }
                }
                resp
            }
            Err(msg) => anthropic_json_error(StatusCode::BAD_GATEWAY, "api_error", &msg),
        },
        Ok(DispatchedSuccess::Stream {
            upstream,
            channel,
            requested_model,
            include_usage,
            expected_choices,
            sensitive,
            route_group,
            failover_count,
            mask_reverse,
            guard,
            ..
        }) => {
            struct AnthropicStreamState<S> {
                stream: S,
                normalizer: ChatSse,
                unmasker: SseFrameUnmasker,
                translator: AnthropicSseTranslator,
                pending: VecDeque<String>,
                finished: bool,
                sensitive: Vec<String>,
                guard: DiagnosticGuard,
            }

            let state = AnthropicStreamState {
                stream: Box::pin(upstream.bytes_stream()),
                normalizer: ChatSse::new(
                    &requested_model,
                    channel.thinking_mode,
                    channel.wire,
                    include_usage,
                )
                .with_expected_choices(expected_choices)
                .with_sensitive_values(sensitive.clone()),
                unmasker: SseFrameUnmasker::new(mask_reverse),
                translator: AnthropicSseTranslator::new(&requested_model, &msg_id),
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
                            for oai_frame in st.normalizer.push_bytes(&chunk) {
                                let unmasked = st.unmasker.process_sse_frame(&oai_frame);
                                for anth_frame in st.translator.push_openai_frame(&unmasked) {
                                    st.pending.push_back(anth_frame);
                                }
                            }
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
                    for oai_frame in st.normalizer.finish() {
                        let unmasked = st.unmasker.process_sse_frame(&oai_frame);
                        for anth_frame in st.translator.push_openai_frame(&unmasked) {
                            st.pending.push_back(anth_frame);
                        }
                    }
                    for anth_frame in st.translator.finish() {
                        st.pending.push_back(anth_frame);
                    }
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
            if let Ok(v) = HeaderValue::from_str(&channel.alias) {
                response
                    .headers_mut()
                    .insert(HeaderName::from_static("x-gateway-upstream-channel"), v);
            }
            if let Some(rg) = route_group {
                if let Ok(v) = HeaderValue::from_str(&rg) {
                    response
                        .headers_mut()
                        .insert(HeaderName::from_static("x-gateway-route-group"), v);
                }
            }
            if failover_count > 0 {
                if let Ok(v) = HeaderValue::from_str(&failover_count.to_string()) {
                    response
                        .headers_mut()
                        .insert(HeaderName::from_static("x-gateway-failovers"), v);
                }
            }
            response
        }
    }
}
