//! 上游 SSE 字节流解析、思维链归一与标准 OpenAI SSE 重写引擎。
//!
//! 关键能力：
//! - 跨任意 TCP/UTF-8 分片边界安全拼装 UTF-8 字符与 LF/CRLF/CR 行；
//! - 按空行聚合多行 `data:` 事件；
//! - 将任意上游帧（`reasoning` / `thinking` / `google.thought` / `<think>` 标签）
//!   统一重写为标准的 `delta.reasoning_content`；
//! - 自动检测未见 `finish_reason` 的异常断流并下发 `incomplete_stream` 错误帧；
//! - 在 `[DONE]` 或 EOF 前刷新 `<think>` 状态机残余缓冲。

use super::error::{openai_error_value, redact_error_strings, redact_known, sanitize_upstream_error};
use super::thinking::{count_tool_calls, normalize_stream_delta, ThinkTagSplitter};
use super::ThinkingMode;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// 单行、单事件与协议适配器保留缓冲的共同字节预算；不是整条流的总量限制。
pub const MAX_SSE_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// 流式归一化统计（供请求诊断日志与使用台账记录）。
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    pub frame_count: usize,
    pub thinking_chars: usize,
    pub content_chars: usize,
    pub refusal_chars: usize,
    pub tool_call_count: usize,
    pub tool_call_chars: usize,
    pub finish_reason: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub cached_prompt_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub saw_error: bool,
    pub completed_cleanly: bool,
    pub diagnostic_note: Option<String>,
}

/// 从 OpenAI / Anthropic / Gemini 兼容 `usage` 对象提取分项 token。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageDetails {
    pub prompt_tokens: Option<u64>,
    pub cached_prompt_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

pub fn extract_usage_details(usage: &Value) -> UsageDetails {
    let Some(obj) = usage.as_object() else {
        return UsageDetails::default();
    };
    let cached_prompt_tokens = obj
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| {
            obj.get("input_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_u64)
        })
        .or_else(|| obj.get("cache_read_input_tokens").and_then(Value::as_u64))
        .or_else(|| obj.get("cached_content_token_count").and_then(Value::as_u64));
    let cache_creation_tokens = obj
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64);
    let raw_prompt = obj
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            obj.get("input_tokens").and_then(Value::as_u64).map(|inp| {
                inp.saturating_add(cached_prompt_tokens.unwrap_or(0))
                    .saturating_add(cache_creation_tokens.unwrap_or(0))
            })
        });
    let completion_tokens = obj
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .or_else(|| obj.get("output_tokens").and_then(Value::as_u64));
    let reasoning_tokens = obj
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| {
            obj.get("output_tokens_details")
                .and_then(|d| d.get("reasoning_tokens"))
                .and_then(Value::as_u64)
        });
    UsageDetails {
        prompt_tokens: raw_prompt,
        cached_prompt_tokens,
        cache_creation_tokens,
        completion_tokens,
        reasoning_tokens,
    }
}

/// 跨分片 UTF-8 与 SSE 行读取器（支持 `\n`、`\r\n` 与单独 `\r`）。
#[derive(Debug, Default)]
pub struct SseLineReader {
    utf8_tail: Vec<u8>,
    line_buf: String,
    trailing_cr: bool,
    started: bool,
    limit_exceeded: bool,
}

impl SseLineReader {
    pub fn limit_exceeded(&self) -> bool {
        self.limit_exceeded
    }

    pub fn feed_bytes(&mut self, chunk: &[u8]) -> Vec<String> {
        if self.limit_exceeded {
            return Vec::new();
        }
        if chunk.len() > 16 * 1024 {
            let mut out = Vec::new();
            for part in chunk.chunks(16 * 1024) {
                out.extend(self.feed_bytes(part));
                if self.limit_exceeded {
                    break;
                }
            }
            return out;
        }
        let mut combined;
        let bytes: &[u8] = if self.utf8_tail.is_empty() {
            chunk
        } else {
            combined = Vec::with_capacity(self.utf8_tail.len() + chunk.len());
            combined.extend_from_slice(&self.utf8_tail);
            combined.extend_from_slice(chunk);
            self.utf8_tail.clear();
            &combined
        };

        let mut text = String::new();
        let mut cursor = 0usize;
        while cursor < bytes.len() {
            match std::str::from_utf8(&bytes[cursor..]) {
                Ok(valid) => {
                    text.push_str(valid);
                    cursor = bytes.len();
                }
                Err(err) => {
                    let valid_len = err.valid_up_to();
                    if valid_len > 0 {
                        let valid = std::str::from_utf8(&bytes[cursor..cursor + valid_len])
                            .unwrap_or("");
                        text.push_str(valid);
                        cursor += valid_len;
                    }
                    match err.error_len() {
                        Some(bad_len) => {
                            text.push('\u{FFFD}');
                            cursor += bad_len;
                        }
                        None => {
                            // 尾部是不完整的 UTF-8 多字节序列，留到下一分片
                            self.utf8_tail.extend_from_slice(&bytes[cursor..]);
                            break;
                        }
                    }
                }
            }
        }

        self.feed_str(&text)
    }

    pub fn feed_str(&mut self, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        if self.limit_exceeded {
            return out;
        }
        for ch in text.chars() {
            if !self.started {
                self.started = true;
                if ch == '\u{FEFF}' {
                    continue;
                }
            }
            if self.trailing_cr {
                self.trailing_cr = false;
                if ch == '\n' {
                    continue;
                }
            }
            match ch {
                '\n' => out.push(std::mem::take(&mut self.line_buf)),
                '\r' => {
                    out.push(std::mem::take(&mut self.line_buf));
                    self.trailing_cr = true;
                }
                _ => {
                    if self.line_buf.len() + ch.len_utf8() > MAX_SSE_BUFFER_BYTES {
                        self.limit_exceeded = true;
                        self.line_buf.clear();
                        self.utf8_tail.clear();
                        break;
                    }
                    self.line_buf.push(ch);
                }
            }
        }
        out
    }

    pub fn flush(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if self.limit_exceeded {
            return out;
        }
        if !self.utf8_tail.is_empty() {
            self.utf8_tail.clear();
            out.extend(self.feed_str("\u{FFFD}"));
        }
        self.trailing_cr = false;
        if !self.line_buf.is_empty() {
            out.push(std::mem::take(&mut self.line_buf));
        }
        out
    }
}

/// 按空行聚合 SSE `data:` 行。
#[derive(Debug, Default)]
pub struct SseEventBuffer {
    data_lines: Vec<String>,
    data_bytes: usize,
    limit_exceeded: bool,
}

impl SseEventBuffer {
    pub fn limit_exceeded(&self) -> bool {
        self.limit_exceeded
    }

    pub fn feed_line(&mut self, raw_line: &str) -> Option<String> {
        if self.limit_exceeded {
            return None;
        }
        let line = raw_line.trim_end();
        if line.is_empty() {
            return self.take_event();
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let payload = rest.strip_prefix(' ').unwrap_or(rest);
            if payload.trim() == "[DONE]" {
                self.data_lines.clear();
                self.data_bytes = 0;
                return Some("[DONE]".to_string());
            }
            let added = payload.len().saturating_add(usize::from(!self.data_lines.is_empty()));
            if added > MAX_SSE_BUFFER_BYTES.saturating_sub(self.data_bytes) {
                self.limit_exceeded = true;
                self.data_lines.clear();
                self.data_bytes = 0;
                return None;
            }
            self.data_bytes += added;
            self.data_lines.push(payload.to_string());
        }
        None
    }

    pub fn take_event(&mut self) -> Option<String> {
        if self.data_lines.is_empty() {
            return None;
        }
        let joined = self.data_lines.join("\n");
        self.data_lines.clear();
        self.data_bytes = 0;
        Some(joined)
    }
}

/// 标准 OpenAI SSE 流式重写器。
pub struct SseStreamNormalizer {
    requested_model: String,
    mode: ThinkingMode,
    lines: SseLineReader,
    events: SseEventBuffer,
    splitters: BTreeMap<u64, ThinkTagSplitter>,
    choice_finished: BTreeMap<u64, bool>,
    expected_choices: Option<u64>,
    sensitive_values: Vec<String>,
    done_emitted: bool,
    template_id: Option<String>,
    template_created: Option<u64>,
    non_sse_buf: String,
    pub stats: StreamStats,
}

impl SseStreamNormalizer {
    pub fn new(requested_model: impl Into<String>, mode: ThinkingMode) -> Self {
        Self {
            requested_model: requested_model.into(),
            mode,
            lines: SseLineReader::default(),
            events: SseEventBuffer::default(),
            splitters: BTreeMap::new(),
            choice_finished: BTreeMap::new(),
            expected_choices: None,
            sensitive_values: Vec::new(),
            done_emitted: false,
            template_id: None,
            template_created: None,
            non_sse_buf: String::new(),
            stats: StreamStats::default(),
        }
    }

    /// 已知请求 n 时要求恰好出现 0..n-1；未知时要求所有已观察到的 choice 完成。
    pub fn with_expected_choices(mut self, expected: Option<u64>) -> Self {
        self.expected_choices = expected;
        self
    }

    /// 请求凭据只用于错误与非 SSE 摘要清洗，正常模型增量不受影响。
    pub fn with_sensitive_values(mut self, values: Vec<String>) -> Self {
        self.sensitive_values = values;
        self
    }

    fn all_choices_finished(&self) -> bool {
        !self.choice_finished.is_empty()
            && self.choice_finished.values().all(|finished| *finished)
            && self.expected_choices.is_none_or(|n| {
                n > 0 && self.choice_finished.len() as u64 == n
                    && self.choice_finished.keys().all(|index| *index < n)
            })
    }

    /// 喂入上游响应字节块，输出可直接发往客户端的 SSE 文本块列表。
    pub fn push_bytes(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        if self.done_emitted {
            return out;
        }
        for part in chunk.chunks(16 * 1024) {
            for line in self.lines.feed_bytes(part) {
                self.handle_line(&line, &mut out);
                if self.done_emitted {
                    break;
                }
            }
            self.check_buffer_limit(&mut out);
            if self.done_emitted {
                break;
            }
        }
        out
    }

    /// 协议适配器可直接传入已解析的 Chat Completions 帧。
    pub fn push_value(&mut self, value: Value) -> Vec<String> {
        let mut out = Vec::new();
        self.handle_value(value, &mut out);
        out
    }

    pub fn is_terminal(&self) -> bool {
        self.done_emitted
    }

    /// 上游结束（EOF），刷新尾部事件、思维链状态机，并在必要时补 `[DONE]` 或断流错误帧。
    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if self.done_emitted {
            return out;
        }
        for line in self.lines.flush() {
            self.handle_line(&line, &mut out);
        }
        self.check_buffer_limit(&mut out);
        if let Some(ev) = self.events.take_event() {
            self.handle_event(&ev, &mut out);
        }

        if !self.done_emitted {
            self.flush_splitters(&mut out);
            if self.stats.saw_error {
                out.push("data: [DONE]\n\n".to_string());
                self.done_emitted = true;
            } else if self.all_choices_finished() {
                self.stats.completed_cleanly = true;
                out.push("data: [DONE]\n\n".to_string());
                self.done_emitted = true;
            } else if self.stats.frame_count > 0 {
                // 上游发送了半截内容但未给 finish_reason 就断开
                self.stats.saw_error = true;
                let err = openai_error_value(
                    "上游流式响应在中途中断（存在未完成或缺失的 choice）",
                    "upstream_incomplete_stream",
                    Some("incomplete_stream"),
                    None,
                );
                out.push(format!("data: {}\n\n", err));
                out.push("data: [DONE]\n\n".to_string());
                self.done_emitted = true;
            } else {
                // 未收到任何有效 SSE 帧（可能是 200 + 非 SSE 文本/JSON）
                self.stats.saw_error = true;
                let raw = self.non_sse_buf.trim();
                let msg = if raw.is_empty() {
                    "上游返回了空流（未收到任何数据）".to_string()
                } else {
                    let short: String = raw.chars().take(300).collect();
                    format!("上游返回非 SSE 响应：{short}")
                };
                let err = openai_error_value(&msg, "gateway_protocol", Some("gateway_protocol"), None);
                out.push(format!("data: {}\n\n", err));
                out.push("data: [DONE]\n\n".to_string());
                self.done_emitted = true;
            }
        }
        out
    }

    fn check_buffer_limit(&mut self, out: &mut Vec<String>) {
        if !self.done_emitted && (self.lines.limit_exceeded() || self.events.limit_exceeded()) {
            self.stats.saw_error = true;
            self.stats.completed_cleanly = false;
            self.stats.diagnostic_note = Some("上游 SSE 单行或单事件超过 8 MiB 缓冲限制".into());
            let error = openai_error_value(
                "上游 SSE 单行或单事件超过 8 MiB 缓冲限制",
                "gateway_protocol", Some("stream_buffer_limit"), None,
            );
            out.push(format!("data: {error}\n\n"));
            out.push("data: [DONE]\n\n".into());
            self.done_emitted = true;
        }
    }

    fn remember_non_sse(&mut self, text: &str) {
        let clean = redact_known(text, &self.sensitive_values);
        let text = clean.as_str();
        let remaining = 1024usize.saturating_sub(self.non_sse_buf.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.non_sse_buf.push_str(&text[..end]);
        if self.non_sse_buf.len() < 1024 {
            self.non_sse_buf.push('\n');
        }
    }

    fn handle_line(&mut self, line: &str, out: &mut Vec<String>) {
        if self.done_emitted {
            return;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with("data:") && !trimmed.starts_with(':') && !trimmed.starts_with("event:") && !trimmed.starts_with("id:") && !trimmed.starts_with("retry:") {
            self.remember_non_sse(trimmed);
        }
        if let Some(ev) = self.events.feed_line(line) {
            self.handle_event(&ev, out);
        }
        if self.events.limit_exceeded() {
            self.check_buffer_limit(out);
        }
    }

    fn handle_event(&mut self, payload: &str, out: &mut Vec<String>) {
        if self.done_emitted {
            return;
        }
        if payload == "[DONE]" {
            self.flush_splitters(out);
            if self.all_choices_finished() || self.stats.saw_error {
                self.stats.completed_cleanly = !self.stats.saw_error;
                out.push("data: [DONE]\n\n".to_string());
            } else if self.stats.frame_count > 0 {
                self.stats.saw_error = true;
                self.stats.diagnostic_note =
                    Some("上游流式已结束，但存在未完成或缺失的 choice".into());
                let err = openai_error_value(
                    "上游发送结束标记，但仍有未完成或缺失的 choice",
                    "upstream_incomplete_stream",
                    Some("incomplete_stream"),
                    None,
                );
                out.push(format!("data: {}\n\n", err));
                out.push("data: [DONE]\n\n".to_string());
            } else {
                // 0 个有效数据帧却收到 [DONE]（常见于代理空流或非 JSON 数据被跳过）
                self.stats.saw_error = true;
                let raw = self.non_sse_buf.trim();
                let msg = if raw.is_empty() {
                    "上游未返回任何有效数据帧（仅收到结束标记）".to_string()
                } else {
                    let short: String = raw.chars().take(300).collect();
                    format!("上游 SSE 数据未包含合法 JSON 帧：{short}")
                };
                self.stats.diagnostic_note = Some(msg.clone());
                let err = openai_error_value(
                    &msg,
                    "upstream_incomplete_stream",
                    Some("incomplete_stream"),
                    None,
                );
                out.push(format!("data: {}\n\n", err));
                out.push("data: [DONE]\n\n".to_string());
            }
            self.done_emitted = true;
            return;
        }

        let Ok(val) = serde_json::from_str::<Value>(payload) else {
            if !payload.trim().is_empty() {
                self.remember_non_sse(payload.trim());
            }
            return;
        };
        self.handle_value(val, out);
    }

    fn handle_value(&mut self, mut val: Value, out: &mut Vec<String>) {
        if self.done_emitted {
            return;
        }
        let Some(root) = val.as_object_mut() else {
            return;
        };

        if root.get("error").is_some_and(|e| !e.is_null()) {
            self.stats.saw_error = true;
            self.flush_splitters(out);
            let mut error = json!({"error": root.get("error").cloned().unwrap_or(Value::Null)});
            redact_error_strings(&mut error, &self.sensitive_values);
            out.push(format!("data: {}\n\n", sanitize_upstream_error(&error)));
            return;
        }

        self.stats.frame_count += 1;
        if self.template_id.is_none() {
            self.template_id = root.get("id").and_then(Value::as_str).map(str::to_string);
        }
        if self.template_created.is_none() {
            self.template_created = root.get("created").and_then(Value::as_u64);
        }
        root.insert("model".into(), Value::String(self.requested_model.clone()));

        if let Some(usage) = root.get("usage") {
            let d = extract_usage_details(usage);
            if let Some(p) = d.prompt_tokens {
                self.stats.prompt_tokens = Some(p);
            }
            if let Some(cp) = d.cached_prompt_tokens {
                self.stats.cached_prompt_tokens = Some(cp);
            }
            if let Some(cc) = d.cache_creation_tokens {
                self.stats.cache_creation_tokens = Some(cc);
            }
            if let Some(c) = d.completion_tokens {
                self.stats.completion_tokens = Some(c);
            }
            if let Some(r) = d.reasoning_tokens {
                self.stats.reasoning_tokens = Some(r);
            }
        }

        if root.get("choices").is_none() && root.get("candidates").is_some() {
            self.stats.diagnostic_note =
                Some("上游返回了原生 Gemini candidates 结构，而非 OpenAI choices".into());
        }

        if let Some(Value::Array(choices)) = root.get_mut("choices") {
            for choice in choices.iter_mut() {
                let Some(choice_obj) = choice.as_object_mut() else {
                    continue;
                };
                let idx = choice_obj.get("index").and_then(Value::as_u64).unwrap_or(0);
                self.choice_finished.entry(idx).or_insert(false);
                let finish = choice_obj.get("finish_reason").and_then(Value::as_str)
                    .filter(|s| !s.is_empty()).map(str::to_owned);
                if !choice_obj.contains_key("delta")
                    && choice_obj.get("message").is_some_and(Value::is_object)
                {
                    if let Some(msg) = choice_obj.remove("message") {
                        choice_obj.insert("delta".into(), msg);
                    }
                }
                if let Some(Value::Object(delta)) = choice_obj.get_mut("delta") {
                    let splitter = self.splitters.entry(idx).or_insert_with(ThinkTagSplitter::new);
                    let (tc, cc) = normalize_stream_delta(delta, self.mode, splitter);
                    let (tool_n, tool_c) = count_tool_calls(delta);
                    self.stats.thinking_chars += tc;
                    self.stats.content_chars += cc;
                    self.stats.refusal_chars += delta.get("refusal").and_then(Value::as_str)
                        .map_or(0, |text| text.chars().count());
                    self.stats.tool_call_count += tool_n;
                    self.stats.tool_call_chars += tool_c;
                }
                if let Some(fr) = finish {
                    // 同帧也可能带正文增量；先归一再合入尾部，不能在 finish 后继续发 delta。
                    let tail = self.splitters.entry(idx).or_insert_with(ThinkTagSplitter::new).finish();
                    if !tail.thinking.is_empty() || !tail.content.is_empty() {
                        let delta = choice_obj.entry("delta").or_insert_with(|| json!({}));
                        if !delta.is_object() {
                            *delta = json!({});
                        }
                        if let Some(delta) = delta.as_object_mut() {
                            for (field, text, count) in [
                                ("reasoning_content", tail.thinking, &mut self.stats.thinking_chars),
                                ("content", tail.content, &mut self.stats.content_chars),
                            ] {
                                if !text.is_empty() {
                                    *count += text.chars().count();
                                    let mut joined = delta.get(field).and_then(Value::as_str).unwrap_or_default().to_owned();
                                    joined.push_str(&text);
                                    delta.insert(field.into(), Value::String(joined));
                                }
                            }
                        }
                    }
                    self.choice_finished.insert(idx, true);
                    self.stats.finish_reason = Some(fr);
                }
            }
        }

        out.push(format!("data: {}\n\n", Value::Object(root.clone())));
    }

    fn flush_splitters(&mut self, out: &mut Vec<String>) {
        for (&idx, splitter) in &mut self.splitters {
            let tail = splitter.finish();
            if tail.thinking.is_empty() && tail.content.is_empty() {
                continue;
            }
            let mut delta = serde_json::Map::new();
            if !tail.thinking.is_empty() {
                self.stats.thinking_chars += tail.thinking.chars().count();
                delta.insert("reasoning_content".into(), Value::String(tail.thinking));
            }
            if !tail.content.is_empty() {
                self.stats.content_chars += tail.content.chars().count();
                delta.insert("content".into(), Value::String(tail.content));
            }
            let chunk = json!({
                "id": self.template_id.clone().unwrap_or_else(|| "chatcmpl-gw".into()),
                "object": "chat.completion.chunk",
                "created": self.template_created.unwrap_or(0),
                "model": self.requested_model,
                "choices": [{
                    "index": idx,
                    "delta": Value::Object(delta),
                    "finish_reason": Value::Null
                }]
            });
            out.push(format!("data: {chunk}\n\n"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizer_merges_split_utf8_think_tags_and_emits_reasoning_content() {
        let mut norm = SseStreamNormalizer::new("my/model", ThinkingMode::Auto);
        let sse = concat!(
            "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"<thi\"}}]}\r\n\r\n",
            "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"nk>推理中\"}}]}\r\n\r\n",
            "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"</think>\\n正文回复\"}}]}\r\n\r\n",
            "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\r\n\r\n",
            "data: [DONE]\r\n\r\n"
        );
        // 故意在多字节中文中间按奇数字节切分
        let raw = sse.as_bytes();
        let mut emitted = Vec::new();
        for chunk in raw.chunks(17) {
            emitted.extend(norm.push_bytes(chunk));
        }
        emitted.extend(norm.finish());

        let joined = emitted.join("");
        assert!(joined.contains("\"model\":\"my/model\""));
        assert!(joined.contains("\"reasoning_content\""));
        assert!(joined.contains("正文回复"));
        assert!(joined.ends_with("data: [DONE]\n\n"));
        assert!(norm.stats.completed_cleanly);
        assert_eq!(norm.stats.thinking_chars, 3);
        assert_eq!(norm.stats.content_chars, 4);
    }

    #[test]
    fn normalizer_flags_incomplete_stream_when_eof_without_finish_reason() {
        let mut norm = SseStreamNormalizer::new("g/m", ThinkingMode::Auto);
        let _ = norm.push_bytes(
            b"data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"half\"}}]}\n\n",
        );
        let tail = norm.finish().join("");
        assert!(tail.contains("incomplete_stream"));
        assert!(norm.stats.saw_error);
        assert!(!norm.stats.completed_cleanly);
    }

    #[test]
    fn normalizer_extracts_google_thought_and_preserves_tool_calls() {
        let mut norm = SseStreamNormalizer::new("ap/g4", ThinkingMode::Auto);
        let sse = concat!(
            "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"think step\",\"extra_content\":{\"google\":{\"thought\":true}}}}]}\n\n",
            "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mut out = norm.push_bytes(sse.as_bytes());
        out.extend(norm.finish());
        let text = out.join("");
        assert!(text.contains("\"reasoning_content\":\"think step\""));
        assert!(text.contains("\"tool_calls\""));
        assert!(text.contains("\"finish_reason\":\"tool_calls\""));
        assert!(norm.stats.completed_cleanly);
        assert_eq!(norm.stats.tool_call_count, 1);
        assert_eq!(norm.stats.tool_call_chars, 2);
        assert!(norm.is_terminal());
    }

    fn parsed_frames(out: &[String]) -> Vec<Value> {
        out.iter()
            .filter_map(|frame| frame.strip_prefix("data: "))
            .filter_map(|payload| serde_json::from_str(payload.trim()).ok())
            .collect()
    }

    #[test]
    fn all_observed_choices_must_finish_at_eof_and_done() {
        for done in [false, true] {
            let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto);
            norm.push_value(json!({"choices":[
                {"index":0,"delta":{"content":"done"},"finish_reason":"stop"},
                {"index":1,"delta":{"content":"unfinished"},"finish_reason":null}
            ]}));
            let out = if done {
                norm.push_bytes(b"data: [DONE]\n\n")
            } else {
                norm.finish()
            };
            assert!(out.join("").contains("incomplete_stream"), "done={done}");
            assert!(!norm.stats.completed_cleanly);
        }
        let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto);
        norm.push_value(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}));
        norm.push_value(json!({"choices":[{"index":2,"delta":{"content":"late"}}]}));
        assert!(norm.finish().concat().contains("incomplete_stream"));
    }

    #[test]
    fn separately_finished_choices_and_usage_only_tail_complete_cleanly() {
        let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto);
        for index in [1, 0] {
            norm.push_value(json!({"choices":[{"index":index,"delta":{},"finish_reason":"stop"}]}));
        }
        norm.push_value(json!({"choices":[],"usage":{"completion_tokens":7}}));
        assert_eq!(norm.finish().concat(), "data: [DONE]\n\n");
        assert!(norm.stats.completed_cleanly);
        assert_eq!(norm.stats.completion_tokens, Some(7));
    }

    #[test]
    fn terminal_delta_is_normalized_and_flushed_before_finish() {
        for fragments in [vec!["<think>推理</think>OK"], vec!["<thi", "nk>推理</think>OK"], vec!["<thi"]] {
            let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto);
            let mut out = Vec::new();
            for (i, text) in fragments.iter().enumerate() {
                let finish = (i + 1 == fragments.len()).then_some("stop");
                out.extend(norm.push_value(json!({"choices":[{
                    "index":0,"delta":{"content":text},"finish_reason":finish
                }]})));
            }
            out.extend(norm.finish());
            let frames = parsed_frames(&out);
            let content = frames.iter().filter_map(|f| f.pointer("/choices/0/delta/content").and_then(Value::as_str)).collect::<String>();
            assert_eq!(content, if fragments == vec!["<thi"] { "<thi" } else { "OK" });
            let finish_at = frames.iter().position(|f| f.pointer("/choices/0/finish_reason") == Some(&json!("stop"))).unwrap();
            assert!(frames[finish_at + 1..].iter().all(|f| f.get("choices").and_then(Value::as_array).is_none_or(Vec::is_empty)));
            assert!(norm.stats.completed_cleanly);
        }
    }

    #[test]
    fn partial_tag_tail_on_terminal_delta_stays_before_finish_for_each_choice() {
        let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto);
        let out = norm.push_value(json!({"choices":[
            {"index":0,"delta":{"content":"<think>x"},"finish_reason":"stop"},
            {"index":1,"delta":{"content":"<thi"},"finish_reason":"length"}
        ]}));
        let frames = parsed_frames(&out);
        for (index, field, expected) in [(0, "reasoning_content", "x"), (1, "content", "<thi")] {
            let choices = frames.iter().filter_map(|f| f["choices"].as_array()).flatten().filter(|c| c["index"] == index).collect::<Vec<_>>();
            let text = choices.iter().filter_map(|c| c["delta"][field].as_str()).collect::<String>();
            assert_eq!(text, expected);
            let finish_at = choices.iter().position(|c| c["finish_reason"].is_string()).unwrap();
            assert!(choices[finish_at + 1..].iter().all(|c| c["delta"].as_object().is_none_or(serde_json::Map::is_empty)));
        }
        assert_eq!(norm.finish().concat(), "data: [DONE]\n\n");
    }

    #[test]
    fn sse_bom_and_cr_delimiters_work_without_waiting_for_next_byte() {
        let mut reader = SseLineReader::default();
        let mut lines = Vec::new();
        for byte in "\u{feff}data: first\r\r".bytes() {
            lines.extend(reader.feed_bytes(&[byte]));
        }
        assert_eq!(lines, ["data: first", ""]);
        assert_eq!(reader.feed_bytes(b"\ndata: second\r"), ["data: second"]);
        assert_eq!(reader.feed_bytes(b"\n\r"), [""]);
        assert!(reader.feed_bytes(b"\n").is_empty());
        assert_eq!(reader.feed_bytes("\u{feff}data: later\n".as_bytes()), ["\u{feff}data: later"]);

        let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto);
        let input = "\u{feff}data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\r\rdata: [DONE]\r\r";
        let mut out = Vec::new();
        for byte in input.bytes() {
            out.extend(norm.push_bytes(&[byte]));
        }
        assert!(norm.is_terminal());
        assert!(norm.stats.completed_cleanly);
        assert!(out.concat().ends_with("data: [DONE]\n\n"));
    }

    #[test]
    fn oversized_line_and_event_terminate_before_eof() {
        let mib = "x".repeat(1024 * 1024);
        for multiline in [false, true] {
            let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto);
            let mut out = Vec::new();
            for _ in 0..9 {
                let bytes = if multiline { format!("data: {mib}\n") } else { mib.clone() };
                out.extend(norm.push_bytes(bytes.as_bytes()));
            }
            assert!(norm.is_terminal(), "multiline={multiline}");
            assert!(norm.stats.saw_error);
            assert!(out.concat().contains("stream_buffer_limit"));
            assert!(!norm.stats.completed_cleanly);
            assert!(norm.finish().is_empty());
        }
    }

    #[test]
    fn normal_text_stream_can_exceed_buffer_limit_in_total() {
        let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto);
        let text = "字".repeat(400_000);
        for _ in 0..8 {
            let bytes = format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"content":text}}]}));
            assert!(!norm.push_bytes(bytes.as_bytes()).is_empty());
            assert!(!norm.is_terminal());
        }
        norm.push_value(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}));
        norm.finish();
        assert!(norm.stats.completed_cleanly);
        assert_eq!(norm.stats.content_chars, 3_200_000);
    }

    #[test]
    fn expected_choice_count_requires_every_index_and_does_not_default_unknown_to_one() {
        for (expected, indices, clean) in [
            (Some(2), vec![0], false), (Some(2), vec![1, 0], true),
            (Some(2), vec![0, 2], false), (Some(1), vec![0, 1], false),
            (Some(0), vec![0], false), (None, vec![3, 7], true),
        ] {
            for done in [false, true] {
                let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto).with_expected_choices(expected);
                for index in &indices {
                    norm.push_value(json!({"choices":[{"index":index,"delta":{},"finish_reason":"stop"}]}));
                }
                let out = if done { norm.push_bytes(b"data: [DONE]\n\n") } else { norm.finish() };
                assert_eq!(norm.stats.completed_cleanly, clean, "expected={expected:?}, indices={indices:?}, done={done}");
                assert_eq!(out.concat().contains("incomplete_stream"), !clean);
            }
        }
    }

    #[test]
    fn refusal_characters_are_counted_separately_from_text_and_thinking_in_all_modes() {
        for mode in [ThinkingMode::Auto, ThinkingMode::FieldsOnly, ThinkingMode::Passthrough] {
            let mut norm = SseStreamNormalizer::new("m", mode);
            let frame = json!({"choices":[{"index":0,"delta":{"refusal":"拒绝🙂","content":"OK","reasoning_content":"想"},"finish_reason":"stop"}]});
            let frames = parsed_frames(&norm.push_value(frame));
            assert_eq!(frames[0]["choices"][0]["delta"]["refusal"], "拒绝🙂");
            norm.finish();
            assert_eq!(norm.stats.refusal_chars, 3);
            assert_eq!(norm.stats.content_chars, 2);
            assert_eq!(norm.stats.thinking_chars, 1);
        }
    }

    #[test]
    fn line_and_event_limits_are_exact_byte_budgets_and_latch_until_terminal() {
        let text = "🙂".repeat(MAX_SSE_BUFFER_BYTES / 4);
        let mut lines = SseLineReader::default();
        assert!(lines.feed_bytes(text.as_bytes()).is_empty());
        assert!(!lines.limit_exceeded());
        assert_eq!(lines.line_buf.len(), MAX_SSE_BUFFER_BYTES);
        assert!(lines.feed_bytes(b"x").is_empty());
        assert!(lines.limit_exceeded());
        assert!(lines.line_buf.is_empty());
        assert!(lines.flush().is_empty());
        assert!(lines.feed_bytes(b"data: [DONE]\n\n").is_empty());

        let mut events = SseEventBuffer::default();
        let half = "x".repeat(MAX_SSE_BUFFER_BYTES / 2);
        events.feed_line(&format!("data:{half}"));
        events.feed_line(&format!("data:{}", &half[1..]));
        assert!(!events.limit_exceeded());
        assert_eq!(events.data_bytes, MAX_SSE_BUFFER_BYTES);
        assert_eq!(events.take_event().unwrap().len(), MAX_SSE_BUFFER_BYTES);
        assert_eq!(events.data_bytes, 0);
        events.feed_line(&format!("data:{text}"));
        assert!(!events.limit_exceeded());
        events.feed_line("data:");
        assert!(events.limit_exceeded());
        assert!(events.data_lines.is_empty());
        assert!(events.take_event().is_none());
        assert!(events.feed_line("data: [DONE]").is_none());
    }

    #[test]
    fn large_network_chunks_keep_earlier_complete_events_before_a_later_limit_error() {
        let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto);
        let mut bytes = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"first\"}}]}\n\n".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', MAX_SSE_BUFFER_BYTES + 1));
        let frames = parsed_frames(&norm.push_bytes(&bytes));
        assert_eq!(frames.first().unwrap()["choices"][0]["delta"]["content"], "first");
        assert_eq!(frames.last().unwrap()["error"]["code"], "stream_buffer_limit");
        assert!(norm.is_terminal());
    }

    #[test]
    fn sensitive_values_are_redacted_before_error_sanitizing_and_preview_truncation() {
        let secret = format!("opaque_fixture_{}", "q".repeat(1400));
        let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto).with_sensitive_values(vec![secret.clone()]);
        let frame = json!({"error":{"message":secret,"code":secret,"param":secret,"private":secret}});
        let output = norm.push_value(frame).concat();
        assert!(!output.contains("opaque_fixture_"));
        assert!(output.contains("***"));
        assert!(!output.contains("private"));

        for bytes in [format!("{secret}\n"), format!("data: invalid {secret}\n\ndata: [DONE]\n\n")] {
            let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto).with_sensitive_values(vec![secret.clone()]);
            let mut output = norm.push_bytes(bytes.as_bytes()).concat();
            output.push_str(&norm.finish().concat());
            assert!(!output.contains("opaque_fixture_"));
            assert!(output.contains("***"));
        }

        let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto).with_sensitive_values(vec![secret.clone()]);
        let output = norm.push_value(json!({"choices":[{"index":0,"delta":{"content":secret},"finish_reason":"stop"}]})).concat();
        assert!(output.contains(&secret));
    }

    #[test]
    fn diagnostic_preview_never_grows_beyond_its_utf8_byte_budget() {
        let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto);
        norm.push_bytes(format!("{}\n", "字".repeat(2000)).as_bytes());
        norm.push_bytes(b"not sse\n");
        assert!(norm.non_sse_buf.len() <= 1024);
        assert!(norm.finish().concat().contains("gateway_protocol"));
    }

    #[test]
    fn normalizer_flags_empty_done_stream_without_frames_as_error() {
        let mut norm = SseStreamNormalizer::new("ap/g4", ThinkingMode::Auto);
        let out = norm.push_bytes(b"data: [DONE]\n\n").join("");
        assert!(norm.is_terminal());
        assert!(norm.stats.saw_error);
        assert!(!norm.stats.completed_cleanly);
        assert!(out.contains("incomplete_stream"));
        assert!(out.contains("data: [DONE]"));
    }

    #[test]
    fn normalizer_extracts_cached_and_reasoning_token_details() {
        let mut norm = SseStreamNormalizer::new("m", ThinkingMode::Auto);
        norm.push_value(json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}));
        norm.push_value(json!({
            "choices":[],
            "usage":{
                "prompt_tokens":1200,
                "completion_tokens":300,
                "prompt_tokens_details":{"cached_tokens":900},
                "cache_creation_input_tokens":50,
                "completion_tokens_details":{"reasoning_tokens":180}
            }
        }));
        norm.finish();
        assert_eq!(norm.stats.prompt_tokens, Some(1200));
        assert_eq!(norm.stats.cached_prompt_tokens, Some(900));
        assert_eq!(norm.stats.cache_creation_tokens, Some(50));
        assert_eq!(norm.stats.completion_tokens, Some(300));
        assert_eq!(norm.stats.reasoning_tokens, Some(180));
    }
}
