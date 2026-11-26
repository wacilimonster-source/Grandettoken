//! 现代编码 CLI（Claude Code / Codex / OpenCode）的本地配置文件读写、局部保注释编辑与校验回滚。
//!
//! 严格遵循四条安全前提：
//!   ① 目标进程未运行检查；
//!   ② 先建 Windows DPAPI 加密备份（含 `existed` 标记，新建文件回滚时会自动删掉）；
//!   ③ 只增改托管条目，保留用户原有注释与其他 provider/配置；
//!   ④ 写入后原子落盘并读回校验，不通过即自动回滚。

use super::*;
use super::inject_codex::{
    codex_apply_toml, codex_drop_top_key, codex_parse_state, codex_remove_toml, codex_set_top_key,
    codex_strip_table,
};
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
    if let Some(sw) = claude_cloud_switch(site, &root) {
        p.blocked = Some(format!(
            "检测到 {sw} 开关：Claude Code 会走该云厂商而忽略 ANTHROPIC_BASE_URL，先关掉它再接入"
        ));
        p.changed = false;
        return;
    }
    let same = env.is_some_and(|e| claude_env_same(e, &root, cfg));
    let ours = env.is_some_and(|e| claude_matches_target(e, cfg));
    p.action = if !path.exists() {
        "新建 settings.json 并写入网关环境配置".into()
    } else if same {
        "已配置,内容和这次要写的一致,无需改动".into()
    } else if ours {
        "更新顶层 model 与 env 中的 BASE_URL / AUTH_TOKEN / 分层模型".into()
    } else {
        "局部合并顶层 model 与 env 中的 BASE_URL / AUTH_TOKEN / 分层模型（保留其他配置）".into()
    };
    p.changed = !same;
    p.writes_selection = p.changed;
    p.configured = ours;
    if ours {
        let cur_m = root
            .as_ref()
            .and_then(|v| v.get("model"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let cur_u = env
            .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
            .and_then(Value::as_str)
            .unwrap_or("");
        p.removal = Some(Removal {
            what: format!("顶层 model 与 env 中的托管接入键（{cur_m} @ {cur_u}），并还原接入前的原值"),
            clears_selection: true,
            count: 1,
        });
    }
}

/// magpie 口径：不写 ANTHROPIC_MODEL（它压掉 /model 的选择），写顶层 model + 分层 env。
const CLAUDE_TIER_ENV: [&str; 3] = [
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];
const CLAUDE_EXTRA_ENV: [&str; 2] = ["ANTHROPIC_SMALL_FAST_MODEL", "CLAUDE_CODE_SUBAGENT_MODEL"];
const CLAUDE_CLOUD_SWITCHES: [&str; 6] = [
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
    "CLAUDE_CODE_USE_MANTLE",
];

fn claude_switch_on(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        _ => false,
    }
}

fn claude_cloud_on(env: &Map<String, Value>) -> Option<&'static str> {
    CLAUDE_CLOUD_SWITCHES
        .iter()
        .copied()
        .find(|k| env.get(*k).is_some_and(claude_switch_on))
}

/// Claude Code 的全局配置：`CLAUDE_CONFIG_DIR` 下是 `.claude.json`，
/// 否则是家目录的 `.claude.json`（magpie `claudeGlobal` 同口径）。
fn claude_global_paths(site: &Site) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(parent) = site.root.parent() {
        out.push(parent.join(".claude.json"));
    }
    out.push(site.root.join(".claude.json"));
    out
}

/// 云开关只要出现在 settings.json 或全局 `.claude.json` 的 env 里就会生效
/// （后者会被 Claude Code 覆盖到环境上），任一命中都拒绝接入。
fn claude_cloud_switch(site: &Site, root: &Option<Value>) -> Option<&'static str> {
    if let Some(sw) = root
        .as_ref()
        .and_then(|v| v.get("env"))
        .and_then(Value::as_object)
        .and_then(claude_cloud_on)
    {
        return Some(sw);
    }
    for p in claude_global_paths(site) {
        if let Ok(Some(v)) = read_json(&p) {
            if let Some(sw) = v.get("env").and_then(Value::as_object).and_then(claude_cloud_on) {
                return Some(sw);
            }
        }
    }
    None
}

fn claude_env_same(env: &Map<String, Value>, root: &Option<Value>, cfg: &CliTargetConfig) -> bool {
    env.get("ANTHROPIC_BASE_URL").and_then(Value::as_str) == Some(cfg.anthropic_base_url.as_str())
        && env.get("ANTHROPIC_AUTH_TOKEN").and_then(Value::as_str) == Some(cfg.api_key.as_str())
        && root.as_ref().and_then(|v| v.get("model")).and_then(Value::as_str) == Some(cfg.model.as_str())
        && CLAUDE_TIER_ENV
            .iter()
            .chain(CLAUDE_EXTRA_ENV.iter())
            .all(|k| env.get(*k).and_then(Value::as_str) == Some(cfg.model.as_str()))
}

pub(super) fn write_claude_code_site(site: &Site, cfg: &CliTargetConfig) -> Result<SiteOutcome, String> {
    let path = site.root.join("settings.json");
    let bom = path.exists() && has_bom(&path);
    let mut root = read_json(&path)?.unwrap_or_else(|| json!({}));
    let before = root.clone();
    let obj = root
        .as_object_mut()
        .ok_or_else(|| format!("{} 不是 JSON 对象", pretty(&path)))?;
    if let Some(sw) = claude_cloud_switch(site, &Some(before.clone())) {
        return Err(format!("检测到 {sw} 开关，BASE_URL 会被忽略；先关掉它再接入"));
    }
    let prev_ours = obj
        .get("env")
        .and_then(Value::as_object)
        .is_some_and(|e| claude_matches_target(e, cfg));
    let same = obj
        .get("env")
        .and_then(Value::as_object)
        .is_some_and(|e| claude_env_same(e, &Some(before.clone()), cfg));
    if same {
        return Ok(SiteOutcome {
            same: true,
            ..Default::default()
        });
    }
    if !prev_ours {
        // 值级 stash（5C）：用户自己的 model / 分层 env / **原来的 endpoint** 都要还回去。
        // 只存非密钥值 —— AUTH_TOKEN 不记，断开时按「删掉」处理。
        let mut refs: Vec<String> = vec![
            "model".into(),
            "env.ANTHROPIC_MODEL".into(),
            "env.ANTHROPIC_BASE_URL".into(),
        ];
        for k in CLAUDE_TIER_ENV.iter().chain(CLAUDE_EXTRA_ENV.iter()) {
            refs.push(format!("env.{k}"));
        }
        let r: Vec<&str> = refs.iter().map(|s| s.as_str()).collect();
        inject_stash::capture(
            Client::ClaudeCode.id(),
            inject_stash::snapshot_json(Some(&before), &r),
        );
    }
    let mut managed = json!({
        "ANTHROPIC_BASE_URL": cfg.anthropic_base_url,
        "ANTHROPIC_AUTH_TOKEN": cfg.api_key,
    });
    for k in CLAUDE_TIER_ENV.iter().chain(CLAUDE_EXTRA_ENV.iter()) {
        managed[k] = json!(cfg.model);
    }
    merge_into(obj, "env", managed);
    if let Some(env) = obj.get_mut("env").and_then(Value::as_object_mut) {
        env.remove("ANTHROPIC_MODEL");
    }
    obj.insert("model".into(), json!(cfg.model));

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
                .is_some_and(|v| {
                    v.get("model").and_then(Value::as_str) == Some(exp_mod.as_str())
                        && v.get("env").is_some_and(|e| {
                            e.get("ANTHROPIC_BASE_URL").and_then(Value::as_str) == Some(exp_url.as_str())
                                && e.get("ANTHROPIC_AUTH_TOKEN").and_then(Value::as_str)
                                    == Some(exp_tok.as_str())
                        })
                })
        },
    )?;

    inject_stash::note_wired(Client::ClaudeCode.id(), &cfg.model);
    Ok(SiteOutcome {
        added: usize::from(!prev_ours),
        updated: usize::from(prev_ours),
        selected: true,
        backup,
        notes: vec!["已写入 settings.json 顶层 model 与分层 env（不写 ANTHROPIC_MODEL，/model 选择不再被压）".into()],
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
    let ours = obj
        .get("env")
        .and_then(Value::as_object)
        .is_some_and(|e| claude_matches_target(e, cfg));
    if !ours {
        return Ok(SiteOutcome::default());
    }
    if let Some(env) = obj.get_mut("env").and_then(Value::as_object_mut) {
        env.remove("ANTHROPIC_BASE_URL");
        env.remove("ANTHROPIC_AUTH_TOKEN");
        for k in CLAUDE_TIER_ENV.iter().chain(CLAUDE_EXTRA_ENV.iter()) {
            env.remove(*k);
        }
    }
    let stash = inject_stash::take(Client::ClaudeCode.id());
    inject_stash::restore_json(obj, &stash);
    if obj.get("env").is_some_and(|e| e.as_object().is_some_and(Map::is_empty)) {
        obj.remove("env");
    }

    let stamp = stamp_now();
    let check_path = path.clone();
    // 断开后 BASE_URL 要么是用户原来的、要么不存在 —— 只要不再是我们写的那个就算过
    let exp_url = cfg.anthropic_base_url.clone();
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
                        .and_then(Value::as_str)
                        != Some(exp_url.as_str())
                })
                .unwrap_or(false)
        },
    )?;

    inject_stash::clear_wired(Client::ClaudeCode.id());
    Ok(SiteOutcome {
        removed: 1,
        clears_selection: true,
        backup,
        notes: vec!["已移除托管键并还原接入前的 model / env 原值".into()],
        ..Default::default()
    })
}

// ───────────────────────── Codex (~/.codex/config.toml) ─────────────────────────

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
                "顶层 model_provider 与 model（{}），还原接入前原值；托管表保留供旧线程打开",
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
    if !had_ours {
        let mut vals = inject_stash::Stash::new();
        vals.insert(
            "model_provider".into(),
            prev_prov.map(Value::String).unwrap_or(Value::Null),
        );
        let (_, prev_model, _, _) = codex_parse_state(&raw);
        vals.insert("model".into(), prev_model.map(Value::String).unwrap_or(Value::Null));
        inject_stash::capture(Client::Codex.id(), vals);
    }
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

    inject_stash::note_wired(Client::Codex.id(), &cfg.model);
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
    // 先按 stash 还原顶层原值（保留行内注释），再摘我们的 model_provider；托管表保留。
    // 这里只 peek：文件里没有我们的东西时不能把原值取走（否则下次再也还原不了）。
    let stash = inject_stash::peek(Client::Codex.id());
    let mut next = raw.clone();
    for key in ["model_provider", "model"] {
        match stash.get(key) {
            Some(Value::String(s)) => next = codex_set_top_key(&next, key, s),
            Some(Value::Null) => next = codex_drop_top_key(&next, key),
            _ => {
                if key == "model" {
                    next = codex_drop_top_key(&next, key);
                }
            }
        }
    }
    let (next, removed) = codex_remove_toml(&next);
    if removed == 0 {
        // 没有我们的东西：不动文件，stash 留在原处供下次使用
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
            let (p, _, _, _) = codex_parse_state(&txt);
            p.as_deref() != Some("grandettoken")
        },
    )?;

    inject_stash::take(Client::Codex.id());
    inject_stash::clear_wired(Client::Codex.id());
    Ok(SiteOutcome {
        removed,
        clears_selection: true,
        backup,
        notes: vec!["已摘顶层 model_provider 并还原接入前原值；托管表保留（旧线程仍可打开）".into()],
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

pub(super) fn read_opencode_json(path: &Path) -> Result<Option<(String, Value)>, String> {
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

/// OpenCode 1 用 `provider`、2 用 `providers`；按文件实际形状认，
/// 两者都没有时两个都写（各版本只读自己那个）。
fn oc_containers(obj: &Map<String, Value>) -> Vec<&'static str> {
    let mut v = Vec::new();
    if obj.get("provider").is_some_and(Value::is_object) {
        v.push("provider");
    }
    if obj.get("providers").is_some_and(Value::is_object) {
        v.push("providers");
    }
    if v.is_empty() {
        v.push("provider");
        v.push("providers");
    }
    v
}

fn oc_gt<'a>(v: &'a Value, container: &str) -> Option<&'a Value> {
    v.get(container).and_then(|p| p.get("grandettoken"))
}

pub(super) fn oc_gt_any(v: &Value) -> Option<&Value> {
    oc_gt(v, "provider").or_else(|| oc_gt(v, "providers"))
}

fn oc_gt_same(g: &Value, cfg: &CliTargetConfig) -> bool {
    g.pointer("/options/baseURL").and_then(Value::as_str) == Some(cfg.base_url.as_str())
        && g.pointer("/options/apiKey").and_then(Value::as_str) == Some(cfg.api_key.as_str())
        && g.get("models").and_then(|m| m.get(&cfg.model)).is_some()
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
    let gt = parsed.as_ref().and_then(oc_gt_any);
    let has_model = gt
        .and_then(|g| g.get("models"))
        .and_then(|m| m.get(&cfg.model))
        .is_some();
    let expected_sel = format!("grandettoken/{}", cfg.model);
    let same = gt.is_some_and(|g| oc_gt_same(g, cfg))
        && parsed
            .as_ref()
            .and_then(|v| v.get("model"))
            .and_then(Value::as_str)
            == Some(expected_sel.as_str());
    p.action = if !path.exists() {
        format!("新建 {fname} 并写入 provider/providers.grandettoken")
    } else if same {
        "已配置,内容和这次要写的一致,无需改动".into()
    } else if gt.is_some() {
        "更新 provider/providers.grandettoken 与选中模型".into()
    } else {
        "新增 provider/providers.grandettoken 并设为当前模型（保留其他 provider）".into()
    };
    p.changed = !same;
    p.writes_selection = p.changed;
    p.configured = has_model;
    if has_model {
        p.removal = Some(Removal {
            what: format!("provider/providers.grandettoken 下的模型「{}」，并还原接入前原值", cfg.model),
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
    let before = root.clone();
    let obj = root
        .as_object_mut()
        .ok_or_else(|| format!("{} 不是对象", pretty(&path)))?;

    let expected_sel = format!("grandettoken/{}", cfg.model);
    let prev_has = oc_gt_any(&before)
        .and_then(|g| g.get("models"))
        .and_then(|m| m.get(&cfg.model))
        .is_some();
    let same = oc_gt_any(&before).is_some_and(|g| oc_gt_same(g, cfg))
        && obj.get("model").and_then(Value::as_str) == Some(expected_sel.as_str());
    if same {
        return Ok(SiteOutcome {
            same: true,
            ..Default::default()
        });
    }
    if !prev_has {
        inject_stash::capture(
            Client::OpenCode.id(),
            inject_stash::snapshot_json(Some(&before), &["model", "small_model"]),
        );
    }

    for container in oc_containers(obj) {
        let providers = obj
            .entry(container.to_string())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("provider/providers 不是 JSON 对象")?;
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
    }
    obj.insert("model".into(), json!(expected_sel));
    obj.insert("small_model".into(), json!(expected_sel));

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
                    let g = oc_gt_any(&v);
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

    inject_stash::note_wired(Client::OpenCode.id(), &cfg.model);
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
    for container in ["provider", "providers"] {
        let Some(providers) = obj.get_mut(container).and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(gt) = providers.get_mut("grandettoken").and_then(Value::as_object_mut) else {
            continue;
        };
        if let Some(models) = gt.get_mut("models").and_then(Value::as_object_mut) {
            if models.remove(&cfg.model).is_some() {
                removed = 1;
            }
            if models.is_empty() {
                providers.remove("grandettoken");
            }
        }
    }
    if removed == 0 {
        return Ok(SiteOutcome::default());
    }
    let expected_sel = format!("grandettoken/{}", cfg.model);
    let clears_selection = obj.get("model").and_then(Value::as_str) == Some(expected_sel.as_str());
    let stash = inject_stash::take(Client::OpenCode.id());
    if stash.is_empty() {
        // 旧版接入没有 stash：指向我们的选中项摘掉，其余不动
        if clears_selection {
            obj.remove("model");
        }
        if obj.get("small_model").and_then(Value::as_str) == Some(expected_sel.as_str()) {
            obj.remove("small_model");
        }
    } else {
        inject_stash::restore_json(obj, &stash);
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
                    oc_gt_any(&v)
                        .and_then(|g| g.get("models"))
                        .and_then(Value::as_object)
                        .is_none_or(|m| !m.contains_key(&exp_model))
                })
        },
    )?;

    inject_stash::clear_wired(Client::OpenCode.id());
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
                        let model = v
                            .get("model")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .or_else(|| env.get("ANTHROPIC_MODEL").and_then(Value::as_str))
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
                    if let Some(gt) = oc_gt_any(&v) {
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
        inject_stash::override_path(Some(root.join("inject-stash.json")));

        // 新建文件时写入 + 回滚会干净删掉文件
        let out = write_claude_code_site(&site, &cfg).unwrap();
        assert_eq!(out.added, 1);
        let written = read_json(&root.join("settings.json")).unwrap().unwrap();
        assert_eq!(written["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:18771");
        // magpie 口径：顶层 model + 分层 env，不写 ANTHROPIC_MODEL
        assert_eq!(written["model"], "group/coding");
        assert_eq!(written["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"], "group/coding");
        assert_eq!(written["env"]["ANTHROPIC_SMALL_FAST_MODEL"], "group/coding");
        assert!(written["env"].get("ANTHROPIC_MODEL").is_none());
        restore(&out.backup).unwrap();
        assert!(!root.join("settings.json").exists(), "新建文件回滚应删除");
        let _ = inject_stash::take(Client::ClaudeCode.id());

        // 原有其他配置与 env 变量必须保留；用户原 model 进 stash、断开还原
        std::fs::write(
            root.join("settings.json"),
            r#"{"theme":"dark","model":"user-main","env":{"KEEP_VAR":"yes","ANTHROPIC_MODEL":"user-env","ANTHROPIC_BASE_URL":"https://api.deepseek.com/anthropic"}}"#,
        )
        .unwrap();
        let out2 = write_claude_code_site(&site, &cfg).unwrap();
        let after = read_json(&root.join("settings.json")).unwrap().unwrap();
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["env"]["KEEP_VAR"], "yes");
        assert_eq!(after["model"], "group/coding");
        assert!(after["env"].get("ANTHROPIC_MODEL").is_none());
        let rm = remove_claude_code_site(&site, &cfg).unwrap();
        assert_eq!(rm.removed, 1);
        let after_rm = read_json(&root.join("settings.json")).unwrap().unwrap();
        assert_eq!(after_rm["env"]["KEEP_VAR"], "yes");
        assert_ne!(after_rm["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:18771");
        assert_eq!(after_rm["model"], "user-main", "断开应还原用户原 model");
        assert_eq!(after_rm["env"]["ANTHROPIC_MODEL"], "user-env", "断开应还原用户原 env 模型");
        assert_eq!(
            after_rm["env"]["ANTHROPIC_BASE_URL"], "https://api.deepseek.com/anthropic",
            "断开应还原用户原来的 endpoint（magpie 踩过的坑：把 BASE_URL 一起带走）"
        );
        let _ = out2;
        inject_stash::override_path(None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_cleanup_leaves_other_local_gateways_alone() {
        // 卸载清理用 source="" 关掉「任意本地网关都算我们的」这条宽松匹配：
        // 本机可能同时跑着别人的本地网关（例如 magpie），不能把它的配置删掉。
        let root = std::env::temp_dir().join(format!("ts-cc-clean-{}", crate::vault::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let site = cli_site(&root, Client::ClaudeCode, "settings.json");
        let orig = r#"{"model":"m","env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:9999","ANTHROPIC_AUTH_TOKEN":"other-gw"}}"#;
        std::fs::write(root.join("settings.json"), orig).unwrap();
        inject_stash::override_path(Some(root.join("inject-stash.json")));

        let cfg = CliTargetConfig {
            model: String::new(),
            display_name: String::new(),
            base_url: String::new(),
            anthropic_base_url: "http://127.0.0.1:18771".into(),
            api_key: String::new(),
            source: String::new(),
        };
        let out = remove_claude_code_site(&site, &cfg).unwrap();
        assert_eq!(out.removed, 0, "别的端口的本地网关不该被当成我们的");
        assert_eq!(
            std::fs::read_to_string(root.join("settings.json")).unwrap(),
            orig,
            "文件必须逐字节不变"
        );
        inject_stash::override_path(None);
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
        inject_stash::override_path(Some(root.join("inject-stash.json")));

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
        // magpie 口径：托管表保留（旧线程靠它打开），只摘 model_provider 并还原原值
        assert!(after.contains("grandettoken-managed-start"));
        assert!(after.contains("model_provider = \"openai\" # 保留行内注释"));
        assert!(after.contains("model = \"gpt-5\""));
        inject_stash::override_path(None);
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
        inject_stash::override_path(Some(root.join("inject-stash.json")));
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
        inject_stash::override_path(None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn opencode_desktop_cli_process_also_blocks_writing() {
        let only_desktop = ProcessSnapshot::from_names(["OpenCode-CLI.exe"]);
        let (running, name) = super::inject_discover::cli_running(Client::OpenCode, &only_desktop);
        assert!(running, "桌面版 opencode-cli.exe 在跑时必须判「在跑」");
        assert_eq!(name, "opencode-cli.exe");
        let mut site = Site {
            client: Client::OpenCode,
            label: Client::OpenCode.label().into(),
            running,
            process: name,
            root: std::env::temp_dir(),
            files: vec![],
        };
        assert!(blocked_by_running(&site).unwrap().contains("opencode-cli.exe"));

        let pty = ProcessSnapshot::from_names(["opencode-pty.exe"]);
        assert_eq!(super::inject_discover::cli_running(Client::OpenCode, &pty),
            (true, "opencode-pty.exe".to_string()));
        let none = ProcessSnapshot::from_names(["unrelated.exe"]);
        let (idle, fallback) = super::inject_discover::cli_running(Client::OpenCode, &none);
        site.running = idle;
        assert!(blocked_by_running(&site).is_none());
        assert_eq!(fallback, "opencode.exe");
        assert!(Client::OpenCode.processes().contains(&"opencode.exe"));
    }
}


/// 卸载清理：整个 grandettoken provider 摘除 + 还原 stash（不同于断开只摘单个模型）。
pub(super) fn cleanup_opencode_site(site: &Site) -> Result<bool, String> {
    let fname = opencode_file_name(site);
    let path = site.root.join(&fname);
    let Some((header, mut root)) = read_opencode_json(&path)? else {
        return Ok(false);
    };
    let bom = has_bom(&path);
    let obj = root
        .as_object_mut()
        .ok_or_else(|| format!("{} 不是对象", pretty(&path)))?;
    let mut changed = false;
    for container in ["provider", "providers"] {
        if let Some(providers) = obj.get_mut(container).and_then(Value::as_object_mut) {
            if providers.remove("grandettoken").is_some() {
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(false);
    }
    for key in ["model", "small_model"] {
        if obj
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| s.starts_with("grandettoken/"))
        {
            obj.remove(key);
        }
    }
    let stash = inject_stash::take(Client::OpenCode.id());
    inject_stash::restore_json(obj, &stash);
    inject_stash::clear_wired(Client::OpenCode.id());
    let body = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    atomic_write_text(&path, &format!("{header}{body}\n"), bom)?;
    Ok(true)
}

pub(super) fn cleanup_codex_site(site: &Site) -> Result<bool, String> {
    let o = remove_codex_site(site)?;
    let path = site.root.join("config.toml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(o.removed > 0);
    };
    let stripped = codex_strip_table(&raw);
    if stripped == raw {
        return Ok(o.removed > 0);
    }
    let bom = has_bom(&path);
    atomic_write_text(&path, &stripped, bom)?;
    Ok(true)
}
