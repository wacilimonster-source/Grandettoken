use super::*;
use axum::{routing::post, Router};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn http_router_bridges_chat_clients_to_a_responses_only_upstream() {
    const RESPONSES_SSE: &str = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_fixture\",\"created_at\":1}}\n\n",
        "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_fixture\",\"name\":\"lookup\",\"arguments\":\"\"}}\n\n",
        "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"delta\":\"{}\"}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"OK\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_fixture\",\"status\":\"completed\",\"usage\":{\"input_tokens\":7,\"output_tokens\":2}}}\n\n",
    );
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
    ).route("/v1/responses", post(|axum::Json(body): axum::Json<Value>| async move {
        use axum::response::IntoResponse;
        assert_eq!(body["model"], "responses-model");
        assert!(body.get("messages").is_none());
        assert!(body.get("stream_options").is_none());
        assert_eq!(body["store"], false);
        if body["input"][0]["content"] == "bridge fixture" {
            assert_eq!(body["tools"][0]["name"], "lookup");
            assert_eq!(body["input"][1]["call_id"], "call_previous");
            assert_eq!(body["input"][2], json!({"type":"function_call_output","call_id":"call_previous","output":"fixture tool result"}));
            assert_eq!(body["max_output_tokens"], 128);
        } else {
            assert_eq!(body["input"][0]["content"], "test fixture");
        }
        if body["stream"] == true {
            return ([(header::CONTENT_TYPE, "text/event-stream")], RESPONSES_SSE).into_response();
        }
        axum::Json(json!({
            "id": "resp_fixture", "object": "response", "status": "completed", "model": "responses-model",
            "output": [{"type":"message","content":[{"type":"output_text","text":"OK"}]},
                {"type":"function_call","name":"lookup","call_id":"call_fixture","arguments":"{}"}],
            "usage":{"input_tokens":7,"output_tokens":2}
        })).into_response()
    }));
    let up_task = tokio::spawn(async move {
        axum::serve(upstream_listener, upstream_router)
            .await
            .unwrap();
    });

    // 2. 启动 Gateway（免上游密钥的本地兼容渠道 + 无网关认证的本机测试模式）
    let file = std::env::temp_dir().join(format!("gw-test-{}.db", crate::vault::new_id()));
    let store = Arc::new(Mutex::new(Store::open(&file).unwrap()));
    let g = Gateway::new(store.clone()).unwrap();
    let responses_channel = {
        let mut inner = lock(&g.inner);
        inner.settings.auth = false;
        inner.settings.lan = false;
        inner.settings.channels.push(Published {
            channel_id: "ch_local".into(),
            vault_id: String::new(),
            no_upstream_key: true,
            source: "openai_compat".into(),
            wire: OpenAiWire::default(),
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
        let mut channel = inner.settings.channels[0].clone();
        channel.channel_id = "ch_responses".into();
        channel.alias = "native".into();
        channel.wire = OpenAiWire::Responses;
        channel.models.clear();
        channel.manual_models = vec!["responses-model".into(), "disabled-model".into()];
        channel.allowed_models = Some(vec!["responses-model".into()]);
        inner.settings.channels.push(channel.clone());
        channel
    };
    let discovery =
        fetch_configured_models(&g.clients.client(false).unwrap(), &responses_channel, None)
            .await
            .unwrap();
    assert!(discovery.fetched.is_empty());
    assert!(discovery.probe_note.unwrap().contains("测连通"));
    let legacy: Published = serde_json::from_value(json!({
        "source": "openai_compat", "alias": "legacy"
    }))
    .unwrap();
    assert_eq!(legacy.wire, OpenAiWire::ChatCompletions);

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

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
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
    assert!(ids.contains(&"native/responses-model"));
    assert!(!ids.contains(&"native/disabled-model"));

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

    let mut response_input = json!({
        "model": "native/responses-model",
        "messages": [
            {"role":"user","content":"bridge fixture"},
            {"role":"assistant","tool_calls":[{"id":"call_previous","type":"function","function":{"name":"lookup","arguments":"{}"}}]},
            {"role":"tool","tool_call_id":"call_previous","content":"fixture tool result"}
        ],
        "tools": [{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}],
        "max_tokens": 128,
        "stream": false
    });
    let response_url = format!("http://{gw_addr}/v1/chat/completions");
    let chat: Value = client
        .post(&response_url)
        .json(&response_input)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(chat["object"], "chat.completion");
    assert_eq!(chat["id"], "chatcmpl-resp_fixture");
    assert_eq!(chat["model"], "native/responses-model");
    assert_eq!(chat["choices"][0]["message"]["content"], "OK");
    assert_eq!(
        chat["choices"][0]["message"]["tool_calls"][0]["id"],
        "call_fixture"
    );
    assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(chat["usage"]["total_tokens"], 9);
    response_input["stream"] = json!(true);
    response_input["stream_options"] = json!({"include_usage":true});
    let streamed = client
        .post(format!("http://{gw_addr}/chat/completions"))
        .json(&response_input)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        streamed.headers()[header::CONTENT_TYPE],
        "text/event-stream; charset=utf-8"
    );
    let streamed = streamed.text().await.unwrap();
    assert!(streamed.contains("\"object\":\"chat.completion.chunk\""));
    assert!(streamed.contains("\"content\":\"OK\""));
    assert!(streamed.contains("\"arguments\":\"{}\""));
    assert!(streamed.contains("\"finish_reason\":\"tool_calls\""));
    assert!(streamed.contains("\"prompt_tokens\":7"));
    assert!(streamed.ends_with("data: [DONE]\n\n"));
    let mut invalid = response_input.clone();
    invalid["n"] = json!(2);
    let wrong = client
        .post(&response_url)
        .json(&invalid)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        wrong.json::<Value>().await.unwrap()["error"]["code"],
        "responses_conversion"
    );
    let blocked = client
        .post(&response_url)
        .header(header::ORIGIN, "https://evil.test")
        .json(&response_input)
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), reqwest::StatusCode::UNAUTHORIZED);
    response_input["model"] = json!("native/disabled-model");
    let disabled = client
        .post(&response_url)
        .json(&response_input)
        .send()
        .await
        .unwrap();
    assert_eq!(disabled.status(), reqwest::StatusCode::NOT_FOUND);
    let result = run_test_chat(
        &g,
        responses_channel,
        "responses-model".into(),
        "test fixture".into(),
        true,
        false,
        None,
    )
    .await;
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(result.content, "OK");
    assert_eq!(result.prompt_tokens, Some(7));

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
    let _ = gw_task.await;
    let _ = up_task.await;
    drop(g);
    drop(store);
    let _ = std::fs::remove_file(file);
}
