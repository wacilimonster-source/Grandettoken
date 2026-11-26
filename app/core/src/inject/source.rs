//! 接入前告诉用户「这个 agent 现在跑在什么上」（对齐 magpie `connectinfo.go Source()`）。
//! 只读登录态的**存在性**与账号标识，不读令牌/密钥值；读取行为在 README 披露。

use super::*;

pub(super) fn source(client: Client, site: &Site) -> String {
    match client {
        Client::ClaudeCode => {
            let dir = site.root.parent().map(Path::to_path_buf).unwrap_or_default();
            for p in [dir.join(".claude.json"), site.root.join(".claude.json")] {
                if let Ok(Some(v)) = read_json(&p) {
                    if v.pointer("/oauthAccount/accountUuid").and_then(Value::as_str).is_some() {
                        return "claude.ai 订阅登录".into();
                    }
                }
            }
            if let Ok(Some(v)) = read_json(&site.root.join("settings.json")) {
                if v.pointer("/env/ANTHROPIC_API_KEY").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                    || v.get("apiKeyHelper").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                {
                    return "自己的 API key".into();
                }
            }
            "未登录/未知".into()
        }
        Client::Codex => {
            let p = site.root.join("auth.json");
            if let Ok(Some(v)) = read_json(&p) {
                if v.get("tokens").is_some() || v.get("access_token").is_some() {
                    return "ChatGPT 登录".into();
                }
                if v.get("OPENAI_API_KEY").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) {
                    return "自己的 API key".into();
                }
            }
            "未登录/未知".into()
        }
        Client::OpenCode => {
            let n = read_opencode_json(&site.files.first().cloned().unwrap_or_default())
                .ok()
                .flatten()
                .map(|(_, v)| {
                    ["provider", "providers"]
                        .iter()
                        .filter_map(|c| v.get(*c).and_then(Value::as_object))
                        .map(|m| m.len())
                        .sum::<usize>()
                })
                .unwrap_or(0);
            if n > 0 {
                format!("{n} 个自有 provider")
            } else {
                "未配置/未知".into()
            }
        }
        _ => String::new(),
    }
}

/// 改后生效方式（对齐 magpie `Notice()`）：读配置只在启动时的客户端要重启/新会话。
pub(super) fn notice(client: Client) -> &'static str {
    match client {
        Client::ClaudeCode => "重启 Claude Code 或开新会话后生效",
        Client::Codex => "重启 Codex（新线程）后生效",
        Client::OpenCode => "重启 OpenCode 后生效",
        Client::Trae => "重启 Trae 后出现在模型列表",
        Client::Workbuddy | Client::WorkbuddyAi => "重启 WorkBuddy 后生效",
        Client::Zcode => "重启 ZCode 后生效",
        Client::Dsh => "开新 dsh 会话后生效",
    }
}
