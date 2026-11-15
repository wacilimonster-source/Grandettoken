//! 本地模型网关的纯协议与适配逻辑（不依赖 Tauri / Axum，便于 `cargo test` 单测）。
//!
//! 职责：
//! - 三类上游接入定义（`openai_compat` / `agent_platform` / `gemini_aistudio`）与旧配置兼容；
//! - 请求体兼容清洗与 400 自动修复（支持 Agent `tools` / `tool_calls`、Google `thinking_config`、
//!   Agent Platform penalty 白名单）；
//! - 响应与 SSE 流式归一化（统一输出 PocketTavern / SillyTavern 自定义源及主流 Agent 兼容的
//!   OpenAI `reasoning_content`，支持 `<think>` 增量切分与 `extra_content.google.thought` 剥离）；
//! - 密钥脱敏与标准化错误信封。

pub mod error;
pub mod request;
pub mod sources;
pub mod stream;
pub mod thinking;

use serde::{Deserialize, Serialize};

/// 三类网关接入源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewaySourceKind {
    OpenAiCompat,
    AgentPlatform,
    GeminiAiStudio,
}

impl GatewaySourceKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "openai_compat" | "openai" | "custom" | "hapi" | "inkstone" => Some(Self::OpenAiCompat),
            "agent_platform" => Some(Self::AgentPlatform),
            "gemini_aistudio" | "gemini" => Some(Self::GeminiAiStudio),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompat => "openai_compat",
            Self::AgentPlatform => "agent_platform",
            Self::GeminiAiStudio => "gemini_aistudio",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::OpenAiCompat => "OpenAI 兼容端点",
            Self::AgentPlatform => "Agent Platform",
            Self::GeminiAiStudio => "Gemini AI Studio",
        }
    }
}

/// 响应端思维链归一策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingMode {
    /// 自动归一：字段统一到 `reasoning_content`，剥离 `google.thought`，并自动切分开头 `<think>`/`<thinking>`/`<thought>` 标签。
    #[default]
    Auto,
    /// 仅转换结构化字段（`reasoning`/`thinking`/`google.thought` → `reasoning_content`），不切分正文标签。
    FieldsOnly,
    /// 仅改写 `model` 与标准 SSE 帧，思维链原样透传。
    Passthrough,
}

impl ThinkingMode {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "fields_only" => Self::FieldsOnly,
            "passthrough" => Self::Passthrough,
            _ => Self::Auto,
        }
    }
}

/// Google 系（Gemini AI Studio / Agent Platform）思考摘要请求策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThoughtRequestPolicy {
    /// 跟随客户端传入的 `include_reasoning` 或 `reasoning_effort`。
    #[default]
    FollowClient,
    /// 对不支持传 `include_reasoning` 的通用 Agent 客户端，除非 `reasoning_effort == "none"`，默认请求思考摘要。
    ForceInclude,
}

impl ThoughtRequestPolicy {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "force_include" => Self::ForceInclude,
            _ => Self::FollowClient,
        }
    }
}

/// 剥离从网页/聊天软件复制密钥或请求头时混入的换行、零宽字符与 NBSP。
pub fn strip_paste_artifacts(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|&c| {
            !matches!(
                c,
                '\r' | '\n' | '\t' | '\u{00A0}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{FEFF}'
            )
        })
        .collect()
}

/// 检查 HTTP 请求头值是否只包含合法可见 ASCII / 空白字符（防止 reqwest / Node 报错）。
pub fn validate_header_value(name: &str, value: &str) -> Result<(), String> {
    if let Some(bad) = value
        .chars()
        .find(|&c| c < ' ' && c != '\t' || c as u32 > 0x7E)
    {
        return Err(format!(
            "请求头「{name}」包含非法字符（U+{:04X}），请检查是否复制了中文或不可见控制符",
            bad as u32
        ));
    }
    Ok(())
}

/// 校验并规范化调用别名：小写英文字母、数字、连字符、下划线，1..=32 字符。
pub fn normalize_alias(raw: &str) -> Result<String, String> {
    let alias = raw.trim().to_ascii_lowercase();
    if alias.is_empty()
        || alias.len() > 32
        || !alias
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
    {
        return Err("别名只能包含英文字母、数字、连字符和下划线（1 到 32 字）".into());
    }
    Ok(alias)
}
