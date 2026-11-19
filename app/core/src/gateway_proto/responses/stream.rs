use super::output::{chat_id, chat_usage, clean_error, finish_reason, function_call, message_part};
use crate::gateway_proto::{
    error::{openai_error_value, redact_error_strings},
    stream::{SseEventBuffer, SseLineReader, MAX_SSE_BUFFER_BYTES},
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

type Part = (u64, &'static str, u64);

struct ToolProgress {
    index: usize,
    arguments: String,
}

pub struct ResponsesChatStream {
    lines: SseLineReader,
    events: SseEventBuffer,
    model: String,
    id: String,
    created: u64,
    include_usage: bool,
    started: bool,
    terminal: bool,
    parts: BTreeMap<Part, usize>,
    tools: BTreeMap<u64, ToolProgress>,
    tool_buffer_bytes: usize,
    sensitive_values: Vec<String>,
}

impl ResponsesChatStream {
    pub fn new(model: impl Into<String>, include_usage: bool) -> Self {
        Self {
            lines: SseLineReader::default(),
            events: SseEventBuffer::default(),
            model: model.into(),
            id: chat_id(&Value::Null),
            created: 0,
            include_usage,
            started: false,
            terminal: false,
            parts: BTreeMap::new(),
            tools: BTreeMap::new(),
            tool_buffer_bytes: 0,
            sensitive_values: Vec::new(),
        }
    }

    pub fn with_sensitive_values(mut self, values: Vec<String>) -> Self {
        self.sensitive_values = values;
        self
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<Value> {
        if self.terminal {
            return Vec::new();
        }
        let mut out = Vec::new();
        for part in bytes.chunks(16 * 1024) {
            let lines = self.lines.feed_bytes(part);
            out.extend(self.read_lines(lines));
            if self.terminal {
                break;
            }
        }
        out
    }

    pub fn finish(&mut self) -> Vec<Value> {
        if self.terminal {
            return Vec::new();
        }
        let lines = self.lines.flush();
        let mut out = self.read_lines(lines);
        if let Some(event) = self.events.take_event() {
            self.event(&event, &mut out);
        }
        if !self.terminal {
            self.error(
                "上游 Responses 流未收到完成事件",
                "incomplete_stream",
                &mut out,
            );
        }
        out
    }

    fn read_lines(&mut self, lines: Vec<String>) -> Vec<Value> {
        let mut out = Vec::new();
        for line in lines {
            if let Some(event) = self.events.feed_line(&line) {
                self.event(&event, &mut out);
            }
            if self.events.limit_exceeded() {
                self.check_buffer_limit(&mut out);
            }
            if self.terminal {
                break;
            }
        }
        self.check_buffer_limit(&mut out);
        out
    }

    fn check_buffer_limit(&mut self, out: &mut Vec<Value>) {
        if !self.terminal && (self.lines.limit_exceeded() || self.events.limit_exceeded()) {
            self.error("上游 SSE 单行或单事件超过 8 MiB 缓冲限制", "stream_buffer_limit", out);
        }
    }

    fn reserve_tool_bytes(&mut self, added: usize, out: &mut Vec<Value>) -> bool {
        if added > MAX_SSE_BUFFER_BYTES.saturating_sub(self.tool_buffer_bytes) {
            self.error("上游 Responses 工具参数累计超过 8 MiB 缓冲限制", "stream_buffer_limit", out);
            return false;
        }
        self.tool_buffer_bytes += added;
        true
    }

    fn event(&mut self, payload: &str, out: &mut Vec<Value>) {
        if self.terminal {
            return;
        }
        let Ok(mut event) = serde_json::from_str::<Value>(payload) else {
            return;
        };
        // 必须在 clean_error/finish_reason 截断之前清洗完整错误值，正常 output 不改写。
        if event["type"].as_str() == Some("error") {
            redact_error_strings(&mut event, &self.sensitive_values);
        } else if let Some(error) = event.pointer_mut("/response/error") {
            redact_error_strings(error, &self.sensitive_values);
        }
        let response = &event["response"];
        let index = event["output_index"].as_u64().unwrap_or_default();
        let result = match event["type"].as_str() {
            Some("response.created" | "response.in_progress") => {
                self.read_metadata(response);
                Ok(())
            }
            Some(
                "response.output_text.delta"
                | "response.refusal.delta"
                | "response.reasoning_summary_text.delta"
                | "response.reasoning_text.delta",
            ) => {
                self.text_delta(&event, index, out);
                Ok(())
            }
            Some("response.output_item.added") if event["item"]["type"] == "function_call" => {
                self.tool_item(index, &event["item"], out)
            }
            Some("response.function_call_arguments.delta") => {
                self.tool_delta(index, event["delta"].as_str().unwrap_or_default(), out);
                Ok(())
            }
            Some("response.function_call_arguments.done") => {
                self.tool_final(index, event["arguments"].as_str().unwrap_or_default(), out)
            }
            Some("response.output_item.done") => self.final_item(index, &event["item"], out),
            Some("response.completed" | "response.incomplete") => self.complete(response, out),
            Some("response.failed") => {
                finish_reason(response, false).and(Err("上游 Responses 请求失败".into()))
            }
            Some("error") => {
                let message = event["message"]
                    .as_str()
                    .or(event["error"]["message"].as_str())
                    .unwrap_or("上游返回 Responses 流式错误");
                Err(clean_error(message))
            }
            _ => Ok(()),
        };
        if let Err(message) = result {
            self.error(&message, "responses_stream_error", out);
        }
    }

    fn read_metadata(&mut self, response: &Value) {
        if self.started {
            return;
        }
        if response["id"].is_string() {
            self.id = chat_id(response);
        }
        if let Some(created) = response["created_at"].as_u64() {
            self.created = created;
        }
    }

    fn text_delta(&mut self, event: &Value, index: u64, out: &mut Vec<Value>) {
        let (field, part_index) = match event["type"].as_str() {
            Some("response.output_text.delta") => ("content", "content_index"),
            Some("response.refusal.delta") => ("refusal", "content_index"),
            Some("response.reasoning_summary_text.delta") => ("reasoning_content", "summary_index"),
            _ => ("reasoning_content", "content_index"),
        };
        let part = (index, field, event[part_index].as_u64().unwrap_or_default());
        self.emit_text(part, event["delta"].as_str().unwrap_or_default(), out);
    }

    fn final_item(&mut self, index: u64, item: &Value, out: &mut Vec<Value>) -> Result<(), String> {
        match item["type"].as_str() {
            Some("function_call") => self.tool_item(index, item, out)?,
            Some("message") => {
                for (i, part) in item["content"].as_array().into_iter().flatten().enumerate() {
                    if let Some((field, text)) = message_part(part) {
                        self.final_text((index, field, i as u64), text, out);
                    }
                }
            }
            Some("reasoning") => {
                for (i, part) in item["summary"].as_array().into_iter().flatten().enumerate() {
                    if let Some(text) = part["text"].as_str() {
                        self.final_text((index, "reasoning_content", i as u64), text, out);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn final_text(&mut self, part: Part, text: &str, out: &mut Vec<Value>) {
        let sent = self.parts.get(&part).copied().unwrap_or_default();
        self.emit_text(part, text.get(sent..).unwrap_or_default(), out);
    }

    fn emit_text(&mut self, part: Part, text: &str, out: &mut Vec<Value>) {
        if !text.is_empty() {
            *self.parts.entry(part).or_default() += text.len();
            self.delta(json!({(part.1):text}), out);
        }
    }

    fn tool_item(&mut self, index: u64, item: &Value, out: &mut Vec<Value>) -> Result<(), String> {
        let mut call = function_call(item)?;
        let arguments = call["function"]["arguments"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if !self.tools.contains_key(&index) {
            if !self.reserve_tool_bytes(arguments.len(), out) {
                return Ok(());
            }
            let tool_index = self.tools.len();
            self.tools.insert(
                index,
                ToolProgress {
                    index: tool_index,
                    arguments,
                },
            );
            call["index"] = json!(tool_index);
            self.delta(json!({"tool_calls":[call]}), out);
            return Ok(());
        }
        // added 有时先发空参数，完成快照可以补上未通过 delta 提供的尾部。
        if !arguments.is_empty() {
            self.tool_final(index, &arguments, out)?;
        }
        Ok(())
    }

    fn tool_final(
        &mut self,
        index: u64,
        arguments: &str,
        out: &mut Vec<Value>,
    ) -> Result<(), String> {
        if let Some(tool) = self.tools.get(&index) {
            let tail = arguments
                .strip_prefix(&tool.arguments)
                .ok_or("上游 Responses 工具参数与已发送内容不一致")?;
            self.tool_delta(index, tail, out);
        }
        Ok(())
    }

    fn tool_delta(&mut self, index: u64, delta: &str, out: &mut Vec<Value>) {
        if delta.is_empty() {
            return;
        }
        if self.tools.contains_key(&index) {
            if !self.reserve_tool_bytes(delta.len(), out) {
                return;
            }
        }
        if let Some(tool) = self.tools.get_mut(&index) {
            tool.arguments.push_str(delta);
            let tool_index = tool.index;
            self.delta(
                json!({"tool_calls":[{"index":tool_index,"function":{"arguments":delta}}]}),
                out,
            );
        }
    }

    fn complete(&mut self, response: &Value, out: &mut Vec<Value>) -> Result<(), String> {
        self.read_metadata(response);
        for (index, item) in response["output"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            self.final_item(index as u64, item, out)?;
            if self.terminal {
                return Ok(());
            }
        }
        let reason = finish_reason(response, !self.tools.is_empty())?;
        self.start(out);
        out.push(self.chunk(json!([{"index":0,"delta":{},"finish_reason":reason}])));
        if self.include_usage {
            if let Some(usage) = chat_usage(&response["usage"]) {
                let mut chunk = self.chunk(json!([]));
                chunk["usage"] = usage;
                out.push(chunk);
            }
        }
        self.terminal = true;
        Ok(())
    }

    fn delta(&mut self, delta: Value, out: &mut Vec<Value>) {
        self.start(out);
        out.push(self.chunk(json!([{"index":0,"delta":delta,"finish_reason":null}])));
    }

    fn start(&mut self, out: &mut Vec<Value>) {
        if !self.started {
            self.started = true;
            out.push(
                self.chunk(json!([{"index":0,"delta":{"role":"assistant"},"finish_reason":null}])),
            );
        }
    }

    fn chunk(&self, choices: Value) -> Value {
        let mut chunk = json!({
            "id":self.id,"model":self.model,"created":self.created,
            "object":"chat.completion.chunk","choices":choices
        });
        if self.include_usage {
            chunk["usage"] = Value::Null;
        }
        chunk
    }

    fn error(&mut self, message: &str, code: &str, out: &mut Vec<Value>) {
        self.terminal = true;
        out.push(openai_error_value(
            message,
            "upstream_error",
            Some(code),
            None,
        ));
    }
}
