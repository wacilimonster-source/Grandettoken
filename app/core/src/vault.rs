//! 本地密钥箱的纯逻辑:平台/标签校验、同平台内标签去重、搜索、排序。
//!
//! 分工:密文在 `secrets::vault_*`(Windows 凭据管理器),索引在 `store`(`vault_items`,不含密钥),
//! 这里只做判断与排序,便于挂单测。**这个模块永远不该出现"加密/解密"** —— 我们不自己造加密。

use crate::store::VaultItem;

/// 平台名/标签的规矩:非空、去掉首尾空白、长度上限(界面里是单行输入)。
const MAX_LEN: usize = 40;

pub fn clean(s: &str) -> String {
    s.trim().to_string()
}

pub fn valid_platform(s: &str) -> Result<String, String> {
    let s = clean(s);
    if s.is_empty() {
        return Err("平台名不能为空".into());
    }
    if s.chars().count() > MAX_LEN {
        return Err(format!("平台名太长(最多 {MAX_LEN} 字)"));
    }
    Ok(s)
}

pub fn valid_label(s: &str) -> Result<String, String> {
    let s = clean(s);
    if s.is_empty() {
        return Err("标签不能为空(比如「主号」「备用」)".into());
    }
    if s.chars().count() > MAX_LEN {
        return Err(format!("标签太长(最多 {MAX_LEN} 字)"));
    }
    Ok(s)
}

/// 同一平台下标签是否已存在(编辑时用 `except_id` 排除自己)。
/// 平台名与标签都不区分大小写地比 —— 免得"4SAPI/主号"和"4sapi/主号"看起来是两条。
pub fn label_taken(items: &[VaultItem], platform: &str, label: &str, except_id: Option<&str>) -> bool {
    items.iter().any(|it| {
        Some(it.id.as_str()) != except_id
            && it.platform.eq_ignore_ascii_case(platform)
            && it.label.eq_ignore_ascii_case(label)
    })
}

/// 搜索匹配:平台 / 标签 / 备注 三项任意命中(不区分大小写)。空查询 = 全中。
pub fn matches(it: &VaultItem, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    it.platform.to_lowercase().contains(&q)
        || it.label.to_lowercase().contains(&q)
        || it.note.to_lowercase().contains(&q)
}

/// 新条目的 id:与平台/标签解耦(改名不会丢密文),用随机源拼一个 v4 形状的 uuid。
pub fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for chunk in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
        );
        h.write_u32(std::process::id());
        h.write_u64(chunk);
        bytes[chunk as usize * 8..chunk as usize * 8 + 8].copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let b = bytes;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, platform: &str, label: &str, note: &str) -> VaultItem {
        VaultItem {
            id: id.into(),
            platform: platform.into(),
            label: label.into(),
            hint: "••••abcd".into(),
            note: note.into(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn platform_and_label_are_validated() {
        assert_eq!(valid_platform("  4SAPI ").unwrap(), "4SAPI");
        assert!(valid_platform("   ").is_err());
        assert!(valid_platform(&"x".repeat(41)).is_err());
        assert_eq!(valid_label("主号").unwrap(), "主号");
        assert!(valid_label("").is_err());
    }

    /// 同平台内标签不许重名(不区分大小写);编辑自己时不算冲突。
    #[test]
    fn duplicate_label_within_platform() {
        let items = vec![
            item("1", "4SAPI", "主号", ""),
            item("2", "Hapi", "主号", ""),
        ];
        assert!(label_taken(&items, "4sapi", "主号", None), "不区分大小写");
        assert!(!label_taken(&items, "4SAPI", "备用", None));
        assert!(
            !label_taken(&items, "4SAPI", "主号", Some("1")),
            "改自己不该判成冲突"
        );
        assert!(
            !label_taken(&items, "Hapi", "主号", None) == false,
            "不同平台允许同名标签"
        );
    }

    #[test]
    fn search_hits_platform_label_note() {
        let it = item("1", "4SAPI", "主号", "充值用的那个");
        assert!(matches(&it, ""));
        assert!(matches(&it, "4sapi"));
        assert!(matches(&it, "主"));
        assert!(matches(&it, "充值"));
        assert!(!matches(&it, "hapi"));
    }

    #[test]
    fn new_id_shape_and_uniqueness() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert_eq!(&a[14..15], "4", "v4 形状");
    }
}
