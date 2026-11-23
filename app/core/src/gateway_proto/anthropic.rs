//! Anthropic Messages (`/v1/messages` & `/v1/messages/count_tokens`) 协议转换器。

use super::error::redact_secrets;
use super::stream::{extract_usage_details, UsageDetails};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct AnthropicRequestMeta {
    pub requested_model: String,
    pub stream: bool,
}

pub fn anthropic_error_value(err_type: &str, message: &str) -> Value {
    json!({
        "type": "error",
        "error": {
            "type": redact_secrets(err_type),
            "message": redact_secrets(message),
        }
    })
}

fn flatten_system(sys: &Value) -> Option<String> {
    match sys {
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| s.clone())
        }
        Value::Array(items) => {
            let parts: Vec<&str> = items
                .iter()
                .filter_map(|item| {
                    if item.get("type").and_then(Value::as_str) == Some("text") {
                        item.get("text").and_then(Value::as_str)
                    } else {
                        item.get("text").and_then(Value::as_str)
                    }
                })
                .collect();
            let joined = parts.join("\n");
            (!joined.trim().is_empty()).then_some(joined)
        }
        _ => None,
    }
}

fn flatten_tool_result_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| item.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(v) if !v.is_null() => v.to_string(),
        _ => String::new(),
    }
}

fn image_block_to_openai(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    let kind = source.get("type").and_then(Value::as_str)?;
    let url = match kind {
        "base64" => {
            let media = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            let data = source.get("data").and_then(Value::as_str)?;
            format!("data:{media};base64,{data}")
        }
        "url" => source.get("url").and_then(Value::as_str)?.to_string(),
        _ => return None,
    };
    Some(json!({
        "type": "image_url",
        "image_url": { "url": url }
    }))
}

pub fn anthropic_to_openai_request(body: &Value) -> Result<(Value, AnthropicRequestMeta), String> {
    let Some(obj) = body.as_object() else {
        return Err("请求体必须是 JSON 对象".into());
    };
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "缺少 model 字段".to_string())?
        .to_string();
    let stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);

    let mut messages: Vec<Value> = Vec::new();
    if let Some(sys) = obj.get("system").and_then(flatten_system) {
        messages.push(json!({ "role": "system", "content": sys }));
    }

    let raw_msgs = obj
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "缺少 messages 数组".to_string())?;
    for msg in raw_msgs {
        let role = msg
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .trim();
        let content = msg.get("content");
        match content {
            Some(Value::String(s)) => {
                messages.push(json!({ "role": role, "content": s }));
            }
            Some(Value::Array(blocks)) => {
                if role == "assistant" {
                    let mut text_parts = Vec::new();
                    let mut tool_calls = Vec::new();
                    for b in blocks {
                        match b.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(t) = b.get("text").and_then(Value::as_str) {
                                    text_parts.push(t);
                                }
                            }
                            Some("tool_use") => {
                                let id = b.get("id").and_then(Value::as_str).unwrap_or("");
                                let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                                let input = b.get("input").cloned().unwrap_or_else(|| json!({}));
                                if !name.is_empty() {
                                    tool_calls.push(json!({
                                        "id": id,
                                        "type": "function",
                                        "function": {
                                            "name": name,
                                            "arguments": input.to_string(),
                                        }
                                    }));
                                }
                            }
                            _ => {}
                        }
                    }
                    let joined = text_parts.join("");
                    let mut out = Map::new();
                    out.insert("role".into(), json!("assistant"));
                    if !joined.is_empty() || tool_calls.is_empty() {
                        out.insert("content".into(), json!(joined));
                    } else {
                        out.insert("content".into(), Value::Null);
                    }
                    if !tool_calls.is_empty() {
                        out.insert("tool_calls".into(), Value::Array(tool_calls));
                    }
                    messages.push(Value::Object(out));
                } else {
                    let mut user_parts = Vec::new();
                    for b in blocks {
                        match b.get("type").and_then(Value::as_str) {
                            Some("tool_result") => {
                                let tool_call_id = b
                                    .get("tool_use_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                let txt = flatten_tool_result_content(b.get("content"));
                                messages.push(json!({
                                    "role": "tool",
                                    "tool_call_id": tool_call_id,
                                    "content": txt,
                                }));
                            }
                            Some("text") => {
                                if let Some(t) = b.get("text").and_then(Value::as_str) {
                                    user_parts.push(json!({ "type": "text", "text": t }));
                                }
                            }
                            Some("image") => {
                                if let Some(img) = image_block_to_openai(b) {
                                    user_parts.push(img);
                                }
                            }
                            _ => {}
                        }
                    }
                    if !user_parts.is_empty() {
                        if user_parts.len() == 1 && user_parts[0]["type"] == "text" {
                            messages.push(json!({
                                "role": role,
                                "content": user_parts[0]["text"].clone(),
                            }));
                        } else {
                            messages.push(json!({
                                "role": role,
                                "content": user_parts,
                            }));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert("messages".into(), Value::Array(messages));
    out.insert("stream".into(), json!(stream));
    if stream {
        out.insert("stream_options".into(), json!({ "include_usage": true }));
    }
    if let Some(max_tok) = obj.get("max_tokens").and_then(Value::as_u64) {
        out.insert("max_tokens".into(), json!(max_tok));
    }
    if let Some(temp) = obj.get("temperature") {
        if temp.is_number() {
            out.insert("temperature".into(), temp.clone());
        }
    }
    if let Some(top_p) = obj.get("top_p") {
        if top_p.is_number() {
            out.insert("top_p".into(), top_p.clone());
        }
    }
    if let Some(stop) = obj.get("stop_sequences") {
        if stop.is_array() {
            out.insert("stop".into(), stop.clone());
        }
    }
    if let Some(tools) = obj.get("tools").and_then(Value::as_array) {
        let converted: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name").and_then(Value::as_str)?;
                let mut fn_obj = Map::new();
                fn_obj.insert("name".into(), json!(name));
                if let Some(desc) = t.get("description").and_then(Value::as_str) {
                    fn_obj.insert("description".into(), json!(desc));
                }
                if let Some(schema) = t.get("input_schema") {
                    fn_obj.insert("parameters".into(), schema.clone());
                }
                Some(json!({
                    "type": "function",
                    "function": Value::Object(fn_obj),
                }))
            })
            .collect();
        if !converted.is_empty() {
            out.insert("tools".into(), Value::Array(converted));
        }
    }
    if let Some(tc) = obj.get("tool_choice") {
        match tc.get("type").and_then(Value::as_str) {
            Some("auto") => {
                out.insert("tool_choice".into(), json!("auto"));
            }
            Some("any") => {
                out.insert("tool_choice".into(), json!("required"));
            }
            Some("none") => {
                out.insert("tool_choice".into(), json!("none"));
            }
            Some("tool") => {
                if let Some(name) = tc.get("name").and_then(Value::as_str) {
                    out.insert(
                        "tool_choice".into(),
                        json!({ "type": "function", "function": { "name": name } }),
                    );
                }
            }
            _ => {}
        }
    }
    if let Some(thinking) = obj.get("thinking").and_then(Value::as_object) {
        match thinking.get("type").and_then(Value::as_str) {
            Some("enabled") => {
                out.insert("include_reasoning".into(), json!(true));
                let budget = thinking
                    .get("budget_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(4096);
                let effort = if budget <= 2048 {
                    "low"
                } else if budget <= 8192 {
                    "medium"
                } else {
                    "high"
                };
                out.insert("reasoning_effort".into(), json!(effort));
            }
            Some("disabled") => {
                out.insert("include_reasoning".into(), json!(false));
                out.insert("reasoning_effort".into(), json!("none"));
            }
            _ => {}
        }
    }

    Ok((
        Value::Object(out),
        AnthropicRequestMeta {
            requested_model: model,
            stream,
        },
    ))
}

fn map_stop_reason(finish_reason: Option<&str>) -> &'static str {
    match finish_reason {
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        _ => "end_turn",
    }
}

fn to_anthropic_usage(details: UsageDetails) -> Value {
    let cached = details.cached_prompt_tokens.unwrap_or(0);
    let cache_create = details.cache_creation_tokens.unwrap_or(0);
    let raw_prompt = details.prompt_tokens.unwrap_or(0);
    let uncached = raw_prompt
        .saturating_sub(cached)
        .saturating_sub(cache_create);
    let mut u = json!({
        "input_tokens": uncached,
        "output_tokens": details.completion_tokens.unwrap_or(0),
    });
    if let Some(c) = details.cached_prompt_tokens {
        u["cache_read_input_tokens"] = json!(c);
    }
    if let Some(cc) = details.cache_creation_tokens {
        u["cache_creation_input_tokens"] = json!(cc);
    }
    u
}

pub fn openai_to_anthropic_response(
    openai: &Value,
    requested_model: &str,
    msg_id: &str,
) -> Result<Value, String> {
    let choice = openai
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .ok_or_else(|| "上游响应缺少 choices".to_string())?;
    let msg = choice
        .get("message")
        .ok_or_else(|| "上游响应缺少 message".to_string())?;

    let mut content: Vec<Value> = Vec::new();
    if let Some(thinking) = msg
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        content.push(json!({
            "type": "thinking",
            "thinking": thinking,
        }));
    }
    if let Some(text) = msg
        .get("content")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        content.push(json!({
            "type": "text",
            "text": text,
        }));
    } else if let Some(refusal) = msg
        .get("refusal")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        content.push(json!({
            "type": "text",
            "text": refusal,
        }));
    }
    if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let id = call.get("id").and_then(Value::as_str).unwrap_or("toolu_0");
            let name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let args_str = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let input_val = serde_json::from_str::<Value>(args_str)
                .ok()
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({ "raw": args_str }));
            content.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input_val,
            }));
        }
    }

    let stop_reason = map_stop_reason(choice.get("finish_reason").and_then(Value::as_str));
    let usage = to_anthropic_usage(extract_usage_details(&openai["usage"]));

    Ok(json!({
        "id": msg_id,
        "type": "message",
        "role": "assistant",
        "model": requested_model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": usage,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OpenBlock {
    Thinking(usize),
    Text(usize),
    Tool { block_index: usize, tool_index: u64 },
}

pub struct AnthropicSseTranslator {
    model: String,
    msg_id: String,
    started: bool,
    finished: bool,
    next_index: usize,
    open: Option<OpenBlock>,
    tool_blocks: BTreeMap<u64, usize>,
    stop_reason: Option<String>,
    usage: UsageDetails,
}

impl AnthropicSseTranslator {
    pub fn new(model: impl Into<String>, msg_id: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            msg_id: msg_id.into(),
            started: false,
            finished: false,
            next_index: 0,
            open: None,
            tool_blocks: BTreeMap::new(),
            stop_reason: None,
            usage: UsageDetails::default(),
        }
    }

    fn sse(event: &str, data: Value) -> String {
        format!("event: {event}\ndata: {data}\n\n")
    }

    fn ensure_started(&mut self, out: &mut Vec<String>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(Self::sse(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.msg_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 }
                }
            }),
        ));
    }

    fn close_open(&mut self, out: &mut Vec<String>) {
        let Some(blk) = self.open.take() else {
            return;
        };
        let idx = match blk {
            OpenBlock::Thinking(i) | OpenBlock::Text(i) => i,
            OpenBlock::Tool { block_index, .. } => block_index,
        };
        out.push(Self::sse(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": idx }),
        ));
    }

    fn ensure_thinking(&mut self, out: &mut Vec<String>) -> usize {
        if let Some(OpenBlock::Thinking(i)) = self.open {
            return i;
        }
        self.close_open(out);
        let idx = self.next_index;
        self.next_index += 1;
        out.push(Self::sse(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": idx,
                "content_block": { "type": "thinking", "thinking": "" }
            }),
        ));
        self.open = Some(OpenBlock::Thinking(idx));
        idx
    }

    fn ensure_text(&mut self, out: &mut Vec<String>) -> usize {
        if let Some(OpenBlock::Text(i)) = self.open {
            return i;
        }
        self.close_open(out);
        let idx = self.next_index;
        self.next_index += 1;
        out.push(Self::sse(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": idx,
                "content_block": { "type": "text", "text": "" }
            }),
        ));
        self.open = Some(OpenBlock::Text(idx));
        idx
    }

    fn ensure_tool(
        &mut self,
        tool_index: u64,
        id: Option<&str>,
        name: Option<&str>,
        out: &mut Vec<String>,
    ) -> usize {
        if let Some(&idx) = self.tool_blocks.get(&tool_index) {
            if matches!(self.open, Some(OpenBlock::Tool { block_index, .. }) if block_index == idx) {
                return idx;
            }
        }
        self.close_open(out);
        let idx = self.next_index;
        self.next_index += 1;
        self.tool_blocks.insert(tool_index, idx);
        out.push(Self::sse(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": idx,
                "content_block": {
                    "type": "tool_use",
                    "id": id.filter(|s| !s.is_empty()).unwrap_or("toolu_0"),
                    "name": name.filter(|s| !s.is_empty()).unwrap_or("tool"),
                    "input": {}
                }
            }),
        ));
        self.open = Some(OpenBlock::Tool {
            block_index: idx,
            tool_index,
        });
        idx
    }

    pub fn push_openai_frame(&mut self, frame: &str) -> Vec<String> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        let Some(payload) = frame.strip_prefix("data: ") else {
            return out;
        };
        let trimmed = payload.trim();
        if trimmed == "[DONE]" {
            out.extend(self.finish());
            return out;
        }
        let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
            return out;
        };
        if let Some(err) = val.get("error").filter(|e| !e.is_null()) {
            self.close_open(&mut out);
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("上游流式响应出错");
            out.push(Self::sse("error", anthropic_error_value("api_error", msg)));
            self.finished = true;
            return out;
        }
        if let Some(usage) = val.get("usage") {
            let d = extract_usage_details(usage);
            if d.prompt_tokens.is_some() {
                self.usage.prompt_tokens = d.prompt_tokens;
            }
            if d.cached_prompt_tokens.is_some() {
                self.usage.cached_prompt_tokens = d.cached_prompt_tokens;
            }
            if d.cache_creation_tokens.is_some() {
                self.usage.cache_creation_tokens = d.cache_creation_tokens;
            }
            if d.completion_tokens.is_some() {
                self.usage.completion_tokens = d.completion_tokens;
            }
            if d.reasoning_tokens.is_some() {
                self.usage.reasoning_tokens = d.reasoning_tokens;
            }
        }
        let Some(choice) = val
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        else {
            return out;
        };
        self.ensure_started(&mut out);
        if let Some(delta) = choice.get("delta") {
            if let Some(rc) = delta
                .get("reasoning_content")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                let idx = self.ensure_thinking(&mut out);
                out.push(Self::sse(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": { "type": "thinking_delta", "thinking": rc }
                    }),
                ));
            }
            if let Some(txt) = delta
                .get("content")
                .or_else(|| delta.get("refusal"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                let idx = self.ensure_text(&mut out);
                out.push(Self::sse(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": { "type": "text_delta", "text": txt }
                    }),
                ));
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let t_idx = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let id = call.get("id").and_then(Value::as_str);
                    let name = call.pointer("/function/name").and_then(Value::as_str);
                    let idx = self.ensure_tool(t_idx, id, name, &mut out);
                    if let Some(args) = call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        out.push(Self::sse(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": idx,
                                "delta": { "type": "input_json_delta", "partial_json": args }
                            }),
                        ));
                    }
                }
            }
        }
        if let Some(fr) = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            self.stop_reason = Some(map_stop_reason(Some(fr)).to_string());
        }
        out
    }

    pub fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.ensure_started(&mut out);
        self.close_open(&mut out);
        let reason = self
            .stop_reason
            .as_deref()
            .unwrap_or(if self.tool_blocks.is_empty() {
                "end_turn"
            } else {
                "tool_use"
            });
        out.push(Self::sse(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": reason,
                    "stop_sequence": Value::Null
                },
                "usage": to_anthropic_usage(self.usage)
            }),
        ));
        out.push(Self::sse("message_stop", json!({ "type": "message_stop" })));
        self.finished = true;
        out
    }
}

fn estimate_str_tokens(s: &str) -> u64 {
    let mut ascii = 0u64;
    let mut non_ascii = 0u64;
    for ch in s.chars() {
        if ch.is_ascii() {
            ascii += 1;
        } else {
            non_ascii += 1;
        }
    }
    ascii.div_ceil(4) + (non_ascii * 2).div_ceil(3)
}

fn estimate_value_tokens(v: &Value) -> u64 {
    match v {
        Value::String(s) => estimate_str_tokens(s),
        Value::Array(arr) => arr.iter().map(estimate_value_tokens).sum(),
        Value::Object(obj) => obj
            .iter()
            .map(|(k, val)| estimate_str_tokens(k) + estimate_value_tokens(val))
            .sum(),
        _ => 1,
    }
}

pub fn estimate_anthropic_input_tokens(body: &Value) -> u64 {
    let mut total = 0u64;
    if let Some(sys) = body.get("system") {
        total += estimate_value_tokens(sys);
    }
    if let Some(msgs) = body.get("messages").and_then(Value::as_array) {
        for m in msgs {
            total += 4 + estimate_value_tokens(m);
        }
    }
    if let Some(tools) = body.get("tools") {
        total += estimate_value_tokens(tools);
    }
    total.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_anthropic_request_with_system_tools_image_and_thinking() {
        let req = json!({
            "model": "group/coding",
            "max_tokens": 1024,
            "system": [{"type":"text","text":"Be concise"}],
            "thinking": {"type":"enabled","budget_tokens":4096},
            "tools": [{"name":"read","description":"read file","input_schema":{"type":"object"}}],
            "tool_choice": {"type":"any"},
            "messages": [
                {"role":"user","content":[
                    {"type":"text","text":"check"},
                    {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}}
                ]},
                {"role":"assistant","content":[
                    {"type":"thinking","thinking":"old"},
                    {"type":"tool_use","id":"t1","name":"read","input":{"path":"a.rs"}}
                ]},
                {"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"t1","content":"ok"}
                ]}
            ]
        });
        let (oai, meta) = anthropic_to_openai_request(&req).unwrap();
        assert_eq!(meta.requested_model, "group/coding");
        assert_eq!(oai["tool_choice"], "required");
        assert_eq!(oai["reasoning_effort"], "medium");
        assert_eq!(oai["include_reasoning"], true);
        let msgs = oai["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["content"][1]["image_url"]["url"], "data:image/png;base64,AAAA");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "read");
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "t1");
    }

    #[test]
    fn converts_openai_non_stream_and_sse_to_anthropic() {
        let oai = json!({
            "choices":[{
                "index":0,
                "message":{
                    "role":"assistant",
                    "reasoning_content":"step 1",
                    "content":"done",
                    "tool_calls":[{"id":"c1","type":"function","function":{"name":"read","arguments":"{\"p\":1}"}}]
                },
                "finish_reason":"tool_calls"
            }],
            "usage":{"prompt_tokens":100,"completion_tokens":25,"prompt_tokens_details":{"cached_tokens":60}}
        });
        let resp = openai_to_anthropic_response(&oai, "group/coding", "msg_1").unwrap();
        assert_eq!(resp["stop_reason"], "tool_use");
        assert_eq!(resp["content"][0]["type"], "thinking");
        assert_eq!(resp["content"][1]["type"], "text");
        assert_eq!(resp["content"][2]["type"], "tool_use");
        assert_eq!(resp["content"][2]["input"]["p"], 1);
        assert_eq!(resp["usage"]["input_tokens"], 40);
        assert_eq!(resp["usage"]["cache_read_input_tokens"], 60);

        let mut tr = AnthropicSseTranslator::new("group/coding", "msg_s");
        let mut events = Vec::new();
        events.extend(tr.push_openai_frame("data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"think\"}}]}\n\n"));
        events.extend(tr.push_openai_frame("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n"));
        events.extend(tr.push_openai_frame("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"fn\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n"));
        events.extend(tr.push_openai_frame("data: [DONE]\n\n"));
        let joined = events.join("");
        assert!(joined.contains("event: message_start"));
        assert!(joined.contains("\"type\":\"thinking_delta\""));
        assert!(joined.contains("\"type\":\"text_delta\""));
        assert!(joined.contains("\"type\":\"input_json_delta\""));
        assert!(joined.contains("\"stop_reason\":\"tool_use\""));
        assert!(joined.contains("event: message_stop"));
    }

    #[test]
    fn count_tokens_estimates_system_messages_and_tools() {
        let count = estimate_anthropic_input_tokens(&json!({
            "system": "You are helpful.",
            "messages": [{"role":"user","content":"你好世界 hello"}],
            "tools": [{"name":"read","input_schema":{"type":"object"}}]
        }));
        assert!(count > 10);
    }
}
