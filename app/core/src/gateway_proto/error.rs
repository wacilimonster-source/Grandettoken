//! 网关错误信封构造、上游错误解析与凭据脱敏。

use serde_json::{json, Value};

/// 构造 OpenAI 兼容错误 JSON 体。
pub fn openai_error_value(message: &str, err_type: &str, code: Option<&str>, param: Option<&str>) -> Value {
    let mut err = serde_json::Map::new();
    err.insert("message".into(), Value::String(redact_secrets(message)));
    err.insert("type".into(), Value::String(redact_secrets(err_type)));
    if let Some(c) = code.filter(|s| !s.is_empty()) {
        err.insert("code".into(), Value::String(redact_secrets(c)));
    }
    if let Some(p) = param.filter(|s| !s.is_empty()) {
        err.insert("param".into(), Value::String(redact_secrets(p)));
    }
    json!({ "error": Value::Object(err) })
}

/// 从上游 HTTP 错误体提取可读摘要与可选错误码，同时脱敏与截断（最多 400 字符）。
pub fn parse_upstream_error_summary(status: u16, body: &str) -> (String, Option<String>) {
    let mut msg = String::new();
    let mut code: Option<String> = None;

    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(err) = v.get("error") {
            if let Some(obj) = err.as_object() {
                msg = obj
                    .get("message")
                    .or_else(|| obj.get("msg"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                code = obj
                    .get("code")
                    .or_else(|| obj.get("status"))
                    .or_else(|| obj.get("type"))
                    .and_then(|c| c.as_str().map(str::to_string).or_else(|| c.as_i64().map(|n| n.to_string())));
            } else if let Some(s) = err.as_str() {
                msg = s.trim().to_string();
            }
        }
        if msg.is_empty() {
            if let Some(s) = v.get("message").and_then(Value::as_str) {
                msg = s.trim().to_string();
            }
        }
    }
    if msg.is_empty() {
        msg = body.trim().to_string();
    }
    if msg.is_empty() {
        msg = "上游服务拒绝请求，请检查密钥、模型权限或额度".to_string();
    }

    let clean = redact_secrets(&msg);
    let short: String = clean.chars().take(400).collect();
    (format!("上游返回 {status} · {short}"), code.map(|c| redact_secrets(&c).chars().take(120).collect()))
}

/// 错误出口仅保留协议字段，避免附加调试对象泄漏凭据。
pub fn sanitize_upstream_error(value: &Value) -> Value {
    let error = value.get("error").unwrap_or(value);
    let message = error.get("message").or_else(|| error.get("msg")).and_then(Value::as_str)
        .or_else(|| error.as_str()).unwrap_or("上游服务返回错误");
    let short: String = redact_secrets(message).chars().take(400).collect();
    let code = error.get("code").and_then(Value::as_str);
    let param = error.get("param").and_then(Value::as_str);
    openai_error_value(&short, "upstream_error", code, param)
}

/// 本次请求已知的敏感值只参与错误清洗，不修改正常模型输出。
pub fn redact_known(input: &str, sensitive: &[String]) -> String {
    let mut text = input.to_string();
    for value in sensitive.iter().filter(|v| !v.is_empty()) {
        text = text.replace(value, "***");
    }
    redact_secrets(&text)
}

pub fn redact_error_strings(value: &mut Value, sensitive: &[String]) {
    match value {
        Value::String(s) => *s = redact_known(s, sensitive),
        Value::Array(items) => items.iter_mut().for_each(|v| redact_error_strings(v, sensitive)),
        Value::Object(fields) => fields.values_mut().for_each(|v| redact_error_strings(v, sensitive)),
        _ => {}
    }
}

/// 剥离文本中可能被上游回显的密钥与令牌（对齐 pockettavern `_redactSensitive` 口径）。
pub fn redact_secrets(input: &str) -> String {
    let mut out = input.to_string();
    for prefix in ["Bearer ", "bearer ", "x-goog-api-key: ", "x-goog-api-key="] {
        out = mask_after_prefix(&out, prefix);
    }
    for token_prefix in ["sk-", "AIza", "gw_", "egw_"] {
        out = mask_token_prefix(&out, token_prefix);
    }
    out
}

fn mask_after_prefix(input: &str, prefix: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find(prefix) {
        result.push_str(&rest[..idx]);
        result.push_str(prefix);
        let after = &rest[idx + prefix.len()..];
        let tok_len = after
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | ')' | '}'))
            .unwrap_or(after.len());
        if tok_len > 0 {
            result.push_str("***");
        }
        rest = &after[tok_len..];
    }
    result.push_str(rest);
    result
}

fn mask_token_prefix(input: &str, prefix: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find(prefix) {
        let prev_is_ident = idx > 0
            && rest[..idx]
                .chars()
                .next_back()
                .map(|c| c.is_ascii_alphanumeric() || c == '_')
                .unwrap_or(false);
        result.push_str(&rest[..idx]);
        let after = &rest[idx + prefix.len()..];
        let body_len = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
            .unwrap_or(after.len());
        if !prev_is_ident && body_len >= 6 {
            result.push_str(prefix);
            result.push_str("***");
            rest = &after[body_len..];
        } else {
            result.push_str(prefix);
            rest = after;
        }
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_metadata_is_redacted_with_the_message() {
        let fake = ["sk-", "syntheticfixture123456789"].concat();
        let value = openai_error_value(&fake, &fake, Some(&fake), Some(&fake));
        assert!(!value.to_string().contains(&fake));
    }

    #[test]
    fn redact_secrets_masks_keys_without_corrupting_normal_text() {
        let raw = "Invalid key sk-1234567890abcdef and Bearer gw_abcdef123456 with AIzaSyDummySecretKey123";
        let masked = redact_secrets(raw);
        assert!(!masked.contains("1234567890abcdef"));
        assert!(!masked.contains("gw_abcdef123456"));
        assert!(!masked.contains("AIzaSyDummySecretKey123"));
        assert!(masked.contains("sk-***"));
        assert!(masked.contains("Bearer ***"));
        assert!(masked.contains("AIza***"));
    }

    #[test]
    fn parse_upstream_error_extracts_message_and_code() {
        let body = r#"{"error":{"message":"Quota exceeded for sk-abcdef123456","code":"RESOURCE_EXHAUSTED"}}"#;
        let (msg, code) = parse_upstream_error_summary(429, body);
        assert!(msg.contains("429"));
        assert!(msg.contains("sk-***"));
        assert!(!msg.contains("abcdef123456"));
        assert_eq!(code.as_deref(), Some("RESOURCE_EXHAUSTED"));
    }
}
