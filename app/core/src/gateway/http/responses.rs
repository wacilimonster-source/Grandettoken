use serde_json::Value;
use crate::gateway_proto::{
    error::{redact_error_strings, redact_known, sanitize_upstream_error},
    responses::{to_chat_completion, ResponsesChatStream},
    stream::{SseStreamNormalizer, StreamStats},
    strip_paste_artifacts, OpenAiWire, ThinkingMode,
};
use super::{Gateway, Published};
use std::{collections::BTreeMap, time::Instant};

const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn sensitive_values(api_key: Option<&str>, headers: &BTreeMap<String, String>) -> Vec<String> {
    let mut values = Vec::new();
    let mut add = |value: &str| {
        for value in [value.to_string(), strip_paste_artifacts(value)] {
            if !value.is_empty() && !values.contains(&value) { values.push(value); }
        }
    };
    if let Some(key) = api_key { add(key); }
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        if name.contains("key") || name.contains("token") || name.contains("secret")
            || name.contains("auth") || name.contains("cookie")
        {
            add(value);
            if name.contains("auth") {
                if let Some((_, token)) = value.trim().split_once(' ') { add(token.trim()); }
            }
            if name.contains("cookie") {
                for cookie in value.split(';') {
                    if let Some((_, token)) = cookie.trim().split_once('=') { add(token.trim()); }
                }
            }
        }
    }
    values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    values
}

pub(super) async fn read_body_capped(mut response: reqwest::Response, sensitive: &[String]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await
        .map_err(|e| redact_known(&format!("读取上游响应失败：{e}"), sensitive))?
    {
        crate::fetch::push_chunk(&mut body, &chunk, MAX_BODY_BYTES)?;
    }
    Ok(body)
}

pub(super) async fn read_error_body(response: reqwest::Response, sensitive: &[String]) -> Result<String, String> {
    let bytes = read_body_capped(response, sensitive).await?;
    let text = String::from_utf8_lossy(&bytes);
    if let Ok(mut error) = serde_json::from_str::<Value>(&text) {
        redact_error_strings(&mut error, sensitive);
        Ok(error.to_string())
    } else {
        Ok(redact_known(&text, sensitive))
    }
}

pub(super) fn refusal_chars(value: &Value) -> usize {
    value.get("choices").and_then(Value::as_array).into_iter().flatten()
        .filter_map(|c| c.pointer("/message/refusal").and_then(Value::as_str))
        .map(|s| s.chars().count()).sum()
}

/// Owns only request diagnostics: dropping a cancelled future must not start network work.
pub(super) struct DiagnosticGuard {
    g: Gateway,
    channel: Published,
    model: String,
    upstream_model: Option<String>,
    route_group: Option<String>,
    failover_count: u8,
    masked_count: usize,
    protocol: String,
    stream: bool,
    started: Instant,
    transforms: Vec<String>,
    pub first_byte_ms: Option<u64>,
    stats: StreamStats,
    recorded: bool,
}
impl DiagnosticGuard {
    pub fn new(g: &Gateway, channel: &Published, model: &str, stream: bool, started: Instant) -> Self {
        let mut channel = channel.clone();
        channel.custom_headers.clear();
        Self { g: g.clone(), channel, model: model.into(), upstream_model: None, route_group: None,
            failover_count: 0, masked_count: 0, protocol: "openai_chat".into(),
            stream, started, transforms: vec![], first_byte_ms: None,
            stats: StreamStats::default(), recorded: false }
    }
    pub fn set_context(
        &mut self,
        channel: &Published,
        upstream_model: &str,
        route_group: Option<String>,
        failover_count: u8,
        masked_count: usize,
        protocol: &str,
    ) {
        let mut clean = channel.clone();
        clean.custom_headers.clear();
        self.channel = clean;
        self.upstream_model = Some(upstream_model.to_string());
        self.route_group = route_group;
        self.failover_count = failover_count;
        self.masked_count = masked_count;
        self.protocol = protocol.to_string();
    }
    pub fn update(&mut self, transforms: &[String], stream: bool) {
        self.transforms = transforms.to_vec();
        self.stream = stream;
    }
    pub fn observe(&mut self, stats: &StreamStats, got_bytes: bool) {
        if got_bytes && self.first_byte_ms.is_none() {
            self.first_byte_ms = Some(self.started.elapsed().as_millis() as u64);
        }
        self.stats = stats.clone();
    }
    pub fn disarm(&mut self) { self.recorded = true; }
    pub fn finish_stream(&mut self, stats: &StreamStats, error: Option<String>) {
        if self.recorded { return; }
        super::record_stream_diag_with_ctx(
            &self.g, &self.channel, &self.model, self.upstream_model.clone(),
            self.route_group.clone(), self.failover_count, self.masked_count, &self.protocol,
            &self.transforms, self.started, self.first_byte_ms, stats, error,
        );
        self.disarm();
    }
}
impl Drop for DiagnosticGuard {
    fn drop(&mut self) {
        if self.recorded { return; }
        self.recorded = true;
        let stats = &self.stats;
        self.g.record_diag(super::GatewayDiagEntry {
            id: 0, timestamp: super::now_unix(), channel_id: self.channel.channel_id.clone(),
            channel_alias: self.channel.alias.clone(), source: self.channel.kind().as_str().into(),
            model: self.model.clone(), upstream_model: self.upstream_model.clone(),
            route_group: self.route_group.clone(), failover_count: self.failover_count,
            masked_count: self.masked_count, protocol: self.protocol.clone(),
            stream: self.stream, status: 499,
            latency_ms: self.started.elapsed().as_millis() as u64, first_byte_ms: self.first_byte_ms,
            frame_count: stats.frame_count, transforms: self.transforms.clone(),
            thinking_chars: stats.thinking_chars, content_chars: stats.content_chars, refusal_chars: stats.refusal_chars,
            tool_call_count: stats.tool_call_count, tool_call_chars: stats.tool_call_chars,
            finish_reason: stats.finish_reason.clone(), prompt_tokens: stats.prompt_tokens,
            cached_prompt_tokens: stats.cached_prompt_tokens, cache_creation_tokens: stats.cache_creation_tokens,
            completion_tokens: stats.completion_tokens, reasoning_tokens: stats.reasoning_tokens,
            estimated_cost_cny: None,
            diag_note: None, error: Some("请求已取消或客户端已断开".into()),
        });
    }
}

pub(super) struct ChatSse {
    chat: SseStreamNormalizer,
    responses: Option<ResponsesChatStream>,
}

impl ChatSse {
    pub fn new(
        model: &str,
        thinking_mode: ThinkingMode,
        wire: OpenAiWire,
        include_usage: bool,
    ) -> Self {
        Self {
            chat: SseStreamNormalizer::new(model, thinking_mode),
            responses: (wire == OpenAiWire::Responses)
                .then(|| ResponsesChatStream::new(model, include_usage)),
        }
    }

    pub fn with_expected_choices(mut self, expected: Option<u64>) -> Self {
        self.chat = self.chat.with_expected_choices(expected);
        self
    }

    pub fn with_sensitive_values(mut self, sensitive: Vec<String>) -> Self {
        self.chat = self.chat.with_sensitive_values(sensitive.clone());
        self.responses = self.responses.map(|s| s.with_sensitive_values(sensitive));
        self
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<String> {
        if let Some(responses) = self.responses.as_mut() {
            return responses
                .push_bytes(bytes)
                .into_iter()
                .flat_map(|frame| self.chat.push_value(frame))
                .collect();
        }
        self.chat.push_bytes(bytes)
    }

    pub fn finish(&mut self) -> Vec<String> {
        let mut frames = Vec::new();
        if let Some(responses) = self.responses.as_mut() {
            for frame in responses.finish() {
                frames.extend(self.chat.push_value(frame));
            }
        }
        frames.extend(self.chat.finish());
        frames
    }

    pub fn is_terminal(&self) -> bool {
        self.chat.is_terminal()
            || self
                .responses
                .as_ref()
                .is_some_and(ResponsesChatStream::is_terminal)
    }

    pub fn stats(&self) -> &StreamStats {
        &self.chat.stats
    }
}

pub(super) fn validate_chat_json(value: Value) -> Result<Value, String> {
    if value.get("error").is_some_and(|e| !e.is_null()) {
        let error = crate::gateway_proto::error::sanitize_upstream_error(&value);
        return Err(error["error"]["message"].as_str().unwrap_or("上游返回错误").into());
    }
    let choices = value.get("choices").and_then(Value::as_array).filter(|c| !c.is_empty())
        .ok_or("上游 JSON 不包含有效 choices")?;
    if choices.iter().any(|c| !c.get("message").is_some_and(Value::is_object)) {
        return Err("上游补全缺少 message 对象".into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn json_error_is_failure_but_tools_and_refusal_are_valid() {
        assert!(validate_chat_json(json!({"error":{"message":"fixture failed"}})).is_err());
        for message in [json!({"role":"assistant","content":null,"tool_calls":[]}), json!({"role":"assistant","refusal":"cannot comply"})] {
            assert!(validate_chat_json(json!({"choices":[{"message":message,"finish_reason":"stop"}]})).is_ok());
        }
        assert!(validate_chat_json(json!({"metadata":"not a completion"})).is_err());
    }
}

pub(super) async fn read_chat_json(
    response: reqwest::Response,
    wire: OpenAiWire,
    model: &str,
    sensitive: &[String],
) -> Result<Value, String> {
    let bytes = read_body_capped(response, sensitive).await?;
    let text = String::from_utf8_lossy(&bytes);
    let mut value = serde_json::from_str::<Value>(&text).map_err(|_| {
        let short: String = redact_known(&text, sensitive).chars().take(300).collect();
        format!("上游返回非 JSON 响应：{short}")
    })?;
    if value.get("error").is_some_and(|e| !e.is_null()) {
        redact_error_strings(&mut value, sensitive);
        let error = sanitize_upstream_error(&value);
        return Err(error["error"]["message"].as_str().unwrap_or("上游返回错误").into());
    }
    let value = match wire {
        OpenAiWire::ChatCompletions => value,
        OpenAiWire::Responses => {
            // Failure conversion may quote upstream text; redact before its own short summary.
            if matches!(value["status"].as_str(), Some("failed" | "cancelled")) {
                redact_error_strings(&mut value, sensitive);
            }
            to_chat_completion(&value, model).map_err(|e| redact_known(&e, sensitive))?
        }
    };
    validate_chat_json(value)
}
