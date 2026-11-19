use super::*;
use std::time::Duration;
use crate::gateway_proto::{ThinkingMode,ThoughtRequestPolicy,OpenAiWire};
use serde_json::{json,Value};
use super::http::*;
mod http_bridge;

#[test]
fn sync_tasks_keep_other_pending_and_errors_when_finishing_out_of_order() {
    let g = Gateway::new(Arc::new(Mutex::new(Store::open_memory().unwrap()))).unwrap();
    let a = g.begin_sync("a");
    let b = g.begin_sync("b");
    assert!(g.finish_sync(b, Ok(())).pending);
    let state = g.finish_sync(a, Err("fixture a failure".into()));
    assert!(!state.pending);
    assert!(state.error.unwrap().contains("fixture a failure"));
    let old = g.begin_sync("a");
    let current = g.begin_sync("a");
    g.finish_sync(current, Ok(()));
    assert!(g.finish_sync(old, Err("stale failure".into())).error.is_none());
    let failure = g.begin_sync("import");
    g.finish_sync(failure, Err("fixture import failed".into()));
    let before = lock(&g.inner).config_revision;
    let retry = g.begin_sync("import");
    assert!(g.finish_sync(retry, Ok(())).error.is_none());
    assert_eq!(lock(&g.inner).config_revision, before, "status-only reads must not invalidate probes");
    g.invalidate_vault("not-yet-published");
    assert_ne!(lock(&g.inner).config_revision, before);
}

#[test]
fn damaged_settings_are_not_treated_as_first_install() {
    let store = Arc::new(Mutex::new(Store::open_memory().unwrap()));
    lock(&store).set_setting(CONFIG_KEY, "{damaged fixture").unwrap();
    assert!(Gateway::new(store.clone()).is_err());
    assert_eq!(lock(&store).get_setting(CONFIG_KEY).unwrap().as_deref(), Some("{damaged fixture"));
}

#[test]
fn missing_settings_allow_initial_defaults() {
    let store = Arc::new(Mutex::new(Store::open_memory().unwrap()));
    let g = Gateway::new(store).unwrap();
    assert!(lock(&g.inner).settings.channels.is_empty());
}

fn edit_fixture_channel() -> Published {
    serde_json::from_value(json!({
        "channelId":"edit", "vaultId":"v", "source":"openai_compat", "template":"custom",
        "baseUrl":"https://old.example/v1", "alias":"old", "host":"old.example",
        "models":["old-model"], "manualModels":["manual"], "allowedModels":["old-model"],
        "updatedAt":10, "lastStatus":"unknown"
    })).unwrap()
}

fn edit_fixture(store: Store) -> Gateway {
    let store = Arc::new(Mutex::new(store));
    lock(&store).vault_upsert(&crate::store::VaultItem {
        id:"v".into(), platform:"openai".into(), label:"keep-label".into(), hint:"***".into(),
        note:"keep-note".into(), api_endpoint:"https://old.example/v1".into(),
        website:"https://old.example".into(), created_at:1, updated_at:10,
    }).unwrap();
    let g = Gateway::new(store).unwrap();
    lock(&g.inner).settings.channels.push(edit_fixture_channel());
    g.save(&lock(&g.inner).settings).unwrap();
    g
}

#[test]
fn user_save_merges_current_probe_without_losing_user_changes() {
    let g = edit_fixture(Store::open_memory().unwrap());
    let mut inner = lock(&g.inner);
    let mut draft = inner.settings.channels[0].clone();
    draft.alias = "edited".into(); draft.system_proxy = false;
    draft.manual_models = vec!["new-manual".into()];
    draft.allowed_models = Some(vec!["new-model".into()]);
    let current = &mut inner.settings.channels[0];
    current.models = vec!["new-model".into()]; current.last_status = "error".into();
    current.last_error = Some("fresh-error".into()); current.last_checked_at = Some(20);
    current.updated_at = 20; current.host = "fresh-host".into();
    draft.merge_current_probe(current);
    let mut expected = current.clone();
    expected.alias = draft.alias.clone(); expected.system_proxy = draft.system_proxy;
    expected.manual_models = draft.manual_models.clone(); expected.allowed_models = draft.allowed_models.clone();
    let mut next = inner.settings.clone(); next.channels[0] = draft;
    g.commit_settings(&mut inner, next).unwrap();
    assert_eq!(serde_json::to_value(&inner.settings.channels[0]).unwrap(), serde_json::to_value(expected).unwrap());
    let mut relinked = inner.settings.channels[0].clone();
    relinked.vault_id = "different-vault".into(); relinked.updated_at = 99;
    relinked.base_url = "https://new.example/v1".into(); relinked.host = "new.example".into();
    relinked.merge_current_probe(&inner.settings.channels[0]);
    assert_eq!(relinked.updated_at, 99); assert_eq!(relinked.host, "new.example");
}

#[test]
fn publish_config_or_vault_cas_failure_does_not_write_endpoint_or_settings() {
    for change in ["config", "vault", "delete"] {
        let g = edit_fixture(Store::open_memory().unwrap());
        let mut inner = lock(&g.inner);
        let revision = inner.config_revision;
        let mut next = inner.settings.clone(); next.channels[0].alias = "new".into();
        next.channels[0].base_url = "https://new.example/v1".into(); next.channels[0].updated_at = 11;
        match change {
            "config" => inner.config_revision += 1,
            "vault" => {
                let store = lock(&g.store); let mut item = store.vault_get("v").unwrap().unwrap();
                item.updated_at = 12; item.note = "new-note".into(); store.vault_upsert(&item).unwrap();
            },
            _ => lock(&g.store).vault_delete("v").unwrap(),
        }
        let before_metadata = lock(&g.store).vault_get("v").unwrap().map(|v| serde_json::to_value(v).unwrap());
        let before_settings = lock(&g.store).get_setting(CONFIG_KEY).unwrap();
        let before_memory = serde_json::to_value(&inner.settings).unwrap();
        assert!(g.commit_publish(&mut inner, revision, next.clone(), Some(("v", 10)), None).is_err());
        assert!(g.commit_publish(&mut inner, revision, next, Some(("v", 10)), Some(("https://new.example/v1", 11))).is_err());
        assert_eq!(lock(&g.store).vault_get("v").unwrap().map(|v| serde_json::to_value(v).unwrap()), before_metadata);
        assert_eq!(lock(&g.store).get_setting(CONFIG_KEY).unwrap(), before_settings);
        assert_eq!(serde_json::to_value(&inner.settings).unwrap(), before_memory);
    }
}

#[test]
fn publish_settings_failure_rolls_back_endpoint_and_retry_commits_only_metadata() {
    let file = std::env::temp_dir().join(format!("gw-publish-{}.db", crate::vault::new_id()));
    {
        let g = edit_fixture(Store::open(&file).unwrap());
        let db = rusqlite::Connection::open(&file).unwrap();
        db.execute_batch("CREATE TRIGGER reject_publish BEFORE INSERT ON settings WHEN NEW.key='local_gateway_v1' BEGIN SELECT RAISE(FAIL,'synthetic publish failure'); END;").unwrap();
        let mut inner = lock(&g.inner);
        let revision = inner.config_revision;
        let mut next = inner.settings.clone(); next.channels[0].base_url = "https://new.example/v1".into();
        next.channels[0].updated_at = 11;
        let old_item = lock(&g.store).vault_get("v").unwrap().unwrap();
        let before = lock(&g.store).get_setting(CONFIG_KEY).unwrap();
        assert!(g.commit_publish(&mut inner, revision, next.clone(), Some(("v", 10)), Some(("https://new.example/v1", 11))).unwrap_err().contains("synthetic publish failure"));
        assert_eq!(serde_json::to_value(lock(&g.store).vault_get("v").unwrap().unwrap()).unwrap(), serde_json::to_value(&old_item).unwrap());
        assert_eq!(lock(&g.store).get_setting(CONFIG_KEY).unwrap(), before);
        assert_eq!(inner.config_revision, revision);
        assert_eq!(inner.settings.channels[0].base_url, "https://old.example/v1");
        db.execute_batch("DROP TRIGGER reject_publish").unwrap();
        g.commit_publish(&mut inner, revision, next, Some(("v", 10)), Some(("https://new.example/v1", 11))).unwrap();
        let mut expected_item = old_item; expected_item.api_endpoint = "https://new.example/v1".into(); expected_item.updated_at = 11;
        assert_eq!(serde_json::to_value(lock(&g.store).vault_get("v").unwrap().unwrap()).unwrap(), serde_json::to_value(expected_item).unwrap());
        assert_eq!(lock(&g.store).get_setting(CONFIG_KEY).unwrap().unwrap(), serde_json::to_string(&inner.settings).unwrap());
        assert!(inner.config_revision > revision);
    }
    std::fs::remove_file(file).unwrap();
}

    #[test]
    fn legacy_groups_keep_system_proxy_and_migrate_sources() {
        let legacy = r#"{"vaultId":"v","source":"gemini","alias":"a","host":"h","models":[],"updatedAt":1}"#;
        let mut group: Published = serde_json::from_str(legacy).unwrap();
        assert!(group.system_proxy);
        assert!(group.normalize_in_place("legacy_0"));
        assert_eq!(group.channel_id, "v");
        assert_eq!(group.source, "gemini_aistudio");
        assert!(group
            .base_url
            .contains("generativelanguage.googleapis.com/v1beta/openai"));

        group.system_proxy = false;
        let restored: Published =
            serde_json::from_str(&serde_json::to_string(&group).unwrap()).unwrap();
        assert!(!restored.system_proxy);
    }

    #[test]
    fn local_no_auth_rejects_browser_origin() {
        let mut s = Settings::default();
        s.auth = false;
        s.lan = false;
        let mut h = HeaderMap::new();
        assert!(!authorized(&h, &s));
        h.insert(header::HOST, "127.0.0.1:18771".parse().unwrap());
        assert!(authorized(&h, &s));
        *h.get_mut(header::HOST).unwrap() = "attacker.example:18771".parse().unwrap();
        assert!(!authorized(&h, &s));
        *h.get_mut(header::HOST).unwrap() = "localhost:18771".parse().unwrap();
        assert!(authorized(&h, &s));
        h.insert(header::ORIGIN, "https://example.com".parse().unwrap());
        assert!(!authorized(&h, &s));
        s.lan = true;
        assert!(!authorized(&HeaderMap::new(), &s));
    }

    #[test]
    fn only_local_or_private_networks_are_allowed() {
        let mut s = Settings::default();
        s.lan = false;
        assert!(peer_allowed("127.0.0.1:1234".parse().unwrap(), &s));
        assert!(peer_allowed("[::1]:1234".parse().unwrap(), &s));
        assert!(peer_allowed("[::ffff:127.0.0.1]:1234".parse().unwrap(), &s));
        assert!(!peer_allowed("192.168.1.10:1234".parse().unwrap(), &s));
        assert!(!peer_allowed("[::ffff:192.168.1.10]:1234".parse().unwrap(), &s));
        s.lan = true;
        assert!(peer_allowed("192.168.1.10:1234".parse().unwrap(), &s));
        assert!(peer_allowed("[::ffff:192.168.1.10]:1234".parse().unwrap(), &s));
        assert!(!peer_allowed("8.8.8.8:1234".parse().unwrap(), &s));
    }

    #[test]
    fn live_channels_survive_vault_timestamp_bump_until_deleted() {
        let file = std::env::temp_dir().join(format!("gw-live-{}.db", crate::vault::new_id()));
        let store = Arc::new(Mutex::new(Store::open(&file).unwrap()));
        lock(&store)
            .vault_upsert(&crate::store::VaultItem {
                id: "v_1".into(),
                platform: "openai".into(),
                label: "k1".into(),
                hint: "sk-***".into(),
                note: String::new(),
                api_endpoint: "https://api.example.com/v1".into(),
                website: String::new(),
                created_at: 10,
                updated_at: 20, // 已更新，比已发布渠道的 updated_at=10 更新
            })
            .unwrap();
        let g = Gateway::new(store.clone()).unwrap();
        lock(&g.inner).settings.channels.push(Published {
            channel_id: "ch_1".into(),
            vault_id: "v_1".into(),
            no_upstream_key: false,
            source: "openai_compat".into(),
            wire: OpenAiWire::default(),
            template: "custom".into(),
            base_url: "https://api.example.com/v1".into(),
            alias: "demo".into(),
            host: "api.example.com".into(),
            models: vec!["gpt-4o".into()],
            manual_models: vec![],
            updated_at: 10,
            allowed_models: None,
            system_proxy: true,
            custom_headers: BTreeMap::new(),
            thinking_mode: ThinkingMode::Auto,
            thought_policy: ThoughtRequestPolicy::FollowClient,
            last_status: "ok".into(),
            last_error: None,
            last_checked_at: Some(10),
        });
        assert_eq!(g.live_channels().len(), 1);
        lock(&store).vault_delete("v_1").unwrap();
        assert!(g.live_channels().is_empty());
        drop(store);
        let _ = std::fs::remove_file(file);
    }


