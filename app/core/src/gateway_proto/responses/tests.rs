use super::*;
use crate::gateway_proto::{stream::SseStreamNormalizer, OpenAiWire, ThinkingMode};
use serde_json::{json, Value};

#[test]
fn defaults_and_base_url_keep_chat_compatibility() {
    let url = "https://example.com/gateway/v1/responses/";
    assert_eq!(OpenAiWire::default().endpoint(), "/chat/completions");
    assert_eq!(
        OpenAiWire::Responses.base_url_input(url),
        "https://example.com/gateway/v1"
    );
    assert_eq!(
        OpenAiWire::ChatCompletions.base_url_input(url),
        url.trim_end_matches('/')
    );
    assert_eq!(
        OpenAiWire::Responses.base_url_input("https://responses"),
        "https://responses"
    );
    assert_eq!(
        serde_json::to_value(OpenAiWire::Responses).unwrap(),
        "responses"
    );
}

#[test]
fn chat_request_preserves_multiturn_conversation_and_flattens_function_schema() {
    let source = json!({
        "model":"upstream-model", "stream":true, "stream_options":{"include_usage":true},
        "messages":[
            {"role":"system","content":"Instructions"},
            {"role":"user","content":[{"type":"text","text":"Look at this"},{"type":"image_url","image_url":{"url":"https://example.com/photo.png","detail":"low"}}]},
            {"role":"assistant","content":null,"tool_calls":[{"id":"call_fixture","type":"function","function":{"name":"lookup","arguments":"{}"}}]},
            {"role":"tool","tool_call_id":"call_fixture","content":"fixture result"},
            {"role":"user","content":"Continue"}
        ],
        "tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}],
        "tool_choice":{"type":"function","function":{"name":"lookup"}},
        "max_tokens":100,"max_completion_tokens":300,"stop":["END"],"include_reasoning":true,"reasoning_effort":"medium",
        "response_format":{"type":"json_schema","json_schema":{"name":"result","schema":{"type":"object"},"strict":true}}
    });
    let (body, transforms) = from_chat_request(&source).unwrap();
    assert_eq!(body["model"], "upstream-model");
    assert_eq!(
        body["input"][0],
        json!({"type":"message","role":"system","content":"Instructions"})
    );
    assert_eq!(
        body["input"][1]["content"][1],
        json!({"type":"input_image","image_url":"https://example.com/photo.png","detail":"low"})
    );
    assert_eq!(
        body["input"][2],
        json!({"type":"function_call","call_id":"call_fixture","name":"lookup","arguments":"{}"})
    );
    assert_eq!(
        body["input"][3],
        json!({"type":"function_call_output","call_id":"call_fixture","output":"fixture result"})
    );
    assert_eq!(body["input"][4]["content"], "Continue");
    assert_eq!(
        body["tools"][0],
        json!({"type":"function","name":"lookup","parameters":{"type":"object"},"strict":false})
    );
    assert_eq!(
        body["tool_choice"],
        json!({"type":"function","name":"lookup"})
    );
    assert_eq!(
        body["text"]["format"],
        json!({"type":"json_schema","name":"result","schema":{"type":"object"},"strict":true})
    );
    assert_eq!(body["max_output_tokens"], 300);
    assert_eq!(
        body["reasoning"],
        json!({"effort":"medium","summary":"auto"})
    );
    assert_eq!(body["store"], false);
    for old in ["messages", "max_tokens", "stop", "stream_options"] {
        assert!(body.get(old).is_none(), "{old}");
    }
    assert!(transforms.iter().any(|t| t == "drop:stop(responses)"));
    assert!(from_chat_request(&json!({"messages":[],"n":2})).is_err());
    assert!(
        from_chat_request(&json!({"messages":[{"role":"tool","content":"missing call ID"}]}))
            .is_err()
    );
}

#[test]
fn nonstream_completion_maps_output_usage_and_terminal_status() {
    let response = json!({
        "id":"resp_fixture","created_at":42,"status":"completed",
        "output":[
            {"type":"reasoning","summary":[{"type":"summary_text","text":"摘要"}]},
            {"type":"message","content":[{"type":"output_text","text":"好"},{"type":"output_text","text":"的"}]},
            {"type":"function_call","call_id":"call_1","name":"lookup","arguments":"{}"}
        ],
        "usage":{"input_tokens":17,"output_tokens":6,"input_tokens_details":{"cached_tokens":4},"output_tokens_details":{"reasoning_tokens":2}}
    });
    let chat = to_chat_completion(&response, "ar/upstream-model").unwrap();
    assert_eq!(chat["object"], "chat.completion");
    assert_eq!(chat["id"], "chatcmpl-resp_fixture");
    assert_eq!(chat["model"], "ar/upstream-model");
    assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(chat["choices"][0]["message"]["content"], "好的");
    assert_eq!(chat["choices"][0]["message"]["reasoning_content"], "摘要");
    assert_eq!(
        chat["choices"][0]["message"]["tool_calls"][0]["id"],
        "call_1"
    );
    assert_eq!(
        chat["usage"],
        json!({"prompt_tokens":17,"completion_tokens":6,"total_tokens":23,
        "prompt_tokens_details":{"cached_tokens":4},"completion_tokens_details":{"reasoning_tokens":2}})
    );
    let incomplete = json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[]});
    assert_eq!(
        to_chat_completion(&incomplete, "m").unwrap()["choices"][0]["finish_reason"],
        "length"
    );
    let failed = json!({"status":"failed","error":{"message":"Upstream refused the request"}});
    assert_eq!(
        to_chat_completion(&failed, "m").unwrap_err(),
        "Upstream refused the request"
    );
}

fn sse(events: &[Value], include_usage: bool) -> (Vec<Value>, String) {
    let mut bridge = ResponsesChatStream::new("ar/upstream-model", include_usage);
    let mut normalizer = SseStreamNormalizer::new("ar/upstream-model", ThinkingMode::Auto);
    let mut mapped = Vec::new();
    for event in events {
        let bytes = format!(
            "event: {}\r\ndata: {event}\r\n\r\n",
            event["type"].as_str().unwrap()
        );
        for byte in bytes.bytes() {
            mapped.extend(bridge.push_bytes(&[byte]));
        }
    }
    mapped.extend(bridge.finish());
    let mut wire = mapped
        .iter()
        .cloned()
        .flat_map(|frame| normalizer.push_value(frame))
        .collect::<String>();
    wire.push_str(&normalizer.finish().concat());
    (mapped, wire)
}

#[test]
fn stream_maps_split_unicode_and_parallel_tool_results_without_duplication() {
    let first_call =
        json!({"type":"function_call","call_id":"call_a","name":"lookup","arguments":""});
    let second_call =
        json!({"type":"function_call","call_id":"call_b","name":"measure","arguments":"{}"});
    let events = [
        json!({"type":"response.created","response":{"id":"resp_fixture","created_at":42}}),
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0,"delta":"摘要"}),
        json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"好"}),
        json!({"type":"response.output_item.added","output_index":2,"item":first_call}),
        json!({"type":"response.output_item.added","output_index":3,"item":second_call}),
        json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"x\":"}),
        json!({"type":"response.function_call_arguments.done","output_index":2,"arguments":"{\"x\":1}"}),
        json!({"type":"response.completed","response":{"id":"resp_fixture","status":"completed","output":[
            {"type":"reasoning","summary":[{"text":"摘要"}]},
            {"type":"message","content":[{"type":"output_text","text":"好的"}]},
            {"type":"function_call","call_id":"call_a","name":"lookup","arguments":"{\"x\":1}"},
            second_call
        ],"usage":{"input_tokens":8,"output_tokens":2}}}),
    ];
    let (frames, wire) = sse(&events, true);
    assert!(frames.iter().all(|f| f["id"] == "chatcmpl-resp_fixture"));
    let deltas: Vec<_> = frames
        .iter()
        .filter_map(|f| f.pointer("/choices/0/delta"))
        .collect();
    let text = deltas
        .iter()
        .filter_map(|d| d["content"].as_str())
        .collect::<String>();
    assert_eq!(text, "好的");
    let tool_arguments = deltas
        .iter()
        .filter_map(|d| d["tool_calls"].as_array())
        .flatten()
        .filter(|c| c["index"] == 0)
        .filter_map(|c| c["function"]["arguments"].as_str())
        .collect::<String>();
    assert_eq!(tool_arguments, "{\"x\":1}");
    assert!(deltas
        .iter()
        .any(|d| d["tool_calls"][0]["index"] == 1 && d["tool_calls"][0]["id"] == "call_b"));
    assert!(wire.contains("\"finish_reason\":\"tool_calls\""));
    assert!(wire.contains("\"prompt_tokens\":8"));
    assert!(wire.contains("摘要"));
    assert!(wire.ends_with("data: [DONE]\n\n"));
    assert!(!sse(&events, false).1.contains("\"usage\""));
}

#[test]
fn response_stream_bom_and_cr_terminal_is_dispatched_before_eof() {
    let mut bridge = ResponsesChatStream::new("m", false);
    let input = format!("\u{feff}data: {}\r\r", json!({
        "type":"response.completed","response":{"status":"completed","output":[]}
    }));
    let mut frames = Vec::new();
    for byte in input.bytes() {
        frames.extend(bridge.push_bytes(&[byte]));
    }
    assert!(bridge.is_terminal());
    assert_eq!(frames.last().unwrap()["choices"][0]["finish_reason"], "stop");
    assert!(bridge.finish().is_empty());
}

#[test]
fn response_stream_oversized_lines_and_events_terminate() {
    let mib = "x".repeat(1024 * 1024);
    for multiline in [false, true] {
        let mut bridge = ResponsesChatStream::new("m", false);
        let mut frames = Vec::new();
        for _ in 0..9 {
            let bytes = if multiline { format!("data: {mib}\n") } else { mib.clone() };
            frames.extend(bridge.push_bytes(bytes.as_bytes()));
        }
        assert!(bridge.is_terminal(), "multiline={multiline}");
        assert_eq!(frames.last().unwrap()["error"]["code"], "stream_buffer_limit");
        assert!(bridge.finish().is_empty());
    }
}

#[test]
fn response_stream_tool_arguments_are_limited_across_all_tools() {
    let mut bridge = ResponsesChatStream::new("m", false);
    let mut frames = Vec::new();
    for index in 0..2 {
        let event = json!({"type":"response.output_item.added","output_index":index,
            "item":{"type":"function_call","call_id":format!("call_{index}"),"name":"read","arguments":""}});
        frames.extend(bridge.push_bytes(format!("data: {event}\n\n").as_bytes()));
    }
    let mib = "x".repeat(1024 * 1024);
    for turn in 0..9 {
        let event = json!({"type":"response.function_call_arguments.delta","output_index":turn % 2,"delta":mib});
        frames.extend(bridge.push_bytes(format!("data: {event}\n\n").as_bytes()));
    }
    assert!(bridge.is_terminal());
    assert_eq!(frames.last().unwrap()["error"]["code"], "stream_buffer_limit");
    assert!(!frames.iter().any(|f| f["choices"][0]["finish_reason"].is_string()));
}

#[test]
fn response_stream_final_tool_snapshot_also_obeys_cumulative_buffer_limit() {
    let mut bridge = ResponsesChatStream::new("m", false);
    let arguments = "x".repeat(5 * 1024 * 1024);
    let mut frames = Vec::new();
    for index in 0..2 {
        let event = json!({"type":"response.output_item.added","output_index":index,
            "item":{"type":"function_call","call_id":format!("call_{index}"),"name":"read","arguments":arguments}});
        frames.extend(bridge.push_bytes(format!("data: {event}\n\n").as_bytes()));
    }
    assert!(bridge.is_terminal());
    assert_eq!(frames.last().unwrap()["error"]["code"], "stream_buffer_limit");
}

#[test]
fn response_stream_long_text_does_not_consume_cumulative_tool_budget() {
    let mut bridge = ResponsesChatStream::new("m", false);
    let text = "字".repeat(400_000);
    let mut bytes_sent = 0;
    for _ in 0..8 {
        let event = json!({"type":"response.output_text.delta","output_index":0,"delta":text});
        let frames = bridge.push_bytes(format!("data: {event}\n\n").as_bytes());
        bytes_sent += frames.iter().filter_map(|f| f["choices"][0]["delta"]["content"].as_str()).map(str::len).sum::<usize>();
        assert!(!bridge.is_terminal());
    }
    assert!(bytes_sent > 8 * 1024 * 1024);
    let event = json!({"type":"response.completed","response":{"status":"completed","output":[]}});
    let frames = bridge.push_bytes(format!("data: {event}\n\n").as_bytes());
    assert_eq!(frames.last().unwrap()["choices"][0]["finish_reason"], "stop");
    assert!(bridge.is_terminal());
}

#[test]
fn response_refusal_counts_survive_mapping_and_normalization() {
    let (frames, _) = sse(&[
        json!({"type":"response.refusal.delta","output_index":0,"content_index":0,"delta":"拒绝"}),
        json!({"type":"response.completed","response":{"status":"completed","output":[
            {"type":"message","content":[{"type":"refusal","refusal":"拒绝🙂"}]}
        ]}}),
    ], false);
    let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto);
    for frame in frames {
        norm.push_value(frame);
    }
    norm.finish();
    assert_eq!(norm.stats.refusal_chars, 3);
    assert_eq!(norm.stats.content_chars, 0);
    assert!(norm.stats.completed_cleanly);
}

#[test]
fn response_tool_limit_counts_final_tails_without_double_counting_snapshots() {
    let limit = crate::gateway_proto::stream::MAX_SSE_BUFFER_BYTES;
    let mut bridge = ResponsesChatStream::new("m", false);
    let mut frames = Vec::new();
    let half = "x".repeat(limit / 2);
    for index in 0..2 {
        let item = json!({"type":"function_call","call_id":format!("call_{index}"),"name":"read","arguments":half});
        for kind in ["response.output_item.added", "response.output_item.done"] {
            let event = json!({"type":kind,"output_index":index,"item":item});
            frames.extend(bridge.push_bytes(format!("data: {event}\n\n").as_bytes()));
        }
    }
    assert!(!bridge.is_terminal());
    let event = json!({"type":"response.function_call_arguments.done","output_index":0,"arguments":format!("{half}🙂")});
    frames.extend(bridge.push_bytes(format!("data: {event}\n\n").as_bytes()));
    assert!(bridge.is_terminal());
    assert_eq!(frames.last().unwrap()["error"]["code"], "stream_buffer_limit");
    let sent = frames.iter().filter_map(|f| f["choices"][0]["delta"]["tool_calls"].as_array()).flatten()
        .filter_map(|c| c["function"]["arguments"].as_str()).map(str::len).sum::<usize>();
    assert_eq!(sent, limit);
}

#[test]
fn response_completed_snapshot_cannot_emit_success_after_tool_limit_error() {
    let mut bridge = ResponsesChatStream::new("m", false);
    let almost = "x".repeat(8 * 1024 * 1024 - 1024);
    let added = json!({"type":"response.output_item.added","output_index":0,
        "item":{"type":"function_call","call_id":"call_0","name":"read","arguments":almost}});
    bridge.push_bytes(format!("data: {added}\n\n").as_bytes());
    assert!(!bridge.is_terminal());
    let event = json!({"type":"response.completed","response":{"status":"completed","output":[
        {"type":"function_call","call_id":"call_0","name":"read","arguments":""},
        {"type":"function_call","call_id":"call_1","name":"read","arguments":"x".repeat(2048)}
    ]}});
    let frames = bridge.push_bytes(format!("data: {event}\n\n").as_bytes());
    assert_eq!(frames.last().unwrap()["error"]["code"], "stream_buffer_limit");
    assert!(frames.iter().all(|f| !f["choices"][0]["finish_reason"].is_string()));
    assert!(bridge.is_terminal());
}

#[test]
fn response_sensitive_errors_are_cleaned_before_truncation_but_text_is_unchanged() {
    let secret = format!("opaque_fixture_{}", "q".repeat(1400));
    for kind in ["error", "response.failed", "response.completed"] {
        let mut bridge = ResponsesChatStream::new("m", false).with_sensitive_values(vec![secret.clone()]);
        let event = json!({"type":kind,"message":secret,"response":{"status":"failed","error":{"message":secret}}});
        let frames = bridge.push_bytes(format!("data: {event}\n\n").as_bytes());
        let output = serde_json::to_string(&frames).unwrap();
        assert!(!output.contains("opaque_fixture_"), "kind={kind}");
        assert!(output.contains("***"));
        assert!(bridge.is_terminal());
    }
    let mut bridge = ResponsesChatStream::new("m", false).with_sensitive_values(vec![secret.clone()]);
    let event = json!({"type":"response.output_text.delta","delta":secret});
    let frames = bridge.push_bytes(format!("data: {event}\n\n").as_bytes());
    assert!(frames.iter().any(|f| f["choices"][0]["delta"]["content"] == secret));
}

#[test]
fn missing_stream_terminal_reports_an_error_instead_of_success() {
    let events = [json!({"type":"response.output_text.delta","delta":"partial"})];
    let (frames, wire) = sse(&events, false);
    assert_eq!(frames.last().unwrap()["error"]["code"], "incomplete_stream");
    assert!(!wire.contains("\"finish_reason\":\"stop\""));
    assert!(wire.ends_with("data: [DONE]\n\n"));
}
