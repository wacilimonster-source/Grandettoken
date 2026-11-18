use crate::gateway_proto::error::redact_secrets;
use serde_json::{json, Value};

pub fn to_chat_completion(response: &Value, model: &str) -> Result<Value, String> {
    let mut content = String::new();
    let mut refusal = String::new();
    let mut reasoning = String::new();
    let mut calls = Vec::new();
    for item in response["output"].as_array().into_iter().flatten() {
        match item["type"].as_str() {
            Some("message") => {
                for part in item["content"].as_array().into_iter().flatten() {
                    if let Some((field, text)) = message_part(part) {
                        if field == "content" {
                            content.push_str(text);
                        } else {
                            refusal.push_str(text);
                        }
                    }
                }
            }
            Some("reasoning") => {
                for part in item["summary"].as_array().into_iter().flatten() {
                    if let Some(text) = part["text"].as_str() {
                        reasoning.push_str(text);
                    }
                }
            }
            Some("function_call") => calls.push(function_call(item)?),
            _ => {}
        }
    }
    let reason = finish_reason(response, !calls.is_empty())?;
    let mut message =
        json!({"role":"assistant","content":(!content.is_empty()).then_some(content)});
    if !refusal.is_empty() {
        message["refusal"] = json!(refusal);
    }
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }
    let mut result = json!({
        "id":chat_id(response),
        "created":response["created_at"].as_u64().unwrap_or_default(),
        "object":"chat.completion",
        "model":model,
        "choices":[{"index":0,"message":message,"finish_reason":reason}]
    });
    if let Some(usage) = chat_usage(&response["usage"]) {
        result["usage"] = usage;
    }
    Ok(result)
}

pub(super) fn message_part(part: &Value) -> Option<(&'static str, &str)> {
    match part["type"].as_str()? {
        "output_text" => Some(("content", part["text"].as_str()?)),
        "refusal" => Some(("refusal", part["refusal"].as_str()?)),
        _ => None,
    }
}

pub(super) fn function_call(item: &Value) -> Result<Value, String> {
    let id = item["call_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("上游 Responses 工具调用缺少 call_id")?;
    let name = item["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("上游 Responses 工具调用缺少 name")?;
    let args = item["arguments"].as_str().unwrap_or_default();
    Ok(json!({"id":id,"type":"function","function":{"name":name,"arguments":args}}))
}

pub(super) fn chat_id(response: &Value) -> String {
    response["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|id| format!("chatcmpl-{id}"))
        .unwrap_or_else(|| "chatcmpl-responses".into())
}

pub(super) fn chat_usage(usage: &Value) -> Option<Value> {
    let input = usage["input_tokens"].as_u64()?;
    let output = usage["output_tokens"].as_u64()?;
    let total = usage["total_tokens"]
        .as_u64()
        .unwrap_or_else(|| input.saturating_add(output));
    let mut value =
        json!({"prompt_tokens":input, "completion_tokens":output, "total_tokens":total});
    if let Some(cached) = usage["input_tokens_details"]["cached_tokens"].as_u64() {
        value["prompt_tokens_details"] = json!({"cached_tokens":cached});
    }
    if let Some(reasoning) = usage["output_tokens_details"]["reasoning_tokens"].as_u64() {
        value["completion_tokens_details"] = json!({"reasoning_tokens":reasoning});
    }
    Some(value)
}

pub(super) fn finish_reason(response: &Value, has_tools: bool) -> Result<&'static str, String> {
    if let Some(message) = response["error"]["message"].as_str() {
        return Err(clean_error(message));
    }
    match response["status"].as_str() {
        Some("completed") if has_tools => Ok("tool_calls"),
        Some("completed") => Ok("stop"),
        Some("incomplete") if response["incomplete_details"]["reason"] == "max_output_tokens" => {
            Ok("length")
        }
        Some("incomplete") if response["incomplete_details"]["reason"] == "content_filter" => {
            Ok("content_filter")
        }
        Some("incomplete") => Err("上游 Responses 未完成".into()),
        Some("failed" | "cancelled") => Err("上游 Responses 请求失败".into()),
        _ => Err("上游未返回 Responses 完成状态".into()),
    }
}

pub(super) fn clean_error(message: &str) -> String {
    redact_secrets(message).chars().take(400).collect()
}
