use crate::codex::{self, ResetCredits};
use std::{collections::HashMap, sync::{Arc, LazyLock, Mutex}, time::{Duration, Instant}};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Default)]
pub(super) struct Inventory {
    last: Option<ResetCredits>,
    resume: Option<Instant>,
}

type Slot = Arc<AsyncMutex<Inventory>>;
static SLOTS: LazyLock<Mutex<HashMap<(String, String), Slot>>> = LazyLock::new(Mutex::default);

pub(super) fn slot(endpoint: &str, token: &str, account: Option<&str>) -> Slot {
    let scope = super::fetch_creds::account_scope(token, account);
    crate::lockx::lock(&SLOTS).entry((endpoint.to_owned(), scope)).or_default().clone()
}

impl Inventory {
    pub(super) fn cooling(&self) -> bool {
        self.resume.is_some_and(|at| Instant::now() < at)
    }

    pub(super) fn pause(&mut self, seconds: u64) {
        self.resume = Some(Instant::now() + Duration::from_secs(seconds));
    }

    pub(super) fn unavailable(&self) -> ResetCredits {
        codex::unavailable(self.last.clone(), "重置卡查询已限流，冷却结束后再查询".into())
    }

    pub(super) fn remember(&mut self, cards: &ResetCredits) {
        self.last = Some(cards.clone());
        self.resume = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_cooldown_is_isolated_and_expires() {
        let account = slot("fixture", "token", Some("account-a"));
        let renewed = slot("fixture", "rotated", Some("account-a"));
        let other = slot("fixture", "token", Some("account-b"));
        assert!(Arc::ptr_eq(&account, &renewed));
        assert!(!Arc::ptr_eq(&account, &other));
        assert!(!Arc::ptr_eq(&slot("fixture", "a", None), &slot("fixture", "b", None)));
        let mut inventory = Inventory::default();
        inventory.pause(60);
        assert!(inventory.cooling());
        inventory.resume = Some(Instant::now());
        assert!(!inventory.cooling());
    }
}
