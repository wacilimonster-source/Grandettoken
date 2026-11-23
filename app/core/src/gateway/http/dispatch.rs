//! 多候选路由调度、首字节前自动容灾、Sticky 粘性与脱敏/反脱敏管道。

use super::{
    infer_zero_output_note, json_error, now_unix, resolve_channel_and_model,
    responses::{self, ChatSse, DiagnosticGuard},
    send_upstream_with_repair, UpstreamReady,
};
use crate::gateway::{
    lock,
    routing::{
        compute_cooldown_secs, derive_session_hash, group_model_id, is_failover_status,
        select_candidates, snapshot_vault_platforms, ResolvedCandidate, RouteGroup,
    },
    Gateway, Published,
};
use crate::gateway_proto::{
    masking::{mask_openai_request, unmask_openai_response, SessionMaskMap, SseFrameUnmasker},
    stream::extract_usage_details,
    thinking::{normalize_non_stream_response, summarize_non_stream_tools},
};
use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures::StreamExt;
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    time::Instant,
};

pub(super) struct DispatchError {
    pub status: StatusCode,
    pub message: String,
    pub code: Option<String>,
    pub err_type: &'static str,
    pub param: Option<&'static str>,
    pub retry_after: Option<HeaderValue>,
}

impl DispatchError {
    pub fn into_openai_response(self) -> Response {
        let mut resp = json_error(
            self.status,
            &self.message,
            self.err_type,
            self.code.as_deref(),
            self.param,
        );
        if let Some(ra) = self.retry_after {
            resp.headers_mut().insert(header::RETRY_AFTER, ra);
        }
        resp
    }
}

pub(super) enum DispatchedSuccess {
    Stream {
        upstream: reqwest::Response,
        channel: Published,
        requested_model: String,
        include_usage: bool,
        expected_choices: Option<u64>,
        sensitive: Vec<String>,
        route_group: Option<String>,
        failover_count: u8,
        mask_reverse: BTreeMap<String, String>,
        guard: DiagnosticGuard,
    },
    Json {
        value: Value,
        status: StatusCode,
        channel: Published,
        route_group: Option<String>,
        failover_count: u8,
    },
}

fn attach_route_headers(
    headers: &mut HeaderMap,
    channel: &Published,
    route_group: Option<&str>,
    failover_count: u8,
) {
    if let Ok(v) = HeaderValue::from_str(&channel.alias) {
        headers.insert(HeaderName::from_static("x-gateway-upstream-channel"), v);
    }
    if let Some(rg) = route_group {
        if let Ok(v) = HeaderValue::from_str(rg) {
            headers.insert(HeaderName::from_static("x-gateway-route-group"), v);
        }
    }
    if failover_count > 0 {
        if let Ok(v) = HeaderValue::from_str(&failover_count.to_string()) {
            headers.insert(HeaderName::from_static("x-gateway-failovers"), v);
        }
    }
}

fn resolve_candidates(
    g: &Gateway,
    requested_model: &str,
    session_hash: Option<&str>,
    now: i64,
) -> Result<(Option<RouteGroup>, Vec<ResolvedCandidate>), DispatchError> {
    if let Some(group_id) = requested_model.strip_prefix("group/") {
        let group = {
            let inner = lock(&g.inner);
            inner
                .settings
                .groups
                .iter()
                .find(|gr| gr.id.eq_ignore_ascii_case(group_id))
                .cloned()
        }
        .ok_or_else(|| DispatchError {
            status: StatusCode::NOT_FOUND,
            message: "路由组不存在或已删除".into(),
            code: Some("model_not_found".into()),
            err_type: "invalid_request_error",
            param: Some("model"),
            retry_after: None,
        })?;
        let live = g.live_channels();
        let vp = snapshot_vault_platforms(g);
        let candidates = {
            let mut inner = lock(&g.inner);
            select_candidates(&group, &live, &vp, &mut inner.routing, session_hash, now)
        };
        if candidates.is_empty() {
            return Err(DispatchError {
                status: StatusCode::NOT_FOUND,
                message: "路由组内暂无可用渠道或模型，请检查成员配置".into(),
                code: Some("model_not_found".into()),
                err_type: "invalid_request_error",
                param: Some("model"),
                retry_after: None,
            });
        }
        Ok((Some(group), candidates))
    } else {
        match resolve_channel_and_model(g, requested_model) {
            Ok((channel, upstream_model)) => Ok((
                None,
                vec![ResolvedCandidate {
                    channel,
                    upstream_model,
                    in_cooldown: false,
                }],
            )),
            Err(_) => {
                // 重新走原路径复用同一 404 / 409 错误口径
                let matches = g.live_channels().into_iter().filter(|c| {
                    crate::gateway::effective_models(c)
                        .into_iter()
                        .any(|id| crate::gateway::model_id(c, &id) == requested_model)
                });
                let count = matches.count();
                if count > 1 {
                    Err(DispatchError {
                        status: StatusCode::CONFLICT,
                        message: "多个渠道的别名和模型相同，请修改其中一个别名或调整可见模型".into(),
                        code: Some("duplicate_model".into()),
                        err_type: "invalid_request_error",
                        param: Some("model"),
                        retry_after: None,
                    })
                } else {
                    Err(DispatchError {
                        status: StatusCode::NOT_FOUND,
                        message: "模型不存在、已下架或密钥已更新，请刷新模型列表".into(),
                        code: Some("model_not_found".into()),
                        err_type: "invalid_request_error",
                        param: Some("model"),
                        retry_after: None,
                    })
                }
            }
        }
    }
}

fn apply_masking_to_request(
    g: &Gateway,
    data: &mut Value,
    session_hash: Option<&str>,
    now: i64,
) -> (usize, BTreeMap<String, String>) {
    let settings = lock(&g.inner).settings.masking.clone();
    if !settings.enabled {
        return (0, BTreeMap::new());
    }
    if settings.sticky {
        if let Some(sh) = session_hash {
            let mut inner = lock(&g.inner);
            let map = inner.mask_store.get_or_create(sh, now);
            let count = mask_openai_request(data, &settings, map);
            return (count, map.reverse.clone());
        }
    }
    let mut ephemeral = SessionMaskMap::default();
    let count = mask_openai_request(data, &settings, &mut ephemeral);
    (count, ephemeral.reverse)
}

fn record_sticky_success(
    g: &Gateway,
    group: Option<&RouteGroup>,
    session_hash: Option<&str>,
    cand: &ResolvedCandidate,
    now: i64,
) {
    if let (Some(grp), Some(sh)) = (group, session_hash) {
        if grp.sticky {
            lock(&g.inner).routing.set_sticky(
                &grp.id,
                sh,
                &cand.channel.channel_id,
                &cand.upstream_model,
                grp.sticky_ttl_secs,
                now,
            );
        }
    }
}

pub(super) async fn execute_dispatched(
    g: &Gateway,
    headers: &HeaderMap,
    mut data: Value,
    requested_model: &str,
    protocol: &str,
) -> Result<DispatchedSuccess, DispatchError> {
    let started = Instant::now();
    let now = now_unix();
    let session_hash = derive_session_hash(headers, &data);
    let (group_opt, candidates) =
        resolve_candidates(g, requested_model, session_hash.as_deref(), now)?;
    let (masked_count, mask_reverse) =
        apply_masking_to_request(g, &mut data, session_hash.as_deref(), now);

    let stream = data.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let route_group = group_opt.as_ref().map(|gr| group_model_id(&gr.id));
    let mut guard = DiagnosticGuard::new(g, &candidates[0].channel, requested_model, stream, started);

    let total_candidates = candidates.len();
    let mut last_err: Option<DispatchError> = None;

    for (idx, cand) in candidates.iter().enumerate() {
        let failover_count = idx.min(u8::MAX as usize) as u8;
        guard.set_context(
            &cand.channel,
            &cand.upstream_model,
            route_group.clone(),
            failover_count,
            masked_count,
            protocol,
        );
        let attempt_body = if total_candidates > 1 {
            data.clone()
        } else {
            std::mem::take(&mut data)
        };

        match send_upstream_with_repair(g, &cand.channel, &cand.upstream_model, attempt_body).await {
            UpstreamReady::Err {
                status,
                message,
                code,
                retry_after,
                transforms,
            } => {
                let can_failover = group_opt.is_some()
                    && idx + 1 < total_candidates
                    && is_failover_status(status, code.as_deref());
                if can_failover {
                    if let Some(ref grp) = group_opt {
                        let secs = compute_cooldown_secs(grp, status, retry_after.as_ref());
                        lock(&g.inner).routing.set_cooldown(
                            &cand.channel.channel_id,
                            &cand.upstream_model,
                            now_unix() + secs as i64,
                            format!("HTTP {}", status.as_u16()),
                        );
                    }
                    last_err = Some(DispatchError {
                        status,
                        message,
                        code,
                        err_type: "upstream_error",
                        param: None,
                        retry_after,
                    });
                    continue;
                }
                super::record_error_diag_ctx(
                    g,
                    &cand.channel,
                    requested_model,
                    Some(cand.upstream_model.clone()),
                    route_group.clone(),
                    failover_count,
                    masked_count,
                    protocol,
                    stream,
                    status.as_u16(),
                    started,
                    transforms,
                    &message,
                );
                guard.disarm();
                let err_type = if code.as_deref() == Some("responses_conversion") {
                    "invalid_request_error"
                } else {
                    "upstream_error"
                };
                return Err(DispatchError {
                    status,
                    message,
                    code,
                    err_type,
                    param: None,
                    retry_after,
                });
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
                    record_sticky_success(
                        g,
                        group_opt.as_ref(),
                        session_hash.as_deref(),
                        cand,
                        now_unix(),
                    );
                    return Ok(DispatchedSuccess::Stream {
                        upstream: resp,
                        channel: cand.channel.clone(),
                        requested_model: requested_model.to_string(),
                        include_usage,
                        expected_choices,
                        sensitive,
                        route_group,
                        failover_count,
                        mask_reverse,
                        guard,
                    });
                }

                let first_byte_ms = Some(started.elapsed().as_millis() as u64);
                let status = resp.status();
                match responses::read_chat_json(resp, cand.channel.wire, requested_model, &sensitive)
                    .await
                {
                    Ok(mut val) => {
                        record_sticky_success(
                            g,
                            group_opt.as_ref(),
                            session_hash.as_deref(),
                            cand,
                            now_unix(),
                        );
                        let (thinking_chars, content_chars) = normalize_non_stream_response(
                            &mut val,
                            requested_model,
                            cand.channel.thinking_mode,
                        );
                        unmask_openai_response(&mut val, &mask_reverse);
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
                        g.record_diag(super::GatewayDiagEntry {
                            id: 0,
                            timestamp: now_unix(),
                            channel_id: cand.channel.channel_id.clone(),
                            channel_alias: cand.channel.alias.clone(),
                            source: cand.channel.kind().as_str().to_string(),
                            model: requested_model.to_string(),
                            upstream_model: Some(cand.upstream_model.clone()),
                            route_group: route_group.clone(),
                            failover_count,
                            masked_count,
                            protocol: protocol.to_string(),
                            stream: false,
                            status: status.as_u16(),
                            latency_ms: started.elapsed().as_millis() as u64,
                            first_byte_ms,
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
                            estimated_cost_cny: None,
                            diag_note,
                            error: None,
                        });
                        guard.disarm();
                        return Ok(DispatchedSuccess::Json {
                            value: val,
                            status,
                            channel: cand.channel.clone(),
                            route_group,
                            failover_count,
                        });
                    }
                    Err(msg) => {
                        if group_opt.is_some() && idx + 1 < total_candidates {
                            if let Some(ref grp) = group_opt {
                                let secs = compute_cooldown_secs(grp, StatusCode::BAD_GATEWAY, None);
                                lock(&g.inner).routing.set_cooldown(
                                    &cand.channel.channel_id,
                                    &cand.upstream_model,
                                    now_unix() + secs as i64,
                                    "HTTP 502".into(),
                                );
                            }
                            last_err = Some(DispatchError {
                                status: StatusCode::BAD_GATEWAY,
                                message: msg,
                                code: Some("gateway_protocol".into()),
                                err_type: "upstream_error",
                                param: None,
                                retry_after: None,
                            });
                            continue;
                        }
                        super::record_error_diag_ctx(
                            g,
                            &cand.channel,
                            requested_model,
                            Some(cand.upstream_model.clone()),
                            route_group.clone(),
                            failover_count,
                            masked_count,
                            protocol,
                            false,
                            502,
                            started,
                            transforms,
                            &msg,
                        );
                        guard.disarm();
                        return Err(DispatchError {
                            status: StatusCode::BAD_GATEWAY,
                            message: msg,
                            code: Some("gateway_protocol".into()),
                            err_type: "upstream_error",
                            param: None,
                            retry_after: None,
                        });
                    }
                }
            }
        }
    }

    guard.disarm();
    Err(last_err.unwrap_or(DispatchError {
        status: StatusCode::BAD_GATEWAY,
        message: "路由组所有候选均不可用".into(),
        code: Some("no_healthy_candidate".into()),
        err_type: "upstream_error",
        param: None,
        retry_after: None,
    }))
}

pub(super) async fn handle_openai_chat(
    g: &Gateway,
    headers: &HeaderMap,
    data: Value,
    requested_model: &str,
) -> Response {
    match execute_dispatched(g, headers, data, requested_model, "openai_chat").await {
        Err(err) => err.into_openai_response(),
        Ok(DispatchedSuccess::Json {
            value,
            status,
            channel,
            route_group,
            failover_count,
        }) => {
            let mut resp = (status, Json(value)).into_response();
            attach_route_headers(resp.headers_mut(), &channel, route_group.as_deref(), failover_count);
            resp
        }
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
            struct OpenAiStreamState<S> {
                stream: S,
                normalizer: ChatSse,
                unmasker: SseFrameUnmasker,
                pending: VecDeque<String>,
                finished: bool,
                sensitive: Vec<String>,
                guard: DiagnosticGuard,
            }

            let state = OpenAiStreamState {
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
                            for frame in st.normalizer.push_bytes(&chunk) {
                                st.pending.push_back(st.unmasker.process_sse_frame(&frame));
                            }
                            st.guard.observe(st.normalizer.stats(), !chunk.is_empty());
                            if !st.normalizer.is_terminal() {
                                continue;
                            }
                            None
                        }
                        Some(Err(e)) => Some(crate::gateway_proto::error::redact_known(
                            &format!("流式传输中断：{e}"),
                            &st.sensitive,
                        )),
                        None => None,
                    };
                    st.finished = true;
                    for frame in st.normalizer.finish() {
                        st.pending.push_back(st.unmasker.process_sse_frame(&frame));
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
            attach_route_headers(
                response.headers_mut(),
                &channel,
                route_group.as_deref(),
                failover_count,
            );
            response
        }
    }
}
