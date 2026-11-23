//! 可选 Sticky Prompt 脱敏与流式跨分片反脱敏（默认关闭）。
//!
//! 规则：
//! - 只在 `MaskingSettings::enabled == true` 时启用；
//! - 同一会话内同一个敏感串始终映射为同一占位符（如 `[MASKED_KEY_01]`），
//!   防止破坏 Prompt Cache 前缀与多轮上下文一致性；
//! - 映射表仅驻留内存（带 TTL 与容量上限），绝不写入数据库或排查日志。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const MASK_TTL_SECS: i64 = 3600;
pub const MAX_MASK_SESSIONS: usize = 1024;
const MAX_TAIL_HOLD_BYTES: usize = 36;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct MaskingSettings {
    pub enabled: bool,
    pub sticky: bool,
    pub mask_api_keys: bool,
    pub mask_emails: bool,
    pub mask_private_keys: bool,
    pub mask_passwords: bool,
    pub custom_patterns: Vec<String>,
}

impl Default for MaskingSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            sticky: true,
            mask_api_keys: true,
            mask_emails: true,
            mask_private_keys: true,
            mask_passwords: true,
            custom_patterns: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionMaskMap {
    pub forward: BTreeMap<String, String>,
    pub reverse: BTreeMap<String, String>,
    counters: BTreeMap<&'static str, usize>,
    pub last_used_at: i64,
}

impl SessionMaskMap {
    pub fn placeholder_for(&mut self, category: &'static str, secret: &str) -> String {
        if let Some(existing) = self.forward.get(secret) {
            return existing.clone();
        }
        let next = self.counters.entry(category).or_insert(0);
        *next += 1;
        let mut token = format!("[MASKED_{category}_{:02}]", *next);
        while self.reverse.contains_key(&token) {
            *next += 1;
            token = format!("[MASKED_{category}_{:02}]", *next);
        }
        self.forward.insert(secret.to_string(), token.clone());
        self.reverse.insert(token.clone(), secret.to_string());
        token
    }
}

#[derive(Debug, Default)]
pub struct StickyMaskStore {
    sessions: BTreeMap<String, SessionMaskMap>,
}

impl StickyMaskStore {
    pub fn prune(&mut self, now_ts: i64) {
        self.sessions
            .retain(|_, m| now_ts.saturating_sub(m.last_used_at) <= MASK_TTL_SECS);
        while self.sessions.len() >= MAX_MASK_SESSIONS {
            if let Some(oldest_key) = self
                .sessions
                .iter()
                .min_by_key(|(_, m)| m.last_used_at)
                .map(|(k, _)| k.clone())
            {
                self.sessions.remove(&oldest_key);
            } else {
                break;
            }
        }
    }

    pub fn get_or_create(&mut self, session_key: &str, now_ts: i64) -> &mut SessionMaskMap {
        if !self.sessions.contains_key(session_key) {
            self.prune(now_ts);
        }
        let entry = self.sessions.entry(session_key.to_string()).or_default();
        entry.last_used_at = now_ts;
        entry
    }
}

/// 检查一段文本是否已经是脱敏占位符，避免二次脱敏。
fn is_placeholder_at(s: &str, idx: usize) -> bool {
    s[idx..].starts_with("[MASKED_")
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_token_body_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

fn mask_pem_blocks(input: &str, map: &mut SessionMaskMap, count: &mut usize) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("-----BEGIN ") {
        let after_begin = &rest[start..];
        let Some(header_end) = after_begin.find("-----") else {
            break;
        };
        let header_line_end = header_end + 5;
        let _ = header_line_end;
        if let Some(end_marker) = after_begin.find("-----END ") {
            let after_end = &after_begin[end_marker..];
            if let Some(close_dashes) = after_end[9..].find("-----") {
                let total_len = end_marker + 9 + close_dashes + 5;
                let block = &after_begin[..total_len];
                if block.contains("PRIVATE KEY") || block.contains("CERTIFICATE") {
                    out.push_str(&rest[..start]);
                    let ph = map.placeholder_for("PEM", block);
                    out.push_str(&ph);
                    *count += 1;
                    rest = &after_begin[total_len..];
                    continue;
                }
            }
        }
        out.push_str(&rest[..start + 11]);
        rest = &rest[start + 11..];
    }
    out.push_str(rest);
    out
}

fn mask_custom_patterns(
    input: &str,
    patterns: &[String],
    map: &mut SessionMaskMap,
    count: &mut usize,
) -> String {
    let mut out = input.to_string();
    for raw in patterns {
        let pat = raw.trim();
        if pat.chars().count() < 4 || pat.starts_with("[MASKED_") {
            continue;
        }
        if out.contains(pat) {
            let matches = out.matches(pat).count();
            let ph = map.placeholder_for("CUSTOM", pat);
            out = out.replace(pat, &ph);
            *count += matches;
        }
    }
    out
}

fn mask_api_keys_in_text(input: &str, map: &mut SessionMaskMap, count: &mut usize) -> String {
    const PREFIXES: &[(&str, usize)] = &[
        ("github_pat_", 16),
        ("sk-svcacct-", 12),
        ("sk-proj-", 12),
        ("sk-ant-", 12),
        ("xoxb-", 12),
        ("xoxp-", 12),
        ("ghp_", 12),
        ("gho_", 12),
        ("ghu_", 12),
        ("ghs_", 12),
        ("AIza", 16),
        ("AKIA", 16),
        ("sk-", 12),
        ("gw_", 16),
    ];
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if is_placeholder_at(input, i) {
            if let Some(end) = input[i..].find(']') {
                out.push_str(&input[i..i + end + 1]);
                i += end + 1;
                continue;
            }
        }
        let mut matched: Option<usize> = None;
        for &(prefix, min_body) in PREFIXES {
            if input[i..].starts_with(prefix) {
                let prev_ident = i > 0 && is_ident_byte(bytes[i - 1]);
                if !prev_ident {
                    let body_start = i + prefix.len();
                    let mut end = body_start;
                    while end < bytes.len() && is_token_body_byte(bytes[end]) {
                        end += 1;
                    }
                    if end - body_start >= min_body {
                        matched = Some(end);
                        break;
                    }
                }
            }
        }
        if let Some(end) = matched {
            let secret = &input[i..end];
            let ph = map.placeholder_for("KEY", secret);
            out.push_str(&ph);
            *count += 1;
            i = end;
            continue;
        }
        let ch = input[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn mask_password_assignments(input: &str, map: &mut SessionMaskMap, count: &mut usize) -> String {
    const KEYS: &[&str] = &[
        "password",
        "passwd",
        "secret",
        "api_key",
        "apikey",
        "access_token",
        "auth_token",
    ];
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let lower = input.to_ascii_lowercase();
    let mut i = 0usize;
    while i < bytes.len() {
        if is_placeholder_at(input, i) {
            if let Some(end) = input[i..].find(']') {
                out.push_str(&input[i..i + end + 1]);
                i += end + 1;
                continue;
            }
        }
        let mut matched_span: Option<(usize, usize, usize)> = None;
        for &kw in KEYS {
            if lower[i..].starts_with(kw) {
                let prev_ident = i > 0 && is_ident_byte(bytes[i - 1]);
                if prev_ident {
                    continue;
                }
                let mut j = i + kw.len();
                while j < bytes.len() && bytes[j] == b' ' {
                    j += 1;
                }
                if j >= bytes.len() || !matches!(bytes[j], b'=' | b':') {
                    continue;
                }
                j += 1;
                while j < bytes.len() && bytes[j] == b' ' {
                    j += 1;
                }
                if j >= bytes.len() {
                    continue;
                }
                let quote = if matches!(bytes[j], b'"' | b'\'') {
                    let q = bytes[j];
                    j += 1;
                    Some(q)
                } else {
                    None
                };
                let val_start = j;
                while j < bytes.len() {
                    let b = bytes[j];
                    if let Some(q) = quote {
                        if b == q || b == b'\n' || b == b'\r' {
                            break;
                        }
                    } else if b.is_ascii_whitespace() || matches!(b, b',' | b';' | b')' | b'}' | b'"' | b'\'') {
                        break;
                    }
                    j += 1;
                }
                let val = &input[val_start..j];
                if val.len() >= 6 && !val.starts_with("[MASKED_") {
                    matched_span = Some((val_start, j, val_start));
                    break;
                }
            }
        }
        if let Some((val_start, val_end, _)) = matched_span {
            out.push_str(&input[i..val_start]);
            let secret = &input[val_start..val_end];
            let ph = map.placeholder_for("SECRET", secret);
            out.push_str(&ph);
            *count += 1;
            i = val_end;
            continue;
        }
        let ch = input[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn is_email_local(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_email_domain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')
}

fn mask_emails_in_text(input: &str, map: &mut SessionMaskMap, count: &mut usize) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'@' && i > 0 {
            let mut l = out.len();
            let out_bytes = out.as_bytes();
            while l > 0 && is_email_local(out_bytes[l - 1]) {
                l -= 1;
            }
            let local_ok = l < out.len() && (l == 0 || !matches!(out_bytes[l - 1], b'/' | b'@'));
            let mut r = i + 1;
            while r < bytes.len() && is_email_domain(bytes[r]) {
                r += 1;
            }
            let domain = &input[i + 1..r];
            let domain_ok = domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && domain
                    .rsplit('.')
                    .next()
                    .is_some_and(|tld| tld.len() >= 2 && tld.bytes().all(|c| c.is_ascii_alphabetic()));
            if local_ok && domain_ok {
                let local = out[l..].to_string();
                let full = format!("{local}@{domain}");
                if !full.starts_with("[MASKED_") {
                    out.truncate(l);
                    let ph = map.placeholder_for("EMAIL", &full);
                    out.push_str(&ph);
                    *count += 1;
                    i = r;
                    continue;
                }
            }
        }
        let ch = input[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

pub fn mask_text(
    input: &str,
    settings: &MaskingSettings,
    map: &mut SessionMaskMap,
) -> (String, usize) {
    if !settings.enabled || input.is_empty() {
        return (input.to_string(), 0);
    }
    let mut count = 0usize;
    let mut text = input.to_string();
    if settings.mask_private_keys {
        text = mask_pem_blocks(&text, map, &mut count);
    }
    if !settings.custom_patterns.is_empty() {
        text = mask_custom_patterns(&text, &settings.custom_patterns, map, &mut count);
    }
    if settings.mask_api_keys {
        text = mask_api_keys_in_text(&text, map, &mut count);
    }
    if settings.mask_passwords {
        text = mask_password_assignments(&text, map, &mut count);
    }
    if settings.mask_emails {
        text = mask_emails_in_text(&text, map, &mut count);
    }
    (text, count)
}

/// 仅脱敏 OpenAI Chat Completions 请求体中的 `messages[*].content` 文本内容，保持 JSON 结构完整。
pub fn mask_openai_request(
    data: &mut Value,
    settings: &MaskingSettings,
    map: &mut SessionMaskMap,
) -> usize {
    if !settings.enabled {
        return 0;
    }
    let mut total = 0usize;
    let Some(msgs) = data.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    for msg in msgs {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };
        if let Some(content) = obj.get_mut("content") {
            match content {
                Value::String(s) => {
                    let (masked, c) = mask_text(s, settings, map);
                    if c > 0 {
                        *s = masked;
                        total += c;
                    }
                }
                Value::Array(parts) => {
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(Value::as_str).map(str::to_string) {
                            let (masked, c) = mask_text(&text, settings, map);
                            if c > 0 {
                                part["text"] = Value::String(masked);
                                total += c;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    total
}

pub fn unmask_text(input: &str, reverse: &BTreeMap<String, String>) -> String {
    if reverse.is_empty() || !input.contains("[MASKED_") {
        return input.to_string();
    }
    let mut out = input.to_string();
    for (ph, secret) in reverse {
        if out.contains(ph) {
            out = out.replace(ph, secret);
        }
    }
    out
}

pub fn unmask_openai_response(data: &mut Value, reverse: &BTreeMap<String, String>) {
    if reverse.is_empty() {
        return;
    }
    let Some(choices) = data.get_mut("choices").and_then(Value::as_array_mut) else {
        return;
    };
    for ch in choices {
        if let Some(msg) = ch.get_mut("message").and_then(Value::as_object_mut) {
            for key in ["content", "reasoning_content", "refusal"] {
                if let Some(Value::String(s)) = msg.get_mut(key) {
                    *s = unmask_text(s, reverse);
                }
            }
            if let Some(Value::Array(calls)) = msg.get_mut("tool_calls") {
                for call in calls {
                    if let Some(Value::String(args)) = call.pointer_mut("/function/arguments") {
                        *args = unmask_text(args, reverse);
                    }
                }
            }
        }
    }
}

/// 跨 SSE 分片的流式反脱敏器，避免 `[MASKED_KEY_01]` 被拆成两帧导致漏还原。
#[derive(Debug, Clone, Default)]
pub struct SseFrameUnmasker {
    reverse: BTreeMap<String, String>,
    buffers: BTreeMap<String, String>,
}

impl SseFrameUnmasker {
    pub fn new(reverse: BTreeMap<String, String>) -> Self {
        Self {
            reverse,
            buffers: BTreeMap::new(),
        }
    }

    pub fn is_active(&self) -> bool {
        !self.reverse.is_empty()
    }

    fn push_field(&mut self, key: &str, piece: &str) -> String {
        if self.reverse.is_empty() {
            return piece.to_string();
        }
        let buf = self.buffers.entry(key.to_string()).or_default();
        buf.push_str(piece);
        for (ph, secret) in &self.reverse {
            if buf.contains(ph) {
                *buf = buf.replace(ph, secret);
            }
        }
        let search_from = buf.len().saturating_sub(MAX_TAIL_HOLD_BYTES);
        let mut start = search_from;
        while start > 0 && !buf.is_char_boundary(start) {
            start -= 1;
        }
        if let Some(rel) = buf[start..].rfind('[') {
            let cut = start + rel;
            let tail = &buf[cut..];
            if !tail.contains(']')
                && ("[MASKED_".starts_with(tail) || tail.starts_with("[MASKED_"))
            {
                let emit = buf[..cut].to_string();
                let remain = tail.to_string();
                *buf = remain;
                return emit;
            }
        }
        std::mem::take(buf)
    }

    fn take_field(&mut self, key: &str) -> String {
        let mut s = self.buffers.remove(key).unwrap_or_default();
        for (ph, secret) in &self.reverse {
            if s.contains(ph) {
                s = s.replace(ph, secret);
            }
        }
        s
    }

    /// 处理一条已经规范化的 OpenAI SSE 帧（`data: {...}\n\n` 或 `data: [DONE]\n\n`）。
    pub fn process_sse_frame(&mut self, frame: &str) -> String {
        if !self.is_active() {
            return frame.to_string();
        }
        let Some(payload) = frame.strip_prefix("data: ") else {
            return frame.to_string();
        };
        let trimmed = payload.trim();
        if trimmed == "[DONE]" {
            return frame.to_string();
        }
        let Ok(mut val) = serde_json::from_str::<Value>(trimmed) else {
            return frame.to_string();
        };
        let Some(choices) = val.get_mut("choices").and_then(Value::as_array_mut) else {
            return frame.to_string();
        };
        for ch in choices {
            let idx = ch.get("index").and_then(Value::as_u64).unwrap_or(0);
            let finished = ch
                .get("finish_reason")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            if let Some(delta) = ch.get_mut("delta").and_then(Value::as_object_mut) {
                for field in ["reasoning_content", "content", "refusal"] {
                    let buf_key = format!("{idx}:{field}");
                    if let Some(Value::String(s)) = delta.get_mut(field) {
                        let out = self.push_field(&buf_key, s);
                        *s = if finished {
                            format!("{out}{}", self.take_field(&buf_key))
                        } else {
                            out
                        };
                    } else if finished {
                        let tail = self.take_field(&buf_key);
                        if !tail.is_empty() {
                            delta.insert(field.into(), Value::String(tail));
                        }
                    }
                }
                if let Some(Value::Array(calls)) = delta.get_mut("tool_calls") {
                    for call in calls {
                        let t_idx = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                        let buf_key = format!("{idx}:tool:{t_idx}");
                        if let Some(Value::String(args)) = call.pointer_mut("/function/arguments") {
                            let out = self.push_field(&buf_key, args);
                            *args = if finished {
                                format!("{out}{}", self.take_field(&buf_key))
                            } else {
                                out
                            };
                        }
                    }
                }
            } else if finished {
                // 如果 finish_reason 同帧没有 delta，确保先收集的尾部挂在 delta 中发出去。
                let mut patch = serde_json::Map::new();
                for field in ["reasoning_content", "content", "refusal"] {
                    let tail = self.take_field(&format!("{idx}:{field}"));
                    if !tail.is_empty() {
                        patch.insert(field.into(), Value::String(tail));
                    }
                }
                if !patch.is_empty() {
                    ch["delta"] = Value::Object(patch);
                }
            }
        }
        format!("data: {val}\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn masking_is_disabled_by_default() {
        let settings = MaskingSettings::default();
        assert!(!settings.enabled);
        let mut req = json!({
            "messages": [{"role":"user","content":"sk-1234567890abcdef alice@example.com"}]
        });
        let mut map = SessionMaskMap::default();
        let n = mask_openai_request(&mut req, &settings, &mut map);
        assert_eq!(n, 0);
        assert!(req["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("sk-1234567890abcdef"));
    }

    #[test]
    fn sticky_masking_preserves_placeholders_and_unmasks_split_sse() {
        let settings = MaskingSettings {
            enabled: true,
            custom_patterns: vec!["TopSecretProject".into()],
            ..Default::default()
        };
        let mut map = SessionMaskMap::default();
        let mut req1 = json!({
            "messages": [{"role":"user","content":"Use sk-proj-abcdef12345678 and mail alice@example.com for TopSecretProject password=mysecretpass"}]
        });
        let n1 = mask_openai_request(&mut req1, &settings, &mut map);
        assert_eq!(n1, 4);
        let masked1 = req1["messages"][0]["content"].as_str().unwrap().to_string();
        assert!(!masked1.contains("sk-proj-abcdef12345678"));
        assert!(!masked1.contains("alice@example.com"));
        assert!(!masked1.contains("TopSecretProject"));
        assert!(!masked1.contains("mysecretpass"));
        assert!(masked1.contains("[MASKED_KEY_01]"));
        assert!(masked1.contains("[MASKED_EMAIL_01]"));
        assert!(masked1.contains("[MASKED_CUSTOM_01]"));
        assert!(masked1.contains("[MASKED_SECRET_01]"));

        // 第二轮同一会话再次出现同密钥，必须冻结为同一占位符
        let mut req2 = json!({
            "messages": [{"role":"user","content":"Retry sk-proj-abcdef12345678"}]
        });
        mask_openai_request(&mut req2, &settings, &mut map);
        assert_eq!(req2["messages"][0]["content"], "Retry [MASKED_KEY_01]");

        // 模拟流式 SSE 将占位符切分在两个帧里
        let mut unmasker = SseFrameUnmasker::new(map.reverse.clone());
        let f1 = unmasker.process_sse_frame("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"key is [MASKED_\"}}]}\n\n");
        let f2 = unmasker.process_sse_frame("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"KEY_01] done\"}}]}\n\n");
        let f3 = unmasker.process_sse_frame("data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
        let v1: Value = serde_json::from_str(f1.strip_prefix("data: ").unwrap().trim()).unwrap();
        let v2: Value = serde_json::from_str(f2.strip_prefix("data: ").unwrap().trim()).unwrap();
        let v3: Value = serde_json::from_str(f3.strip_prefix("data: ").unwrap().trim()).unwrap();
        let combined = format!(
            "{}{}{}",
            v1["choices"][0]["delta"]["content"].as_str().unwrap_or(""),
            v2["choices"][0]["delta"]["content"].as_str().unwrap_or(""),
            v3["choices"][0]["delta"]["content"].as_str().unwrap_or(""),
        );
        assert_eq!(combined, "key is sk-proj-abcdef12345678 done");
    }
}
