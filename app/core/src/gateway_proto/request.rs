//! 上游请求体规范化与 400 自动修复。
//!
//! 兼顾两类下游调用方：
//! - SillyTavern / PocketTavern（自带 `include_reasoning`、`reasoning_effort`、采样参数、停止串等）；
//! - 主流 Agent（`tools`、`tool_choice`、`parallel_tool_calls`、`response_format`、`max_completion_tokens`、多模态内容）。

use super::{GatewaySourceKind, ThoughtRequestPolicy};
use serde_json::{json, Value};

/// Gemini 系列输出上限（对齐 eatgemini 65536）。
pub const GEMINI_OUTPUT_LIMIT: u64 = 65536;

/// Vertex OpenAI 兼容端点（Agent Platform）实测允许传 penalty 的白名单模型。
pub const VERTEX_PENALTY_ALLOWED_MODELS: &[&str] = &["google/gemini-3.1-flash-lite"];

/// Vertex AI（Agent Platform）可配置安全分类（不含已废弃的 HARM_CATEGORY_CIVIC_INTEGRITY）。
pub const VERTEX_SAFETY_CATEGORIES: &[&str] = &[
    "HARM_CATEGORY_HATE_SPEECH",
    "HARM_CATEGORY_DANGEROUS_CONTENT",
    "HARM_CATEGORY_HARASSMENT",
    "HARM_CATEGORY_SEXUALLY_EXPLICIT",
];

const GOOGLE_REPAIRABLE_PARAMS: &[&str] = &[
    "temperature",
    "top_p",
    "top_k",
    "max_tokens",
    "max_completion_tokens",
    "stop",
    "reasoning_effort",
    "seed",
    "frequency_penalty",
    "presence_penalty",
    "parallel_tool_calls",
    "include_reasoning",
];

/// 请求体准备结果（附带变换记录，供诊断日志展示）。
#[derive(Debug, Clone, Default)]
pub struct PreparedRequest {
    pub transforms: Vec<String>,
}

/// 按接入类型与模型对请求体进行发送前清洗与思考参数映射。
pub fn prepare_upstream_body(
    kind: GatewaySourceKind,
    base_url: &str,
    model: &str,
    thought_policy: ThoughtRequestPolicy,
    data: &mut Value,
) -> PreparedRequest {
    let mut out = PreparedRequest::default();
    let Some(obj) = data.as_object_mut() else {
        return out;
    };

    // 非流式请求不发 stream_options（OpenAI / Gemini 语义：非流式带它可能 400）。
    if obj.get("stream") != Some(&Value::Bool(true)) && obj.remove("stream_options").is_some() {
        out.transforms.push("drop:stream_options(non-stream)".into());
    }

    match kind {
        GatewaySourceKind::OpenAiCompat => {
            // OpenRouter 消费 include_reasoning；标准 OpenAI / 中转大多不认识，提前移除防止严格端点 400
            let is_openrouter = base_url.to_ascii_lowercase().contains("openrouter.ai");
            if !is_openrouter && obj.remove("include_reasoning").is_some() {
                out.transforms.push("consume:include_reasoning".into());
            }
        }
        GatewaySourceKind::GeminiAiStudio => {
            sanitize_google_body(obj, model, false, thought_policy, &mut out);
        }
        GatewaySourceKind::AgentPlatform => {
            sanitize_google_body(obj, model, true, thought_policy, &mut out);
        }
    }

    out
}

fn sanitize_google_body(
    obj: &mut serde_json::Map<String, Value>,
    model: &str,
    is_vertex: bool,
    thought_policy: ThoughtRequestPolicy,
    out: &mut PreparedRequest,
) {
    let model_lower = model.trim().to_ascii_lowercase();
    let bare_model = model_lower.strip_prefix("google/").unwrap_or(&model_lower);

    let include_reasoning = obj
        .remove("include_reasoning")
        .and_then(|v| v.as_bool());

    // 兼容层未收录/明确不支持的字段一律剥离（保留 tools / tool_choice / response_format 以支持 Agent）
    for k in [
        "seed",
        "logit_bias",
        "logprobs",
        "top_logprobs",
        "n",
        "user",
        "metadata",
        "parallel_tool_calls",
    ] {
        if obj.remove(k).is_some() {
            out.transforms.push(format!("strip:{k}"));
        }
    }

    // max_completion_tokens → max_tokens，并按 65536 封顶
    if let Some(v) = obj.remove("max_completion_tokens") {
        obj.entry("max_tokens".to_string()).or_insert(v);
        out.transforms.push("map:max_completion_tokens->max_tokens".into());
    }
    if obj.get("max_tokens").and_then(Value::as_u64).unwrap_or(0) > GEMINI_OUTPUT_LIMIT {
        obj.insert("max_tokens".to_string(), Value::from(GEMINI_OUTPUT_LIMIT));
        out.transforms.push("clamp:max_tokens".into());
    }

    // Agent Platform penalty 白名单（对齐 pockettavern kVertexPenaltyAllowedModels）
    if is_vertex && !VERTEX_PENALTY_ALLOWED_MODELS.contains(&model_lower.as_str()) {
        for k in ["frequency_penalty", "presence_penalty"] {
            if obj.remove(k).is_some() {
                out.transforms.push(format!("strip:{k}(vertex-whitelist)"));
            }
        }
    }

    // Gemini 3.x / 4.x 采样参数与惩罚项清理（对齐 eatgemini / gateway.rs 既定规则）
    if bare_model.starts_with("gemini-3") || bare_model.starts_with("gemini-4") {
        let keep_sampling_31_lite = is_vertex && model_lower == "google/gemini-3.1-flash-lite";
        if !keep_sampling_31_lite {
            for k in [
                "temperature",
                "top_p",
                "top_k",
                "frequency_penalty",
                "presence_penalty",
            ] {
                if obj.remove(k).is_some() {
                    out.transforms.push(format!("strip:{k}(gemini-3+)"));
                }
            }
        }
    }

    if is_vertex && bare_model.starts_with("gemini") {
        apply_vertex_safety_off(obj, out);
    }

    // 思考参数校验与 thinking_config 映射
    let supports_thinking = bare_model.starts_with("gemini-2.5")
        || bare_model.starts_with("gemini-3")
        || bare_model.starts_with("gemini-4");

    if !supports_thinking {
        if obj.remove("reasoning_effort").is_some() {
            out.transforms.push("strip:reasoning_effort(unsupported-model)".into());
        }
        return;
    }

    // 校验 reasoning_effort 档位
    let valid_efforts: &[&str] = if (bare_model.starts_with("gemini-3") || bare_model.starts_with("gemini-4"))
        && bare_model.contains("flash-lite")
    {
        &["low", "high", "none"]
    } else {
        &["minimal", "low", "medium", "high", "none"]
    };

    if let Some(raw_effort) = obj.get("reasoning_effort").and_then(Value::as_str) {
        if !valid_efforts.contains(&raw_effort) {
            obj.remove("reasoning_effort");
            out.transforms.push("strip:reasoning_effort(invalid-level)".into());
        } else if raw_effort == "minimal" && bare_model.starts_with("gemini-3") {
            // 既定规则：gemini-3 兼容层不直接放行 minimal，除非转为 thinking_budget
            obj.remove("reasoning_effort");
            out.transforms.push("strip:reasoning_effort(minimal-on-g3)".into());
        }
    }

    let effort_opt = obj
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(str::to_string);
    let is_thinking_only = bare_model == "gemini-4-hydrogen";
    let wants_thoughts = match include_reasoning {
        Some(true) => true,
        Some(false) => false,
        None => match thought_policy {
            ThoughtRequestPolicy::ForceInclude => true,
            ThoughtRequestPolicy::FollowClient => {
                // SillyTavern custom 源不会在发往上游的请求体里保留 include_reasoning，只会传 reasoning_effort；
                // 对始终思考模型（如 gemini-4-hydrogen），未显式关闭时也应请求思维链摘要。
                effort_opt.as_deref().is_some_and(|e| e != "none") || is_thinking_only
            }
        },
    };

    if wants_thoughts && effort_opt.as_deref() != Some("none") {
        // Google 兼容端点禁止 reasoning_effort 与 extra_body.google.thinking_config 同传
        let effort = obj
            .remove("reasoning_effort")
            .and_then(|v| v.as_str().map(str::to_string));
        let mut thinking_cfg = serde_json::Map::new();
        thinking_cfg.insert("include_thoughts".into(), Value::Bool(true));

        if let Some(ref lv) = effort {
            if bare_model.starts_with("gemini-2.5") {
                let budget = match lv.as_str() {
                    "minimal" => 128,
                    "low" => 1024,
                    "medium" => 8192,
                    _ => 24576,
                };
                thinking_cfg.insert("thinking_budget".into(), Value::from(budget));
            } else {
                thinking_cfg.insert("thinking_level".into(), Value::String(lv.clone()));
            }
        }

        let extra = obj
            .entry("extra_body".to_string())
            .or_insert_with(|| json!({}));
        if let Some(extra_obj) = extra.as_object_mut() {
            let google = extra_obj
                .entry("google".to_string())
                .or_insert_with(|| json!({}));
            if let Some(google_obj) = google.as_object_mut() {
                google_obj.insert("thinking_config".into(), Value::Object(thinking_cfg));
                out.transforms.push("map:thinking_config(include_thoughts)".into());
            }
        }
    } else if effort_opt.as_deref() == Some("none") && bare_model.starts_with("gemini-2.5") {
        // 保持或允许修复器回退
    }
}

fn apply_vertex_safety_off(obj: &mut serde_json::Map<String, Value>, out: &mut PreparedRequest) {
    if let Some(extra_val) = obj.get("extra_body") {
        let Some(extra_obj) = extra_val.as_object() else {
            return;
        };
        if let Some(google_val) = extra_obj.get("google") {
            let Some(google_obj) = google_val.as_object() else {
                return;
            };
            if google_obj.contains_key("safety_settings")
                || google_obj.contains_key("safetySettings")
            {
                return;
            }
        }
    }
    let extra = obj
        .entry("extra_body".to_string())
        .or_insert_with(|| json!({}));
    if let Some(extra_obj) = extra.as_object_mut() {
        let google = extra_obj
            .entry("google".to_string())
            .or_insert_with(|| json!({}));
        if let Some(google_obj) = google.as_object_mut() {
            let settings: Vec<Value> = VERTEX_SAFETY_CATEGORIES
                .iter()
                .map(|cat| json!({ "category": cat, "threshold": "OFF" }))
                .collect();
            google_obj.insert("safety_settings".into(), Value::Array(settings));
            out.transforms.push("set:safety_settings(off)".into());
        }
    }
}

/// 当上游返回 400 时，尝试识别并剥离导致 400 的参数以进行单次自动修复重试。
pub fn repairable_params_on_400(kind: GatewaySourceKind, err_body: &str) -> Option<Vec<String>> {
    let v: Value = serde_json::from_str(err_body).ok()?;
    let err = v.get("error")?;
    let msg = err.get("message").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
    // 值域和业务约束错误不能通过删掉客户端的预算/停止条件来修复。
    let unsupported = ["not supported", "unsupported", "unrecognized", "unknown argument", "unknown parameter", "unknown name", "not enabled", "not allowed"].iter().any(|s| msg.contains(s));
    let invalid_value = ["invalid value", "must be", "out of range", "greater than", "less than", "maximum", "minimum"].iter().any(|s| msg.contains(s));
    if !unsupported || invalid_value { return None; }

    if let Some(p) = err.get("param").and_then(Value::as_str) {
        if GOOGLE_REPAIRABLE_PARAMS.contains(&p) {
            return Some(vec![p.to_string()]);
        }
        if p == "safety_settings" || p == "safetySettings" {
            return Some(vec!["extra_body.google.safety_settings".to_string()]);
        }
        if p == "thinking_config" {
            return Some(vec!["extra_body.google.thinking_config".to_string()]);
        }
        if p == "extra_body" {
            return Some(vec!["extra_body".to_string()]);
        }
    }

    if msg.contains("penalty") {
        return Some(vec![
            "frequency_penalty".to_string(),
            "presence_penalty".to_string(),
        ]);
    }
    if msg.contains("include_reasoning") {
        return Some(vec!["include_reasoning".to_string()]);
    }
    if matches!(
        kind,
        GatewaySourceKind::GeminiAiStudio | GatewaySourceKind::AgentPlatform
    ) {
        if msg.contains("safety_settings") || msg.contains("safetysettings") {
            return Some(vec!["extra_body.google.safety_settings".to_string()]);
        }
        if msg.contains("thinking_config") {
            return Some(vec!["extra_body.google.thinking_config".to_string()]);
        }
        if msg.contains("extra_body") {
            return Some(vec!["extra_body".to_string()]);
        }
        if msg.contains("reasoning_effort") {
            return Some(vec!["reasoning_effort".to_string()]);
        }
        if msg.contains("parallel_tool_calls") {
            return Some(vec!["parallel_tool_calls".to_string()]);
        }
    }
    None
}

fn remove_json_path(obj: &mut serde_json::Map<String, Value>, path: &str) -> bool {
    let Some((head, tail)) = path.split_once('.') else {
        return obj.remove(path).is_some();
    };
    let Some(Value::Object(child)) = obj.get_mut(head) else {
        return false;
    };
    let removed = remove_json_path(child, tail);
    if removed && child.is_empty() {
        obj.remove(head);
    }
    removed
}

/// 将可修复参数从请求体移除（返回实际移除的参数名）。
pub fn apply_400_repairs(data: &mut Value, params: &[String]) -> Vec<String> {
    let Some(obj) = data.as_object_mut() else {
        return vec![];
    };
    let mut removed = Vec::new();
    for p in params {
        if remove_json_path(obj, p) {
            removed.push(p.clone());
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_sanitize_strips_unsupported_fields_and_preserves_tools() {
        let mut body: Value = serde_json::from_str(
            r#"{"model":"gemini-2.5-flash","messages":[],"stream":true,
                "seed":1,"logit_bias":{"x":1},"logprobs":true,"top_logprobs":3,
                "n":1,"user":"u","metadata":{},"parallel_tool_calls":true,
                "tools":[{"type":"function","function":{"name":"ping"}}],
                "stream_options":{"include_usage":true}}"#,
        )
        .unwrap();
        prepare_upstream_body(
            GatewaySourceKind::GeminiAiStudio,
            "",
            "gemini-2.5-flash",
            ThoughtRequestPolicy::FollowClient,
            &mut body,
        );
        let obj = body.as_object().unwrap();
        for k in [
            "seed",
            "logit_bias",
            "logprobs",
            "top_logprobs",
            "n",
            "user",
            "metadata",
            "parallel_tool_calls",
        ] {
            assert!(!obj.contains_key(k), "{k} 应被剥离");
        }
        assert!(obj.contains_key("stream_options"));
        assert!(obj.contains_key("tools"));
    }

    #[test]
    fn gemini_sanitize_drops_stream_options_when_not_streaming() {
        let mut body: Value =
            serde_json::from_str(r#"{"stream":false,"stream_options":{"include_usage":true}}"#)
                .unwrap();
        prepare_upstream_body(
            GatewaySourceKind::GeminiAiStudio,
            "",
            "gemini-2.5-flash",
            ThoughtRequestPolicy::FollowClient,
            &mut body,
        );
        assert!(body.as_object().unwrap().get("stream_options").is_none());
    }

    #[test]
    fn gemini_sanitize_maps_completion_tokens_and_clamps() {
        let mut body: Value =
            serde_json::from_str(r#"{"max_completion_tokens":900000}"#).unwrap();
        prepare_upstream_body(
            GatewaySourceKind::GeminiAiStudio,
            "",
            "gemini-2.5-flash",
            ThoughtRequestPolicy::FollowClient,
            &mut body,
        );
        let obj = body.as_object().unwrap();
        assert_eq!(obj["max_tokens"], 65536);
        assert!(!obj.contains_key("max_completion_tokens"));
    }

    #[test]
    fn gemini_thinking_config_replaces_reasoning_effort_when_thoughts_requested() {
        let mut body: Value = serde_json::from_str(
            r#"{"include_reasoning":true,"reasoning_effort":"high","stream":true}"#,
        )
        .unwrap();
        prepare_upstream_body(
            GatewaySourceKind::GeminiAiStudio,
            "",
            "gemini-2.5-pro",
            ThoughtRequestPolicy::FollowClient,
            &mut body,
        );
        let obj = body.as_object().unwrap();
        assert!(!obj.contains_key("include_reasoning"));
        assert!(!obj.contains_key("reasoning_effort"));
        assert_eq!(
            obj["extra_body"]["google"]["thinking_config"]["include_thoughts"],
            true
        );
        assert_eq!(
            obj["extra_body"]["google"]["thinking_config"]["thinking_budget"],
            24576
        );
    }

    #[test]
    fn agent_platform_enforces_vertex_penalty_whitelist() {
        let mut blocked: Value = serde_json::from_str(
            r#"{"frequency_penalty":0.5,"presence_penalty":0.3,"include_reasoning":true,"reasoning_effort":"low"}"#,
        )
        .unwrap();
        prepare_upstream_body(
            GatewaySourceKind::AgentPlatform,
            "",
            "google/gemini-4-hydrogen",
            ThoughtRequestPolicy::FollowClient,
            &mut blocked,
        );
        let obj = blocked.as_object().unwrap();
        assert!(!obj.contains_key("frequency_penalty"));
        assert!(!obj.contains_key("presence_penalty"));
        assert_eq!(
            obj["extra_body"]["google"]["thinking_config"]["thinking_level"],
            "low"
        );

        let mut allowed: Value =
            serde_json::from_str(r#"{"frequency_penalty":0.5,"presence_penalty":0.3}"#).unwrap();
        prepare_upstream_body(
            GatewaySourceKind::AgentPlatform,
            "",
            "google/gemini-3.1-flash-lite",
            ThoughtRequestPolicy::FollowClient,
            &mut allowed,
        );
        assert_eq!(allowed["frequency_penalty"], 0.5);
        assert_eq!(allowed["presence_penalty"], 0.3);
    }

    #[test]
    fn follow_client_maps_reasoning_effort_without_include_reasoning_and_hydrogen_default() {
        // SillyTavern custom 源不传 include_reasoning，仅传 reasoning_effort
        let mut st_req: Value = serde_json::from_str(
            r#"{"model":"agent/google/gemini-4-hydrogen","stream":true,"reasoning_effort":"medium","presence_penalty":0.1}"#,
        )
        .unwrap();
        prepare_upstream_body(
            GatewaySourceKind::AgentPlatform,
            "",
            "google/gemini-4-hydrogen",
            ThoughtRequestPolicy::FollowClient,
            &mut st_req,
        );
        let st_obj = st_req.as_object().unwrap();
        assert!(!st_obj.contains_key("reasoning_effort"));
        assert!(!st_obj.contains_key("presence_penalty"));
        assert_eq!(
            st_obj["extra_body"]["google"]["thinking_config"]["include_thoughts"],
            true
        );
        assert_eq!(
            st_obj["extra_body"]["google"]["thinking_config"]["thinking_level"],
            "medium"
        );

        // 既无 include_reasoning 也无 reasoning_effort 时，思考专用模型 gemini-4-hydrogen 仍请求思维链摘要
        let mut bare_hydrogen: Value = serde_json::from_str(r#"{"stream":true}"#).unwrap();
        prepare_upstream_body(
            GatewaySourceKind::AgentPlatform,
            "",
            "google/gemini-4-hydrogen",
            ThoughtRequestPolicy::FollowClient,
            &mut bare_hydrogen,
        );
        assert_eq!(
            bare_hydrogen["extra_body"]["google"]["thinking_config"]["include_thoughts"],
            true
        );

        // 客户端显式关闭（include_reasoning=false 或 reasoning_effort="none"）时不强加 thinking_config
        let mut off_req: Value =
            serde_json::from_str(r#"{"include_reasoning":false,"reasoning_effort":"medium"}"#)
                .unwrap();
        prepare_upstream_body(
            GatewaySourceKind::AgentPlatform,
            "",
            "google/gemini-4-hydrogen",
            ThoughtRequestPolicy::FollowClient,
            &mut off_req,
        );
        assert!(off_req["extra_body"]["google"].get("thinking_config").is_none());
        let off_safety = off_req["extra_body"]["google"]["safety_settings"]
            .as_array()
            .unwrap();
        assert_eq!(off_safety.len(), 4);
        assert!(off_safety.iter().all(|s| s["threshold"] == "OFF"));

        let mut none_req: Value =
            serde_json::from_str(r#"{"reasoning_effort":"none"}"#).unwrap();
        prepare_upstream_body(
            GatewaySourceKind::GeminiAiStudio,
            "",
            "gemini-3.5-flash",
            ThoughtRequestPolicy::FollowClient,
            &mut none_req,
        );
        assert!(none_req.get("extra_body").is_none());
    }

    #[test]
    fn agent_platform_sets_gemini_safety_settings_off_without_overriding_client() {
        // 1. AgentPlatform + 支持 thinking 的 Gemini 模型：同时写入 safety_settings 与 thinking_config
        let mut g38: Value = serde_json::from_str(r#"{"reasoning_effort":"high"}"#).unwrap();
        let prep38 = prepare_upstream_body(
            GatewaySourceKind::AgentPlatform,
            "",
            "google/gemini-3.8-flash",
            ThoughtRequestPolicy::FollowClient,
            &mut g38,
        );
        assert!(prep38.transforms.iter().any(|t| t == "set:safety_settings(off)"));
        let safety = g38["extra_body"]["google"]["safety_settings"]
            .as_array()
            .unwrap();
        assert_eq!(safety.len(), VERTEX_SAFETY_CATEGORIES.len());
        for (item, cat) in safety.iter().zip(VERTEX_SAFETY_CATEGORIES.iter()) {
            assert_eq!(item["category"], *cat);
            assert_eq!(item["threshold"], "OFF");
        }
        assert_eq!(
            g38["extra_body"]["google"]["thinking_config"]["thinking_level"],
            "high"
        );

        // 2. AgentPlatform + 不支持 thinking 的 Gemini 2.0 模型：仍写入 safety_settings
        let mut g20: Value = serde_json::from_str(r#"{"stream":true}"#).unwrap();
        prepare_upstream_body(
            GatewaySourceKind::AgentPlatform,
            "",
            "google/gemini-2.0-flash",
            ThoughtRequestPolicy::FollowClient,
            &mut g20,
        );
        assert_eq!(
            g20["extra_body"]["google"]["safety_settings"]
                .as_array()
                .unwrap()
                .len(),
            4
        );

        // 3. 调用方已显式传入 safety_settings 或 safetySettings 时不覆盖
        let mut explicit_snake: Value = serde_json::from_str(
            r#"{"extra_body":{"google":{"safety_settings":[{"category":"HARM_CATEGORY_HATE_SPEECH","threshold":"BLOCK_LOW_AND_ABOVE"}]}}}"#,
        )
        .unwrap();
        prepare_upstream_body(
            GatewaySourceKind::AgentPlatform,
            "",
            "google/gemini-3.8-flash",
            ThoughtRequestPolicy::FollowClient,
            &mut explicit_snake,
        );
        assert_eq!(
            explicit_snake["extra_body"]["google"]["safety_settings"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            explicit_snake["extra_body"]["google"]["safety_settings"][0]["threshold"],
            "BLOCK_LOW_AND_ABOVE"
        );

        let mut explicit_camel: Value = serde_json::from_str(
            r#"{"extra_body":{"google":{"safetySettings":[]}}}"#,
        )
        .unwrap();
        prepare_upstream_body(
            GatewaySourceKind::AgentPlatform,
            "",
            "google/gemini-3.8-flash",
            ThoughtRequestPolicy::FollowClient,
            &mut explicit_camel,
        );
        assert!(explicit_camel["extra_body"]["google"].get("safety_settings").is_none());
        assert!(explicit_camel["extra_body"]["google"]["safetySettings"]
            .as_array()
            .unwrap()
            .is_empty());

        // 4. AgentPlatform + 非 Gemini 模型不写入
        let mut non_gemini: Value = serde_json::from_str(r#"{"stream":true}"#).unwrap();
        prepare_upstream_body(
            GatewaySourceKind::AgentPlatform,
            "",
            "anthropic/claude-sonnet-4",
            ThoughtRequestPolicy::FollowClient,
            &mut non_gemini,
        );
        assert!(non_gemini.get("extra_body").is_none());

        // 5. GeminiAiStudio 不写入 safety_settings
        let mut aistudio: Value = serde_json::from_str(r#"{"stream":true}"#).unwrap();
        prepare_upstream_body(
            GatewaySourceKind::GeminiAiStudio,
            "",
            "gemini-2.5-flash",
            ThoughtRequestPolicy::FollowClient,
            &mut aistudio,
        );
        assert!(aistudio.get("extra_body").is_none());
    }

    #[test]
    fn repair_400_strips_only_safety_settings_and_preserves_thinking_config() {
        let err = r#"{"error":{"message":"Invalid JSON payload received. Unknown name \"safety_settings\" at 'extra_body.google': Cannot find field."}}"#;
        let params = repairable_params_on_400(GatewaySourceKind::AgentPlatform, err).unwrap();
        assert_eq!(params, vec!["extra_body.google.safety_settings"]);

        let mut both = json!({
            "model": "google/gemini-4-hydrogen",
            "extra_body": {
                "google": {
                    "safety_settings": [{"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "OFF"}],
                    "thinking_config": {"include_thoughts": true}
                }
            }
        });
        let removed = apply_400_repairs(&mut both, &params);
        assert_eq!(removed, vec!["extra_body.google.safety_settings"]);
        assert!(both["extra_body"]["google"].get("safety_settings").is_none());
        assert_eq!(
            both["extra_body"]["google"]["thinking_config"]["include_thoughts"],
            true
        );

        let mut only_safety = json!({
            "model": "google/gemini-2.0-flash",
            "extra_body": {
                "google": {
                    "safety_settings": [{"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "OFF"}]
                }
            }
        });
        let removed2 = apply_400_repairs(&mut only_safety, &params);
        assert_eq!(removed2, vec!["extra_body.google.safety_settings"]);
        assert!(only_safety.get("extra_body").is_none());
    }

    #[test]
    fn value_errors_never_remove_client_limits() {
        for param in ["max_completion_tokens", "stop", "frequency_penalty"] {
            let error = json!({"error":{"param":param,"message":"Invalid value: must be greater than zero"}}).to_string();
            assert!(repairable_params_on_400(GatewaySourceKind::OpenAiCompat, &error).is_none());
        }
    }

    #[test]
    fn repairable_params_detects_penalty_and_param_fields() {
        let p = repairable_params_on_400(
            GatewaySourceKind::AgentPlatform,
            r#"{"error":{"code":400,"message":"Penalty is not enabled for this model"}}"#,
        )
        .unwrap();
        assert_eq!(p, vec!["frequency_penalty", "presence_penalty"]);

        let p2 = repairable_params_on_400(
            GatewaySourceKind::OpenAiCompat,
            r#"{"error":{"message":"Unrecognized request argument: include_reasoning"}}"#,
        )
        .unwrap();
        assert_eq!(p2, vec!["include_reasoning"]);
    }
}
