//! Google Cloud 赠金账本。只接收登录页上的结构化金额，账号和日期一起原子保存。
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::{config::Config, fetch::ChannelView, providers::Kind, store::Store};

pub const ID: &str = "google-cloud";
const KEY: &str = "google_cloud_credits_v1";
static REVISION: AtomicU64 = AtomicU64::new(0);

/// 账本成功写入后才推进，让后台调度立即重新读取连接和账号状态。
fn revision() -> u64 {
    REVISION.load(Ordering::Acquire)
}

#[derive(Default)]
pub struct LedgerCache {
    cached: Option<Result<Ledger, String>>,
    revision: u64,
    read_at: Option<std::time::Instant>,
}

impl LedgerCache {
    pub fn get(&mut self, store: &Store, now: std::time::Instant) -> &Result<Ledger, String> {
        self.get_at(store, now, revision())
    }

    /// 修订号显式传入：并行测试里全局写入计数会被同模块其它用例推进，
    /// 直接用全局值会让「稳态不重读」的断言变成竞态。
    fn get_at(&mut self, store: &Store, now: std::time::Instant, revision: u64) -> &Result<Ledger, String> {
        if self.cached.is_none() || self.revision != revision
            || self.read_at.is_some_and(|at| now.duration_since(at).as_secs() >= 60)
        {
            self.cached = Some(load(store));
            self.revision = revision;
            self.read_at = Some(now);
        }
        self.cached.as_ref().unwrap()
    }
}

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
    store.set_setting(KEY, &raw)?;
    REVISION.fetch_add(1, Ordering::Release);
    Ok(())
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

pub const GC_IDLE_SLEEP_CAP_SEC: u64 = 60;
pub const GC_PENDING_SLEEP_SEC: u64 = 5;
pub const GC_TIMEOUT_SEC: i64 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcAction {
    DestroyHidden,
    MarkClosed,
    Timeout,
    Open,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcTick {
    pub action: GcAction,
    pub next_check_in: std::time::Duration,
}

fn gc_tick_with_pending_sleep(
    now: i64,
    hidden: bool,
    pending: bool,
    window_open: bool,
    interactive: bool,
    started: i64,
    updated: i64,
    interval: i64,
    has_account: bool,
    pending_sleep_sec: u64,
) -> GcTick {
    use std::time::Duration;
    let interval = interval.max(60);
    let cap = Duration::from_secs(GC_IDLE_SLEEP_CAP_SEC);
    let pending_sleep = Duration::from_secs(pending_sleep_sec);

    let action = if hidden {
        GcAction::DestroyHidden
    } else if pending && !window_open {
        GcAction::MarkClosed
    } else if pending && !interactive && now - started >= GC_TIMEOUT_SEC {
        GcAction::Timeout
    } else if !pending && has_account && now - updated.max(started) >= interval {
        GcAction::Open
    } else {
        GcAction::Idle
    };

    let next_check_in = if hidden {
        cap
    } else if pending {
        match action {
            GcAction::MarkClosed | GcAction::Timeout => {
                if has_account {
                    let due_in = updated.max(started) + interval - now;
                    Duration::from_secs(due_in.clamp(1, GC_IDLE_SLEEP_CAP_SEC as i64) as u64)
                } else {
                    cap
                }
            }
            _ => pending_sleep,
        }
    } else if !has_account {
        cap
    } else if matches!(action, GcAction::Open) {
        pending_sleep
    } else {
        let due_in = updated.max(started) + interval - now;
        Duration::from_secs(due_in.clamp(1, GC_IDLE_SLEEP_CAP_SEC as i64) as u64)
    };

    GcTick {
        action,
        next_check_in,
    }
}

pub fn gc_tick(
    now: i64,
    hidden: bool,
    pending: bool,
    window_open: bool,
    interactive: bool,
    started: i64,
    updated: i64,
    interval: i64,
    has_account: bool,
) -> GcTick {
    gc_tick_with_pending_sleep(
        now,
        hidden,
        pending,
        window_open,
        interactive,
        started,
        updated,
        interval,
        has_account,
        GC_PENDING_SLEEP_SEC,
    )
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
    fn scheduler_cache_reloads_on_write_and_expires_even_without_notification() {
        let s = Store::open_memory().unwrap();
        let mut cache = LedgerCache::default();
        let mut rev = 0;
        let now = std::time::Instant::now();
        assert!(cache.get_at(&s, now, rev).as_ref().unwrap().selected.is_none());
        let before = revision();
        capture(&s, sample(A), None, 1000).unwrap();
        assert!(revision() > before, "成功写入必须推进修订号");
        rev += 1;
        assert_eq!(cache.get_at(&s, now, rev).as_ref().unwrap().selected.as_deref(), Some(A));
        // 模拟外部改写：常规 tick 不重新解析，到兜底期限必须发现损坏。
        s.set_setting(KEY, "invalid json").unwrap();
        assert!(cache.get_at(&s, now + std::time::Duration::from_secs(5), rev).is_ok());
        assert!(cache.get_at(&s, now + std::time::Duration::from_secs(60), rev).is_err());
        s.set_setting(KEY, "{}").unwrap();
        capture(&s, sample(B), None, 1020).unwrap();
        rev += 1;
        assert_eq!(cache.get_at(&s, now + std::time::Duration::from_secs(61), rev).as_ref().unwrap().selected.as_deref(), Some(B));
        disconnect(&s).unwrap();
        rev += 1;
        assert!(cache.get_at(&s, now + std::time::Duration::from_secs(62), rev).as_ref().unwrap().selected.is_none());
    }
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

    #[test]
    fn gc_tick_table_driven_branches_and_boundaries() {
        use std::time::Duration;
        let cap = Duration::from_secs(GC_IDLE_SLEEP_CAP_SEC);
        let p5 = Duration::from_secs(GC_PENDING_SLEEP_SEC);

        // 1. hidden 优先销毁并睡 CAP
        assert_eq!(
            gc_tick(100, true, true, true, false, 10, 10, 60, true),
            GcTick {
                action: GcAction::DestroyHidden,
                next_check_in: cap,
            }
        );

        // 2. pending 且窗口已关闭 -> MarkClosed，下次按剩余刷新间隔睡眠
        assert_eq!(
            gc_tick(50, false, true, false, false, 30, 10, 60, true),
            GcTick {
                action: GcAction::MarkClosed,
                next_check_in: Duration::from_secs(40), // started(30) + 60 - 50 = 40
            }
        );

        // 3. pending 且非交互满 90s(恰好到期边界) -> Timeout
        assert_eq!(
            gc_tick(120, false, true, true, false, 30, 10, 120, true),
            GcTick {
                action: GcAction::Timeout,
                next_check_in: Duration::from_secs(30), // started(30) + 120 - 120 = 30
            }
        );
        // 交互式会话满 90s 不超时，继续 5s 轮询
        assert_eq!(
            gc_tick(120, false, true, true, true, 30, 10, 120, true),
            GcTick {
                action: GcAction::Idle,
                next_check_in: p5,
            }
        );

        // 4. 无 pending、有账号、恰好到期 -> Open，且下一次进入 5s 轮询
        assert_eq!(
            gc_tick(160, false, false, false, false, 40, 100, 60, true),
            GcTick {
                action: GcAction::Open,
                next_check_in: p5,
            }
        );

        // 5. started > updated 边界 + interval < 60 被抬到 60
        assert_eq!(
            gc_tick(140, false, false, false, false, 100, 50, 15, true),
            GcTick {
                action: GcAction::Idle,
                next_check_in: Duration::from_secs(20), // max(100,50) + max(15,60) - 140 = 20
            }
        );
        assert_eq!(
            gc_tick(160, false, false, false, false, 100, 50, 15, true),
            GcTick {
                action: GcAction::Open,
                next_check_in: p5,
            }
        );

        // 6. 无结算账号 -> Idle + CAP
        assert_eq!(
            gc_tick(500, false, false, false, false, 0, 0, 60, false),
            GcTick {
                action: GcAction::Idle,
                next_check_in: cap,
            }
        );
    }

    fn simulate_schedule(pending_sleep_sec: u64, fixed_step_sec: Option<i64>) -> Vec<(i64, GcAction)> {
        let mut t = 0i64;
        let interval = 120i64;
        let updated = 10i64;
        let mut started = 0i64;
        let mut pending = false;
        let mut events = Vec::new();
        // set_config 切换 hidden 的时刻会触发 GC_WAKE.notify_one()
        let config_wakes = [500i64, 520i64];

        while t <= 600 {
            let hidden = (500..520).contains(&t);
            // 第 2 次打开窗口(约 t=250)后，模拟用户在 t=267 关掉窗口
            let window_open = pending && !(267..350).contains(&t);
            let tick = gc_tick_with_pending_sleep(
                t,
                hidden,
                pending,
                window_open,
                false,
                started,
                updated,
                interval,
                true,
                pending_sleep_sec,
            );
            match tick.action {
                GcAction::DestroyHidden => {
                    if pending || window_open {
                        events.push((t, tick.action));
                    }
                    pending = false;
                    started = 0;
                }
                GcAction::MarkClosed | GcAction::Timeout => {
                    events.push((t, tick.action));
                    pending = false;
                }
                GcAction::Open => {
                    events.push((t, tick.action));
                    pending = true;
                    started = t;
                }
                GcAction::Idle => {}
            }
            let step = fixed_step_sec.unwrap_or(tick.next_check_in.as_secs() as i64).max(1);
            let next_t = t + step;
            if fixed_step_sec.is_none() {
                if let Some(&wake_t) = config_wakes.iter().find(|&&w| w > t && w < next_t) {
                    t = wake_t;
                    continue;
                }
            }
            t = next_t;
        }
        events
    }

    fn schedules_equivalent(old: &[(i64, GcAction)], new: &[(i64, GcAction)]) -> bool {
        if old.len() != new.len() {
            return false;
        }
        old.iter()
            .zip(new.iter())
            .all(|((t1, a1), (t2, a2))| a1 == a2 && (t1 - t2).abs() <= 5)
    }

    #[test]
    fn gc_tick_matches_legacy_5s_loop_on_600s_grid_and_rejects_bad_pending_cap() {
        let legacy = simulate_schedule(GC_PENDING_SLEEP_SEC, Some(5));
        let precise = simulate_schedule(GC_PENDING_SLEEP_SEC, None);
        assert!(
            !legacy.is_empty() && schedules_equivalent(&legacy, &precise),
            "精确睡眠与旧 5s 轮询触发动作序列必须一致且时间差 <= 5s:\nlegacy={legacy:?}\nprecise={precise:?}"
        );

        // 反向样本:把 pending 分支的 5s 改成 CAP(60s),等价性断言必须变红
        let bad_pending = simulate_schedule(GC_IDLE_SLEEP_CAP_SEC, None);
        assert!(
            !schedules_equivalent(&legacy, &bad_pending),
            "pending 分支改用 CAP 时等价性测试必须变红:\nlegacy={legacy:?}\nbad={bad_pending:?}"
        );
    }
}
