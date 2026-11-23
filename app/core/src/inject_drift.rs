//! 漂移检测（对齐 magpie `Agent.Check`）：模型还是我们的、但接线被别处改写时发现。
//! 修复走既有「预览 → 确认写入」流程（Sync 只补我们写过的块），不自动改文件。

use super::*;
use super::inject_codex::codex_parse_state;

pub fn drift_all() -> Vec<(String, String, String)> {
    let mut probed: HashMap<String, bool> = HashMap::new();
    let is_running = |p: &str, probed: &mut HashMap<String, bool>| {
        *probed.entry(p.to_string()).or_insert_with(|| process_running(p))
    };
    let sites = inject_discover::detect_cli_sites(&mut probed, &is_running);
    let mut out = Vec::new();
    for site in sites {
        if let Some(msg) = check(site.client, &site) {
            out.push((site.client.id().to_string(), site.label.clone(), msg));
        }
    }
    out
}

pub(super) fn check(client: Client, site: &Site) -> Option<String> {
    let wired = inject_stash::wired_model(client.id());
    match client {
        Client::ClaudeCode => {
            let v = read_json(&site.root.join("settings.json")).ok().flatten()?;
            let env = v.get("env").and_then(Value::as_object)?;
            let url = env.get("ANTHROPIC_BASE_URL").and_then(Value::as_str).unwrap_or("");
            if !is_local_gateway_url(url) {
                return None;
            }
            let top = v.get("model").and_then(Value::as_str).unwrap_or("");
            let tier = env
                .get("ANTHROPIC_DEFAULT_OPUS_MODEL")
                .and_then(Value::as_str)
                .unwrap_or("");
            if top.is_empty() || tier.is_empty() {
                Some("BASE_URL 仍指向网关，但顶层 model / 分层 env 被外部改写；重新写入可修复".into())
            } else {
                None
            }
        }
        Client::Codex => {
            let raw = std::fs::read_to_string(site.root.join("config.toml")).ok()?;
            let (prov, model, url, _) = codex_parse_state(&raw);
            if prov.as_deref() == Some("grandettoken") && url.is_none() {
                return Some("model_provider 指向网关但托管表被删；重新写入可修复".into());
            }
            if let Some(w) = wired {
                if model.as_deref() == Some(w.as_str()) && prov.as_deref() != Some("grandettoken") {
                    return Some("model 仍是接入时的模型，但 model_provider 被外部改走；重新写入可修复".into());
                }
            }
            None
        }
        Client::OpenCode => {
            let path = site.files.first()?;
            let (_, v) = read_opencode_json(path).ok().flatten()?;
            let sel = v.get("model").and_then(Value::as_str).unwrap_or("");
            if sel.starts_with("grandettoken/") && oc_gt_any(&v).is_none() {
                Some("选中项仍指向网关 provider，但 provider 块被外部删除；重新写入可修复".into())
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drift_detects_rewritten_wiring_only_when_ours() {
        let dir = std::env::temp_dir().join(format!("ts-drift-{}", crate::vault::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let site = Site {
            client: Client::ClaudeCode,
            label: "Claude Code".into(),
            running: false,
            process: "claude.exe".into(),
            root: dir.clone(),
            files: vec![dir.join("settings.json")],
        };
        // 不是我们的接线 → 不报漂移
        std::fs::write(dir.join("settings.json"), r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.anthropic.com"}}"#).unwrap();
        assert!(check(Client::ClaudeCode, &site).is_none());
        // 是我们的 BASE_URL 但顶层 model 被删 → 报漂移
        std::fs::write(
            dir.join("settings.json"),
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:18771"}}"#,
        )
        .unwrap();
        assert!(check(Client::ClaudeCode, &site).is_some());
        // 补回顶层 model 与分层 env → 不报
        std::fs::write(
            dir.join("settings.json"),
            r#"{"model":"group/x","env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:18771","ANTHROPIC_DEFAULT_OPUS_MODEL":"group/x"}}"#,
        )
        .unwrap();
        assert!(check(Client::ClaudeCode, &site).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}