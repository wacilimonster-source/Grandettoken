//! Quota recovery: bounded retries and credential-scoped, successful snapshots only.
use crate::{fetch::FailKind, providers::FetchResult, store::Store};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryView {
    pub phase: String,
    pub attempts: u32,
    pub next_retry_at: i64,
}

#[derive(Clone)]
pub struct Recovery {
    pub attempts: u32,
    pub started: i64,
    pub next: i64,
    pub kind: Option<FailKind>,
}

impl Recovery {
    pub fn failed(previous: Option<&Self>, now: i64, kind: Option<FailKind>) -> Self {
        let attempts = previous.map_or(1, |p| p.attempts.saturating_add(1));
        let started = previous.map_or(now, |p| p.started);
        let retryable = kind.map_or(true, |k| k.retryable());
        let delay = if retryable && attempts < 4 && now - started < 60 {
            [2, 5, 15][attempts as usize - 1]
        } else {
            match attempts {
                0..=4 => 60,
                5 => 300,
                _ => 900,
            }
        };
        let delay = match kind {
            Some(FailKind::RateLimited(wait)) => delay.max(wait.min(86_400) as i64),
            _ => delay,
        };
        Self {
            attempts,
            started,
            next: now + delay,
            kind,
        }
    }

    pub fn view(&self, now: i64) -> RecoveryView {
        let phase = if self.kind == Some(FailKind::Auth) {
            "auth"
        } else if self.kind.map_or(true, |k| k.retryable())
            && self.attempts < 4
            && now - self.started < 60
        {
            "retrying"
        } else {
            "unavailable"
        };
        RecoveryView {
            phase: phase.into(),
            attempts: self.attempts,
            next_retry_at: self.next,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub scope: String,
    pub ts: i64,
    pub result: FetchResult,
}

impl Snapshot {
    pub fn new(scope: String, ts: i64, mut result: FetchResult) -> Self {
        // Extra/error strings may contain upstream text. Persist only typed quota data.
        result.extra.retain(|(k, _)| k == "套餐");
        result.error = None;
        if let Some(cards) = &mut result.reset_credits {
            cards.error = None;
        }
        Self { scope, ts, result }
    }
    pub fn load(store: &Store, id: &str, scope: &str) -> Option<Self> {
        if scope.is_empty() {
            return None;
        }
        let raw = store
            .get_setting(&format!("quota_snapshot_v1:{id}"))
            .ok()??;
        let snapshot: Self = serde_json::from_str(&raw).ok()?;
        (snapshot.scope == scope && snapshot.result.valid && snapshot.ts > 0).then_some(snapshot)
    }

    /// 上一次成功快照属于哪本账(凭据 scope)。**不要求**与当前 scope 相同 ——
    /// 换账号/换令牌的那一刻,正是要拿它来判断"上一本账是谁"。
    /// 快照坏了 / 没有快照 → None(按"没有历史"处理,不能据此误判换号)。
    pub fn stored_scope(store: &Store, id: &str) -> Option<String> {
        let raw = store
            .get_setting(&format!("quota_snapshot_v1:{id}"))
            .ok()??;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        v.get("scope")
            .and_then(|s| s.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
    pub fn save(&self, store: &Store, id: &str) -> Result<(), String> {
        if !self.result.valid || self.scope.is_empty() || self.ts <= 0 {
            return Err("只保存有凭据范围的成功额度快照".into());
        }
        store.set_setting(
            &format!("quota_snapshot_v1:{id}"),
            &serde_json::to_string(self).map_err(|e| e.to_string())?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retries_are_bounded_and_rate_limit_is_respected() {
        let mut r = Recovery::failed(None, 100, Some(FailKind::Transport));
        assert_eq!(r.next, 102);
        for (now, next) in [(102, 107), (107, 122), (122, 182), (182, 482), (482, 1382)] {
            r = Recovery::failed(Some(&r), now, Some(FailKind::Transport));
            assert_eq!(r.next, next);
        }
        assert_eq!(r.view(482).phase, "unavailable");
        assert_eq!(
            Recovery::failed(None, 100, Some(FailKind::Auth))
                .view(100)
                .phase,
            "auth"
        );
        assert_eq!(
            Recovery::failed(None, 100, Some(FailKind::RateLimited(120))).next,
            220
        );
    }
    #[test]
    fn restart_snapshot_is_scoped_and_contains_no_error_or_arbitrary_extra() {
        let store = Store::open_memory().unwrap();
        let result = FetchResult {
            valid: true,
            remaining: Some(42.0),
            error: Some("secret".into()),
            extra: vec![("unknown".into(), "secret".into())],
            ..Default::default()
        };
        Snapshot::new("account-a".into(), 100, result)
            .save(&store, "test")
            .unwrap();
        assert!(Snapshot::load(&store, "test", "account-b").is_none());
        assert!(Snapshot::load(&store, "test", "").is_none());
        // 换账号判定要能读到"上一本账"的 scope(即使它和当前 scope 不同)
        assert_eq!(Snapshot::stored_scope(&store, "test").as_deref(), Some("account-a"));
        assert_eq!(Snapshot::stored_scope(&store, "missing"), None);
        store.set_setting("quota_snapshot_v1:broken", "{").unwrap();
        assert_eq!(Snapshot::stored_scope(&store, "broken"), None);
        let loaded = Snapshot::load(&store, "test", "account-a").unwrap();
        assert_eq!(loaded.result.remaining, Some(42.0));
        assert_eq!(loaded.ts, 100);
        assert!(loaded.result.error.is_none());
        assert!(loaded.result.extra.is_empty());
        assert!(store.latest_snapshot("test").unwrap().is_none());
    }

    #[test]
    fn failed_snapshot_cannot_replace_success_and_corruption_is_ignored() {
        let store = Store::open_memory().unwrap();
        Snapshot::new(
            "a".into(),
            100,
            FetchResult {
                valid: true,
                remaining: Some(9.0),
                ..Default::default()
            },
        )
        .save(&store, "t")
        .unwrap();
        assert!(Snapshot::new("a".into(), 200, FetchResult::default())
            .save(&store, "t")
            .is_err());
        assert_eq!(Snapshot::load(&store, "t", "a").unwrap().ts, 100);
        store.set_setting("quota_snapshot_v1:t", "broken").unwrap();
        assert!(Snapshot::load(&store, "t", "a").is_none());
    }

    #[test]
    fn deadline_and_business_errors_end_quick_retries() {
        let old = Recovery::failed(None, 100, Some(FailKind::Transport));
        assert_eq!(
            Recovery::failed(Some(&old), 161, Some(FailKind::Server))
                .view(161)
                .phase,
            "unavailable"
        );
        assert_eq!(
            Recovery::failed(None, 100, Some(FailKind::Business)).next,
            160
        );
        assert_eq!(
            Recovery::failed(None, 100, Some(FailKind::Client))
                .view(100)
                .phase,
            "unavailable"
        );
    }

    #[test]
    fn percent_snapshot_preserves_windows_and_expiry_metadata() {
        let store = Store::open_memory().unwrap();
        let result = FetchResult {
            valid: true,
            kind: crate::providers::Kind::Percent,
            windows: vec![crate::providers::Window {
                key: "weekly".into(),
                label: "本周".into(),
                percent: 20.0,
                remain_percent: 80.0,
                status: "ok".into(),
                resets_at: Some("2026-11-07T00:00:00Z".into()),
                main: true,
            }],
            expiry_complete: true,
            ..Default::default()
        };
        Snapshot::new("a".into(), 100, result)
            .save(&store, "t")
            .unwrap();
        let r = Snapshot::load(&store, "t", "a").unwrap().result;
        assert_eq!(r.windows[0].remain_percent, 80.0);
        assert_eq!(
            r.windows[0].resets_at.as_deref(),
            Some("2026-11-07T00:00:00Z")
        );
        assert!(r.expiry_complete);
    }
}
