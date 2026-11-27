use super::*;
use crate::{store::{Store, VaultItem}, vault::transaction::CredentialStore};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

const UPSTREAM_KEY: &str = "opaque-upstream-fixture-value";
const HEADER_KEY: &str = "opaque-header-fixture-value";

struct FixtureCredentials;
impl CredentialStore for FixtureCredentials {
    fn get(&self, _: &str) -> Result<Option<String>, String> { Ok(Some(UPSTREAM_KEY.into())) }
    fn set(&self, _: &str, _: &str) -> Result<(), String> { Ok(()) }
    fn delete(&self, _: &str) -> Result<(), String> { Ok(()) }
}

struct HttpFixture {
    g: Gateway,
    channel: Published,
    client: reqwest::Client,
    url: String,
    upstream_started: Arc<tokio::sync::Notify>,
    tasks: Vec<JoinHandle<()>>,
}
impl Drop for HttpFixture {
    fn drop(&mut self) { for task in &self.tasks { task.abort(); } }
}
impl HttpFixture {
    async fn new() -> Self {
        let upstream_started = Arc::new(tokio::sync::Notify::new());
        let notify = upstream_started.clone();
        let router = Router::new().route("/v1/chat/completions", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let notify = notify.clone();
            async move {
                assert_eq!(headers[header::AUTHORIZATION], format!("Bearer {UPSTREAM_KEY}"));
                assert_eq!(headers["x-api-key"], HEADER_KEY);
                let model = body["model"].as_str().unwrap();
                notify.notify_one();
                match model {
                    "error-json" | "http-error" => {
                        let status = if model == "http-error" { StatusCode::TOO_MANY_REQUESTS } else { StatusCode::OK };
                        (status, Json(json!({"error": {
                            "message": format!("failed {UPSTREAM_KEY} {HEADER_KEY}"),
                            "code": UPSTREAM_KEY, "param": HEADER_KEY, "type": HEADER_KEY,
                            "debug": {"credential": UPSTREAM_KEY}
                        }}))).into_response()
                    }
                    "noncompletion" => Json(json!({"metadata":"not a completion"})).into_response(),
                    "error-sse" => (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        format!("data: {}\n\n", json!({"error": {
                            "message": format!("failed {UPSTREAM_KEY} {HEADER_KEY}"),
                            "code": UPSTREAM_KEY, "param": HEADER_KEY, "debug": HEADER_KEY
                        }})),
                    ).into_response(),
                    "refusal" if body["stream"] == true => (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"cannot comply\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_string(),
                    ).into_response(),
                    "refusal" => Json(json!({"choices":[{"message":{"role":"assistant","content":null,"refusal":"cannot comply"},"finish_reason":"stop"}]})).into_response(),
                    "missing-choice" => (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"one\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_string(),
                    ).into_response(),
                    "large-json" | "large-error" => {
                        let status = if model == "large-error" { StatusCode::BAD_GATEWAY } else { StatusCode::OK };
                        (status, Body::from_stream(futures::stream::iter((0..129).map(|_| Ok::<_, std::io::Error>(Bytes::from(vec![b'x'; 65536])))))).into_response()
                    }
                    "waiting" => {
                        std::future::pending::<()>().await;
                        unreachable!()
                    }
                    "waiting-sse" => {
                        let first = futures::stream::once(async { Ok::<_, std::io::Error>(Bytes::from_static(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n")) });
                        ([(header::CONTENT_TYPE, "text/event-stream")], Body::from_stream(first.chain(futures::stream::pending()))).into_response()
                    }
                    _ => Json(json!({
                        "object":"chat.completion", "model":model,
                        "choices":[{"index":0,"message":{"role":"assistant","content":format!("normal {UPSTREAM_KEY} {HEADER_KEY}")},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":7,"completion_tokens":2,"total_tokens":9},
                        "extension":{"kept":true}
                    })).into_response(),
                }
            }
        }));
        let up_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_addr = up_listener.local_addr().unwrap();
        let up_task = tokio::spawn(async move { axum::serve(up_listener, router).await.unwrap(); });
        let store = Arc::new(Mutex::new(Store::open_memory().unwrap()));
        lock(&store).vault_upsert(&VaultItem {
            id:"v_http_fixture".into(), platform:"openai".into(), label:"fixture".into(), hint:"***".into(), note:String::new(),
            api_endpoint:format!("http://{up_addr}/v1"), website:String::new(), created_at:1, updated_at:1,
        }).unwrap();
        let g = Gateway::with_credentials(store, Arc::new(FixtureCredentials), Arc::new(FixtureCredentials)).unwrap();
        let channel: Published = serde_json::from_value(json!({
            "channelId":"http_fixture", "vaultId":"v_http_fixture", "source":"openai_compat", "alias":"fixture",
            "baseUrl":format!("http://{up_addr}/v1"), "systemProxy":false,
            "customHeaders":{"X-Api-Key":HEADER_KEY},
            "models":["normal","error-json","http-error","noncompletion","error-sse","refusal","missing-choice","large-json","large-error","waiting","waiting-sse"]
        })).unwrap();
        {
            let mut inner = lock(&g.inner);
            inner.settings.auth = false;
            inner.settings.lan = false;
            inner.settings.channels = vec![channel.clone()];
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = build_router(g.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        Self { g, channel, client:reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap(),
            url:format!("http://{addr}/v1/chat/completions"), upstream_started, tasks:vec![up_task, task] }
    }

    async fn request(&self, model: &str, stream: bool) -> reqwest::Response {
        self.client.post(&self.url).json(&json!({"model":format!("fixture/{model}"),"messages":[{"role":"user","content":"fixture"}],"stream":stream})).send().await.unwrap()
    }

    async fn test_chat(&self, model: &str, stream: bool) -> ChatTestResult {
        run_test_chat(&self.g, self.channel.clone(), model.into(), "fixture".into(), stream, false, None).await
    }
}

fn assert_no_credentials(value: &str) {
    assert!(!value.contains(UPSTREAM_KEY), "upstream key leaked: {value}");
    assert!(!value.contains(HEADER_KEY), "custom header leaked: {value}");
}

#[tokio::test]
async fn http_errors_redact_request_credentials_without_clearing_normal_body() {
    let f = HttpFixture::new().await;
    for (model, stream) in [("error-json",false),("http-error",false),("error-sse",true)] {
        let response = f.request(model, stream).await;
        if !stream { assert!(!response.status().is_success(), "{model} must be a failure"); }
        let text = response.text().await.unwrap();
        assert_no_credentials(&text);
        assert!(!text.contains("debug"));
        let result = f.test_chat(model, stream).await;
        assert!(!result.ok);
        assert_no_credentials(&serde_json::to_string(&result).unwrap());
    }
    assert_no_credentials(&serde_json::to_string(&lock(&f.g.inner).diagnostics).unwrap());
    let value: Value = f.request("normal",false).await.json().await.unwrap();
    assert_eq!(value["choices"][0]["message"]["content"], format!("normal {UPSTREAM_KEY} {HEADER_KEY}"));
    assert_eq!(value["extension"]["kept"], true);
    assert_eq!(value["usage"]["total_tokens"], 9);
}

#[tokio::test]
async fn http_noncompletion_200_is_protocol_failure_and_test_chat_is_not_ok() {
    let f = HttpFixture::new().await;
    assert_eq!(f.request("noncompletion",false).await.status(), StatusCode::BAD_GATEWAY);
    let result = f.test_chat("noncompletion",false).await;
    assert!(!result.ok);
    assert_eq!(result.status, 502);
}

#[tokio::test]
async fn http_refusal_is_valid_output_for_json_and_sse_diagnostics() {
    let f = HttpFixture::new().await;
    for stream in [false,true] {
        let response = f.request("refusal",stream).await;
        assert!(response.status().is_success());
        assert!(response.text().await.unwrap().contains("cannot comply"));
        let result = f.test_chat("refusal",stream).await;
        assert!(result.ok, "{:?}", result.error);
        assert!(result.diag_note.is_none(), "{:?}", result.diag_note);
    }
    assert!(lock(&f.g.inner).diagnostics.iter().all(|e| e.diag_note.is_none() && e.error.is_none()));
}

#[tokio::test]
async fn http_expected_n_detects_missing_stream_choice() {
    let f = HttpFixture::new().await;
    let text = f.client.post(&f.url).json(&json!({
        "model":"fixture/missing-choice","messages":[{"role":"user","content":"fixture"}],"stream":true,"n":2
    })).send().await.unwrap().text().await.unwrap();
    assert!(text.contains("incomplete_stream"), "{text}");
    let inner = lock(&f.g.inner);
    assert_eq!(inner.diagnostics.len(),1);
    assert_eq!(inner.diagnostics[0].status,502);
}

#[tokio::test]
async fn http_json_and_error_bodies_stop_at_eight_mib() {
    let f = HttpFixture::new().await;
    for model in ["large-json","large-error"] {
        let response = f.request(model,false).await;
        assert!(!response.status().is_success());
        let text = response.text().await.unwrap();
        assert!(text.contains("响应过大"), "{model}: {text}");
        assert!(text.len() < 2000);
        let result = f.test_chat(model,false).await;
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("响应过大"));
    }
}

#[tokio::test]
async fn http_cancelled_test_records_one_diagnostic_before_and_after_headers() {
    for model in ["waiting","waiting-sse"] {
        let f = HttpFixture::new().await;
        let g = f.g.clone();
        let channel = f.channel.clone();
        let task = tokio::spawn(async move {
            run_test_chat(&g, channel, model.into(), "fixture".into(), true, false, None).await
        });
        tokio::time::timeout(Duration::from_secs(3), f.upstream_started.notified()).await.unwrap();
        task.abort();
        let _ = task.await;
        let inner = lock(&f.g.inner);
        assert_eq!(inner.diagnostics.len(),1, "{model}");
        assert_eq!(inner.diagnostics[0].status,499);
        assert!(inner.diagnostics[0].error.as_deref().unwrap().contains("取消"));
    }
}

#[tokio::test]
async fn http_dropped_sse_response_records_one_cancel_without_polling() {
    let f = HttpFixture::new().await;
    let response = execute_inference_http(&f.g, f.channel.clone(), "fixture/waiting-sse", "waiting-sse", json!({"stream":true})).await;
    assert!(lock(&f.g.inner).diagnostics.is_empty());
    drop(response);
    let inner = lock(&f.g.inner);
    assert_eq!(inner.diagnostics.len(),1);
    assert_eq!(inner.diagnostics[0].status,499);
}

#[tokio::test]
async fn http_dropped_partial_sse_retains_counts_and_first_byte_in_one_cancel() {
    let f = HttpFixture::new().await;
    let response = execute_inference_http(&f.g, f.channel.clone(), "fixture/waiting-sse", "waiting-sse", json!({"stream":true})).await;
    let mut body = response.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(3), body.next()).await.unwrap().unwrap().unwrap();
    assert!(String::from_utf8_lossy(&first).contains("partial"));
    assert!(lock(&f.g.inner).diagnostics.is_empty());
    drop(body);
    let inner = lock(&f.g.inner);
    assert_eq!(inner.diagnostics.len(),1);
    assert_eq!(inner.diagnostics[0].status,499);
    assert_eq!(inner.diagnostics[0].content_chars,7);
    assert_eq!(inner.diagnostics[0].frame_count,1);
    assert!(inner.diagnostics[0].first_byte_ms.is_some());
}

#[tokio::test]
async fn http_completed_request_records_once_without_drop_duplicate() {
    let f = HttpFixture::new().await;
    let result = f.test_chat("normal",false).await;
    assert!(result.ok);
    assert_eq!(lock(&f.g.inner).diagnostics.len(),1);
    assert!(f.request("refusal",true).await.text().await.unwrap().ends_with("data: [DONE]\n\n"));
    assert_eq!(lock(&f.g.inner).diagnostics.len(),2);
}

#[test]
fn upstream_send_timeout_maps_to_504_instead_of_unreachable() {
    let (status, code, msg) = classify_upstream_send_error(true, "operation timed out");
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(code, "upstream_timeout");
    assert!(msg.contains("等待上游响应超时"));
    let (status, code, msg) = classify_upstream_send_error(false, "tcp connect error");
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(code, "upstream_unreachable");
    assert!(msg.contains("无法连接上游服务"));
}

#[tokio::test]
async fn agent_platform_gemini_sends_safety_off_and_repairs_400_without_dropping_thinking() {
    let captured: Arc<Mutex<Vec<(String, Option<String>, Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_srv = captured.clone();
    let router = Router::new().route(
        "/chat/completions",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let captured_srv = captured_srv.clone();
            async move {
                let auth_present = headers.contains_key(header::AUTHORIZATION);
                assert!(!auth_present, "AgentPlatform must not send Authorization header");
                let goog_key = headers
                    .get("x-goog-api-key")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let model = body["model"].as_str().unwrap_or("").to_string();
                captured_srv
                    .lock()
                    .unwrap()
                    .push((model.clone(), goog_key, body.clone()));
                if model == "google/gemini-4-hydrogen"
                    && body.pointer("/extra_body/google/safety_settings").is_some()
                {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "error": {
                                "message": "Invalid JSON payload received. Unknown name \"safety_settings\" at 'extra_body.google': Cannot find field."
                            }
                        })),
                    )
                        .into_response();
                }
                Json(json!({
                    "object": "chat.completion",
                    "model": model,
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}
                }))
                .into_response()
            }
        }),
    );

    let up_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up_addr = up_listener.local_addr().unwrap();
    let up_task = tokio::spawn(async move { axum::serve(up_listener, router).await.unwrap(); });

    let store = Arc::new(Mutex::new(Store::open_memory().unwrap()));
    lock(&store)
        .vault_upsert(&VaultItem {
            id: "v_ap_fixture".into(),
            platform: "google".into(),
            label: "ap-fixture".into(),
            hint: "***".into(),
            note: String::new(),
            api_endpoint: format!("http://{up_addr}"),
            website: String::new(),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let g = Gateway::with_credentials(store, Arc::new(FixtureCredentials), Arc::new(FixtureCredentials))
        .unwrap();
    let channel: Published = serde_json::from_value(json!({
        "channelId": "ap_fixture",
        "vaultId": "v_ap_fixture",
        "source": "agent_platform",
        "alias": "agent",
        "baseUrl": format!("http://{up_addr}"),
        "systemProxy": false,
        "models": ["google/gemini-3.8-flash", "google/gemini-4-hydrogen"]
    }))
    .unwrap();
    {
        let mut inner = lock(&g.inner);
        inner.settings.auth = false;
        inner.settings.lan = false;
        inner.settings.channels = vec![channel.clone()];
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gw_router = build_router(g.clone());
    let gw_task = tokio::spawn(async move {
        axum::serve(
            listener,
            gw_router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("http://{addr}/v1/chat/completions");

    // 1. 正常 Gemini 请求应携带 x-goog-api-key 和 4 项 OFF 的 safety_settings
    let resp1 = client
        .post(&url)
        .json(&json!({
            "model": "agent/google/gemini-3.8-flash",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "low"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);

    // 2. 上游若对 safety_settings 报 400，应只剥离 safety_settings 并保留 thinking_config 重试成功
    let resp2 = client
        .post(&url)
        .json(&json!({
            "model": "agent/google/gemini-4-hydrogen",
            "messages": [{"role": "user", "content": "think"}],
            "reasoning_effort": "medium"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let body2: Value = resp2.json().await.unwrap();
    assert_eq!(body2["choices"][0]["message"]["content"], "ok");

    let recorded = captured.lock().unwrap().clone();
    assert_eq!(recorded.len(), 3);

    assert_eq!(recorded[0].0, "google/gemini-3.8-flash");
    assert_eq!(recorded[0].1.as_deref(), Some(UPSTREAM_KEY));
    let safety0 = recorded[0].2["extra_body"]["google"]["safety_settings"]
        .as_array()
        .unwrap();
    assert_eq!(safety0.len(), 4);
    assert!(safety0.iter().all(|s| s["threshold"] == "OFF"));
    assert_eq!(
        recorded[0].2["extra_body"]["google"]["thinking_config"]["thinking_level"],
        "low"
    );

    // 第 2 次是首次 hydrogen 400，第 3 次是修复重试
    assert_eq!(recorded[1].0, "google/gemini-4-hydrogen");
    assert!(recorded[1].2.pointer("/extra_body/google/safety_settings").is_some());

    assert_eq!(recorded[2].0, "google/gemini-4-hydrogen");
    assert!(recorded[2].2.pointer("/extra_body/google/safety_settings").is_none());
    assert_eq!(
        recorded[2].2["extra_body"]["google"]["thinking_config"]["include_thoughts"],
        true
    );
    assert_eq!(
        recorded[2].2["extra_body"]["google"]["thinking_config"]["thinking_level"],
        "medium"
    );

    gw_task.abort();
    up_task.abort();
}
