//! 入站 User-Agent 归因（7C）：仅展示、不作鉴权（UA 可伪造）。


tokio::task_local! {
    pub(crate) static REQ_AGENT: Option<&'static str>;
}

/// 按入站 User-Agent 前缀认出是哪个 agent（magpie 同做法；同密钥多 agent 靠它区分）。
pub(crate) fn agent_of_ua(ua: &str) -> Option<&'static str> {
    let u = ua.to_ascii_lowercase();
    for (prefix, id) in [
        ("claude-cli", "claude-code"),
        ("claude-code", "claude-code"),
        ("codex", "codex"),
        ("opencode", "opencode"),
        ("trae", "trae"),
        ("workbuddy", "workbuddy"),
        ("zcode", "zcode"),
        ("deepseek harness", "dsh"),
        ("dsh", "dsh"),
    ] {
        if u.starts_with(prefix) {
            return Some(id);
        }
    }
    None
}

pub(crate) async fn agent_layer(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let ua = req
        .headers()
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    REQ_AGENT.scope(agent_of_ua(ua), next.run(req)).await
}
