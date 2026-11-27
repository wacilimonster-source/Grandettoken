//! 本地密钥箱的纯逻辑:平台与分组校验、搜索、排序。
//!
//! 分工:密文在 `secrets::vault_*`(Windows 凭据管理器),索引在 `store`(`vault_items`,不含密钥),
//! 这里只做判断与排序,便于挂单测。**这个模块永远不该出现"加密/解密"** —— 我们不自己造加密。

use crate::store::{Store, VaultItem};

pub mod transaction;

pub fn vault_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() { return Ok(String::new()); }
    let parsed = reqwest::Url::parse(value).map_err(|_| "请输入完整的网址（包含 http:// 或 https://）")?;
    let has_secret_param = parsed.query_pairs().any(|(name, _)| {
        let n = name.to_ascii_lowercase().replace('-', "_");
        n == "key" || n.contains("api_key") || n.contains("token") || n.contains("secret") || n.contains("password") || n.contains("authorization")
    });
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() || !parsed.username().is_empty() || parsed.password().is_some() || has_secret_param {
        return Err("网址需为不含密钥或凭据的 HTTP(S) 地址".into());
    }
    Ok(value.to_string())
}

pub fn vault_default_channel(store: &Store, vault_id: &str) -> Result<Option<String>, String> {
    for id in ["4sapi", "hapi", "opencode-go"] {
        if store.get_setting(&format!("vault_default_channel:{id}"))?.as_deref() == Some(vault_id) {
            return Ok(Some(id.to_string()));
        }
    }
    Ok(None)
}

pub fn ensure_vault_mutable(store: &Store, vault_id: &str) -> Result<(), String> {
    if vault_default_channel(store, vault_id)?.is_some() {
        return Err("渠道默认密钥仅供查看，请在渠道管理中修改".into());
    }
    Ok(())
}

#[cfg(test)]
mod vault_url_tests {
    use super::vault_url;
    #[test]
    fn default_records_are_protected_without_blocking_other_keys() {
        let path = std::env::temp_dir().join(format!("vault-default-test-{}.db", crate::vault::new_id()));
        let store = crate::store::Store::open(&path).unwrap();
        store.set_setting("vault_default_channel:4sapi", "default-1").unwrap();
        assert_eq!(super::vault_default_channel(&store, "default-1").unwrap().as_deref(), Some("4sapi"));
        assert!(super::ensure_vault_mutable(&store, "default-1").is_err());
        assert!(super::ensure_vault_mutable(&store, "normal-key").is_ok());
        drop(store);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn accepts_plain_addresses_and_rejects_embedded_credentials() {
        assert_eq!(vault_url(" https://example.com/v1 ").unwrap(), "https://example.com/v1");
        assert_eq!(vault_url("").unwrap(), "");
        assert_eq!(vault_url("https://example.com/v1?api-version=1").unwrap(), "https://example.com/v1?api-version=1");
        assert!(vault_url("https://user:password@example.com/v1").is_err());
        assert!(vault_url("https://example.com/v1?key=secret").is_err());
        assert!(vault_url("javascript:alert(1)").is_err());
    }
}



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
        return Err("分组不能为空(比如「主号」「备用」)".into());
    }
    if s.chars().count() > MAX_LEN {
        return Err(format!("分组太长(最多 {MAX_LEN} 字)"));
    }
    Ok(s)
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
            api_endpoint: String::new(),
            website: String::new(),
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
