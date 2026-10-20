//! 密钥存储:Windows 凭据管理器。
//!
//! 密钥永不写入 JSON、SQLite、日志或导出文件,只交给系统凭据库,
//! 由当前 Windows 账户的登录凭据保护。这样配置文件可以随便备份或分享。
//!
//! 界面仍是明文输入,不做二次确认、不设主密码 —— 加密由系统承担。

const SERVICE: &str = "TokenScope";

/// 密钥箱用**另一个服务名**:与渠道密钥分开,删渠道密钥不会碰到密钥箱里的条目,
/// 反之亦然。密文同样由当前 Windows 账户的登录凭据保护。
const VAULT_SERVICE: &str = "TokenScope:vault";

fn vault_entry(id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(VAULT_SERVICE, id).map_err(|e| format!("凭据库不可用: {e}"))
}

/// 密钥箱:写一条。id 由调用方生成(与平台/标签解耦,改名不会丢密文)。
pub fn vault_set(id: &str, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("密钥不能为空".into());
    }
    vault_entry(id)?
        .set_password(key)
        .map_err(|e| format!("写入凭据失败: {e}"))
}

/// 密钥箱:读一条。Ok(None) = 没这条(索引与凭据不同步的情形由调用方处理)。
pub fn vault_get(id: &str) -> Result<Option<String>, String> {
    let e = vault_entry(id)?;
    match e.get_password() {
        Ok(k) => Ok(Some(k)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(format!("读取凭据失败: {err}")),
    }
}

pub fn vault_delete(id: &str) -> Result<(), String> {
    match vault_entry(id)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("删除凭据失败: {e}")),
    }
}

/// 尾 4 位掩码(纯函数,密钥箱与渠道密钥共用同一口径)。
pub fn mask_of(key: &str) -> String {
    let tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("••••{tail}")
}

fn entry(provider_id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, provider_id).map_err(|e| format!("凭据库不可用: {e}"))
}

pub fn set(provider_id: &str, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("密钥不能为空".into());
    }
    entry(provider_id)?
        .set_password(key)
        .map_err(|e| format!("写入凭据失败: {e}"))
}

/// Ok(None) = 服务正常但没配过这把 key;Err = 凭据管理器本身不可用
/// (服务停了 / 组策略禁用)。之前两种混成 None,凭据库坏了界面也说"未配置密钥",
/// 用户反复重填也填不进去(扫描报告 P2-12)。
pub fn get(provider_id: &str) -> Result<Option<String>, String> {
    let e = entry(provider_id)?;
    match e.get_password() {
        Ok(k) => Ok(Some(k)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(format!("读取凭据失败: {err}")),
    }
}

pub fn delete(provider_id: &str) -> Result<(), String> {
    match entry(provider_id)?.delete_credential() {
        Ok(()) => Ok(()),
        // 本来就没有,视为成功
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("删除凭据失败: {e}")),
    }
}

// ───────────── 渠道密钥 → 密钥箱条目 的引用(设计稿 product-vault-channel-bridge)─────────────

/// 引用指针的服务名。与 `TokenScope`(直接填的值)、`TokenScope:vault`(箱子密文)**三者分离**,
/// 所以三种操作互不误删:删渠道密钥不会碰箱子,删箱子不会碰别处的值。
const LINK_SERVICE: &str = "TokenScope:link";

/// 指针值的前缀。加前缀是为了让"指针"和"真值"在凭据管理器里一眼可分 ——
/// 万一有人手工把一把以 `vault:` 开头的密钥填进来,也能明确它不会被当成指针。
const LINK_PREFIX: &str = "vault:";

/// 渠道密钥的解析结果。
///
/// 为什么要分这么细:界面必须能区分"没配过"和"引用的条目被删了"——
/// 前者该去渠道卡填,后者该去密钥箱看。混成一句"未配置密钥"会把人引到错误的方向。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// 这个渠道没配密钥(既没有引用,也没有直接填的值)
    None,
    /// 直接填在渠道卡上的值
    Inline(String),
    /// 引用密钥箱里的某一条,且那条还在
    Vault { id: String, key: String },
    /// 引用还在,但箱子条目已被删除(断链)。**故意不自动回退到 inline** ——
    /// 回退会让"我明明删了箱子里的,它还在用旧密钥"这种诡异行为出现
    Dangling(String),
}

impl Resolved {
    /// 能拿去发请求的密钥。断链与未配置都是 `None`。
    pub fn key(&self) -> Option<&str> {
        match self {
            Resolved::Inline(k) | Resolved::Vault { key: k, .. } => Some(k),
            _ => None,
        }
    }
    pub fn is_dangling(&self) -> bool {
        matches!(self, Resolved::Dangling(_))
    }
}

fn link_entry(channel_id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(LINK_SERVICE, channel_id).map_err(|e| format!("凭据库不可用: {e}"))
}

/// 让渠道引用箱子条目。只写指针 —— **不复制密钥值**(裁决 1:同一把钥匙不存两份)。
pub fn link_set(channel_id: &str, vault_id: &str) -> Result<(), String> {
    link_entry(channel_id)?
        .set_password(&format!("{LINK_PREFIX}{vault_id}"))
        .map_err(|e| format!("写密钥引用失败: {e}"))
}

/// 该渠道引用的箱子条目 id。`Ok(None)` = 没有引用(用的是直接填的值)。
pub fn link_get(channel_id: &str) -> Result<Option<String>, String> {
    match link_entry(channel_id)?.get_password() {
        Ok(p) => Ok(p
            .strip_prefix(LINK_PREFIX)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("读密钥引用失败: {e}")),
    }
}

pub fn link_clear(channel_id: &str) -> Result<(), String> {
    match link_entry(channel_id)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("清除密钥引用失败: {e}")),
    }
}

/// 渠道密钥的**唯一读取入口**:先看有没有引用,再退回直接填的值。
///
/// 所有"要用这把 keys 发请求"的地方都该走这里,而不是直接 `get()` ——
/// 否则引用模式的渠道会读到空,表现为"明明配了却取不到数"。
pub fn resolve(channel_id: &str) -> Result<Resolved, String> {
    if let Some(vid) = link_get(channel_id)? {
        return match vault_get(&vid)? {
            Some(k) => Ok(Resolved::Vault { id: vid, key: k }),
            // 指针还在、箱子那条没了:如实报断链,不静默退回 inline
            None => Ok(Resolved::Dangling(vid)),
        };
    }
    Ok(match get(channel_id)? {
        Some(k) => Resolved::Inline(k),
        None => Resolved::None,
    })
}

// exists() 与 masked() 已删除(报告 P2-15):两者都用 .ok().flatten() 把
// "凭据管理器本身坏了"吞成 None/false,让界面把故障报成"未配置密钥"。
// 需要回显尾号的地方请直接 get() 再 mask_of(),Err 要有它自己的文案。

#[cfg(test)]
mod tests {
    use super::*;

    /// 断链必须与"未配置"分开:两者的补救动作完全不同
    /// (断链去密钥箱看,未配置去渠道卡填)。
    #[test]
    fn dangling_is_distinct_from_none() {
        let dangling = Resolved::Dangling("uuid-1".into());
        let none = Resolved::None;
        assert!(dangling.is_dangling());
        assert!(!none.is_dangling());
        // 两者都拿不出可用密钥 —— 发请求的行为一致,差别只在界面怎么说
        assert_eq!(dangling.key(), None);
        assert_eq!(none.key(), None);
    }

    /// 同一个值,不管来自"直接填"还是"引用箱子",`key()` 拿到的都是它本身。
    /// 上层(取数/签到)因此不需要关心来源。
    #[test]
    fn both_sources_yield_the_same_key() {
        let inline = Resolved::Inline("sk-abc".into());
        let vault = Resolved::Vault {
            id: "uuid-1".into(),
            key: "sk-abc".into(),
        };
        assert_eq!(inline.key(), Some("sk-abc"));
        assert_eq!(vault.key(), Some("sk-abc"));
        assert!(!inline.is_dangling() && !vault.is_dangling());
    }

    /// 尾 4 位掩码:密钥箱与渠道密钥共用同一口径(设计稿要求两处显示一致)。
    #[test]
    fn mask_keeps_only_tail4() {
        assert_eq!(mask_of("sk-1234567890abcd"), "••••abcd");
        assert_eq!(mask_of("abcd"), "••••abcd");
        assert_eq!(mask_of("ab"), "••••ab");
    }
}
