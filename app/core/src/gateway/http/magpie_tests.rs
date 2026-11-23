use super::*;
use crate::gateway::routing::{RouteGroup, RouteTarget, RoutingStrategy};
use crate::gateway_proto::masking::MaskingSettings;
use crate::store::{Store, VaultItem};
use crate::vault::transaction::CredentialStore;
use axum::body::Body;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tokio::task::JoinHandle;

const UPSTREAM_SECRET: &str = "magpie-upstream-fixture-secret";
const GATEWAY_TOKEN: &str = "gw_magpie_fixture_access_token_01";
const MASK_SECRET: &str = "sk-proj-secret1234567890abcdef";

struct MagpieCreds;
impl CredentialStore for MagpieCreds {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        if key == crate::gateway::SECRET_KEY {
            Ok(Some(GATEWAY_TOKEN.into()))
        } else {
            Ok(Some(UPSTREAM_SECRET.into()))
        }
    }
    fn set(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn delete(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
}

struct MultiUpstreamFixture {
    g: Gateway,
    client: reqwest::Client,
    base_url: String,
    primary_hits: Arc<AtomicUsize>,
    backup_hits: Arc<AtomicUsize>,
    primary_bodies: Arc<Mutex<Vec<Value>>>,
    _backup_bodies: Arc<Mutex<Vec<Value>>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for MultiUpstreamFixture {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

#[derive(Clone, Copy)]
enum UpstreamBehavior {
    AlwaysOkWithCache,
    FailThenOk(StatusCode, Option<&'static str>),
    Always400,
    Fail503Json,
    AnthropicJsonRich,
    AnthropicSseRich,
    EchoMaskedJson,
    SplitMaskedSse,
}

async fn spawn_upstream(
    behavior: UpstreamBehavior,
    hits: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Value>>>,
) -> (String, JoinHandle<()>) {
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let hits = hits.clone();
            let bodies = bodies.clone();
            async move {
                let n = hits.fetch_add(1, Ordering::SeqCst);
                crate::gateway::lock(&bodies).push(body.clone());
                match behavior {
                    UpstreamBehavior::FailThenOk(status, retry_after) => {
                        if n == 0 {
                            let mut resp = (
                                status,
                                Json(json!({ "error": { "message": format!("upstream error {UPSTREAM_SECRET}"), "type": "rate_limit" } })),
                            )
                                .into_response();
                            if let Some(ra) = retry_after {
                                resp.headers_mut().insert(
                                    header::RETRY_AFTER,
                                    HeaderValue::from_static(ra),
                                );
                            }
                            return resp;
                        }
                        Json(json!({
                            "id": "chatcmpl_p_ok",
                            "object": "chat.completion",
                            "model": body["model"],
                            "choices": [{"index": 0, "message": {"role": "assistant", "content": "primary recovered"}, "finish_reason": "stop"}],
                            "usage": {"prompt_tokens": 50, "completion_tokens": 10, "total_tokens": 60}
                        })).into_response()
                    }
                    UpstreamBehavior::Always400 => (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": { "message": "invalid parameter foo", "type": "invalid_request_error" } })),
                    )
                        .into_response(),
                    UpstreamBehavior::Fail503Json => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({ "error": { "message": "service unavailable" } })),
                    )
                        .into_response(),
                    UpstreamBehavior::AlwaysOkWithCache => Json(json!({
                        "id": "chatcmpl_b_ok",
                        "object": "chat.completion",
                        "model": body["model"],
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": "backup ok"}, "finish_reason": "stop"}],
                        "usage": {
                            "prompt_tokens": 100,
                            "completion_tokens": 20,
                            "total_tokens": 120,
                            "prompt_tokens_details": { "cached_tokens": 80 },
                            "completion_tokens_details": { "reasoning_tokens": 5 }
                        }
                    })).into_response(),
                    UpstreamBehavior::AnthropicJsonRich => Json(json!({
                        "id": "chatcmpl_anth_json",
                        "object": "chat.completion",
                        "model": body["model"],
                        "choices": [{
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "reasoning_content": "Let me inspect tool.",
                                "content": "Calling get_weather now.",
                                "tool_calls": [{
                                    "id": "call_weather_1",
                                    "type": "function",
                                    "function": { "name": "get_weather", "arguments": "{\"city\":\"Shanghai\"}" }
                                }]
                            },
                            "finish_reason": "tool_calls"
                        }],
                        "usage": {
                            "prompt_tokens": 120,
                            "completion_tokens": 35,
                            "total_tokens": 155,
                            "prompt_tokens_details": { "cached_tokens": 90 }
                        }
                    })).into_response(),
                    UpstreamBehavior::AnthropicSseRich => {
                        let frames = concat!(
                            "data: {\"id\":\"cmpl_1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"Thinking...\"}}]}\n\n",
                            "data: {\"id\":\"cmpl_1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Answer: \"}}]}\n\n",
                            "data: {\"id\":\"cmpl_1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"calc\",\"arguments\":\"{\\\"x\\\":\"}}]}}]}\n\n",
                            "data: {\"id\":\"cmpl_1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"42}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":60,\"completion_tokens\":18,\"prompt_tokens_details\":{\"cached_tokens\":40}}}\n\n",
                            "data: [DONE]\n\n"
                        );
                        (
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            frames.to_string(),
                        )
                            .into_response()
                    }
                    UpstreamBehavior::EchoMaskedJson => {
                        let user_msg = body["messages"]
                            .as_array()
                            .and_then(|arr| arr.last())
                            .and_then(|m| m.get("content"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        Json(json!({
                            "id": "chatcmpl_mask",
                            "object": "chat.completion",
                            "model": body["model"],
                            "choices": [{"index": 0, "message": {"role": "assistant", "content": format!("echo:{user_msg}")}, "finish_reason": "stop"}],
                            "usage": {"prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28}
                        })).into_response()
                    }
                    UpstreamBehavior::SplitMaskedSse => {
                        let chunks = vec![
                            Ok::<_, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"cmpl_m\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"token=\"}}]}\n\n",
                            )),
                            Ok::<_, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"cmpl_m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"[MASKED_\"}}]}\n\n",
                            )),
                            Ok::<_, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"cmpl_m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"KEY_01]!\"}}]}\n\n",
                            )),
                            Ok::<_, std::io::Error>(Bytes::from_static(
                                b"data: {\"id\":\"cmpl_m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                            )),
                        ];
                        (
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            Body::from_stream(futures::stream::iter(chunks)),
                        )
                            .into_response()
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{addr}/v1"), task)
}

impl MultiUpstreamFixture {
    async fn new(
        primary_beh: UpstreamBehavior,
        backup_beh: UpstreamBehavior,
        group: RouteGroup,
        auth: bool,
        masking: MaskingSettings,
    ) -> Self {
        let primary_hits = Arc::new(AtomicUsize::new(0));
        let backup_hits = Arc::new(AtomicUsize::new(0));
        let primary_bodies = Arc::new(Mutex::new(Vec::new()));
        let backup_bodies = Arc::new(Mutex::new(Vec::new()));

        let (p_url, p_task) =
            spawn_upstream(primary_beh, primary_hits.clone(), primary_bodies.clone()).await;
        let (b_url, b_task) =
            spawn_upstream(backup_beh, backup_hits.clone(), backup_bodies.clone()).await;

        let store = Arc::new(Mutex::new(Store::open_memory().unwrap()));
        {
            let s = crate::gateway::lock(&store);
            s.vault_upsert(&VaultItem {
                id: "v_primary".into(),
                platform: "openai".into(),
                label: "primary".into(),
                hint: "***".into(),
                note: String::new(),
                api_endpoint: p_url.clone(),
                website: String::new(),
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
            s.vault_upsert(&VaultItem {
                id: "v_backup".into(),
                platform: "openai".into(),
                label: "backup".into(),
                hint: "***".into(),
                note: String::new(),
                api_endpoint: b_url.clone(),
                website: String::new(),
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        }
        let g = Gateway::with_credentials(store, Arc::new(MagpieCreds), Arc::new(MagpieCreds)).unwrap();
        let ch_primary: Published = serde_json::from_value(json!({
            "channelId": "ch_primary",
            "vaultId": "v_primary",
            "source": "openai_compat",
            "alias": "primary",
            "baseUrl": p_url,
            "systemProxy": false,
            "models": ["m-a", "m-bad"]
        }))
        .unwrap();
        let ch_backup: Published = serde_json::from_value(json!({
            "channelId": "ch_backup",
            "vaultId": "v_backup",
            "source": "openai_compat",
            "alias": "backup",
            "baseUrl": b_url,
            "systemProxy": false,
            "models": ["m-b"]
        }))
        .unwrap();
        {
            let mut inner = crate::gateway::lock(&g.inner);
            inner.settings.auth = auth;
            inner.settings.lan = false;
            inner.settings.channels = vec![ch_primary, ch_backup];
            inner.settings.groups = vec![group];
            inner.settings.masking = masking;
        }
        let gw_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gw_addr = gw_listener.local_addr().unwrap();
        let router = build_router(g.clone());
        let gw_task = tokio::spawn(async move {
            axum::serve(
                gw_listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            g,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            base_url: format!("http://{gw_addr}"),
            primary_hits,
            backup_hits,
            primary_bodies,
            _backup_bodies: backup_bodies,
            tasks: vec![p_task, b_task, gw_task],
        }
    }
}

fn order_group(id: &str) -> RouteGroup {
    RouteGroup {
        id: id.into(),
        name: "Test Group".into(),
        strategy: RoutingStrategy::Order,
        sticky: false,
        sticky_ttl_secs: 1800,
        cooldown_secs: 60,
        targets: vec![
            RouteTarget {
                channel_id: "ch_primary".into(),
                model: "m-a".into(),
            },
            RouteTarget {
                channel_id: "ch_backup".into(),
                model: "m-b".into(),
            },
        ],
    }
}

#[tokio::test]
async fn group_pre_first_byte_failover_sets_cooldown_headers_and_ledger() {
    let f = MultiUpstreamFixture::new(
        UpstreamBehavior::FailThenOk(StatusCode::TOO_MANY_REQUESTS, Some("30")),
        UpstreamBehavior::AlwaysOkWithCache,
        order_group("coding"),
        false,
        MaskingSettings::default(),
    )
    .await;

    // 1. Check /v1/models lists group/coding
    let models_json: Value = f
        .client
        .get(format!("{}/v1/models", f.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let model_ids: Vec<&str> = models_json["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert!(
        model_ids.contains(&"group/coding"),
        "missing group/coding in /v1/models: {model_ids:?}"
    );

    // 2. First call: primary 429 (Retry-After: 30) -> pre-first-byte failover to backup
    let resp1 = f
        .client
        .post(format!("{}/v1/chat/completions", f.base_url))
        .json(&json!({
            "model": "group/coding",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);
    assert_eq!(resp1.headers()["x-gateway-route-group"], "group/coding");
    assert_eq!(resp1.headers()["x-gateway-upstream-channel"], "backup");
    assert_eq!(resp1.headers()["x-gateway-failovers"], "1");
    let body1: Value = resp1.json().await.unwrap();
    assert_eq!(body1["choices"][0]["message"]["content"], "backup ok");

    let cds = f.g.cooldown_snapshots(now_unix());
    assert_eq!(cds.len(), 1);
    assert_eq!(cds[0].channel_id, "ch_primary");
    assert!((25..=31).contains(&cds[0].remaining_secs), "cooldown {:?}", cds[0]);

    // 3. Second call: primary skipped because of cooldown -> directly hits backup without failover
    let resp2 = f
        .client
        .post(format!("{}/v1/chat/completions", f.base_url))
        .json(&json!({
            "model": "group/coding",
            "messages": [{"role": "user", "content": "hi again"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    assert_eq!(resp2.headers()["x-gateway-upstream-channel"], "backup");
    assert!(resp2.headers().get("x-gateway-failovers").is_none());
    assert_eq!(f.primary_hits.load(Ordering::SeqCst), 1);
    assert_eq!(f.backup_hits.load(Ordering::SeqCst), 2);

    // 4. Verify persistent SQLite ledger captured failover, cached tokens, and reasoning tokens
    let summary = crate::gateway::lock(&f.g.store)
        .gateway_usage_summary(0, 20)
        .unwrap();
    assert_eq!(summary.total_requests, 2);
    assert_eq!(summary.ok_requests, 2);
    assert_eq!(summary.failover_requests, 1);
    assert_eq!(summary.cached_prompt_tokens, 160);
    assert_eq!(summary.reasoning_tokens, 10);
    assert_eq!(summary.cache_hit_rate, Some(0.8));
}

#[tokio::test]
async fn group_400_bad_request_does_not_failover() {
    let f = MultiUpstreamFixture::new(
        UpstreamBehavior::Always400,
        UpstreamBehavior::AlwaysOkWithCache,
        order_group("coding"),
        false,
        MaskingSettings::default(),
    )
    .await;

    let resp = f
        .client
        .post(format!("{}/v1/chat/completions", f.base_url))
        .json(&json!({
            "model": "group/coding",
            "messages": [{"role": "user", "content": "bad param"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(f.primary_hits.load(Ordering::SeqCst), 1);
    assert_eq!(f.backup_hits.load(Ordering::SeqCst), 0);

    let summary = crate::gateway::lock(&f.g.store)
        .gateway_usage_summary(0, 20)
        .unwrap();
    assert_eq!(summary.total_requests, 1);
    assert_eq!(summary.error_requests, 1);
    assert_eq!(summary.failover_requests, 0);
}

#[tokio::test]
async fn sticky_session_pins_same_upstream_across_turns() {
    let group = RouteGroup {
        id: "stickygrp".into(),
        name: "Sticky Rotate".into(),
        strategy: RoutingStrategy::Rotate,
        sticky: true,
        sticky_ttl_secs: 1800,
        cooldown_secs: 60,
        targets: vec![
            RouteTarget {
                channel_id: "ch_primary".into(),
                model: "m-a".into(),
            },
            RouteTarget {
                channel_id: "ch_backup".into(),
                model: "m-b".into(),
            },
        ],
    };
    let f = MultiUpstreamFixture::new(
        UpstreamBehavior::AlwaysOkWithCache,
        UpstreamBehavior::AlwaysOkWithCache,
        group,
        false,
        MaskingSettings::default(),
    )
    .await;

    let mut chosen = Vec::new();
    for _ in 0..2 {
        let resp = f
            .client
            .post(format!("{}/v1/chat/completions", f.base_url))
            .header("x-session-id", "sess-sticky-A")
            .json(&json!({
                "model": "group/stickygrp",
                "messages": [{"role": "user", "content": "turn"}],
                "stream": false
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        chosen.push(
            resp.headers()["x-gateway-upstream-channel"]
                .to_str()
                .unwrap()
                .to_string(),
        );
    }
    assert_eq!(chosen[0], chosen[1], "sticky session must hit same channel");

    let resp_other = f
        .client
        .post(format!("{}/v1/chat/completions", f.base_url))
        .header("x-session-id", "sess-sticky-B")
        .json(&json!({
            "model": "group/stickygrp",
            "messages": [{"role": "user", "content": "other"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    let other_ch = resp_other.headers()["x-gateway-upstream-channel"]
        .to_str()
        .unwrap()
        .to_string();
    assert_ne!(
        other_ch, chosen[0],
        "different session with Rotate strategy should rotate to next channel"
    );
}

#[tokio::test]
async fn anthropic_messages_auth_count_tokens_failover_json_and_sse() {
    let f = MultiUpstreamFixture::new(
        UpstreamBehavior::Fail503Json,
        UpstreamBehavior::AnthropicJsonRich,
        order_group("claude"),
        true,
        MaskingSettings::default(),
    )
    .await;

    // 1. Unauthorized without x-api-key
    let unauth = f
        .client
        .post(format!("{}/v1/messages", f.base_url))
        .json(&json!({
            "model": "group/claude",
            "max_tokens": 128,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);
    let err_body: Value = unauth.json().await.unwrap();
    assert_eq!(err_body["type"], "error");
    assert_eq!(err_body["error"]["type"], "authentication_error");

    // 2. count_tokens with x-api-key
    let cnt = f
        .client
        .post(format!("{}/v1/messages/count_tokens", f.base_url))
        .header("x-api-key", GATEWAY_TOKEN)
        .json(&json!({
            "model": "group/claude",
            "system": "You are a helpful coding assistant.",
            "messages": [{"role": "user", "content": "Count my tokens please"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(cnt.status(), StatusCode::OK);
    let cnt_json: Value = cnt.json().await.unwrap();
    assert!(cnt_json["input_tokens"].as_u64().unwrap_or(0) > 0);

    // 3. Non-stream Anthropic /v1/messages with primary 503 failover -> backup rich JSON
    let resp = f
        .client
        .post(format!("{}/v1/messages", f.base_url))
        .header("x-api-key", GATEWAY_TOKEN)
        .json(&json!({
            "model": "group/claude",
            "max_tokens": 256,
            "system": "sys prompt",
            "messages": [{"role": "user", "content": "check weather"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["x-gateway-upstream-channel"], "backup");
    assert_eq!(resp.headers()["x-gateway-failovers"], "1");
    let msg: Value = resp.json().await.unwrap();
    assert_eq!(msg["type"], "message");
    assert_eq!(msg["stop_reason"], "tool_use");
    assert_eq!(msg["usage"]["cache_read_input_tokens"], 90);
    let blocks = msg["content"].as_array().unwrap();
    assert!(blocks.iter().any(|b| b["type"] == "thinking"));
    assert!(blocks.iter().any(|b| b["type"] == "text"));
    assert!(blocks.iter().any(|b| b["type"] == "tool_use" && b["name"] == "get_weather"));

    // 4. Streaming Anthropic /v1/messages with thinking_delta + text_delta + input_json_delta
    let f_sse = MultiUpstreamFixture::new(
        UpstreamBehavior::AnthropicSseRich,
        UpstreamBehavior::AnthropicSseRich,
        order_group("claude-sse"),
        true,
        MaskingSettings::default(),
    )
    .await;
    let sse_resp = f_sse
        .client
        .post(format!("{}/v1/messages", f_sse.base_url))
        .header("x-api-key", GATEWAY_TOKEN)
        .json(&json!({
            "model": "primary/m-a",
            "max_tokens": 128,
            "stream": true,
            "messages": [{"role": "user", "content": "stream it"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(sse_resp.status(), StatusCode::OK);
    let sse_text = sse_resp.text().await.unwrap();
    assert!(sse_text.contains("event: message_start"), "{sse_text}");
    assert!(sse_text.contains("thinking_delta"), "{sse_text}");
    assert!(sse_text.contains("text_delta"), "{sse_text}");
    assert!(sse_text.contains("input_json_delta"), "{sse_text}");
    assert!(sse_text.contains("cache_read_input_tokens"), "{sse_text}");
    assert!(sse_text.contains("event: message_stop"), "{sse_text}");
}

#[tokio::test]
async fn sticky_prompt_masking_default_off_enabled_sticky_and_cross_chunk_sse_unmask() {
    // 1. Default masking (disabled): secret passes through untouched
    let f_off = MultiUpstreamFixture::new(
        UpstreamBehavior::EchoMaskedJson,
        UpstreamBehavior::EchoMaskedJson,
        order_group("mask-off"),
        false,
        MaskingSettings::default(),
    )
    .await;
    let off_resp: Value = f_off
        .client
        .post(format!("{}/v1/chat/completions", f_off.base_url))
        .json(&json!({
            "model": "primary/m-a",
            "messages": [{"role": "user", "content": format!("secret {MASK_SECRET}")}],
            "stream": false
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        off_resp["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .contains(MASK_SECRET),
        "default-off masking must not alter prompt"
    );

    // 2. Enabled sticky masking: masks in upstream, sticks across turns, restores in JSON & split SSE, zero secret in SQLite/diagnostics
    let masking_on = MaskingSettings {
        enabled: true,
        sticky: true,
        ..MaskingSettings::default()
    };
    let f_on = MultiUpstreamFixture::new(
        UpstreamBehavior::EchoMaskedJson,
        UpstreamBehavior::SplitMaskedSse,
        order_group("mask-on"),
        false,
        masking_on,
    )
    .await;

    for turn in 0..2 {
        let resp: Value = f_on
            .client
            .post(format!("{}/v1/chat/completions", f_on.base_url))
            .header("x-session-id", "sess-mask-1")
            .json(&json!({
                "model": "primary/m-a",
                "messages": [{"role": "user", "content": format!("turn {turn} uses {MASK_SECRET}")}],
                "stream": false
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let restored = resp["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(restored.contains(MASK_SECRET), "client must receive restored secret: {restored}");
    }
    let upstream_seen = crate::gateway::lock(&f_on.primary_bodies);
    assert_eq!(upstream_seen.len(), 2);
    for req in upstream_seen.iter() {
        let txt = req["messages"][0]["content"].as_str().unwrap();
        assert!(!txt.contains(MASK_SECRET), "upstream must never see plaintext secret: {txt}");
        assert!(txt.contains("[MASKED_KEY_01]"), "sticky placeholder must remain [MASKED_KEY_01]: {txt}");
    }

    // Cross-SSE-chunk unmasking on backup channel ("backup/m-b")
    let sse_resp = f_on
        .client
        .post(format!("{}/v1/chat/completions", f_on.base_url))
        .header("x-session-id", "sess-mask-1")
        .json(&json!({
            "model": "backup/m-b",
            "messages": [{"role": "user", "content": format!("stream {MASK_SECRET}")}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(sse_resp.status(), StatusCode::OK);
    let sse_body = sse_resp.text().await.unwrap();
    assert!(
        sse_body.contains(MASK_SECRET),
        "split [MASKED_ / KEY_01] SSE chunks must be unmasked into client stream: {sse_body}"
    );

    // Ensure neither diagnostics nor SQLite ledger retains plaintext MASK_SECRET
    let diag_dump = serde_json::to_string(&crate::gateway::lock(&f_on.g.inner).diagnostics).unwrap();
    assert!(!diag_dump.contains(MASK_SECRET));
    let summary = crate::gateway::lock(&f_on.g.store)
        .gateway_usage_summary(0, 20)
        .unwrap();
    assert_eq!(summary.total_requests, 3);
    assert!(summary.recent.iter().all(|r| r.masked_count >= 1));
    let ledger_dump = serde_json::to_string(&summary).unwrap();
    assert!(!ledger_dump.contains(MASK_SECRET));
}

/// 7C：入站 User-Agent 归因必须落到**流式**请求的台账行上。
/// 流式 body 由连接任务轮询，middleware 的 task-local 作用域已退出，
/// 所以 agent 只能在建 guard 时取好；这里用真实 SSE 请求验证。
#[tokio::test]
async fn streamed_request_records_inbound_user_agent_in_ledger() {
    let f = MultiUpstreamFixture::new(
        UpstreamBehavior::AnthropicSseRich,
        UpstreamBehavior::AnthropicSseRich,
        order_group("ua"),
        true,
        MaskingSettings::default(),
    )
    .await;

    let resp = f
        .client
        .post(format!("{}/v1/messages", f.base_url))
        .header("x-api-key", GATEWAY_TOKEN)
        .header("user-agent", "codex/0.133.0")
        .json(&json!({
            "model": "primary/m-a",
            "max_tokens": 128,
            "stream": true,
            "messages": [{"role": "user", "content": "attribute me"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(body.contains("event: message_stop"), "{body}");

    let summary = crate::gateway::lock(&f.g.store)
        .gateway_usage_summary(0, 20)
        .unwrap();
    let row = summary
        .recent
        .iter()
        .find(|r| r.stream && r.status == 200)
        .expect("streamed request should be in the ledger");
    assert_eq!(
        row.agent.as_deref(),
        Some("codex"),
        "streamed request lost its inbound UA attribution: {row:?}"
    );
}
