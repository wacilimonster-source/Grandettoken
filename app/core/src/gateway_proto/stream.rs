//! 上游 SSE 字节流解析、思维链归一与标准 OpenAI SSE 重写引擎。
//!
//! 关键能力：
//! - 跨任意 TCP/UTF-8 分片边界安全拼装 UTF-8 字符与 LF/CRLF/CR 行；
//! - 按空行聚合多行 `data:` 事件；
//! - 将任意上游帧（`reasoning` / `thinking` / `google.thought` / `<think>` 标签）
//!   统一重写为标准的 `delta.reasoning_content`；
//! - 自动检测未见 `finish_reason` 的异常断流并下发 `incomplete_stream` 错误帧；
//! - 在 `[DONE]` 或 EOF 前刷新 `<think>` 状态机残余缓冲。

use super::error::openai_error_value;
use super::thinking::{count_tool_calls, normalize_stream_delta, ThinkTagSplitter};
use super::ThinkingMode;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// 流式归一化统计（供请求诊断日志记录）。
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    pub frame_count: usize,
    pub thinking_chars: usize,
    pub content_chars: usize,
    pub tool_call_count: usize,
    pub tool_call_chars: usize,
    pub finish_reason: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub saw_error: bool,
    pub completed_cleanly: bool,
    pub diagnostic_note: Option<String>,
}

/// 跨分片 UTF-8 与 SSE 行读取器（支持 `\n`、`\r\n` 与单独 `\r`）。
#[derive(Debug, Default)]
pub struct SseLineReader {
    utf8_tail: Vec<u8>,
    line_buf: String,
    trailing_cr: bool,
}

impl SseLineReader {
    pub fn feed_bytes(&mut self, chunk: &[u8]) -> Vec<String> {
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
        for ch in text.chars() {
            if self.trailing_cr {
                self.trailing_cr = false;
                out.push(std::mem::take(&mut self.line_buf));
                if ch == '\n' {
                    continue;
                }
            }
            match ch {
                '\n' => out.push(std::mem::take(&mut self.line_buf)),
                '\r' => self.trailing_cr = true,
                _ => self.line_buf.push(ch),
            }
        }
        out
    }

    pub fn flush(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.utf8_tail.is_empty() {
            self.line_buf.push('\u{FFFD}');
            self.utf8_tail.clear();
        }
        if self.trailing_cr {
            self.trailing_cr = false;
            out.push(std::mem::take(&mut self.line_buf));
            return out;
        }
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
}

impl SseEventBuffer {
    pub fn feed_line(&mut self, raw_line: &str) -> Option<String> {
        let line = raw_line.trim_end();
        if line.is_empty() {
            return self.take_event();
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let payload = rest.strip_prefix(' ').unwrap_or(rest);
            if payload.trim() == "[DONE]" {
                self.data_lines.clear();
                return Some("[DONE]".to_string());
            }
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
    finish_seen: bool,
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
            finish_seen: false,
            done_emitted: false,
            template_id: None,
            template_created: None,
            non_sse_buf: String::new(),
            stats: StreamStats::default(),
        }
    }

    /// 喂入上游响应字节块，输出可直接发往客户端的 SSE 文本块列表。
    pub fn push_bytes(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for line in self.lines.feed_bytes(chunk) {
            self.handle_line(&line, &mut out);
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
        for line in self.lines.flush() {
            self.handle_line(&line, &mut out);
        }
        if let Some(ev) = self.events.take_event() {
            self.handle_event(&ev, &mut out);
        }

        if !self.done_emitted {
            self.flush_splitters(&mut out);
            if self.stats.saw_error {
                out.push("data: [DONE]\n\n".to_string());
                self.done_emitted = true;
            } else if self.finish_seen {
                self.stats.completed_cleanly = true;
                out.push("data: [DONE]\n\n".to_string());
                self.done_emitted = true;
            } else if self.stats.frame_count > 0 {
                // 上游发送了半截内容但未给 finish_reason 就断开
                self.stats.saw_error = true;
                let err = openai_error_value(
                    "上游流式响应在中途中断（未收到完成标志）",
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

    fn handle_line(&mut self, line: &str, out: &mut Vec<String>) {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with("data:") && !trimmed.starts_with(':') && !trimmed.starts_with("event:") && !trimmed.starts_with("id:") && !trimmed.starts_with("retry:") {
            if self.non_sse_buf.len() < 1024 {
                self.non_sse_buf.push_str(trimmed);
                self.non_sse_buf.push('\n');
            }
        }
        if let Some(ev) = self.events.feed_line(line) {
            self.handle_event(&ev, out);
        }
    }

    fn handle_event(&mut self, payload: &str, out: &mut Vec<String>) {
        if self.done_emitted {
            return;
        }
        if payload == "[DONE]" {
            self.flush_splitters(out);
            if self.finish_seen || self.stats.saw_error {
                self.stats.completed_cleanly = !self.stats.saw_error;
                out.push("data: [DONE]\n\n".to_string());
            } else if self.stats.frame_count > 0 {
                self.stats.saw_error = true;
                self.stats.diagnostic_note =
                    Some("上游流式只收到结束标记，未见 finish_reason".into());
                let err = openai_error_value(
                    "上游只发送了结束标记，没有合法完成帧",
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
            if !payload.trim().is_empty() && self.non_sse_buf.len() < 1024 {
                self.non_sse_buf.push_str(payload.trim());
                self.non_sse_buf.push('\n');
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
            out.push(format!("data: {}\n\n", Value::Object(root.clone())));
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

        if let Some(usage) = root.get("usage").and_then(Value::as_object) {
            if let Some(p) = usage.get("prompt_tokens").and_then(Value::as_u64) {
                self.stats.prompt_tokens = Some(p);
            }
            if let Some(c) = usage.get("completion_tokens").and_then(Value::as_u64) {
                self.stats.completion_tokens = Some(c);
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
                if let Some(fr) = choice_obj
                    .get("finish_reason")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    self.finish_seen = true;
                    self.stats.finish_reason = Some(fr.to_string());
                    // 遇到完成帧前先刷出可能尚在缓冲里的最后几字节正文/思考
                    let splitter = self.splitters.entry(idx).or_insert_with(ThinkTagSplitter::new);
                    let tail = splitter.finish();
                    if !tail.thinking.is_empty() || !tail.content.is_empty() {
                        let mut extra_delta = serde_json::Map::new();
                        if !tail.thinking.is_empty() {
                            self.stats.thinking_chars += tail.thinking.chars().count();
                            extra_delta
                                .insert("reasoning_content".into(), Value::String(tail.thinking));
                        }
                        if !tail.content.is_empty() {
                            self.stats.content_chars += tail.content.chars().count();
                            extra_delta.insert("content".into(), Value::String(tail.content));
                        }
                        let tail_chunk = json!({
                            "id": self.template_id.clone().unwrap_or_else(|| "chatcmpl-gw".into()),
                            "object": "chat.completion.chunk",
                            "created": self.template_created.unwrap_or(0),
                            "model": self.requested_model,
                            "choices": [{
                                "index": idx,
                                "delta": Value::Object(extra_delta),
                                "finish_reason": Value::Null
                            }]
                        });
                        out.push(format!("data: {tail_chunk}\n\n"));
                    }
                }
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
                    self.stats.tool_call_count += tool_n;
                    self.stats.tool_call_chars += tool_c;
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
}
