//! 现代编码 CLI（Claude Code / Codex / OpenCode）的本地配置文件读写、局部保注释编辑与校验回滚。
//!
//! 严格遵循四条安全前提：
//!   ① 目标进程未运行检查；
//!   ② 先建 Windows DPAPI 加密备份（含 `existed` 标记，新建文件回滚时会自动删掉）；
//!   ③ 只增改托管条目，保留用户原有注释与其他 provider/配置；
//!   ④ 写入后原子落盘并读回校验，不通过即自动回滚。

use super::*;
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct CliTargetConfig {
    pub model: String,
    pub display_name: String,
    pub base_url: String,
    pub anthropic_base_url: String,
    pub api_key: String,
    pub source: String,
}

impl CliTargetConfig {
    pub fn from_model_spec(spec: &ModelSpec) -> Self {
        Self {
            model: spec.id.clone(),
            display_name: spec.display_name.clone(),
            base_url: spec.base_url.clone(),
            anthropic_base_url: anthropic_root_url(&spec.base_url),
            api_key: spec.key.clone(),
            source: spec.source.clone(),
        }
    }
}

pub fn anthropic_root_url(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('/');
    s.strip_suffix("/v1").unwrap_or(s).to_string()
}

pub(super) fn is_local_gateway_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://127.0.0.1:")
        || lower.starts_with("http://localhost:")
        || lower.starts_with("http://[::1]:")
}

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

pub(super) fn detect_cli_sites(
    probed: &mut HashMap<String, bool>,
    is_running: &dyn Fn(&str, &mut HashMap<String, bool>) -> bool,
) -> Vec<Site> {
    let mut out = Vec::new();
    let Some(h) = home() else {
        return out;
    };

    let claude_root = h.join(".claude");
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

    let codex_root = h.join(".codex");
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

    let mut oc_candidates = vec![h.join(".config").join("opencode")];
    if let Some(ad) = appdata() {
        oc_candidates.push(ad.join("opencode"));
    }
    if let Some(oc_root) = oc_candidates.into_iter().find(|d| d.is_dir()) {
        let jsonc = oc_root.join("opencode.jsonc");
        let cfg_file = if jsonc.exists() {
            jsonc
        } else {
            oc_root.join("opencode.json")
        };
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

fn atomic_write_text(path: &Path, text: &str, bom: bool) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("建目录 {} 失败: {}", pretty(parent), os(&e)))?;
    }
    let mut final_text = String::with_capacity(text.len() + 3);
    if bom {
        final_text.push('\u{feff}');
    }
    final_text.push_str(text);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| format!("文件名非法: {}", pretty(path)))?;
    let tmp = path.with_file_name(format!("{name}.tmp-inject"));
    std::fs::write(&tmp, final_text.as_bytes()).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("写临时文件失败: {} {}", os(&e), pretty(&tmp))
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!(
            "替换 {} 失败: {e} {}(客户端在跑就先退出它)",
            pretty(path),
            os_hint(&e)
        )
    })
}

// ───────────────────────── Claude Code (~/.claude/settings.json) ─────────────────────────

pub(super) fn claude_site_readable(site: &Site) -> Result<(), String> {
    let p = site.root.join("settings.json");
    if let Some(v) = read_json(&p)? {
        if !v.is_object() {
            return Err("settings.json 根节点不是 JSON 对象".into());
        }
        if let Some(env) = v.get("env") {
            if !env.is_object() {
                return Err("settings.json 中的 env 不是 JSON 对象".into());
            }
        }
    }
    Ok(())
}

fn claude_matches_target(env: &Map<String, Value>, cfg: &CliTargetConfig) -> bool {
    let cur_url = env
        .get("ANTHROPIC_BASE_URL")
        .and_then(Value::as_str)
        .unwrap_or("");
    if cur_url.is_empty() {
        return false;
    }
    if cur_url == cfg.anthropic_base_url {
        return true;
    }
    if cfg.source == "gateway" && is_local_gateway_url(cur_url) {
        return true;
    }
    crate::models::source_of_url(cur_url) == Some(cfg.source.as_str())
}

pub(super) fn claude_preview(site: &Site, cfg: &CliTargetConfig, p: &mut Preview) {
    let path = site.root.join("settings.json");
    let root = read_json(&path).ok().flatten();
    let env = root.as_ref().and_then(|v| v.get("env")).and_then(Value::as_object);
    let same = env.is_some_and(|e| {
        e.get("ANTHROPIC_BASE_URL").and_then(Value::as_str) == Some(cfg.anthropic_base_url.as_str())
            && e.get("ANTHROPIC_AUTH_TOKEN").and_then(Value::as_str) == Some(cfg.api_key.as_str())
            && e.get("ANTHROPIC_MODEL").and_then(Value::as_str) == Some(cfg.model.as_str())
    });
    let ours = env.is_some_and(|e| claude_matches_target(e, cfg));
    p.action = if !path.exists() {
        "新建 settings.json 并写入网关环境配置".into()
    } else if same {
        "已配置,内容和这次要写的一致,无需改动".into()
    } else if ours {
        "更新 env 中的 ANTHROPIC_BASE_URL / AUTH_TOKEN / MODEL".into()
    } else {
        "局部合并 env 中的 ANTHROPIC_BASE_URL / AUTH_TOKEN / MODEL（保留其他配置）".into()
    };
    p.changed = !same;
    p.writes_selection = p.changed;
    p.configured = ours;
    if ours {
        let cur_m = env
            .and_then(|e| e.get("ANTHROPIC_MODEL"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let cur_u = env
            .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
            .and_then(Value::as_str)
            .unwrap_or("");
        p.removal = Some(Removal {
            what: format!("env 中的托管接入键（{cur_m} @ {cur_u}）"),
            clears_selection: true,
            count: 1,
        });
    }
}

pub(super) fn write_claude_code_site(site: &Site, cfg: &CliTargetConfig) -> Result<SiteOutcome, String> {
    let path = site.root.join("settings.json");
    let bom = path.exists() && has_bom(&path);
    let mut root = read_json(&path)?.unwrap_or_else(|| json!({}));
    let obj = root
        .as_object_mut()
        .ok_or_else(|| format!("{} 不是 JSON 对象", pretty(&path)))?;
    let prev_ours = obj
        .get("env")
        .and_then(Value::as_object)
        .is_some_and(|e| claude_matches_target(e, cfg));
    let same = obj.get("env").and_then(Value::as_object).is_some_and(|e| {
        e.get("ANTHROPIC_BASE_URL").and_then(Value::as_str) == Some(cfg.anthropic_base_url.as_str())
            && e.get("ANTHROPIC_AUTH_TOKEN").and_then(Value::as_str) == Some(cfg.api_key.as_str())
            && e.get("ANTHROPIC_MODEL").and_then(Value::as_str) == Some(cfg.model.as_str())
    });
    if same {
        return Ok(SiteOutcome {
            same: true,
            ..Default::default()
        });
    }
    merge_into(
        obj,
        "env",
        json!({
            "ANTHROPIC_BASE_URL": cfg.anthropic_base_url,
            "ANTHROPIC_AUTH_TOKEN": cfg.api_key,
            "ANTHROPIC_MODEL": cfg.model,
        }),
    );

    let stamp = stamp_now();
    let check_path = path.clone();
    let exp_url = cfg.anthropic_base_url.clone();
    let exp_tok = cfg.api_key.clone();
    let exp_mod = cfg.model.clone();
    let backup = commit_layered(
        site,
        &[path.clone()],
        Some(&["settings.json".to_string()]),
        &stamp,
        move || write_json(&path, &root, bom, 2),
        move || {
            read_json(&check_path)
                .ok()
                .flatten()
                .and_then(|v| v.get("env").cloned())
                .is_some_and(|e| {
                    e.get("ANTHROPIC_BASE_URL").and_then(Value::as_str) == Some(exp_url.as_str())
                        && e.get("ANTHROPIC_AUTH_TOKEN").and_then(Value::as_str)
                            == Some(exp_tok.as_str())
                        && e.get("ANTHROPIC_MODEL").and_then(Value::as_str) == Some(exp_mod.as_str())
                })
        },
    )?;

    Ok(SiteOutcome {
        added: usize::from(!prev_ours),
        updated: usize::from(prev_ours),
        selected: true,
        backup,
        notes: vec!["已写入 ~/.claude/settings.json（通过 Anthropic /v1/messages 入口接入）".into()],
        ..Default::default()
    })
}

pub(super) fn remove_claude_code_site(site: &Site, cfg: &CliTargetConfig) -> Result<SiteOutcome, String> {
    let path = site.root.join("settings.json");
    let Some(mut root) = read_json(&path)? else {
        return Ok(SiteOutcome::default());
    };
    let bom = has_bom(&path);
    let Some(obj) = root.as_object_mut() else {
        return Err(format!("{} 不是 JSON 对象", pretty(&path)));
    };
    let Some(env) = obj.get_mut("env").and_then(Value::as_object_mut) else {
        return Ok(SiteOutcome::default());
    };
    if !claude_matches_target(env, cfg) {
        return Ok(SiteOutcome::default());
    }
    env.remove("ANTHROPIC_BASE_URL");
    env.remove("ANTHROPIC_AUTH_TOKEN");
    env.remove("ANTHROPIC_MODEL");
    if env.is_empty() {
        obj.remove("env");
    }

    let stamp = stamp_now();
    let check_path = path.clone();
    let backup = commit_layered(
        site,
        &[path.clone()],
        Some(&["settings.json".to_string()]),
        &stamp,
        move || write_json(&path, &root, bom, 2),
        move || {
            read_json(&check_path)
                .ok()
                .flatten()
                .map(|v| {
                    v.get("env")
                        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
                        .is_none()
                })
                .unwrap_or(false)
        },
    )?;

    Ok(SiteOutcome {
        removed: 1,
        clears_selection: true,
        backup,
        ..Default::default()
    })
}

// ───────────────────────── Codex (~/.codex/config.toml) ─────────────────────────

const CODEX_START: &str = "# grandettoken-managed-start";
const CODEX_END: &str = "# grandettoken-managed-end";

fn toml_quote(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn toml_unquote(raw: &str) -> String {
    let s = raw.trim();
    if let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => break,
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    if let Some(inner) = s.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
        return inner.to_string();
    }
    s.to_string()
}

fn strip_toml_inline_comment(line: &str) -> (&str, &str) {
    let bytes = line.as_bytes();
    let mut in_dq = false;
    let mut in_sq = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if in_dq {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_dq = false;
            }
        } else if in_sq {
            if b == b'\'' {
                in_sq = false;
            }
        } else {
            match b {
                b'"' => in_dq = true,
                b'\'' => in_sq = true,
                b'#' => return (&line[..i], &line[i..]),
                _ => {}
            }
        }
        i += 1;
    }
    (line, "")
}

fn parse_toml_kv(line: &str) -> Option<(&str, String)> {
    let (code, _) = strip_toml_inline_comment(line);
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.starts_with('[') {
        return None;
    }
    let (k, v) = trimmed.split_once('=')?;
    Some((k.trim(), toml_unquote(v)))
}

fn is_toml_section_header(line: &str) -> Option<&str> {
    let (code, _) = strip_toml_inline_comment(line);
    let t = code.trim();
    if t.starts_with('[') && t.ends_with(']') {
        Some(t)
    } else {
        None
    }
}

pub fn codex_parse_state(raw: &str) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
    let clean = raw.trim_start_matches('\u{feff}');
    let mut top_provider = None;
    let mut top_model = None;
    let mut base_url = None;
    let mut token = None;
    let mut section = String::new();

    for line in clean.lines() {
        if let Some(hdr) = is_toml_section_header(line) {
            section = hdr.trim_matches(['[', ']']).trim().to_string();
            continue;
        }
        let Some((k, v)) = parse_toml_kv(line) else {
            continue;
        };
        if section.is_empty() {
            if k == "model_provider" {
                top_provider = Some(v);
            } else if k == "model" {
                top_model = Some(v);
            }
        } else if section == "model_providers.grandettoken" {
            if k == "base_url" {
                base_url = Some(v);
            } else if k == "experimental_bearer_token" {
                token = Some(v);
            }
        }
    }
    (top_provider, top_model, base_url, token)
}

fn upsert_top_toml_key(lines: &mut Vec<String>, key: &str, value: &str) {
    let first_section = lines
        .iter()
        .position(|l| is_toml_section_header(l).is_some())
        .unwrap_or(lines.len());
    for line in &mut lines[..first_section] {
        if let Some((k, _)) = parse_toml_kv(line) {
            if k == key {
                let (_, comment) = strip_toml_inline_comment(line);
                let suffix = if comment.is_empty() {
                    String::new()
                } else {
                    format!(" {comment}")
                };
                *line = format!("{key} = {}{suffix}", toml_quote(value));
                return;
            }
        }
    }
    lines.insert(first_section, format!("{key} = {}", toml_quote(value)));
}

pub fn codex_apply_toml(raw: &str, cfg: &CliTargetConfig) -> (String, bool) {
    let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let clean = raw.trim_start_matches('\u{feff}');
    let mut lines: Vec<String> = if clean.is_empty() {
        Vec::new()
    } else {
        clean.lines().map(str::to_string).collect()
    };

    upsert_top_toml_key(&mut lines, "model_provider", "grandettoken");
    upsert_top_toml_key(&mut lines, "model", &cfg.model);

    let block = vec![
        CODEX_START.to_string(),
        "[model_providers.grandettoken]".to_string(),
        "name = \"Grandettoken Gateway\"".to_string(),
        format!("base_url = {}", toml_quote(&cfg.base_url)),
        "wire_api = \"chat\"".to_string(),
        format!("experimental_bearer_token = {}", toml_quote(&cfg.api_key)),
        CODEX_END.to_string(),
    ];

    if let (Some(s), Some(e)) = (
        lines.iter().position(|l| l.trim() == CODEX_START),
        lines.iter().position(|l| l.trim() == CODEX_END),
    ) {
        if s <= e {
            lines.splice(s..=e, block);
        } else {
            lines.extend(block);
        }
    } else if let Some(hdr_idx) = lines
        .iter()
        .position(|l| is_toml_section_header(l) == Some("[model_providers.grandettoken]"))
    {
        let end_idx = lines[hdr_idx + 1..]
            .iter()
            .position(|l| is_toml_section_header(l).is_some())
            .map(|rel| hdr_idx + 1 + rel)
            .unwrap_or(lines.len());
        lines.splice(hdr_idx..end_idx, block);
    } else {
        if lines.last().is_some_and(|l| !l.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.extend(block);
    }

    let mut out = lines.join(eol);
    out.push_str(eol);
    let changed = out != clean;
    (out, changed)
}

pub fn codex_remove_toml(raw: &str) -> (String, usize) {
    let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let clean = raw.trim_start_matches('\u{feff}');
    let mut lines: Vec<String> = clean.lines().map(str::to_string).collect();
    let mut removed = 0usize;

    if let (Some(s), Some(e)) = (
        lines.iter().position(|l| l.trim() == CODEX_START),
        lines.iter().position(|l| l.trim() == CODEX_END),
    ) {
        if s <= e {
            lines.drain(s..=e);
            removed = 1;
        }
    } else if let Some(hdr_idx) = lines
        .iter()
        .position(|l| is_toml_section_header(l) == Some("[model_providers.grandettoken]"))
    {
        let end_idx = lines[hdr_idx + 1..]
            .iter()
            .position(|l| is_toml_section_header(l).is_some())
            .map(|rel| hdr_idx + 1 + rel)
            .unwrap_or(lines.len());
        lines.drain(hdr_idx..end_idx);
        removed = 1;
    }

    let first_section = lines
        .iter()
        .position(|l| is_toml_section_header(l).is_some())
        .unwrap_or(lines.len());
    let mut had_our_provider = false;
    let mut remove_indices = Vec::new();
    for (idx, line) in lines[..first_section].iter().enumerate() {
        if let Some((k, v)) = parse_toml_kv(line) {
            if k == "model_provider" && v == "grandettoken" {
                had_our_provider = true;
                remove_indices.push(idx);
            }
        }
    }
    if had_our_provider {
        for (idx, line) in lines[..first_section].iter().enumerate() {
            if let Some((k, _)) = parse_toml_kv(line) {
                if k == "model" {
                    remove_indices.push(idx);
                }
            }
        }
        removed = removed.max(1);
    }
    remove_indices.sort_unstable();
    remove_indices.dedup();
    for idx in remove_indices.into_iter().rev() {
        lines.remove(idx);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut out = lines.join(eol);
    if !out.is_empty() {
        out.push_str(eol);
    }
    (out, removed)
}

pub(super) fn codex_preview(site: &Site, cfg: &CliTargetConfig, p: &mut Preview) {
    let path = site.root.join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let (prov, model, url, tok) = codex_parse_state(&raw);
    let same = prov.as_deref() == Some("grandettoken")
        && model.as_deref() == Some(cfg.model.as_str())
        && url.as_deref() == Some(cfg.base_url.as_str())
        && tok.as_deref() == Some(cfg.api_key.as_str());
    let ours = prov.as_deref() == Some("grandettoken") || url.is_some();
    p.action = if !path.exists() {
        "新建 config.toml 并写入 grandettoken 提供方".into()
    } else if same {
        "已配置,内容和这次要写的一致,无需改动".into()
    } else if ours {
        "更新顶层 model 与 [model_providers.grandettoken]（保留原有注释）".into()
    } else {
        "设置 model_provider = \"grandettoken\" 并追加托管区块（保留原有注释）".into()
    };
    p.changed = !same;
    p.writes_selection = p.changed;
    p.configured = ours;
    if ours {
        p.removal = Some(Removal {
            what: format!(
                "[model_providers.grandettoken]（{}）",
                model.as_deref().unwrap_or("")
            ),
            clears_selection: prov.as_deref() == Some("grandettoken"),
            count: 1,
        });
    }
}

pub(super) fn write_codex_site(site: &Site, cfg: &CliTargetConfig) -> Result<SiteOutcome, String> {
    let path = site.root.join("config.toml");
    let bom = path.exists() && has_bom(&path);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("读 {} 失败: {e}", pretty(&path))),
    };
    let (prev_prov, _, prev_url, _) = codex_parse_state(&raw);
    let had_ours = prev_prov.as_deref() == Some("grandettoken") || prev_url.is_some();
    let (next, changed) = codex_apply_toml(&raw, cfg);
    if !changed {
        return Ok(SiteOutcome {
            same: true,
            ..Default::default()
        });
    }

    let stamp = stamp_now();
    let check_path = path.clone();
    let exp_model = cfg.model.clone();
    let exp_url = cfg.base_url.clone();
    let exp_tok = cfg.api_key.clone();
    let backup = commit_layered(
        site,
        &[path.clone()],
        Some(&["config.toml".to_string()]),
        &stamp,
        move || atomic_write_text(&path, &next, bom),
        move || {
            let Ok(txt) = std::fs::read_to_string(&check_path) else {
                return false;
            };
            let (p, m, u, t) = codex_parse_state(&txt);
            p.as_deref() == Some("grandettoken")
                && m.as_deref() == Some(exp_model.as_str())
                && u.as_deref() == Some(exp_url.as_str())
                && t.as_deref() == Some(exp_tok.as_str())
        },
    )?;

    Ok(SiteOutcome {
        added: usize::from(!had_ours),
        updated: usize::from(had_ours),
        selected: true,
        backup,
        notes: vec!["已更新 ~/.codex/config.toml（保留了全部注释与其余配置节）".into()],
        ..Default::default()
    })
}

pub(super) fn remove_codex_site(site: &Site) -> Result<SiteOutcome, String> {
    let path = site.root.join("config.toml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(SiteOutcome::default());
    };
    let bom = has_bom(&path);
    let (next, removed) = codex_remove_toml(&raw);
    if removed == 0 {
        return Ok(SiteOutcome::default());
    }
    let stamp = stamp_now();
    let check_path = path.clone();
    let backup = commit_layered(
        site,
        &[path.clone()],
        Some(&["config.toml".to_string()]),
        &stamp,
        move || atomic_write_text(&path, &next, bom),
        move || {
            let Ok(txt) = std::fs::read_to_string(&check_path) else {
                return false;
            };
            let (p, _, u, _) = codex_parse_state(&txt);
            p.as_deref() != Some("grandettoken") && u.is_none()
        },
    )?;

    Ok(SiteOutcome {
        removed,
        clears_selection: true,
        backup,
        ..Default::default()
    })
}

// ───────────────────────── OpenCode (opencode.jsonc / opencode.json) ─────────────────────────

pub fn strip_jsonc_comments_and_trailing_commas(raw: &str) -> String {
    let clean = raw.trim_start_matches('\u{feff}');
    let bytes = clean.as_bytes();
    let mut no_comments = String::with_capacity(clean.len());
    let mut i = 0usize;
    let mut in_str = false;

    while i < bytes.len() {
        if in_str {
            let ch = clean[i..].chars().next().unwrap_or('\0');
            if ch == '\\' {
                no_comments.push(ch);
                i += ch.len_utf8();
                if i < bytes.len() {
                    let next = clean[i..].chars().next().unwrap_or('\0');
                    no_comments.push(next);
                    i += next.len_utf8();
                }
                continue;
            }
            if ch == '"' {
                in_str = false;
            }
            no_comments.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if bytes[i] == b'"' {
            in_str = true;
            no_comments.push('"');
            i += 1;
            continue;
        }
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    no_comments.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        let ch = clean[i..].chars().next().unwrap_or('\0');
        no_comments.push(ch);
        i += ch.len_utf8();
    }

    // 剥离尾逗号
    let nb = no_comments.as_bytes();
    let mut out = String::with_capacity(no_comments.len());
    let mut j = 0usize;
    in_str = false;
    while j < nb.len() {
        if in_str {
            let ch = no_comments[j..].chars().next().unwrap_or('\0');
            if ch == '\\' {
                out.push(ch);
                j += ch.len_utf8();
                if j < nb.len() {
                    let next = no_comments[j..].chars().next().unwrap_or('\0');
                    out.push(next);
                    j += next.len_utf8();
                }
                continue;
            }
            if ch == '"' {
                in_str = false;
            }
            out.push(ch);
            j += ch.len_utf8();
            continue;
        }
        if nb[j] == b'"' {
            in_str = true;
            out.push('"');
            j += 1;
            continue;
        }
        if nb[j] == b',' {
            let mut k = j + 1;
            while k < nb.len() && nb[k].is_ascii_whitespace() {
                k += 1;
            }
            if k < nb.len() && matches!(nb[k], b'}' | b']') {
                j += 1;
                continue;
            }
        }
        let ch = no_comments[j..].chars().next().unwrap_or('\0');
        out.push(ch);
        j += ch.len_utf8();
    }
    out
}

fn split_leading_jsonc_comments(raw: &str) -> (&str, &str) {
    let clean = raw.trim_start_matches('\u{feff}');
    let bytes = clean.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            break;
        }
    }
    (&clean[..i], &clean[i..])
}

fn read_opencode_json(path: &Path) -> Result<Option<(String, Value)>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("读 {} 失败: {e}", pretty(path))),
    };
    let (header, _) = split_leading_jsonc_comments(&raw);
    let stripped = strip_jsonc_comments_and_trailing_commas(&raw);
    if stripped.trim().is_empty() {
        return Ok(None);
    }
    let val: Value = serde_json::from_str(&stripped)
        .map_err(|e| format!("解析 {} 失败(结构可能已变,本次不写): {e}", pretty(path)))?;
    if !val.is_object() {
        return Err(format!("{} 根节点不是 JSON 对象", pretty(path)));
    }
    Ok(Some((header.to_string(), val)))
}

fn opencode_file_name(site: &Site) -> String {
    site.files
        .first()
        .and_then(|f| f.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "opencode.json".into())
}

pub(super) fn opencode_site_readable(site: &Site) -> Result<(), String> {
    let fname = opencode_file_name(site);
    let _ = read_opencode_json(&site.root.join(fname))?;
    Ok(())
}

pub(super) fn opencode_preview(site: &Site, cfg: &CliTargetConfig, p: &mut Preview) {
    let fname = opencode_file_name(site);
    let path = site.root.join(&fname);
    let parsed = read_opencode_json(&path).ok().flatten().map(|(_, v)| v);
    let gt = parsed
        .as_ref()
        .and_then(|v| v.get("provider"))
        .and_then(|pr| pr.get("grandettoken"));
    let has_model = gt
        .and_then(|g| g.get("models"))
        .and_then(|m| m.get(&cfg.model))
        .is_some();
    let expected_sel = format!("grandettoken/{}", cfg.model);
    let same = gt.is_some_and(|g| {
        g.pointer("/options/baseURL").and_then(Value::as_str) == Some(cfg.base_url.as_str())
            && g.pointer("/options/apiKey").and_then(Value::as_str) == Some(cfg.api_key.as_str())
            && has_model
    }) && parsed
        .as_ref()
        .and_then(|v| v.get("model"))
        .and_then(Value::as_str)
        == Some(expected_sel.as_str());
    p.action = if !path.exists() {
        format!("新建 {fname} 并写入 provider.grandettoken")
    } else if same {
        "已配置,内容和这次要写的一致,无需改动".into()
    } else if gt.is_some() {
        "更新 provider.grandettoken 与选中模型".into()
    } else {
        "新增 provider.grandettoken 并设为当前模型（保留其他 provider）".into()
    };
    p.changed = !same;
    p.writes_selection = p.changed;
    p.configured = has_model;
    if has_model {
        p.removal = Some(Removal {
            what: format!("provider.grandettoken 下的模型「{}」", cfg.model),
            clears_selection: parsed
                .as_ref()
                .and_then(|v| v.get("model"))
                .and_then(Value::as_str)
                == Some(expected_sel.as_str()),
            count: 1,
        });
    }
    if fname.ends_with(".jsonc") {
        p.notes.push("会保留文件头部注释以及所有其他 provider 配置".into());
    }
}

pub(super) fn write_opencode_site(site: &Site, cfg: &CliTargetConfig) -> Result<SiteOutcome, String> {
    let fname = opencode_file_name(site);
    let path = site.root.join(&fname);
    let bom = path.exists() && has_bom(&path);
    let (header, mut root) = read_opencode_json(&path)?.unwrap_or_else(|| (String::new(), json!({})));
    let obj = root
        .as_object_mut()
        .ok_or_else(|| format!("{} 不是对象", pretty(&path)))?;

    let expected_sel = format!("grandettoken/{}", cfg.model);
    let prev_has = obj
        .get("provider")
        .and_then(|p| p.get("grandettoken"))
        .and_then(|g| g.get("models"))
        .and_then(|m| m.get(&cfg.model))
        .is_some();
    let same = obj
        .get("provider")
        .and_then(|p| p.get("grandettoken"))
        .is_some_and(|g| {
            g.pointer("/options/baseURL").and_then(Value::as_str) == Some(cfg.base_url.as_str())
                && g.pointer("/options/apiKey").and_then(Value::as_str)
                    == Some(cfg.api_key.as_str())
                && g.get("models").and_then(|m| m.get(&cfg.model)).is_some()
        })
        && obj.get("model").and_then(Value::as_str) == Some(expected_sel.as_str());
    if same {
        return Ok(SiteOutcome {
            same: true,
            ..Default::default()
        });
    }

    let providers = obj
        .entry("provider".to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("provider 不是 JSON 对象")?;
    let gt = providers
        .entry("grandettoken".to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("provider.grandettoken 不是 JSON 对象")?;
    gt.insert("npm".into(), json!("@ai-sdk/openai-compatible"));
    gt.insert("name".into(), json!("Grandettoken Gateway"));
    merge_into(
        gt,
        "options",
        json!({
            "baseURL": cfg.base_url,
            "apiKey": cfg.api_key,
        }),
    );
    let models = gt
        .entry("models".to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("provider.grandettoken.models 不是对象")?;
    models.insert(cfg.model.clone(), json!({ "name": cfg.display_name }));
    obj.insert("model".into(), json!(expected_sel));

    let body_json = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    let final_text = format!("{header}{body_json}\n");

    let stamp = stamp_now();
    let check_path = path.clone();
    let exp_url = cfg.base_url.clone();
    let exp_key = cfg.api_key.clone();
    let exp_model = cfg.model.clone();
    let backup = commit_layered(
        site,
        &[path.clone()],
        Some(&[fname]),
        &stamp,
        move || atomic_write_text(&path, &final_text, bom),
        move || {
            read_opencode_json(&check_path)
                .ok()
                .flatten()
                .is_some_and(|(_, v)| {
                    let g = v.pointer("/provider/grandettoken");
                    g.and_then(|x| x.pointer("/options/baseURL"))
                        .and_then(Value::as_str)
                        == Some(exp_url.as_str())
                        && g.and_then(|x| x.pointer("/options/apiKey"))
                            .and_then(Value::as_str)
                            == Some(exp_key.as_str())
                        && g.and_then(|x| x.get("models"))
                            .and_then(|m| m.get(&exp_model))
                            .is_some()
                        && v.get("model").and_then(Value::as_str)
                            == Some(format!("grandettoken/{exp_model}").as_str())
                })
        },
    )?;

    Ok(SiteOutcome {
        added: usize::from(!prev_has),
        updated: usize::from(prev_has),
        selected: true,
        backup,
        ..Default::default()
    })
}

pub(super) fn remove_opencode_site(site: &Site, cfg: &CliTargetConfig) -> Result<SiteOutcome, String> {
    let fname = opencode_file_name(site);
    let path = site.root.join(&fname);
    let Some((header, mut root)) = read_opencode_json(&path)? else {
        return Ok(SiteOutcome::default());
    };
    let bom = has_bom(&path);
    let obj = root
        .as_object_mut()
        .ok_or_else(|| format!("{} 不是对象", pretty(&path)))?;

    let mut removed = 0usize;
    let mut clears_selection = false;
    let mut left_models = 0usize;
    if let Some(gt) = obj
        .get_mut("provider")
        .and_then(|p| p.get_mut("grandettoken"))
        .and_then(Value::as_object_mut)
    {
        if let Some(models) = gt.get_mut("models").and_then(Value::as_object_mut) {
            if models.remove(&cfg.model).is_some() {
                removed = 1;
            }
            left_models = models.len();
        }
    }
    if removed == 0 {
        return Ok(SiteOutcome::default());
    }
    if left_models == 0 {
        if let Some(providers) = obj.get_mut("provider").and_then(Value::as_object_mut) {
            providers.remove("grandettoken");
        }
    }
    let expected_sel = format!("grandettoken/{}", cfg.model);
    if obj.get("model").and_then(Value::as_str) == Some(expected_sel.as_str()) {
        obj.remove("model");
        clears_selection = true;
    }

    let body_json = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    let final_text = format!("{header}{body_json}\n");
    let stamp = stamp_now();
    let check_path = path.clone();
    let exp_model = cfg.model.clone();
    let backup = commit_layered(
        site,
        &[path.clone()],
        Some(&[fname]),
        &stamp,
        move || atomic_write_text(&path, &final_text, bom),
        move || {
            read_opencode_json(&check_path)
                .ok()
                .flatten()
                .is_some_and(|(_, v)| {
                    v.pointer("/provider/grandettoken/models")
                        .and_then(Value::as_object)
                        .is_none_or(|m| !m.contains_key(&exp_model))
                })
        },
    )?;

    Ok(SiteOutcome {
        removed,
        clears_selection,
        backup,
        ..Default::default()
    })
}

// ───────────────────────── 状态读取 ─────────────────────────

pub(super) fn cli_read_state(client: Client, mine: &[&Site], st: &mut ClientState) {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    match client {
        Client::ClaudeCode => {
            for s in mine {
                if let Ok(Some(v)) = read_json(&s.root.join("settings.json")) {
                    if let Some(env) = v.get("env").and_then(Value::as_object) {
                        let url = env
                            .get("ANTHROPIC_BASE_URL")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let model = env
                            .get("ANTHROPIC_MODEL")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if !model.is_empty() {
                            if st.selected.is_none() {
                                st.selected = Some(model.to_string());
                            }
                            let src = if is_local_gateway_url(url) {
                                "gateway".to_string()
                            } else {
                                crate::models::source_of_url(url)
                                    .unwrap_or("")
                                    .to_string()
                            };
                            if !src.is_empty() && seen.insert((model.to_string(), src.clone())) {
                                st.models.push(ConfiguredModel {
                                    id: model.to_string(),
                                    source: src,
                                });
                            }
                        }
                    }
                }
            }
            st.note = Some("支持通过本地网关 /v1/messages 入口接入（支持路由组与容灾）".into());
        }
        Client::Codex => {
            for s in mine {
                if let Ok(raw) = std::fs::read_to_string(s.root.join("config.toml")) {
                    let (prov, model, url, _) = codex_parse_state(&raw);
                    if st.selected.is_none() {
                        st.selected = model.clone();
                    }
                    if prov.as_deref() == Some("grandettoken") {
                        if let Some(m) = model {
                            let src = url
                                .as_deref()
                                .map(|u| {
                                    if is_local_gateway_url(u) {
                                        "gateway".to_string()
                                    } else {
                                        crate::models::source_of_url(u)
                                            .unwrap_or("")
                                            .to_string()
                                    }
                                })
                                .unwrap_or_else(|| "gateway".into());
                            if seen.insert((m.clone(), src.clone())) {
                                st.models.push(ConfiguredModel { id: m, source: src });
                            }
                        }
                    }
                }
            }
            st.note = Some("行级局部修改 config.toml，保留原注释与其他 provider".into());
        }
        Client::OpenCode => {
            for s in mine {
                let fname = opencode_file_name(s);
                if let Ok(Some((_, v))) = read_opencode_json(&s.root.join(fname)) {
                    if st.selected.is_none() {
                        st.selected = v.get("model").and_then(Value::as_str).map(String::from);
                    }
                    if let Some(gt) = v.pointer("/provider/grandettoken") {
                        let url = gt
                            .pointer("/options/baseURL")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let src = if is_local_gateway_url(url) {
                            "gateway".to_string()
                        } else {
                            crate::models::source_of_url(url)
                                .unwrap_or("gateway")
                                .to_string()
                        };
                        if let Some(models) = gt.get("models").and_then(Value::as_object) {
                            for id in models.keys() {
                                if seen.insert((id.clone(), src.clone())) {
                                    st.models.push(ConfiguredModel {
                                        id: id.clone(),
                                        source: src.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
            st.note = Some("支持 opencode.jsonc / opencode.json 保注释写入".into());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_site(root: &Path, client: Client, file: &str) -> Site {
        Site {
            client,
            label: client.label().into(),
            running: false,
            process: client.process().into(),
            root: root.to_path_buf(),
            files: vec![root.join(file)],
        }
    }

    fn gw_cfg(model: &str) -> CliTargetConfig {
        CliTargetConfig {
            model: model.into(),
            display_name: format!("GW-{model}"),
            base_url: "http://127.0.0.1:18771/v1".into(),
            anthropic_base_url: "http://127.0.0.1:18771".into(),
            api_key: "gw_local_token".into(),
            source: "gateway".into(),
        }
    }

    #[test]
    fn claude_code_creates_merges_and_rolls_back_missing_file() {
        let root = std::env::temp_dir().join(format!("ts-cc-{}", crate::vault::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let site = cli_site(&root, Client::ClaudeCode, "settings.json");
        let cfg = gw_cfg("group/coding");

        // 新建文件时写入 + 回滚会干净删掉文件
        let out = write_claude_code_site(&site, &cfg).unwrap();
        assert_eq!(out.added, 1);
        let written = read_json(&root.join("settings.json")).unwrap().unwrap();
        assert_eq!(written["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:18771");
        assert_eq!(written["env"]["ANTHROPIC_MODEL"], "group/coding");
        restore(&out.backup).unwrap();
        assert!(!root.join("settings.json").exists(), "新建文件回滚应删除");

        // 原有其他配置与 env 变量必须保留
        std::fs::write(
            root.join("settings.json"),
            r#"{"theme":"dark","env":{"KEEP_VAR":"yes"}}"#,
        )
        .unwrap();
        let out2 = write_claude_code_site(&site, &cfg).unwrap();
        let after = read_json(&root.join("settings.json")).unwrap().unwrap();
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["env"]["KEEP_VAR"], "yes");
        let rm = remove_claude_code_site(&site, &cfg).unwrap();
        assert_eq!(rm.removed, 1);
        let after_rm = read_json(&root.join("settings.json")).unwrap().unwrap();
        assert_eq!(after_rm["env"]["KEEP_VAR"], "yes");
        assert!(after_rm["env"].get("ANTHROPIC_BASE_URL").is_none());
        let _ = out2;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn codex_toml_preserves_comments_other_providers_and_cleans_up() {
        let root = std::env::temp_dir().join(format!("ts-codex-{}", crate::vault::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let orig = concat!(
            "# 顶部个人注释\n",
            "model_provider = \"openai\" # 保留行内注释\n",
            "model = \"gpt-5\"\n\n",
            "[model_providers.custom_other]\n",
            "name = \"Other\"\n",
            "base_url = \"https://other.example/v1\"\n",
        );
        std::fs::write(root.join("config.toml"), orig).unwrap();
        let site = cli_site(&root, Client::Codex, "config.toml");
        let cfg = gw_cfg("group/coding");

        let out = write_codex_site(&site, &cfg).unwrap();
        assert_eq!(out.added, 1);
        let text = std::fs::read_to_string(root.join("config.toml")).unwrap();
        assert!(text.contains("# 顶部个人注释"));
        assert!(text.contains("# 保留行内注释"));
        assert!(text.contains("[model_providers.custom_other]"));
        assert!(text.contains("https://other.example/v1"));
        assert!(text.contains("base_url = \"http://127.0.0.1:18771/v1\""));
        let (p, m, u, t) = codex_parse_state(&text);
        assert_eq!(p.as_deref(), Some("grandettoken"));
        assert_eq!(m.as_deref(), Some("group/coding"));
        assert_eq!(u.as_deref(), Some("http://127.0.0.1:18771/v1"));
        assert_eq!(t.as_deref(), Some("gw_local_token"));

        let rm = remove_codex_site(&site).unwrap();
        assert_eq!(rm.removed, 1);
        let after = std::fs::read_to_string(root.join("config.toml")).unwrap();
        assert!(after.contains("[model_providers.custom_other]"));
        assert!(!after.contains("grandettoken-managed-start"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn opencode_jsonc_preserves_leading_comments_and_other_providers() {
        let root = std::env::temp_dir().join(format!("ts-oc-{}", crate::vault::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let orig = concat!(
            "// OpenCode 用户配置\n",
            "/* 保留头部说明 */\n",
            "{\n",
            "  \"provider\": {\n",
            "    // 其他服务商\n",
            "    \"deepseek\": { \"options\": { \"apiKey\": \"ds-keep\", }, },\n",
            "  },\n",
            "}\n",
        );
        std::fs::write(root.join("opencode.jsonc"), orig).unwrap();
        let site = cli_site(&root, Client::OpenCode, "opencode.jsonc");
        let cfg = gw_cfg("group/coding");

        let out = write_opencode_site(&site, &cfg).unwrap();
        assert_eq!(out.added, 1);
        let raw = std::fs::read_to_string(root.join("opencode.jsonc")).unwrap();
        assert!(raw.starts_with("// OpenCode 用户配置"));
        assert!(raw.contains("/* 保留头部说明 */"));
        let (_, val) = read_opencode_json(&root.join("opencode.jsonc")).unwrap().unwrap();
        assert_eq!(val["provider"]["deepseek"]["options"]["apiKey"], "ds-keep");
        assert_eq!(
            val["provider"]["grandettoken"]["options"]["baseURL"],
            "http://127.0.0.1:18771/v1"
        );
        assert_eq!(val["model"], "grandettoken/group/coding");

        let rm = remove_opencode_site(&site, &cfg).unwrap();
        assert_eq!(rm.removed, 1);
        let (_, val_after) = read_opencode_json(&root.join("opencode.jsonc")).unwrap().unwrap();
        assert!(val_after["provider"].get("grandettoken").is_none());
        assert_eq!(val_after["provider"]["deepseek"]["options"]["apiKey"], "ds-keep");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn opencode_desktop_cli_process_also_blocks_writing() {
        let only_desktop = |p: &str, cache: &mut HashMap<String, bool>| {
            *cache.entry(p.to_string()).or_insert_with(|| p == "opencode-cli.exe")
        };
        let mut probed = HashMap::new();
        let (running, name) = cli_running(Client::OpenCode, &mut probed, &only_desktop);
        assert!(running, "桌面版 opencode-cli.exe 在跑时必须判「在跑」");
        assert_eq!(name, "opencode-cli.exe");
        let site = Site {
            client: Client::OpenCode,
            label: Client::OpenCode.label().into(),
            running,
            process: name,
            root: std::env::temp_dir(),
            files: vec![],
        };
        assert!(blocked_by_running(&site).is_some());

        let none = |p: &str, cache: &mut HashMap<String, bool>| {
            *cache.entry(p.to_string()).or_insert(false)
        };
        let mut probed2 = HashMap::new();
        let (idle, fallback) = cli_running(Client::OpenCode, &mut probed2, &none);
        assert!(!idle, "两个进程名都没命中才算空闲");
        assert_eq!(fallback, "opencode.exe");
        assert!(Client::OpenCode.processes().contains(&"opencode.exe"));
    }
}
