//! 跨来源合并键 `platformId`。
//!
//! 内置平台直接用协议枚举（`deepseek` / `trae` / ...），它们本来就在所有电脑上
//! 是同一个字符串；自定义渠道的本机 id（`custom-1`）只在那一台电脑有意义，
//! 绝不能拿来当跨机合并键。自定义渠道必须由用户在桌面端选一个统一的
//! “同步平台标签”，映射成 `custom:<label>`。

use crate::config::Config;
use crate::providers;

use super::model::valid_platform_id;

/// 内置 provider id 的集合。
pub fn is_builtin(channel_id: &str) -> bool {
    providers::PROVIDERS.iter().any(|p| p.id == channel_id)
}

/// 把本机渠道映射成跨来源合并键。
///
/// - 内置渠道：`platformId == provider id`。
/// - 自定义渠道：优先用 `syncPlatformLabels[channel_id]`；没有标签时退回
///   `custom:<本机 id>`（稳定但只在当前电脑有意义），由 UI 提示用户设置标签。
pub fn platform_id_for(channel_id: &str, cfg: &Config) -> String {
    if is_builtin(channel_id) {
        return channel_id.to_string();
    }
    if let Some(label) = cfg.sync_platform_labels.get(channel_id) {
        if let Some(normalized) = normalize_label(label) {
            return format!("custom:{normalized}");
        }
    }
    let local = normalize_label(channel_id)
        .unwrap_or_else(|| channel_id.chars().filter(|c| c.is_ascii_alphanumeric()).collect());
    if local.is_empty() {
        "custom:unknown".into()
    } else {
        format!("custom:{local}")
    }
}

/// 同步平台标签的规范化：小写、空白转 `-`、只留 `[a-z0-9.-]`、截断 40。
pub fn normalize_label(input: &str) -> Option<String> {
    let raw = input.trim().to_lowercase();
    if raw.is_empty() || raw.contains("://") || raw.contains('@') {
        return None;
    }
    if raw.starts_with("sk-")
        || raw.starts_with("sk_")
        || raw.starts_with("ghp_")
        || raw.starts_with("gho_")
        || raw.starts_with("github_pat_")
    {
        return None;
    }
    let mut out = String::new();
    let mut last_dash = false;
    for c in raw.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '.' {
            c
        } else if c == '_' || c == '-' || c.is_whitespace() {
            '-'
        } else {
            continue;
        };
        if c == '-' {
            if last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        out.push(c);
        if out.chars().count() >= 40 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        return None;
    }
    let candidate = format!("custom:{out}");
    if valid_platform_id(&candidate) {
        Some(out)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_ids_pass_through() {
        let cfg = Config::default();
        assert_eq!(platform_id_for("deepseek", &cfg), "deepseek");
        assert_eq!(platform_id_for("workbuddy-ai", &cfg), "workbuddy-ai");
    }

    #[test]
    fn custom_without_label_is_local_stable() {
        let cfg = Config::default();
        assert_eq!(platform_id_for("custom-1", &cfg), "custom:custom-1");
    }

    #[test]
    fn custom_with_label_is_shared() {
        let mut cfg = Config::default();
        cfg.sync_platform_labels
            .insert("custom-1".into(), "My Relay".into());
        assert_eq!(platform_id_for("custom-1", &cfg), "custom:my-relay");
    }

    #[test]
    fn label_normalization() {
        assert_eq!(normalize_label("  My Relay  ").as_deref(), Some("my-relay"));
        assert_eq!(normalize_label("a__b").as_deref(), Some("a-b"));
        assert_eq!(normalize_label("https://x"), None);
        assert_eq!(normalize_label("a@b.com"), None);
        assert_eq!(normalize_label("sk-123"), None);
        assert_eq!(normalize_label(""), None);
    }
}
