use serde_json::Value;
use tokenscope_core::gateway_proto::{
    error::redact_secrets,
    responses::{to_chat_completion, ResponsesChatStream},
    stream::{SseStreamNormalizer, StreamStats},
    OpenAiWire, ThinkingMode,
};

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

pub(super) async fn read_chat_json(
    response: reqwest::Response,
    wire: OpenAiWire,
    model: &str,
) -> Result<Value, String> {
    let text = response
        .text()
        .await
        .map_err(|e| redact_secrets(&format!("读取上游响应失败：{e}")))?;
    let value = serde_json::from_str::<Value>(&text).map_err(|_| {
        let short: String = redact_secrets(&text).chars().take(300).collect();
        format!("上游返回非 JSON 响应：{short}")
    })?;
    match wire {
        OpenAiWire::ChatCompletions => Ok(value),
        OpenAiWire::Responses => to_chat_completion(&value, model),
    }
}
