use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use super::{lock, Gateway};
use crate::{
    store::Store,
    store_gateway::{uses_4sapi_pricing, PricingLookup},
};

pub struct VaultMeta {
    pub id: String,
    pub platform: String,
    pub label: String,
    pub updated_at: i64,
}

pub(super) struct Projections {
    vault: RevisionCache<HashMap<String, VaultMeta>>,
    pricing: RevisionCache<Option<PricingLookup>>,
}

struct RevisionCache<T> {
    revision: Arc<AtomicU64>,
    value: Mutex<Option<(u64, Arc<T>)>>,
    #[cfg(test)]
    loads: AtomicU64,
}

impl<T> RevisionCache<T> {
    fn new(revision: Arc<AtomicU64>) -> Self {
        Self {
            revision,
            value: Mutex::new(None),
            #[cfg(test)]
            loads: AtomicU64::new(0),
        }
    }

    fn get(
        &self,
        store: &Mutex<Store>,
        load: impl FnOnce(&Store) -> Result<T, String>,
    ) -> Result<Arc<T>, String> {
        let mut cached = lock(&self.value);
        if let Some((revision, data)) = cached.as_ref() {
            if *revision == self.revision.load(Ordering::Acquire) {
                return Ok(data.clone());
            }
        }
        let db = lock(store);
        let revision = self.revision.load(Ordering::Acquire);
        #[cfg(test)]
        self.loads.fetch_add(1, Ordering::Relaxed);
        let data = Arc::new(load(&db)?);
        *cached = Some((revision, data.clone()));
        Ok(data)
    }
}

impl Projections {
    pub(super) fn new(store: &Store) -> Self {
        Self {
            vault: RevisionCache::new(store.vault_revision.clone()),
            pricing: RevisionCache::new(store.model_revision.clone()),
        }
    }
}

impl Gateway {
    pub(super) fn vault_index(&self) -> Result<Arc<HashMap<String, VaultMeta>>, String> {
        self.projections.vault.get(&self.store, |store| {
            Ok(store
                .vault_list()?
                .into_iter()
                .map(|v| {
                    let meta = VaultMeta {
                        id: v.id,
                        platform: v.platform,
                        label: v.label,
                        updated_at: v.updated_at,
                    };
                    (meta.id.clone(), meta)
                })
                .collect())
        })
    }

    pub(super) fn channel_cost(
        &self,
        template: &str,
        base_url: &str,
        model: &str,
        prompt: Option<u64>,
        cached: Option<u64>,
        completion: Option<u64>,
    ) -> Option<f64> {
        if !uses_4sapi_pricing(template, base_url) {
            return None;
        }
        let pricing = self
            .projections
            .pricing
            .get(&self.store, |db| Ok(PricingLookup::load(db)))
            .ok()?;
        pricing
            .as_ref()
            .as_ref()?
            .estimate(model, prompt, cached, completion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{models::ModelCache, store::VaultItem};
    use serde_json::json;

    #[test]
    fn repeated_projection_reads_load_once_until_the_committed_revision_changes() {
        let db = Store::open_memory().unwrap();
        db.vault_upsert(&VaultItem {
            id: "v".into(),
            platform: "4sapi".into(),
            label: "fixture".into(),
            updated_at: 1,
            created_at: 1,
            hint: String::new(),
            note: String::new(),
            website: String::new(),
            api_endpoint: String::new(),
        })
        .unwrap();
        let cache = |ratio| ModelCache {
            pricing: json!({
                "auto_groups": ["default"], "group_ratio": { "default": 1.0 },
                "data": [{ "model_name": "fixture", "quota_type": 0,
                    "model_ratio": ratio, "completion_ratio": 1.0, "enable_groups": ["default"] }]
            })
            .to_string(),
            ..Default::default()
        };
        cache(1.0).save(&db).unwrap();
        let gateway = Gateway::new(Arc::new(Mutex::new(db))).unwrap();
        let price = || gateway.channel_cost("4sapi", "", "fixture", Some(1_000_000), None, Some(0));
        for _ in 0..10 {
            assert_eq!(gateway.vault_index().unwrap()["v"].platform, "4sapi");
            assert_eq!(price(), Some(2.0));
        }
        assert_eq!(gateway.projections.vault.loads.load(Ordering::Relaxed), 1);
        assert_eq!(gateway.projections.pricing.loads.load(Ordering::Relaxed), 1);
        {
            let db = lock(&gateway.store);
            db.vault_delete("v").unwrap();
            cache(2.0).save(&db).unwrap();
        }
        assert!(gateway.vault_index().unwrap().is_empty());
        assert_eq!(price(), Some(4.0));
        assert_eq!(gateway.projections.vault.loads.load(Ordering::Relaxed), 2);
        assert_eq!(gateway.projections.pricing.loads.load(Ordering::Relaxed), 2);
    }
}
