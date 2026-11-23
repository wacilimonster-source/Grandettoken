//! 客户端配置目录/文件发现（对齐 magpie `internal/agent` 的环境变量优先与文件优先级）。
//! 目录认环境变量（CLAUDE_CONFIG_DIR / CODEX_HOME / OPENCODE_CONFIG_DIR / XDG_CONFIG_HOME），
//! OpenCode 文件按 jsonc → json → config.json 取第一个存在的；都没有时新建 opencode.json。

use super::*;

pub(super) fn cli_running(
    client: Client,
    probed: &mut HashMap<String, bool>,
    is_running: &dyn Fn(&str, &mut HashMap<String, bool>) -> bool,
) -> (bool, String) {
    for p in client.processes() {
        if is_running(p, probed) {
            return (true, (*p).to_string());
        }
    }
    (false, client.process().to_string())
}

fn env_dir(key: &str) -> Option<PathBuf> {
    let v = std::env::var_os(key)?;
    let p = PathBuf::from(v);
    if p.is_absolute() {
        Some(p)
    } else {
        None
    }
}

pub(super) fn claude_root(h: &Path) -> PathBuf {
    env_dir("CLAUDE_CONFIG_DIR").unwrap_or_else(|| h.join(".claude"))
}

pub(super) fn codex_root(h: &Path) -> PathBuf {
    env_dir("CODEX_HOME").unwrap_or_else(|| h.join(".codex"))
}

pub(super) fn opencode_root(h: &Path) -> Option<PathBuf> {
    if let Some(d) = env_dir("OPENCODE_CONFIG_DIR") {
        return Some(d);
    }
    let xdg = env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| h.join(".config"));
    let mut candidates = vec![xdg.join("opencode")];
    if let Some(ad) = appdata() {
        candidates.push(ad.join("opencode"));
    }
    candidates.into_iter().find(|d| d.is_dir())
}

pub(super) fn opencode_config_file(root: &Path) -> PathBuf {
    for name in ["opencode.jsonc", "opencode.json", "config.json"] {
        let p = root.join(name);
        if p.exists() {
            return p;
        }
    }
    root.join("opencode.json")
}

pub(super) fn detect_cli_sites(
    probed: &mut HashMap<String, bool>,
    is_running: &dyn Fn(&str, &mut HashMap<String, bool>) -> bool,
) -> Vec<Site> {
    let mut out = Vec::new();
    let Some(h) = home() else {
        return out;
    };

    let claude_root = claude_root(&h);
    if claude_root.is_dir() {
        let (running, process) = cli_running(Client::ClaudeCode, probed, is_running);
        out.push(Site {
            client: Client::ClaudeCode,
            label: Client::ClaudeCode.label().to_string(),
            running,
            process,
            root: claude_root.clone(),
            files: vec![claude_root.join("settings.json")],
        });
    }

    let codex_root = codex_root(&h);
    if codex_root.is_dir() {
        let (running, process) = cli_running(Client::Codex, probed, is_running);
        out.push(Site {
            client: Client::Codex,
            label: Client::Codex.label().to_string(),
            running,
            process,
            root: codex_root.clone(),
            files: vec![codex_root.join("config.toml")],
        });
    }

    if let Some(oc_root) = opencode_root(&h) {
        let cfg_file = opencode_config_file(&oc_root);
        let (running, process) = cli_running(Client::OpenCode, probed, is_running);
        out.push(Site {
            client: Client::OpenCode,
            label: Client::OpenCode.label().to_string(),
            running,
            process,
            root: oc_root,
            files: vec![cfg_file],
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_file_priority_and_v2_shape() {
        let dir = std::env::temp_dir().join(format!("ts-oc-{}", crate::vault::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(opencode_config_file(&dir), dir.join("opencode.json"));
        std::fs::write(dir.join("config.json"), "{}").unwrap();
        assert_eq!(opencode_config_file(&dir), dir.join("config.json"));
        std::fs::write(dir.join("opencode.json"), "{}").unwrap();
        assert_eq!(opencode_config_file(&dir), dir.join("opencode.json"));
        std::fs::write(dir.join("opencode.jsonc"), "{}").unwrap();
        assert_eq!(opencode_config_file(&dir), dir.join("opencode.jsonc"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}