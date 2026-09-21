//! 密钥存储:Windows 凭据管理器。
//!
//! 密钥永不写入 JSON、SQLite、日志或导出文件,只交给系统凭据库,
//! 由当前 Windows 账户的登录凭据保护。这样配置文件可以随便备份或分享。
//!
//! 界面仍是明文输入,不做二次确认、不设主密码 —— 加密由系统承担。

const SERVICE: &str = "TokenScope";

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

pub fn exists(provider_id: &str) -> bool {
    get(provider_id).ok().flatten().is_some()
}

/// 只回显尾部 4 位,用于界面确认"存的是哪把 key",不泄露完整密钥。
pub fn masked(provider_id: &str) -> Option<String> {
    let k = get(provider_id).ok().flatten()?;
    let tail: String = k.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    Some(format!("••••{}", tail))
}
