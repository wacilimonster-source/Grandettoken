//! 客户端「通过本地网关接入」模式：把外部客户端指向 Grandettoken 本地网关，
//! 真实上游 API Key 始终留在 Windows 凭据管理器中。

use super::*;

#[derive(Debug, Clone)]
pub struct GatewayInjectSpec {
    pub base_url: String,
    pub anthropic_base_url: String,
    pub api_key: String,
    pub model_id: String,
    pub display_name: String,
    pub is_group: bool,
}

impl GatewayInjectSpec {
    pub fn new(port: u16, api_key: &str, model_id: &str, is_group: bool) -> Self {
        let base_url = format!("http://127.0.0.1:{port}/v1");
        let anthropic_base_url = format!("http://127.0.0.1:{port}");
        let key = if api_key.trim().is_empty() {
            "grandettoken-local".to_string()
        } else {
            api_key.trim().to_string()
        };
        Self {
            base_url,
            anthropic_base_url,
            api_key: key,
            model_id: model_id.trim().to_string(),
            display_name: format!("GW-{}", model_id.trim()),
            is_group,
        }
    }

    pub fn to_model_spec(&self) -> ModelSpec {
        ModelSpec {
            id: self.model_id.clone(),
            source: "gateway".to_string(),
            source_name: "Grandettoken 本地网关".to_string(),
            display_name: self.display_name.clone(),
            context_tokens: None,
            base_url: self.base_url.clone(),
            key: self.api_key.clone(),
            multimodal: true,
            api_protocol: Some(crate::models::ApiProtocol::OpenAiChatCompletions),
        }
    }

    pub fn to_cli_config(&self) -> CliTargetConfig {
        CliTargetConfig {
            model: self.model_id.clone(),
            display_name: self.display_name.clone(),
            base_url: self.base_url.clone(),
            anthropic_base_url: self.anthropic_base_url.clone(),
            api_key: self.api_key.clone(),
            source: "gateway".to_string(),
        }
    }
}

pub fn preview_gateway(
    client: Client,
    spec: &GatewayInjectSpec,
    opts: &Opts,
) -> Vec<Preview> {
    let model_spec = spec.to_model_spec();
    let mut previews = preview(client, &model_spec, opts);
    for p in &mut previews {
        // 网关接入模式写进外部客户端的是本地网关令牌，不是真实上游 API Key
        p.writes_secret = false;
        p.notes.insert(
            0,
            if spec.is_group {
                format!(
                    "本地网关接入（虚拟路由组）：真实上游密钥留在 Windows 凭据管理器，客户端仅写入 {} 与本地网关访问令牌。",
                    if client == Client::ClaudeCode {
                        &spec.anthropic_base_url
                    } else {
                        &spec.base_url
                    }
                )
            } else {
                format!(
                    "本地网关接入：真实上游密钥留在 Windows 凭据管理器，客户端仅写入 {} 与本地网关访问令牌。",
                    if client == Client::ClaudeCode {
                        &spec.anthropic_base_url
                    } else {
                        &spec.base_url
                    }
                )
            },
        );
        p.notes.push("每份备份均经 Windows DPAPI 加密，失败或回滚可一键还原。".into());
    }
    previews
}

pub fn apply_gateway(
    client: Client,
    spec: &GatewayInjectSpec,
    opts: &Opts,
) -> Result<Vec<Report>, String> {
    let model_spec = spec.to_model_spec();
    apply(client, &model_spec, opts)
}

pub fn remove_gateway(client: Client, spec: &GatewayInjectSpec) -> Result<Vec<Report>, String> {
    let model_spec = spec.to_model_spec();
    remove(client, &model_spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_inject_spec_never_exposes_upstream_secret_and_supports_workbuddy() {
        let root = std::env::temp_dir().join(format!("ts-gw-inj-{}", crate::vault::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("models.json"), "[]").unwrap();
        std::fs::write(root.join("settings.json"), "{}").unwrap();
        let site = Site {
            client: Client::Workbuddy,
            label: "WorkBuddy".into(),
            running: false,
            process: "WorkBuddy.exe".into(),
            root: root.clone(),
            files: vec![root.join("models.json"), root.join("settings.json")],
        };
        let gw_spec = GatewayInjectSpec::new(18771, "gw_local_access", "group/coding", true);
        let mspec = gw_spec.to_model_spec();
        assert_eq!(mspec.display_name, "GW-group/coding");
        let out = write_workbuddy_site(&site, &mspec, &Opts::default()).unwrap();
        assert_eq!(out.added, 1);
        let models = read_json(&root.join("models.json")).unwrap().unwrap();
        assert_eq!(models[0]["url"], "http://127.0.0.1:18771/v1");
        assert_eq!(models[0]["apiKey"], "gw_local_access");
        assert_eq!(models[0]["id"], "group/coding");
        let _ = std::fs::remove_dir_all(&root);
    }
}
