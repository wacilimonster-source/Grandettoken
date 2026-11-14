//! Google Cloud 赠金账本。只接收登录页上的结构化金额，账号和日期一起原子保存。
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{config::Config, fetch::ChannelView, providers::Kind, store::Store};

pub const ID: &str = "google-cloud";
const KEY: &str = "google_cloud_credits_v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credit {
    pub id: String,
    pub name: String,
    pub remaining: f64,
    pub unit: String,
    pub scope: String,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Account {
    pub credits: Vec<Credit>,
    pub updated_at: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Ledger {
    pub selected: Option<String>,
    pub accounts: BTreeMap<String, Account>,
}

/// 网页桥只传必要的表格字段。凭据和原始 HTML 不进入账本。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    pub account: String,
    pub rows: Vec<CaptureRow>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRow {
    pub id: String,
    pub name: String,
    pub status: String,
    pub amount: String,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditView {
    pub account: Option<String>,
    pub credits: Vec<Credit>,
}

pub fn valid_account(s: &str) -> bool {
    s.len() == 20
        && s.split('-').count() == 3
        && s.split('-')
            .all(|p| p.len() == 6 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub fn load(store: &Store) -> Result<Ledger, String> {
    match store.get_setting(KEY)? {
        Some(raw) => {
            serde_json::from_str(&raw).map_err(|_| "Google 赠金记录损坏，请重新连接".into())
        }
        None => Ok(Ledger::default()),
    }
}

fn save(store: &Store, ledger: &Ledger) -> Result<(), String> {
    let raw = serde_json::to_string(ledger).map_err(|_| "赠金记录无法序列化".to_string())?;
    store.set_setting(KEY, &raw)
}

/// 页面固定为英文数字格式。未知币种、逗号格式或非有限金额一律报错。
fn money(text: &str) -> Result<(f64, String), String> {
    let text = text.trim();
    let prefixes = [
        ("US$", "USD"),
        ("USD", "USD"),
        ("$", "USD"),
        ("CN¥", "CNY"),
        ("CNY", "CNY"),
        ("€", "EUR"),
        ("EUR", "EUR"),
        ("£", "GBP"),
        ("GBP", "GBP"),
        ("JP¥", "JPY"),
        ("JPY", "JPY"),
    ];
    let (prefix, unit) = prefixes
        .iter()
        .find(|(p, _)| text.starts_with(p))
        .ok_or("赠金币种无法识别，请将控制台语言设为 English 后重试")?;
    let raw = text[prefix.len()..].trim();
    let parts: Vec<_> = raw.split('.').collect();
    if parts.len() > 2 || parts.iter().any(|p| p.is_empty()) {
        return Err("赠金金额格式无法识别".into());
    }
    let groups: Vec<_> = parts[0].split(',').collect();
    if groups
        .iter()
        .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
        || (groups.len() > 1 && (groups[0].len() > 3 || groups[1..].iter().any(|p| p.len() != 3)))
        || (parts.len() == 2 && !parts[1].bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("赠金金额格式无法识别".into());
    }
    let amount: f64 = raw.replace(',', "").parse().map_err(|_| "赠金金额无效")?;
    if !amount.is_finite() || amount < 0.0 {
        return Err("赠金金额无效".into());
    }
    Ok((amount, unit.to_string()))
}

pub fn capture(
    store: &Store,
    data: Capture,
    expected: Option<&str>,
    now: i64,
) -> Result<(), String> {
    if !valid_account(&data.account) || expected.is_some_and(|id| id != data.account) {
        return Err("结算账号已变化，请重新连接".into());
    }
    if data.rows.is_empty() || data.rows.len() > 200 {
        return Err("赠金表格尚未加载或记录过多".into());
    }
    let mut ledger = load(store)?;
    let old = ledger.accounts.get(&data.account);
    let mut credits = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for row in data.rows {
        if row.id.is_empty()
            || row.id.len() > 512
            || row.name.len() > 256
            || row.scope.len() > 2048
            || !ids.insert(row.id.clone())
        {
            return Err("赠金记录标识无效或重复".into());
        }
        match row.status.trim().to_lowercase().as_str() {
            "expired" | "used" | "used up" | "已过期" | "已用完" => continue,
            "available" | "可用" => {}
            _ => return Err("赠金状态无法识别，已保留上次余额".into()),
        }
        let (remaining, unit) = money(&row.amount)?;
        let expires_at = old
            .and_then(|a| a.credits.iter().find(|c| c.id == row.id))
            .and_then(|c| c.expires_at);
        credits.push(Credit {
            id: row.id,
            name: row.name,
            remaining,
            unit,
            scope: row.scope,
            expires_at,
        });
    }
    if let Some(first) = credits.first() {
        if credits.iter().any(|c| c.unit != first.unit) {
            return Err("赠金包含多种币种，无法合并余额".into());
        }
    }
    if !credits.iter().map(|c| c.remaining).sum::<f64>().is_finite() {
        return Err("赠金总金额无效".into());
    }
    ledger.accounts.insert(
        data.account.clone(),
        Account {
            credits,
            updated_at: now,
            error: None,
        },
    );
    ledger.selected = Some(data.account);
    save(store, &ledger)
}

pub fn set_error(store: &Store, account: &str) -> Result<(), String> {
    let mut ledger = load(store)?;
    if let Some(a) = ledger.accounts.get_mut(account) {
        a.error = Some("Google 赠金刷新失败，请打开结算页面检查登录与表格".into());
        save(store, &ledger)?;
    }
    Ok(())
}

pub fn set_expiry(
    store: &Store,
    account: &str,
    credit: &str,
    at: Option<i64>,
) -> Result<(), String> {
    if at.is_some_and(|t| !(1..=253402300799).contains(&t)) {
        return Err("到期时间无效".into());
    }
    let mut ledger = load(store)?;
    if ledger.selected.as_deref() != Some(account) {
        return Err("结算账号已变化，请刷新后重试".into());
    }
    let c = ledger
        .accounts
        .get_mut(account)
        .and_then(|a| a.credits.iter_mut().find(|c| c.id == credit))
        .ok_or("这笔赠金已变化，请刷新后重试")?;
    c.expires_at = at;
    save(store, &ledger)
}

pub fn disconnect(store: &Store) -> Result<(), String> {
    let mut ledger = load(store)?;
    ledger.selected = None;
    save(store, &ledger)
}

pub fn channel(store: &Store, cfg: &Config, now: i64) -> ChannelView {
    let mut v = ChannelView {
        id: ID.into(),
        name: "Gemini · Google Cloud".into(),
        short: "G".into(),
        color: "#4285f4".into(),
        kind: Kind::Amount,
        unit: "USD".into(),
        auth_source: "google-cloud".into(),
        auth_label: "Google Cloud 结算账号赠金".into(),
        hidden: cfg.hidden_channels.iter().any(|id| id == ID),
        ..Default::default()
    };
    match load(store) {
        Ok(ledger) => {
            let account = ledger.selected.clone();
            if let Some(a) = account.as_ref().and_then(|id| ledger.accounts.get(id)) {
                v.has_key = true;
                v.remaining = Some(a.credits.iter().map(|c| c.remaining).sum());
                v.unit = a
                    .credits
                    .first()
                    .map(|c| c.unit.clone())
                    .unwrap_or("USD".into());
                v.updated_at = a.updated_at;
                v.error = a.error.clone();
                let stale_after = if cfg.stale_after_sec > 0 {
                    cfg.stale_after_sec
                } else {
                    crate::view::DEFAULT_STALE_SEC
                };
                v.stale = a.error.is_some() || now.saturating_sub(a.updated_at) >= stale_after;
                v.valid = !v.stale;
                v.google_cloud = Some(CreditView {
                    account,
                    credits: a.credits.clone(),
                });
            } else {
                v.google_cloud = Some(CreditView {
                    account,
                    credits: Vec::new(),
                });
            }
        }
        Err(e) => {
            v.error = Some(e);
        }
    }
    v
}

/// 窗口销毁只负责派发，须等管理器释放旧标签后才可创建同名页面。
pub async fn wait_for_window_release(
    mut occupied: impl FnMut() -> bool,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    while occupied() {
        if tokio::time::Instant::now() >= deadline {
            return Err("上次结算窗口尚未关闭，请稍后重试".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(account: &str) -> Capture {
        serde_json::from_value(serde_json::json!({"account":account,"rows":[
            {"id":"live","name":"Free Trial","status":"Available","amount":"$300.00","scope":"Eligible usage"},
            {"id":"expired","name":"Free Trial","status":"Expired","amount":"$300.00","scope":"Eligible usage"}
        ]})).unwrap()
    }
    const A: &str = "AAAAAA-BBBBBB-CCCCCC";
    const B: &str = "DDDDDD-EEEEEE-FFFFFF";
    #[test]
    fn expired_rows_are_excluded_and_failure_retains_snapshot() {
        let s = Store::open_memory().unwrap();
        capture(&s, sample(A), None, 1000).unwrap();
        let v = channel(&s, &Config::default(), 1001);
        assert_eq!(v.remaining, Some(300.0));
        assert!(v.valid);
        let mut bad = sample(A);
        bad.rows[0].status = "Unknown".into();
        assert!(capture(&s, bad, Some(A), 1010).is_err());
        set_error(&s, A).unwrap();
        let v = channel(&s, &Config::default(), 1010);
        assert!(v.stale && !v.valid);
        assert_eq!(v.updated_at, 1000);
        assert_eq!(v.remaining, Some(300.0));
    }
    #[test]
    fn expiry_is_scoped_and_survives_refresh() {
        let s = Store::open_memory().unwrap();
        capture(&s, sample(A), None, 1000).unwrap();
        set_expiry(&s, A, "live", Some(2000)).unwrap();
        capture(&s, sample(A), Some(A), 1010).unwrap();
        assert_eq!(
            load(&s).unwrap().accounts[A].credits[0].expires_at,
            Some(2000)
        );
        capture(&s, sample(B), None, 1020).unwrap();
        assert!(set_expiry(&s, A, "live", Some(3000)).is_err());
        assert!(capture(&s, sample(A), Some(B), 1030).is_err());
        assert_eq!(load(&s).unwrap().accounts[B].credits[0].expires_at, None);
        disconnect(&s).unwrap();
        assert!(!channel(&s, &Config::default(), 1040).has_key);
    }
    #[test]
    fn clock_expiry_does_not_rewrite_reported_balance() {
        let s = Store::open_memory().unwrap();
        capture(&s, sample(A), None, 1000).unwrap();
        set_expiry(&s, A, "live", Some(1005)).unwrap();
        assert_eq!(channel(&s, &Config::default(), 1010).remaining, Some(300.0));
        assert!(channel(&s, &Config::default(), 999999).stale);
    }
    #[test]
    fn malformed_money_and_mixed_currencies_fail_closed() {
        for input in ["$NaN", "$-1", "$1,23", "$1.2.3", "¥300.00", "$inf"] {
            assert!(money(input).is_err(), "{input}");
        }
        assert_eq!(money("$1,234.50").unwrap(), (1234.5, "USD".into()));
        let s = Store::open_memory().unwrap();
        let mut data = sample(A);
        data.rows[1].status = "Available".into();
        data.rows[1].amount = "€10.00".into();
        assert!(capture(&s, data, None, 1000).is_err());
        assert!(load(&s).unwrap().selected.is_none());
    }

    #[test]
    fn failed_writes_leave_balance_and_manual_date_unchanged() {
        let name = crate::sync::crypto::random_source().unwrap();
        let path = std::env::temp_dir().join(format!("grandettoken-google-{name}.db"));
        let s = Store::open(&path).unwrap();
        capture(&s, sample(A), None, 1000).unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_google_write BEFORE UPDATE ON settings
             WHEN NEW.key = 'google_cloud_credits_v1'
             BEGIN SELECT RAISE(ABORT, 'test write failure'); END;",
            )
            .unwrap();
        assert!(set_expiry(&s, A, "live", Some(2000)).is_err());
        let mut data = sample(A);
        data.rows[0].amount = "$250.00".into();
        assert!(capture(&s, data, Some(A), 1010).is_err());
        let ledger = load(&s).unwrap();
        assert_eq!(ledger.accounts[A].credits[0].expires_at, None);
        assert_eq!(ledger.accounts[A].credits[0].remaining, 300.0);
        assert_eq!(ledger.accounts[A].updated_at, 1000);
        drop(connection);
        drop(s);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn all_expired_is_zero_and_hidden_is_preserved() {
        let s = Store::open_memory().unwrap();
        let mut data = sample(A);
        data.rows[0].status = "Expired".into();
        capture(&s, data, None, 1000).unwrap();
        let mut cfg = Config::default();
        cfg.hidden_channels.push(ID.into());
        let view = channel(&s, &cfg, 1001);
        assert_eq!(view.remaining, Some(0.0));
        assert!(view.hidden && view.has_key && view.valid);
    }
    #[test]
    fn window_release_waits_for_delayed_removal_and_reports_timeout() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut observations = 0;
            wait_for_window_release(
                || {
                    observations += 1;
                    observations < 3
                },
                std::time::Duration::from_secs(1),
            )
            .await
            .unwrap();
            assert_eq!(observations, 3);
            assert!(wait_for_window_release(|| true, std::time::Duration::ZERO)
                .await
                .is_err());
            wait_for_window_release(|| false, std::time::Duration::ZERO)
                .await
                .unwrap();
        });
    }
}
