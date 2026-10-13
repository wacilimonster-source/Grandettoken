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

// exists() 与 masked() 已删除(报告 P2-15):两者都用 .ok().flatten() 把
// "凭据管理器本身坏了"吞成 None/false,让界面把故障报成"未配置密钥"。
// 需要回显尾号的地方请直接 get() 再 mask_of(),Err 要有它自己的文案。
