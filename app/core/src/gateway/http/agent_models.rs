//! Agent Platform 模型清单探测。
//!
//! 官方 Vertex OpenAI 兼容端点不提供 `/models`；`publishers.google.models.list`
//! 只接受 OAuth，API Key 会得到 401/404。这里用 `countTokens` 逐个校验内置候选清单：
//! 200 = 该密钥/项目可用，404 = 不存在或无权，其余按「未知」保留，避免瞬时故障误删模型。

use super::*;
use crate::gateway_proto::sources::AGENT_PLATFORM_PRESET_MODELS;

/// Agent Platform 用 `countTokens` 探测内置候选模型的可用性：
/// 单请求 5s、并发 8、整轮 12s 封顶，网络异常时尽快退回内置全量清单。
const PROBE_REQUEST_TIMEOUT_SECS: u64 = 5;
const PROBE_TOTAL_TIMEOUT_SECS: u64 = 12;
const PROBE_CONCURRENCY: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentProbeState {
    Available,
    Missing,
    Unknown,
}

/// 从官方 Vertex OpenAI 兼容 Base URL 推导 `publishers/google/models` 前缀。
///
/// 只接受 `https://[LOCATION-]aiplatform.googleapis.com/v1/projects/{p}/locations/{l}/endpoints/openapi`；
/// 自建/转售端点结构不同，走 `/models` 或内置清单，不能拿项目路径去拼 countTokens。
pub(super) fn agent_platform_probe_root(base_url: &str) -> Option<String> {
    let url = reqwest::Url::parse(base_url).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if host != "aiplatform.googleapis.com" && !host.ends_with("-aiplatform.googleapis.com") {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    if segments.len() != 7
        || segments[1] != "projects"
        || segments[3] != "locations"
        || segments[5] != "endpoints"
        || segments[6] != "openapi"
    {
        return None;
    }
    Some(format!(
        "{}://{}/{}/projects/{}/locations/{}/publishers/google/models",
        url.scheme(),
        host,
        segments[0],
        segments[2],
        segments[4]
    ))
}

/// 用 `countTokens` 校验内置候选模型。返回顺序与传入清单一致。
pub(super) async fn probe_agent_platform_models(
    client: &reqwest::Client,
    probe_root: &str,
    hdrs: &[(String, String)],
    models: &[&str],
) -> Vec<(String, AgentProbeState)> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(PROBE_TOTAL_TIMEOUT_SECS);
    let requests: Vec<(String, String)> = models
        .iter()
        .map(|model| {
            (
                model.to_string(),
                format!(
                    "{probe_root}/{}:countTokens",
                    model.strip_prefix("google/").unwrap_or(model)
                ),
            )
        })
        .collect();
    let mut results: Vec<(String, AgentProbeState)> = futures::stream::iter(requests.into_iter().map(|(model, url)| {
        let client = client.clone();
        let hdrs = hdrs.to_vec();
        async move {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return (model, AgentProbeState::Unknown);
            }
            let timeout = remaining.min(Duration::from_secs(PROBE_REQUEST_TIMEOUT_SECS));
            let req = client.post(&url).timeout(timeout).json(&json!({
                "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }]
            }));
            let req = match apply_headers(req, &hdrs) {
                Ok(req) => req,
                Err(_) => return (model, AgentProbeState::Unknown),
            };
            match req.send().await {
                Ok(resp) if resp.status().is_success() => (model, AgentProbeState::Available),
                Ok(resp) if resp.status() == StatusCode::NOT_FOUND => (model, AgentProbeState::Missing),
                Ok(_) | Err(_) => (model, AgentProbeState::Unknown),
            }
        }
    }))
    .buffer_unordered(PROBE_CONCURRENCY)
    .collect()
    .await;
    let states: std::collections::HashMap<String, AgentProbeState> = results.drain(..).collect();
    models
        .iter()
        .map(|model| {
            (
                model.to_string(),
                states.get(*model).copied().unwrap_or(AgentProbeState::Unknown),
            )
        })
        .collect()
}

/// 读取 Agent Platform 的可见模型清单。
///
/// - 官方 Vertex 端点：`countTokens` 校验内置全量候选，过滤明确 404 的模型；
/// - 自建 / 转售端点：优先取 OpenAI `/models` 实时全量，失败时退回内置全量清单；
/// - 任何情况下都保留手动增补（`all_channel_models` 合并）。
pub(super) async fn fetch_agent_platform_models(
    client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    custom_headers: &BTreeMap<String, String>,
    host: String,
    sensitive: &[String],
) -> Result<ModelFetchResult, String> {
    let kind = GatewaySourceKind::AgentPlatform;
    // 统一先校验请求头（含密钥必须存在），错误口径与其它源一致。
    let hdrs = build_upstream_headers(kind, api_key, custom_headers)
        .map_err(|e| redact_known(&e, sensitive))?;
    let full_catalog = || ModelFetchResult {
        fetched: AGENT_PLATFORM_PRESET_MODELS.iter().map(|s| s.to_string()).collect(),
        host: host.clone(),
        probe_note: None,
    };
    let fallback = |note: String| ModelFetchResult {
        probe_note: Some(note),
        ..full_catalog()
    };

    if let Some(root) = agent_platform_probe_root(base_url) {
        let states = probe_agent_platform_models(client, &root, &hdrs, AGENT_PLATFORM_PRESET_MODELS).await;
        let available = states
            .iter()
            .filter(|(_, state)| *state == AgentProbeState::Available)
            .count();
        let unknown = states
            .iter()
            .filter(|(_, state)| *state == AgentProbeState::Unknown)
            .count();
        if available == 0 {
            return Ok(fallback(
                "未能确认 Agent Platform 模型可用性（网络/密钥/项目权限），已载入内置全量清单".into(),
            ));
        }
        // Unknown 可能只是瞬时失败，保留在清单里；只有明确 404 的模型才隐藏。
        let fetched = states
            .iter()
            .filter(|(_, state)| *state != AgentProbeState::Missing)
            .map(|(model, _)| model.to_string())
            .collect();
        return Ok(ModelFetchResult {
            fetched,
            host,
            probe_note: (unknown > 0).then(|| {
                format!("已按账号校验可用模型；{unknown} 个模型可用性未能确认，已一并保留")
            }),
        });
    }

    match fetch_openai_models(client, kind, base_url, api_key, custom_headers, host.clone()).await {
        Ok(res) if !res.fetched.is_empty() => Ok(res),
        Ok(res) => {
            let reason = res.probe_note.unwrap_or_else(|| "上游模型列表为空".into());
            Ok(fallback(format!("{reason}；已载入内置全量模型清单，可手动增补")))
        }
        Err(err) => Ok(fallback(format!(
            "读取上游模型列表失败（{err}），已载入内置全量模型清单，请用「测连通」验证密钥与网络"
        ))),
    }
}
