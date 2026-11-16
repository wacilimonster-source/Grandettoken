//! 只在本轮取数内共享桌面端快照，下轮重新读取，账号切换无需跨轮缓存失效。
use crate::workbuddy::{self, DesktopCredential};
use std::sync::OnceLock;

#[derive(Default)]
pub(crate) struct Credentials(OnceLock<Vec<DesktopCredential>>);

impl Credentials {
    fn get(&self) -> &[DesktopCredential] {
        self.get_with(|| {
            if cfg!(test) { return Vec::new(); }
            workbuddy::collect_desktop_credentials(chrono::Local::now().timestamp_millis(), true)
                .unwrap_or_else(|e| {
                    crate::diaglog::write_keyed("wb", "tokens-empty", &format!("桌面端凭据为空:{}", e.message()));
                    Vec::new()
                })
        })
    }

    fn get_with(&self, read: impl FnOnce() -> Vec<DesktopCredential>) -> &[DesktopCredential] {
        self.0.get_or_init(read)
    }

    pub fn tokens(&self, now_ms: i64, region: Option<bool>) -> Vec<String> {
        workbuddy::desktop_tokens_from(now_ms, region, self.get().to_vec())
    }

    pub fn accounts(&self, region: bool) -> Vec<String> {
        self.get().iter().filter(|c| c.is_global() == region && !c.uin.is_empty())
            .map(|c| c.uin.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_share_one_snapshot_and_next_cycle_has_new_accounts() {
        let cycle = Credentials::default();
        let reads = std::cell::Cell::new(0);
        let read = || { reads.set(reads.get() + 1); Vec::new() };
        let probe = Credentials::default();
        probe.get_with(read);
        probe.get_with(read);
        assert_eq!(reads.get(), 1, "同轮只能扫描一次");
        cycle.0.set(vec![DesktopCredential {
            domain: "www.workbuddy.ai".into(), uin: "account-a".into(), ..Default::default()
        }]).unwrap();
        assert!(cycle.accounts(false).is_empty());
        assert_eq!(cycle.accounts(true), vec!["account-a"]);
        assert_eq!(cycle.accounts(true), vec!["account-a"]);
        let next = Credentials::default();
        next.0.set(vec![DesktopCredential {
            domain: "www.workbuddy.ai".into(), uin: "account-b".into(), ..Default::default()
        }]).unwrap();
        assert_eq!(next.accounts(true), vec!["account-b"]);
    }
}
