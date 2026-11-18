//! 跟随官方当前会话，持久保存每个账号的最后成功配额。
use crate::{
    antigravity,
    config::Config,
    fetch::{self, ChannelView},
    providers::{self, FetchResult},
    store::Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, OnceLock};

const KEY: &str = "antigravity_accounts_v1";
pub const PREFIX: &str = "antigravity:";
static GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static CACHE: OnceLock<Mutex<Option<Attempt>>> = OnceLock::new();

pub fn is_channel(id: &str) -> bool {
    id == "antigravity" || account_id(id).is_some()
}

pub fn account_id(id: &str) -> Option<&str> {
    id.strip_prefix(PREFIX).filter(|s| {
        s.len() == 24
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}

#[derive(Clone)]
struct Attempt {
    path: std::path::PathBuf,
    at: std::time::Instant,
    result: Result<Observation, String>,
    failures: u32,
    cooldown: std::time::Duration,
}

impl Attempt {
    fn cached(
        &self,
        path: &std::path::Path,
        now: std::time::Instant,
    ) -> Option<Result<Observation, String>> {
        if now.saturating_duration_since(self.at) >= self.cooldown {
            return None;
        }
        Some(if self.path == path {
            self.result.clone()
        } else {
            Err("CLI 路径已变更，将在采集冷却结束后生效".into())
        })
    }
}

#[derive(Debug, Clone)]
struct Observation {
    account: String,
    result: FetchResult,
    at: i64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Ledger {
    records: Vec<Record>,
    current: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    account: String,
    at: i64,
    result: FetchResult,
}

impl Ledger {
    fn load(store: &Store) -> Result<Self, String> {
        match store.get_setting(KEY).map_err(|_| "账号记录读取失败")? {
            Some(raw) => {
                let ledger: Self =
                    serde_json::from_str(&raw).map_err(|_| "账号记录损坏，已保留原始数据")?;
                let unique: std::collections::HashSet<_> =
                    ledger.records.iter().map(|r| &r.account).collect();
                if unique.len() != ledger.records.len()
                    || ledger.records.iter().any(|r| {
                        account_id(&format!("{PREFIX}{}", r.account)).is_none()
                            || !r.result.valid
                            || r.at <= 0
                            || chrono::DateTime::from_timestamp(r.at, 0).is_none()
                    })
                {
                    return Err("账号记录格式异常，已保留原始数据".into());
                }
                Ok(ledger)
            }
            None => Ok(Self::default()),
        }
    }

    fn accept(&mut self, observation: &Observation) {
        if self.records.iter().any(|r| r.at > observation.at) {
            return;
        }
        if let Some(record) = self
            .records
            .iter_mut()
            .find(|r| r.account == observation.account)
        {
            if observation.at >= record.at {
                record.at = observation.at;
                record.result = observation.result.clone();
            }
        } else {
            self.records.push(Record {
                account: observation.account.clone(),
                at: observation.at,
                result: observation.result.clone(),
            });
        }
        self.current = Some(observation.account.clone());
    }
}

fn parse(body: &Value, at: i64) -> Result<Observation, String> {
    let account = body
        .get("account")
        .and_then(Value::as_str)
        .filter(|s| account_id(&format!("{PREFIX}{s}")).is_some())
        .ok_or("官方回调尚未提供可靠账号身份")?;
    let quota = body
        .get("quota")
        .and_then(Value::as_object)
        .ok_or("官方回调缺少额度")?;
    let mut groups = Vec::new();
    for (key, bucket) in quota {
        if key.len() > 80
            || !key
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
        {
            continue;
        }
        let Some(fraction) = bucket
            .get("remaining_fraction")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
        else {
            continue;
        };
        if bucket.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let group = if key == "gemini-weekly" || key == "gemini-5h" {
            "Gemini Models"
        } else if key.starts_with("3p-") {
            "Claude and GPT models"
        } else {
            key.as_str()
        };
        let reset = bucket
            .get("reset_time")
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.to_rfc3339());
        groups.push(json!({"name": group, "buckets": [{"id": key, "name": key,
            "window": if key.ends_with("weekly") { "weekly" } else if key.ends_with("5h") { "5h" } else { "" },
            "remaining_fraction": fraction, "reset_time": reset}]}));
    }
    let result = antigravity::extract_antigravity(&json!({"groups": groups}));
    if !result.valid {
        return Err("官方回调尚未提供有效配额".into());
    }
    Ok(Observation {
        account: account.into(),
        result,
        at,
    })
}

async fn observe(path: std::path::PathBuf, backoff: u64) -> Result<Observation, String> {
    tokio::task::spawn_blocking(move || {
        // 锁覆盖阻塞任务生命周期，调用方取消 await 后仍保证单飞和冷却记账。
        let mut cache = CACHE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(result) = cache
            .as_ref()
            .and_then(|hit| hit.cached(&path, std::time::Instant::now()))
        {
            return result;
        }
        let result = crate::antigravity_console::capture(&path)
            .and_then(|body| parse(&body, fetch::now_ts()));
        let failures = if result.is_err() {
            cache
                .as_ref()
                .filter(|hit| hit.path == path)
                .map_or(1, |hit| hit.failures.saturating_add(1))
        } else {
            0
        };
        let minimum = antigravity::AUTO_COOLDOWN_SECS as u64;
        let cooldown = if failures >= 3 {
            backoff.max(minimum)
        } else {
            minimum
        };
        *cache = Some(Attempt {
            path,
            at: std::time::Instant::now(),
            result: result.clone(),
            failures,
            cooldown: std::time::Duration::from_secs(cooldown),
        });
        result
    })
    .await
    .map_err(|_| "Antigravity 账号采集任务异常退出".to_string())
    .and_then(|r| r)
}

pub(crate) async fn probe_usage(cli_path: &str) -> antigravity::AgyOutcome {
    let _gate = GATE
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    match observe(
        std::path::PathBuf::from(cli_path),
        Config::default().backoff_interval_sec,
    )
    .await
    {
        Ok(observation) => antigravity::AgyOutcome {
            result: observation.result,
            fail_kind: None,
            err_code: None,
            bytes: 0,
            shape: "account,quota".into(),
        },
        Err(error) => antigravity::AgyOutcome {
            result: FetchResult {
                error: Some(error),
                kind: providers::Kind::Percent,
                unit: "%".into(),
                ..Default::default()
            },
            fail_kind: Some("business"),
            err_code: Some(crate::errcodes::BODY_SHAPE),
            bytes: 0,
            shape: String::new(),
        },
    }
}

/// 返回采集状态占位；最终列表在同一轮结束时展开为账号卡。
pub(crate) async fn collect(store: &Arc<Mutex<Store>>, cfg: &Config) -> ChannelView {
    let _gate = GATE
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let def = providers::find("antigravity").expect("built-in Antigravity");
    let mut base = fetch::placeholder(def);
    let path = antigravity::resolve_agy_path()
        .map_err(|_| "未检测到 agy CLI，请安装并在官方客户端完成登录".to_string());
    base.has_key = path.is_ok();
    let observed = match path {
        Ok(path) => observe(path, cfg.backoff_interval_sec).await,
        Err(error) => Err(error),
    };
    let result = observed.and_then(|observation| {
        let store = store.lock().unwrap_or_else(|p| p.into_inner());
        persist(&store, cfg, &observation)
    });
    base.valid = result.is_ok();
    base.error = result.err();
    base
}

fn persist(store: &Store, cfg: &Config, observation: &Observation) -> Result<(), String> {
    let mut ledger = Ledger::load(store)?;
    let id = format!("{PREFIX}{}", observation.account);
    if cfg.hidden_channels.contains(&id) {
        ledger.current = None;
    } else {
        ledger.accept(observation);
    }
    let raw = serde_json::to_string(&ledger).map_err(|_| "账号记录序列化失败")?;
    store
        .set_setting(KEY, &raw)
        .map_err(|_| "账号记录保存失败，本轮数据未持久化".into())
}

pub(crate) fn expand(base: &ChannelView, store: &Store, cfg: &Config) -> Vec<ChannelView> {
    let ledger = match Ledger::load(store) {
        Ok(ledger) => ledger,
        Err(error) => {
            let mut view = base.clone();
            view.valid = false;
            view.error = Some(error);
            return vec![view];
        }
    };
    if ledger.records.is_empty() {
        if let Some(scope) = crate::recovery::Snapshot::stored_scope(store, "antigravity") {
            if let Some(snapshot) = crate::recovery::Snapshot::load(store, "antigravity", &scope) {
                let mut view = fetch::build_view(
                    providers::find("antigravity").unwrap(),
                    snapshot.result,
                    true,
                    snapshot.ts,
                    store,
                    cfg,
                    None,
                    "",
                    1,
                );
                view.name = "Antigravity · 账号待确认".into();
                view.auth_label = "旧版快照 · 账号待确认".into();
                view.error = base.error.clone();
                view.hidden = base.hidden;
                return vec![view];
            }
        }
        return vec![base.clone()];
    }
    ledger
        .records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let id = format!("{PREFIX}{}", record.account);
            let current =
                base.valid && !base.hidden && ledger.current.as_deref() == Some(&record.account);
            let mut view = fetch::build_view(
                providers::find("antigravity").unwrap(),
                record.result.clone(),
                !current,
                record.at,
                store,
                cfg,
                None,
                "",
                1,
            );
            view.id = id.clone();
            let alias = cfg
                .antigravity_account_names
                .get(&id)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty());
            view.name = format!(
                "Antigravity · {}",
                alias
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("账号 {}", index + 1))
            );
            view.auth_label = if current {
                "当前账号"
            } else {
                "上次记录"
            }
            .into();
            view.error = base.error.clone();
            view.hidden = base.hidden || cfg.hidden_channels.contains(&id);
            view.extra
                .push(("账号状态".into(), view.auth_label.clone()));
            view.extra.push((
                "采集时间".into(),
                chrono::DateTime::from_timestamp(record.at, 0)
                    .unwrap()
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            ));
            view
        })
        .collect()
}

pub(crate) fn expand_all(rows: Vec<ChannelView>, store: &Store, cfg: &Config) -> Vec<ChannelView> {
    rows.into_iter()
        .flat_map(|view| {
            if view.id == "antigravity" {
                expand(&view, store, cfg)
            } else {
                vec![view]
            }
        })
        .collect()
}

pub(crate) fn previous_view(rows: &[ChannelView], id: &str) -> Option<ChannelView> {
    if let Some(view) = rows.iter().find(|view| view.id == id) {
        return Some(view.clone());
    }
    if id != "antigravity" {
        return None;
    }
    let first = rows.iter().find(|view| account_id(&view.id).is_some())?;
    let mut base = fetch::placeholder(providers::find("antigravity").unwrap());
    base.has_key = true;
    base.valid = rows
        .iter()
        .any(|view| account_id(&view.id).is_some() && view.valid && !view.stale);
    base.error = first.error.clone();
    Some(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "需要本机官方 agy 登录，验证真实采集与账本展开"]
    fn live_current_account_roundtrip() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let store = Arc::new(Mutex::new(Store::open_memory().unwrap()));
                let cfg = Config::default();
                let base = collect(&store, &cfg).await;
                assert!(base.valid, "{}", base.error.as_deref().unwrap_or_default());
                let rows = expand(&base, &store.lock().unwrap(), &cfg);
                assert_eq!(rows.len(), 1);
                assert!(account_id(&rows[0].id).is_some());
                assert!(!rows[0].stale);
                assert_eq!(rows[0].auth_label, "当前账号");
                assert!(rows[0].windows.iter().any(|w| w.main));
                let cli = antigravity::resolve_agy_path().unwrap();
                let probe = probe_usage(&cli.to_string_lossy()).await;
                assert!(probe.result.valid);
                assert_eq!(probe.result.remaining, rows[0].remaining);
            });
    }

    fn observed(account: &str, percent: f64, at: i64) -> Observation {
        parse(&json!({"account": account, "quota": {"gemini-weekly": {"remaining_fraction": percent / 100.0}}}), at).unwrap()
    }

    #[test]
    fn switching_keeps_two_ledgers_and_returning_updates_original() {
        let a = "aaaaaaaaaaaaaaaaaaaaaaaa";
        let b = "bbbbbbbbbbbbbbbbbbbbbbbb";
        let mut ledger = Ledger::default();
        ledger.accept(&observed(a, 80.0, 100));
        ledger.accept(&observed(b, 30.0, 200));
        assert_eq!(ledger.records.len(), 2);
        assert_eq!(ledger.records[0].at, 100);
        assert_eq!(ledger.records[0].result.remaining, Some(80.0));
        ledger.accept(&observed(a, 70.0, 300));
        assert_eq!(ledger.records.len(), 2);
        assert_eq!(ledger.current.as_deref(), Some(a));
        assert_eq!(ledger.records[1].result.remaining, Some(30.0));
        ledger.accept(&observed(a, 90.0, 50));
        assert_eq!(ledger.records[0].result.remaining, Some(70.0));
        ledger.accept(&observed(b, 99.0, 200));
        assert_eq!(ledger.current.as_deref(), Some(a));
    }

    #[test]
    fn missing_identity_or_quota_is_rejected() {
        assert!(parse(&json!({"quota":{}}), 100).is_err());
        assert!(parse(
            &json!({"account":"aaaaaaaaaaaaaaaaaaaaaaaa", "quota":{}}),
            100
        )
        .is_err());
        assert!(!is_channel("antigravity:token"));
        assert!(is_channel("antigravity:aaaaaaaaaaaaaaaaaaaaaaaa"));
    }

    #[test]
    fn cooldown_covers_success_failure_and_path_changes_with_injected_clock() {
        let at = std::time::Instant::now();
        let path = std::path::PathBuf::from("fixture-agy");
        let interval = std::time::Duration::from_secs(antigravity::AUTO_COOLDOWN_SECS as u64);
        let mut attempt = Attempt {
            path: path.clone(),
            at,
            result: Ok(observed("aaaaaaaaaaaaaaaaaaaaaaaa", 80.0, 100)),
            failures: 0,
            cooldown: interval,
        };
        assert!(attempt.cached(&path, at + interval / 2).unwrap().is_ok());
        assert!(attempt
            .cached(std::path::Path::new("another-agy"), at + interval / 2)
            .unwrap()
            .is_err());
        assert!(attempt.cached(&path, at + interval).is_none());
        attempt.result = Err("读取失败".into());
        assert!(attempt.cached(&path, at + interval / 2).unwrap().is_err());
        assert!(attempt.cached(&path, at + interval).is_none());
        attempt.failures = 3;
        attempt.cooldown = std::time::Duration::from_secs(Config::default().backoff_interval_sec);
        assert!(attempt.cached(&path, at + interval).unwrap().is_err());
        assert!(attempt.cached(&path, at + attempt.cooldown).is_none());
    }

    #[test]
    fn hidden_account_keeps_snapshot_and_ledger_survives_restart() {
        let store = Store::open_memory().unwrap();
        let a = observed("aaaaaaaaaaaaaaaaaaaaaaaa", 80.0, 100);
        let mut cfg = Config::default();
        persist(&store, &cfg, &a).unwrap();
        let id = format!("{PREFIX}{}", a.account);
        cfg.hidden_channels.push(id);
        persist(&store, &cfg, &observed(&a.account, 20.0, 200)).unwrap();
        let reloaded = Ledger::load(&store).unwrap();
        assert_eq!(reloaded.records[0].at, 100);
        assert_eq!(reloaded.records[0].result.remaining, Some(80.0));
        assert!(reloaded.current.is_none());
        let b = observed("bbbbbbbbbbbbbbbbbbbbbbbb", 30.0, 300);
        persist(&store, &cfg, &b).unwrap();
        let switched = Ledger::load(&store).unwrap();
        assert_eq!(switched.records[0].at, 100);
        assert_eq!(switched.records[0].result.remaining, Some(80.0));
        assert_eq!(switched.records[1].result.remaining, Some(30.0));
        assert_eq!(switched.current.as_deref(), Some(b.account.as_str()));
    }

    #[test]
    fn legacy_unassigned_snapshot_survives_failure_and_new_identity_starts_own_record() {
        let store = Store::open_memory().unwrap();
        let old = observed("aaaaaaaaaaaaaaaaaaaaaaaa", 80.0, 100);
        crate::recovery::Snapshot::new("legacy-path-scope".into(), old.at, old.result)
            .save(&store, "antigravity")
            .unwrap();
        let cfg = Config::default();
        let base = fetch::placeholder(providers::find("antigravity").unwrap());
        let previous = expand(&base, &store, &cfg);
        assert_eq!(previous[0].id, "antigravity");
        assert!(previous[0].stale);
        assert!(previous[0].name.contains("账号待确认"));
        assert_eq!(previous[0].remaining, Some(80.0));
        persist(
            &store,
            &cfg,
            &observed("bbbbbbbbbbbbbbbbbbbbbbbb", 30.0, 200),
        )
        .unwrap();
        let identified = expand(&base, &store, &cfg);
        assert_eq!(identified.len(), 1);
        assert_eq!(identified[0].id, "antigravity:bbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(identified[0].remaining, Some(30.0));
    }

    #[test]
    fn unrelated_retry_reuses_account_rows_as_provider_status() {
        let rows = vec![ChannelView {
            id: "antigravity:aaaaaaaaaaaaaaaaaaaaaaaa".into(),
            valid: true,
            stale: false,
            ..Default::default()
        }];
        assert!(previous_view(&rows, "antigravity").unwrap().valid);
        assert!(previous_view(&rows, "codex").is_none());
        let old = vec![ChannelView {
            stale: true,
            ..rows[0].clone()
        }];
        assert!(!previous_view(&old, "antigravity").unwrap().valid);
    }

    #[test]
    fn database_write_failure_preserves_original_ledger() {
        let mut random = [0u8; 8];
        getrandom::getrandom(&mut random).unwrap();
        let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let file = std::env::temp_dir().join(format!("grandettoken-ag-accounts-{suffix}.db"));
        {
            let store = Store::open(&file).unwrap();
            let cfg = Config::default();
            persist(
                &store,
                &cfg,
                &observed("aaaaaaaaaaaaaaaaaaaaaaaa", 80.0, 100),
            )
            .unwrap();
            let original = store.get_setting(KEY).unwrap();
            let connection = rusqlite::Connection::open(&file).unwrap();
            connection.execute_batch("CREATE TRIGGER fail_account_write BEFORE INSERT ON settings BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END;").unwrap();
            let failure = persist(
                &store,
                &cfg,
                &observed("bbbbbbbbbbbbbbbbbbbbbbbb", 20.0, 200),
            );
            assert!(failure.unwrap_err().contains("保存失败"));
            assert_eq!(store.get_setting(KEY).unwrap(), original);
        }
        for suffix in ["", "-wal", "-shm"] {
            let path = std::path::PathBuf::from(format!("{}{suffix}", file.display()));
            if path.exists() {
                std::fs::remove_file(path).unwrap();
            }
        }
    }

    #[test]
    fn persisted_accounts_expand_independently_and_failures_preserve_records() {
        let store = Store::open_memory().unwrap();
        let mut ledger = Ledger::default();
        ledger.accept(&observed("aaaaaaaaaaaaaaaaaaaaaaaa", 80.0, 100));
        ledger.accept(&observed("bbbbbbbbbbbbbbbbbbbbbbbb", 30.0, 200));
        store
            .set_setting(KEY, &serde_json::to_string(&ledger).unwrap())
            .unwrap();
        let mut cfg = Config::default();
        cfg.hidden_channels
            .push("antigravity:aaaaaaaaaaaaaaaaaaaaaaaa".into());
        let mut base = fetch::placeholder(providers::find("antigravity").unwrap());
        base.valid = true;
        let rows = expand(&base, &store, &cfg);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].stale && rows[0].hidden);
        assert!(!rows[1].stale);
        assert_eq!(rows[0].remaining, Some(80.0));
        assert_eq!(rows[1].remaining, Some(30.0));
        base.valid = false;
        base.error = Some("读取失败".into());
        let old = expand(&base, &store, &cfg);
        assert!(old.iter().all(|r| r.stale && r.error.is_some()));
        assert_eq!(Ledger::load(&store).unwrap().records.len(), 2);
        base.hidden = true;
        assert!(expand(&base, &store, &cfg)
            .iter()
            .all(|r| r.hidden && r.stale));
    }
}
