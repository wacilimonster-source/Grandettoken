//! Codex config.toml 的行级保注释编辑与托管表生命周期（对齐 magpie codex.go 的表保留语义）。

use super::inject_cli::CliTargetConfig;

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
    // magpie 口径：托管表一旦写过就保留（旧线程靠它打开），断开只摘顶层 model_provider；
    // 顶层 model 由调用方按 stash 还原或删除。
    let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let clean = raw.trim_start_matches('\u{feff}');
    let mut lines: Vec<String> = clean.lines().map(str::to_string).collect();

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
    for idx in remove_indices.into_iter().rev() {
        lines.remove(idx);
    }
    let table_present = lines.iter().any(|l| {
        l.trim() == CODEX_START || is_toml_section_header(l) == Some("[model_providers.grandettoken]")
    });
    let removed = usize::from(had_our_provider || table_present);
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut out = lines.join(eol);
    if !out.is_empty() {
        out.push_str(eol);
    }
    (out, removed)
}

pub fn codex_drop_top_key(raw: &str, key: &str) -> String {
    let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let clean = raw.trim_start_matches('\u{feff}');
    let mut lines: Vec<String> = clean.lines().map(str::to_string).collect();
    let first_section = lines
        .iter()
        .position(|l| is_toml_section_header(l).is_some())
        .unwrap_or(lines.len());
    let idx = lines[..first_section]
        .iter()
        .position(|l| parse_toml_kv(l).is_some_and(|(k, _)| k == key));
    if let Some(i) = idx {
        lines.remove(i);
    }
    let mut out = lines.join(eol);
    if !out.is_empty() {
        out.push_str(eol);
    }
    out
}

pub fn codex_set_top_key(raw: &str, key: &str, value: &str) -> String {
    let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let clean = raw.trim_start_matches('\u{feff}');
    let mut lines: Vec<String> = if clean.is_empty() {
        Vec::new()
    } else {
        clean.lines().map(str::to_string).collect()
    };
    upsert_top_toml_key(&mut lines, key, value);
    let mut out = lines.join(eol);
    out.push_str(eol);
    out
}


/// 卸载清理：托管表整段摘除（网关已不存在，旧线程无需再打开）。
pub fn codex_strip_table(raw: &str) -> String {
    let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let clean = raw.trim_start_matches('\u{feff}');
    let mut lines: Vec<String> = clean.lines().map(str::to_string).collect();
    if let (Some(st), Some(en)) = (
        lines.iter().position(|l| l.trim() == CODEX_START),
        lines.iter().position(|l| l.trim() == CODEX_END),
    ) {
        if st <= en {
            lines.drain(st..=en);
        }
    } else if let Some(hdr) = lines
        .iter()
        .position(|l| is_toml_section_header(l) == Some("[model_providers.grandettoken]"))
    {
        let end = lines[hdr + 1..]
            .iter()
            .position(|l| is_toml_section_header(l).is_some())
            .map(|rel| hdr + 1 + rel)
            .unwrap_or(lines.len());
        lines.drain(hdr..end);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut out = lines.join(eol);
    if !out.is_empty() {
        out.push_str(eol);
    }
    out
}
