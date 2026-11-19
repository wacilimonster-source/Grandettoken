//! 思维链归一与 `<think>` 标签流式切分状态机。
//!
//! 目标：
//! 1. 将上游异构的思维链输出（`reasoning_content` / `reasoning` / `thinking` /
//!    `extra_content.google.thought` / 回复开头的 `<think>...</think>` 等标签）
//!    统一规整到 OpenAI 业界与 PocketTavern/SillyTavern 原生识别的 `reasoning_content` 字段；
//! 2. 保留 `extra_content.google.thought_signature` 供多轮工具调用使用；
//! 3. 跨 SSE 分片安全，绝不因分片把 `</think>` 拆开而漏出半截标签或误吞正常正文。

use super::ThinkingMode;
use serde_json::Value;

#[derive(Debug, Clone, Copy)]
struct TagTemplate {
    prefix: &'static str,
    suffix: &'static str,
}

const TEMPLATES: &[TagTemplate] = &[
    TagTemplate {
        prefix: "<think>",
        suffix: "</think>",
    },
    TagTemplate {
        prefix: "<thinking>",
        suffix: "</thinking>",
    },
    TagTemplate {
        prefix: "<thought>",
        suffix: "</thought>",
    },
    TagTemplate {
        prefix: "<|start|>assistant<|channel|>analysis<|message|>",
        suffix: "<|start|>assistant<|channel|>final<|message|>",
    },
];

const MAX_PREFIX_LEN: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
enum SplitState {
    Unknown,
    InThinking { suffix: &'static str },
    InContent { strip_leading_newlines: bool },
}

/// 增量思维链标签切分器（对齐 pockettavern `ReasoningSplitter` 状态机）。
#[derive(Debug, Clone)]
pub struct ThinkTagSplitter {
    state: SplitState,
    buf: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SplitOutput {
    pub thinking: String,
    pub content: String,
}

impl ThinkTagSplitter {
    pub fn new() -> Self {
        Self {
            state: SplitState::Unknown,
            buf: String::new(),
        }
    }

    /// 喂入一段正文增量，返回当前可安全发出的 `(thinking, content)`。
    pub fn push(&mut self, chunk: &str) -> SplitOutput {
        let mut out = SplitOutput::default();
        if chunk.is_empty() {
            return out;
        }
        if matches!(
            self.state,
            SplitState::InContent {
                strip_leading_newlines: false
            }
        ) {
            out.content.push_str(chunk);
            return out;
        }
        self.buf.push_str(chunk);
        self.pump(&mut out);
        out
    }

    /// 流结束时刷出剩余缓冲（防止极短回复或未闭合标签停留在缓冲里）。
    pub fn finish(&mut self) -> SplitOutput {
        let mut out = SplitOutput::default();
        if self.buf.is_empty() {
            return out;
        }
        match self.state {
            SplitState::Unknown => {
                // 全程未凑齐任何已知开头标签，当作普通正文放行
                out.content.push_str(&self.buf);
                self.buf.clear();
                self.state = SplitState::InContent {
                    strip_leading_newlines: false,
                };
            }
            SplitState::InThinking { .. } => {
                // 未闭合的思考标签：保留到 thinking 区（避免半截思维链吐进正文）
                out.thinking.push_str(&self.buf);
                self.buf.clear();
            }
            SplitState::InContent {
                strip_leading_newlines,
            } => {
                let s = if strip_leading_newlines {
                    self.buf.trim_start_matches(['\r', '\n'])
                } else {
                    self.buf.as_str()
                };
                out.content.push_str(s);
                self.buf.clear();
                self.state = SplitState::InContent {
                    strip_leading_newlines: false,
                };
            }
        }
        out
    }

    fn pump(&mut self, out: &mut SplitOutput) {
        if self.state == SplitState::Unknown {
            // 允许标签前有少量空白/换行（常见于某些兼容网关的首帧前导换行）
            let trimmed_start = self.buf.trim_start_matches([' ', '\t', '\r', '\n']);
            if trimmed_start.is_empty() {
                if self.buf.len() > 16 {
                    out.content.push_str(&self.buf);
                    self.buf.clear();
                    self.state = SplitState::InContent {
                        strip_leading_newlines: false,
                    };
                }
                return;
            }
            // 检查是否命中任一模板前缀，或仍是某个模板前缀的真前缀
            let mut could_match = false;
            let mut matched: Option<TagTemplate> = None;
            for tpl in TEMPLATES {
                if trimmed_start.starts_with(tpl.prefix) {
                    matched = Some(*tpl);
                    break;
                }
                if tpl.prefix.starts_with(trimmed_start) && trimmed_start.len() <= MAX_PREFIX_LEN {
                    could_match = true;
                }
            }
            if let Some(tpl) = matched {
                let rest = trimmed_start[tpl.prefix.len()..].to_string();
                let rest_cleaned = rest.trim_start_matches(['\r', '\n']).to_string();
                self.buf = rest_cleaned;
                self.state = SplitState::InThinking { suffix: tpl.suffix };
            } else if could_match {
                return;
            } else {
                out.content.push_str(&self.buf);
                self.buf.clear();
                self.state = SplitState::InContent {
                    strip_leading_newlines: false,
                };
                return;
            }
        }

        if let SplitState::InThinking { suffix } = self.state {
            if let Some(pos) = self.buf.find(suffix) {
                let thought = self.buf[..pos].trim_end_matches(['\r', '\n']);
                out.thinking.push_str(thought);
                let after = self.buf[pos + suffix.len()..].to_string();
                self.buf = after;
                self.state = SplitState::InContent {
                    strip_leading_newlines: true,
                };
            } else {
                // 尾部留出最多 suffix.len()-1 字节（按 UTF-8 字符边界回退），防止跨分片切断闭合标签
                let keep_target = suffix.len().saturating_sub(1);
                if self.buf.len() > keep_target {
                    let mut split_at = self.buf.len() - keep_target;
                    while split_at > 0 && !self.buf.is_char_boundary(split_at) {
                        split_at -= 1;
                    }
                    if split_at > 0 {
                        out.thinking.push_str(&self.buf[..split_at]);
                        self.buf.drain(..split_at);
                    }
                }
                return;
            }
        }

        if let SplitState::InContent {
            strip_leading_newlines,
        } = self.state
        {
            if strip_leading_newlines {
                let trimmed = self.buf.trim_start_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    // 仅剩换行时暂存；避免过度缓冲，超过 8 字节就放行
                    if self.buf.len() > 8 {
                        out.content.push_str(&self.buf);
                        self.buf.clear();
                        self.state = SplitState::InContent {
                            strip_leading_newlines: false,
                        };
                    }
                    return;
                }
                out.content.push_str(trimmed);
                self.buf.clear();
                self.state = SplitState::InContent {
                    strip_leading_newlines: false,
                };
            } else if !self.buf.is_empty() {
                out.content.push_str(&self.buf);
                self.buf.clear();
            }
        }
    }
}

/// 一次性切分完整文本（非流式响应使用）。
pub fn split_think_tags_complete(text: &str) -> SplitOutput {
    let mut splitter = ThinkTagSplitter::new();
    let mut out = splitter.push(text);
    let tail = splitter.finish();
    out.thinking.push_str(&tail.thinking);
    out.content.push_str(&tail.content);
    out
}

/// 检查 `extra_content.google.thought == true`（或直接 `thought == true`）；若为真则返回 true，并清理 `thought` 标记同时保留 `thought_signature`。
fn consume_google_thought_flag(container: &mut serde_json::Map<String, Value>) -> bool {
    let mut is_thought = false;
    if let Some(Value::Object(extra)) = container.get_mut("extra_content") {
        if let Some(Value::Object(google)) = extra.get_mut("google") {
            if google.get("thought").and_then(Value::as_bool) == Some(true) {
                is_thought = true;
                google.remove("thought");
            }
        }
    }
    if container.get("thought").and_then(Value::as_bool) == Some(true) {
        is_thought = true;
        container.remove("thought");
    }
    is_thought
}

/// 归并 `reasoning_content` / `reasoning` / `thinking` 到标准 `reasoning_content`。
fn collect_existing_reasoning(obj: &mut serde_json::Map<String, Value>) -> String {
    let mut out = String::new();
    if let Some(s) = obj.get("reasoning_content").and_then(Value::as_str) {
        out.push_str(s);
    }
    if let Some(v) = obj.remove("reasoning") {
        if let Some(s) = v.as_str() {
            if out.is_empty() {
                out.push_str(s);
            }
        }
    }
    if let Some(v) = obj.remove("thinking") {
        if let Some(s) = v.as_str() {
            if out.is_empty() {
                out.push_str(s);
            }
        }
    }
    out
}

/// 提取 `content` 文本（兼容字符串或 OpenAI 多段内容数组）。
fn extract_text_from_value(val: &Value) -> Option<String> {
    if let Some(s) = val.as_str() {
        return Some(s.to_string());
    }
    let arr = val.as_array()?;
    let mut joined = String::new();
    for part in arr {
        if let Some(t) = part.get("text").and_then(Value::as_str) {
            joined.push_str(t);
        }
    }
    (!joined.is_empty()).then_some(joined)
}

/// 统计 `delta` / `message` 中的 `tool_calls` 次数与参数字符数。
pub fn count_tool_calls(container: &serde_json::Map<String, Value>) -> (usize, usize) {
    let Some(arr) = container.get("tool_calls").and_then(Value::as_array) else {
        return (0, 0);
    };
    let mut count = 0usize;
    let mut chars = 0usize;
    for call in arr {
        let has_id = call
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        let name_nonempty = call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        if has_id || name_nonempty {
            count += 1;
        }
        if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
            chars += args.chars().count();
        }
    }
    (count, chars)
}

/// 统计非流式响应中所有 `choices[*].message.tool_calls` 的次数与参数字符数。
pub fn summarize_non_stream_tools(value: &Value) -> (usize, usize) {
    let Some(choices) = value.get("choices").and_then(Value::as_array) else {
        return (0, 0);
    };
    let mut total_count = 0usize;
    let mut total_chars = 0usize;
    for choice in choices {
        if let Some(msg) = choice.get("message").and_then(Value::as_object) {
            let (c, ch) = count_tool_calls(msg);
            total_count += c;
            total_chars += ch;
        }
    }
    (total_count, total_chars)
}

/// 归一化单条流式 delta 对象（返回 `(thinking_chars, content_chars)` 统计）。
pub fn normalize_stream_delta(
    delta: &mut serde_json::Map<String, Value>,
    mode: ThinkingMode,
    splitter: &mut ThinkTagSplitter,
) -> (usize, usize) {
    if mode == ThinkingMode::Passthrough {
        let tc = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str)
            .map(|s| s.chars().count())
            .unwrap_or(0);
        let cc = delta
            .get("content")
            .and_then(extract_text_from_value)
            .map(|s| s.chars().count())
            .unwrap_or(0);
        return (tc, cc);
    }

    let mut reasoning = collect_existing_reasoning(delta);
    let google_thought = consume_google_thought_flag(delta);
    let raw_content = delta.get("content").and_then(extract_text_from_value);

    if google_thought {
        if let Some(c) = raw_content {
            reasoning.push_str(&c);
            delta.remove("content");
        }
    } else if mode == ThinkingMode::Auto && reasoning.is_empty() {
        if let Some(c) = raw_content {
            let split = splitter.push(&c);
            if !split.thinking.is_empty() {
                reasoning.push_str(&split.thinking);
            }
            if split.content.is_empty() {
                delta.remove("content");
            } else {
                delta.insert("content".into(), Value::String(split.content));
            }
        }
    }

    if !reasoning.is_empty() {
        delta.insert("reasoning_content".into(), Value::String(reasoning.clone()));
    } else {
        delta.remove("reasoning_content");
    }

    let tc = reasoning.chars().count();
    let cc = delta
        .get("content")
        .and_then(extract_text_from_value)
        .map(|s| s.chars().count())
        .unwrap_or(0);
    (tc, cc)
}

/// 归一化非流式响应体，返回 `(thinking_chars, content_chars)`。
pub fn normalize_non_stream_response(
    value: &mut Value,
    requested_model: &str,
    mode: ThinkingMode,
) -> (usize, usize) {
    let mut thinking_chars = 0usize;
    let mut content_chars = 0usize;

    let Some(root) = value.as_object_mut() else {
        return (0, 0);
    };
    root.insert("model".into(), Value::String(requested_model.to_string()));

    let Some(Value::Array(choices)) = root.get_mut("choices") else {
        return (0, 0);
    };
    for choice in choices.iter_mut() {
        let Some(Value::Object(msg)) = choice.as_object_mut().and_then(|c| c.get_mut("message")) else {
            continue;
        };
        if mode == ThinkingMode::Passthrough {
            thinking_chars += msg
                .get("reasoning_content")
                .or_else(|| msg.get("reasoning"))
                .and_then(Value::as_str)
                .map(|s| s.chars().count())
                .unwrap_or(0);
            content_chars += msg
                .get("content")
                .and_then(extract_text_from_value)
                .map(|s| s.chars().count())
                .unwrap_or(0);
            continue;
        }

        let mut reasoning = collect_existing_reasoning(msg);
        let google_thought = consume_google_thought_flag(msg);
        if google_thought {
            if let Some(c) = msg.get("content").and_then(extract_text_from_value) {
                reasoning.push_str(&c);
                msg.insert("content".into(), Value::String(String::new()));
            }
        } else if mode == ThinkingMode::Auto && reasoning.is_empty() {
            if let Some(c) = msg.get("content").and_then(extract_text_from_value) {
                let split = split_think_tags_complete(&c);
                if !split.thinking.is_empty() || split.content != c {
                    reasoning = split.thinking;
                    msg.insert("content".into(), Value::String(split.content));
                }
            }
        }

        if !reasoning.is_empty() {
            thinking_chars += reasoning.chars().count();
            msg.insert("reasoning_content".into(), Value::String(reasoning));
        }
        content_chars += msg
            .get("content")
            .and_then(extract_text_from_value)
            .map(|s| s.chars().count())
            .unwrap_or(0);
    }

    (thinking_chars, content_chars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn splitter_handles_fragmented_think_tags_and_utf8() {
        let mut s = ThinkTagSplitter::new();
        let parts = ["<thi", "nk>\n先思考中文", "内容</thi", "nk>\n\n最终回复正文"];
        let mut thinking = String::new();
        let mut content = String::new();
        for p in parts {
            let out = s.push(p);
            thinking.push_str(&out.thinking);
            content.push_str(&out.content);
        }
        let tail = s.finish();
        thinking.push_str(&tail.thinking);
        content.push_str(&tail.content);
        assert_eq!(thinking, "先思考中文内容");
        assert_eq!(content, "最终回复正文");
    }

    #[test]
    fn splitter_passes_through_normal_text_without_tag() {
        let mut s = ThinkTagSplitter::new();
        let mut content = String::new();
        let mut thinking = String::new();
        for p in ["你好，", "这里是普通", "回复<notathink>"] {
            let out = s.push(p);
            thinking.push_str(&out.thinking);
            content.push_str(&out.content);
        }
        let tail = s.finish();
        thinking.push_str(&tail.thinking);
        content.push_str(&tail.content);
        assert!(thinking.is_empty());
        assert_eq!(content, "你好，这里是普通回复<notathink>");
    }

    #[test]
    fn google_thought_delta_moves_content_to_reasoning_and_keeps_signature() {
        let mut splitter = ThinkTagSplitter::new();
        let mut d = json!({
            "content": "思考摘要片段",
            "extra_content": { "google": { "thought": true, "thought_signature": "sig_1" } }
        });
        let (tc, cc) = normalize_stream_delta(
            d.as_object_mut().unwrap(),
            ThinkingMode::Auto,
            &mut splitter,
        );
        assert_eq!(tc, 6);
        assert_eq!(cc, 0);
        assert_eq!(d["reasoning_content"], "思考摘要片段");
        assert!(d.get("content").is_none());
        assert_eq!(d["extra_content"]["google"]["thought_signature"], "sig_1");
        assert!(d["extra_content"]["google"].get("thought").is_none());
    }

    #[test]
    fn empty_think_tags_match_stream_and_nonstream_without_changing_other_modes() {
        for tag in ["think", "thinking", "thought"] {
            for body in ["", "\n\n"] {
                let text = format!("<{tag}>{body}</{tag}>\r\nOK");
                for mode in [ThinkingMode::Auto, ThinkingMode::FieldsOnly, ThinkingMode::Passthrough] {
                    let mut response = json!({"choices":[{"message":{"content":text}}]});
                    let counts = normalize_non_stream_response(&mut response, "m", mode);
                    let mut splitter = ThinkTagSplitter::new();
                    let mut delta = json!({"content":text});
                    normalize_stream_delta(delta.as_object_mut().unwrap(), mode, &mut splitter);
                    let tail = splitter.finish();
                    let stream_content = format!("{}{}", delta["content"].as_str().unwrap_or_default(), tail.content);
                    assert_eq!(response["choices"][0]["message"]["content"], stream_content, "tag={tag}, mode={mode:?}");
                    if mode == ThinkingMode::Auto {
                        assert_eq!(stream_content, "OK");
                        assert_eq!(counts, (0, 2));
                    } else {
                        assert_eq!(stream_content, text);
                    }
                    assert!(response["choices"][0]["message"].get("reasoning_content").is_none());
                }
            }
        }
        let text = "<think></think>";
        let mut response = json!({"choices":[{"message":{"content":text}}]});
        assert_eq!(normalize_non_stream_response(&mut response, "m", ThinkingMode::Auto), (0, 0));
        assert_eq!(response["choices"][0]["message"]["content"], "");
    }

    #[test]
    fn non_stream_response_normalizes_think_tags_and_fields() {
        let mut resp = json!({
            "id": "1",
            "model": "deepseek-reasoner",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "<think>步骤一</think>\n答案" },
                "finish_reason": "stop"
            }]
        });
        let (tc, cc) = normalize_non_stream_response(&mut resp, "ds/r1", ThinkingMode::Auto);
        assert_eq!(resp["model"], "ds/r1");
        assert_eq!(resp["choices"][0]["message"]["reasoning_content"], "步骤一");
        assert_eq!(resp["choices"][0]["message"]["content"], "答案");
        assert_eq!(tc, 3);
        assert_eq!(cc, 2);
    }
}
