//! 三类上游接入定义、模板预设、Base URL 规范化与鉴权头策略。

use super::{strip_paste_artifacts, validate_header_value, GatewaySourceKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// PocketTavern 同款 Agent Platform 默认端点（Google Vertex AI OpenAI 兼容入口）。
pub const AGENT_PLATFORM_DEFAULT_BASE_URL: &str =
    "https://aiplatform.googleapis.com/v1/projects/project-11ce9e7c-f85d-4842-951/locations/global/endpoints/openapi";

/// Agent Platform 预置可用模型（Vertex OpenAI 兼容端点不提供 `/models` 接口）。
pub const AGENT_PLATFORM_PRESET_MODELS: &[&str] = &[
    "google/gemini-3.8-flash",
    "google/gemini-3.5-flash-lite",
    "google/gemini-3.1-flash-lite",
    "google/gemini-4-argon",
    "google/gemini-4-hydrogen",
];

/// Gemini AI Studio OpenAI 兼容端点。
pub const GEMINI_AISTUDIO_OPENAI_BASE_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/openai";

/// Gemini AI Studio 原生模型列表端点。
pub const GEMINI_AISTUDIO_MODELS_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000";

/// 网关访问上游的默认 User-Agent（对齐官方 OpenAI SDK，避免空 UA 被中转站 WAF 拦截且不暴露特殊指纹）。
pub const DEFAULT_UPSTREAM_USER_AGENT: &str = "OpenAI/JS 4.77.0";

/// 供前端展示的 OpenAI 兼容快捷模板。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAiPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub default_alias: &'static str,
    pub hint: &'static str,
}

pub const OPENAI_PRESETS: &[OpenAiPreset] = &[
    OpenAiPreset {
        id: "custom",
        name: "自定义 OpenAI 端点",
        base_url: "",
        default_alias: "custom",
        hint: "填写任意 /v1 兼容地址，或从密钥箱自动带入",
    },
    OpenAiPreset {
        id: "hapi",
        name: "Hapi",
        base_url: "https://ai.yuchuantest.com/v1",
        default_alias: "hapi",
        hint: "Hapi 官方 OpenAI 兼容入口",
    },
    OpenAiPreset {
        id: "inkstone",
        name: "书生端砚",
        base_url: "https://discovery-api.intern-ai.org.cn/v1",
        default_alias: "inkstone",
        hint: "书生浦语 / 端砚平台 OpenAI 兼容入口",
    },
    OpenAiPreset {
        id: "4sapi",
        name: "4SAPI",
        base_url: "https://4sapi.com/v1",
        default_alias: "4sapi",
        hint: "4SAPI 聚合网关入口",
    },
    OpenAiPreset {
        id: "opencode-go",
        name: "OpenCode Go",
        base_url: "https://opencode.ai/inference/go/openai/v1",
        default_alias: "go",
        hint: "OpenCode Go OpenAI 兼容入口",
    },
    OpenAiPreset {
        id: "deepseek",
        name: "DeepSeek 官方",
        base_url: "https://api.deepseek.com/v1",
        default_alias: "deepseek",
        hint: "DeepSeek 官方接口，原生支持 reasoning_content",
    },
    OpenAiPreset {
        id: "openrouter",
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        default_alias: "openrouter",
        hint: "OpenRouter 多模型聚合入口",
    },
    OpenAiPreset {
        id: "siliconflow",
        name: "硅基流动 SiliconFlow",
        base_url: "https://api.siliconflow.cn/v1",
        default_alias: "siliconflow",
        hint: "SiliconFlow OpenAI 兼容入口",
    },
    OpenAiPreset {
        id: "openai",
        name: "OpenAI 官方",
        base_url: "https://api.openai.com/v1",
        default_alias: "openai",
        hint: "OpenAI 官方 Chat Completions 入口",
    },
    OpenAiPreset {
        id: "ollama",
        name: "本地 Ollama",
        base_url: "http://127.0.0.1:11434/v1",
        default_alias: "ollama",
        hint: "本机 Ollama（可勾选无需上游密钥）",
    },
];

pub fn find_preset(id: &str) -> Option<&'static OpenAiPreset> {
    let id = id.trim().to_ascii_lowercase();
    OPENAI_PRESETS.iter().find(|p| p.id == id)
}

/// 规范化上游 Base URL：
/// - 去掉首尾空白与尾斜杠；
/// - 去掉误填的 `/chat/completions` 或 `/models` 后缀；
/// - 缺协议时：私网 / 回环地址补 `http://`，其余补 `https://`；
/// - 仅在路径为空时默认补 `/v1`；已有 `/v1`、`/v1beta/openai`、`/api/paas/v4`、`/endpoints/openapi` 等路径时保持原样。
pub fn normalize_base_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("API 地址不能为空".into());
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else if is_private_host_prefix(trimmed) {
        format!("http://{trimmed}")
    } else {
        format!("https://{trimmed}")
    };

    let url = reqwest::Url::parse(&with_scheme).map_err(|_| format!("API 地址格式无效：{trimmed}"))?;
    match url.scheme() {
        "http" | "https" => {}
        s => return Err(format!("不支持的协议 {s}，仅支持 http / https")),
    }
    if url.host_str().unwrap_or("").is_empty() {
        return Err("API 地址缺少主机名".into());
    }

    let mut clean = with_scheme.trim_end_matches('/').to_string();
    for suffix in ["/chat/completions", "/models"] {
        if let Some(stripped) = clean.strip_suffix(suffix) {
            clean = stripped.trim_end_matches('/').to_string();
        }
    }

    // 重新解析确认 path 是否为空
    let parsed = reqwest::Url::parse(&clean).map_err(|_| format!("API 地址格式无效：{trimmed}"))?;
    let path = parsed.path().trim_end_matches('/');
    if path.is_empty() {
        clean.push_str("/v1");
    }
    Ok(clean)
}

fn is_private_host_prefix(s: &str) -> bool {
    let authority = s.split(['/', '?', '#']).next().unwrap_or(s);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower == "::1" || lower.ends_with(".local") {
        return true;
    }
    if let Ok(ip) = lower.parse::<std::net::Ipv4Addr>() {
        return ip.is_loopback() || ip.is_private() || ip.is_link_local();
    }
    false
}

/// 从规范化 URL 提取主机名用于界面展示。
pub fn host_of_url(base_url: &str) -> String {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

/// 解析历史渠道配置或兼容别名：返回 `(kind, template, base_url)`。
pub fn resolve_source_defaults(
    source_raw: &str,
    template_raw: &str,
    base_url_raw: &str,
) -> Result<(GatewaySourceKind, String, String), String> {
    let kind = GatewaySourceKind::parse(source_raw)
        .ok_or_else(|| format!("不支持的接入类型：{source_raw}"))?;
    let template = if !template_raw.trim().is_empty() {
        template_raw.trim().to_ascii_lowercase()
    } else if matches!(source_raw.trim().to_ascii_lowercase().as_str(), "hapi" | "inkstone") {
        source_raw.trim().to_ascii_lowercase()
    } else {
        String::new()
    };

    let base_url = match kind {
        GatewaySourceKind::GeminiAiStudio => {
            if base_url_raw.trim().is_empty() {
                GEMINI_AISTUDIO_OPENAI_BASE_URL.to_string()
            } else {
                normalize_base_url(base_url_raw)?
            }
        }
        GatewaySourceKind::AgentPlatform => {
            if base_url_raw.trim().is_empty() {
                AGENT_PLATFORM_DEFAULT_BASE_URL.to_string()
            } else {
                normalize_base_url(base_url_raw)?
            }
        }
        GatewaySourceKind::OpenAiCompat => {
            if !base_url_raw.trim().is_empty() {
                normalize_base_url(base_url_raw)?
            } else if let Some(preset) = find_preset(&template) {
                if preset.base_url.is_empty() {
                    return Err("请填写 OpenAI 兼容端点地址（或先在密钥箱填写调用端点）".into());
                }
                preset.base_url.to_string()
            } else {
                return Err("请填写 OpenAI 兼容端点地址（或先在密钥箱填写调用端点）".into());
            }
        }
    };
    Ok((kind, template, base_url))
}

/// 构造发往上游的请求头：
/// - `AgentPlatform`：**仅使用 `x-goog-api-key`**，严格禁止附带 `Authorization: Bearer`（实测必 401）；
/// - `GeminiAiStudio`（OpenAI 兼容接口）：使用 `Authorization: Bearer`；
/// - `OpenAiCompat`：有密钥时使用 `Authorization: Bearer`，无上游密钥时跳过该头；
/// - 所有头值先经粘贴伪影剥离并做 HTTP 头字符合法性校验。
pub fn build_upstream_headers(
    kind: GatewaySourceKind,
    api_key: Option<&str>,
    custom_headers: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>, String> {
    let mut map: BTreeMap<String, (String, String)> = BTreeMap::new();

    for (k, v) in custom_headers {
        let name = k.trim();
        if name.is_empty() {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        // 安全防线：Agent Platform 绝不透传 Authorization 头；其余源保留用户覆盖权
        if kind == GatewaySourceKind::AgentPlatform && lower == "authorization" {
            continue;
        }
        if lower == "content-type" || lower == "content-length" || lower == "host" {
            continue;
        }
        let cleaned = strip_paste_artifacts(v);
        validate_header_value(name, &cleaned)?;
        map.insert(lower, (name.to_string(), cleaned));
    }

    if !map
        .get("user-agent")
        .is_some_and(|(_, v)| !v.trim().is_empty())
    {
        map.insert(
            "user-agent".into(),
            ("User-Agent".into(), DEFAULT_UPSTREAM_USER_AGENT.into()),
        );
    }

    if let Some(raw_key) = api_key {
        let cleaned_key = strip_paste_artifacts(raw_key);
        if !cleaned_key.is_empty() {
            match kind {
                GatewaySourceKind::AgentPlatform => {
                    validate_header_value("x-goog-api-key", &cleaned_key)?;
                    map.insert(
                        "x-goog-api-key".into(),
                        ("x-goog-api-key".into(), cleaned_key),
                    );
                }
                GatewaySourceKind::GeminiAiStudio | GatewaySourceKind::OpenAiCompat => {
                    let bearer = format!("Bearer {cleaned_key}");
                    validate_header_value("Authorization", &bearer)?;
                    map.insert("authorization".into(), ("Authorization".into(), bearer));
                }
            }
        } else if kind == GatewaySourceKind::AgentPlatform {
            return Err("Agent Platform 密钥为空".into());
        }
    } else if kind == GatewaySourceKind::AgentPlatform {
        return Err("Agent Platform 必须配置密钥".into());
    }

    Ok(map.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_base_url_handles_variants() {
        assert_eq!(
            normalize_base_url("https://api.openai.com/v1/").unwrap(),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.openai.com/v1/chat/completions").unwrap(),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            normalize_base_url("api.deepseek.com").unwrap(),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            normalize_base_url("127.0.0.1:11434").unwrap(),
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(
            normalize_base_url("https://generativelanguage.googleapis.com/v1beta/openai/").unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
        assert!(normalize_base_url("").is_err());
        assert!(normalize_base_url("ftp://example.com").is_err());
    }

    #[test]
    fn agent_platform_headers_use_x_goog_api_key_and_strip_bearer() {
        let mut custom = BTreeMap::new();
        custom.insert("Authorization".into(), "Bearer leak".into());
        custom.insert("X-Test".into(), "ok\u{200B}".into());
        let hdrs = build_upstream_headers(
            GatewaySourceKind::AgentPlatform,
            Some("AIzaTest123\n "),
            &custom,
        )
        .unwrap();
        assert!(!hdrs.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization")));
        assert!(hdrs
            .iter()
            .any(|(k, v)| k == "x-goog-api-key" && v == "AIzaTest123"));
        assert!(hdrs.iter().any(|(k, v)| k == "X-Test" && v == "ok"));
        assert!(hdrs
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("user-agent") && v == DEFAULT_UPSTREAM_USER_AGENT));
    }

    #[test]
    fn upstream_headers_default_user_agent_and_allow_custom_override() {
        let default_hdrs = build_upstream_headers(
            GatewaySourceKind::OpenAiCompat,
            Some("sk-test123456"),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            default_hdrs
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>(),
            vec![DEFAULT_UPSTREAM_USER_AGENT]
        );

        let mut custom = BTreeMap::new();
        custom.insert("user-agent".into(), "CustomClient/2.0\n".into());
        let overridden = build_upstream_headers(
            GatewaySourceKind::OpenAiCompat,
            Some("sk-test123456"),
            &custom,
        )
        .unwrap();
        assert_eq!(
            overridden
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>(),
            vec!["CustomClient/2.0"]
        );
    }

    #[test]
    fn header_validation_rejects_non_ascii_characters() {
        let mut custom = BTreeMap::new();
        custom.insert("X-Custom".into(), "中文密钥".into());
        assert!(build_upstream_headers(GatewaySourceKind::OpenAiCompat, None, &custom).is_err());
        assert!(build_upstream_headers(
            GatewaySourceKind::AgentPlatform,
            Some("密钥错误"),
            &BTreeMap::new()
        )
        .is_err());
    }

    #[test]
    fn legacy_sources_resolve_cleanly() {
        let (kind, _, url) = resolve_source_defaults("gemini", "", "").unwrap();
        assert_eq!(kind, GatewaySourceKind::GeminiAiStudio);
        assert_eq!(url, GEMINI_AISTUDIO_OPENAI_BASE_URL);

        let (kind, tpl, url) = resolve_source_defaults("hapi", "", "").unwrap();
        assert_eq!(kind, GatewaySourceKind::OpenAiCompat);
        assert_eq!(tpl, "hapi");
        assert_eq!(url, "https://ai.yuchuantest.com/v1");

        let (kind, _, url) = resolve_source_defaults("agent_platform", "", "").unwrap();
        assert_eq!(kind, GatewaySourceKind::AgentPlatform);
        assert_eq!(url, AGENT_PLATFORM_DEFAULT_BASE_URL);
    }
}
