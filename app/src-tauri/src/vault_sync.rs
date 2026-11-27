use std::sync::{Arc, Mutex};
use tauri::{Manager, Emitter};
use tokenscope_core::{config::Config, fetch::now_ts, models, secrets, store::Store};

use crate::{gateway, lock_rw, source_base, AppState};

pub fn vault_defaults_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub use tokenscope_core::vault::{ensure_vault_mutable, vault_default_channel};

/// 复用旧版自动导入的默认条目；其他用户创建的条目不会被标记为只读。
/// 返回本次发生密钥或调用端点变化的 `vault_id` 列表，供网关自动同步。
pub fn import_channel_keys(app: &tauri::AppHandle, store: &Arc<Mutex<Store>>, cfg: &Config) -> tokenscope_core::vault::transaction::ImportResult {
    let _guard = lock_rw(vault_defaults_lock());
    tokenscope_core::vault::transaction::import_batch([
        ("4sapi", "4SAPI", "https://4sapi.com"),
        ("hapi", "Hapi", "https://ai.yuchuantest.com"),
        ("opencode-go", "OpenCode Go", "https://opencode.ai"),
    ], |(id, platform, website)| import_one(app, store, cfg, id, platform, website))
}

fn import_one(app: &tauri::AppHandle, store: &Arc<Mutex<Store>>, cfg: &Config, id: &str, platform: &str, website: &str) -> Result<Option<String>, String> {
        let mapping = format!("vault_default_channel:{id}");
        let mut existing_id = lock_rw(store).get_setting(&mapping)?;
        let items = lock_rw(store).vault_list()?;
        let legacy_marker = lock_rw(store)
            .get_setting(&format!("vault_channel_import_v1:{id}"))?
            .is_some();
        let legacy: Vec<_> = items
            .iter()
            .filter(|v| {
                legacy_marker && v.platform.eq_ignore_ascii_case(platform) && v.label == "渠道当前密钥"
            })
            .collect();
        if existing_id.is_none() && legacy.len() == 1 {
            lock_rw(store).set_setting(&mapping, &legacy[0].id)?;
            existing_id = Some(legacy[0].id.clone());
        }
        let existing = existing_id
            .as_ref()
            .and_then(|vid| items.iter().find(|v| &v.id == vid))
            .cloned();
        let key = match secrets::channel_key(id)? {
            Some(key) if !key.trim().is_empty() => key,
            _ => {
                // 渠道密钥被移除后，不保留仍可调用的旧副本；只读记录继续显示“未配置”。
                if let Some(mut item) = existing {
                    if secrets::vault_get(&item.id)?.is_some() {
                        item.hint.clear();
                        item.updated_at = now_ts().max(item.updated_at.saturating_add(1));
                        let committed = with_invalidation(app, &item.id, || tokenscope_core::vault::transaction::commit(&tokenscope_core::vault::transaction::NativeVault, &item.id, None, || lock_rw(store).vault_upsert(&item)));
                        committed?;
                        return Ok(Some(item.id));
                    }
                }
                return Ok(None);
            }
        };
        let mut item = if let Some(v) = existing {
            v
        } else if legacy.len() == 1 {
            legacy[0].clone()
        } else {
            let now = now_ts();
            tokenscope_core::store::VaultItem {
                id: tokenscope_core::vault::new_id(),
                platform: platform.into(),
                label: "渠道当前密钥".into(),
                hint: String::new(),
                note: String::new(),
                api_endpoint: String::new(),
                website: website.into(),
                created_at: now,
                updated_at: now,
            }
        };
        let is_new = !items.iter().any(|v| v.id == item.id);
        let old_key = secrets::vault_get(&item.id)?;
        let def = models::find_source(id).ok_or("渠道配置缺失")?;
        let cache = {
            let s = lock_rw(store);
            models::ModelCache::load(&s)
        };
        let endpoint = source_base(cfg, cache.as_ref(), def);
        let changed = old_key.as_deref() != Some(key.as_str()) || item.api_endpoint != endpoint
            || item.hint != secrets::mask_of(&key);
        if changed || is_new {
            item.hint = secrets::mask_of(&key);
            item.api_endpoint = endpoint;
            if !is_new {
                item.updated_at = now_ts().max(item.updated_at.saturating_add(1));
            }
            let committed = with_invalidation(app, &item.id, || tokenscope_core::vault::transaction::commit(&tokenscope_core::vault::transaction::NativeVault, &item.id, Some(&key), || lock_rw(store).vault_upsert_default(&item, &mapping)));
            committed?;
            return Ok(Some(item.id));
        } else {
            lock_rw(store).set_setting(&mapping, &item.id)?;
        }
        Ok(None)
}

fn with_invalidation<T>(app: &tauri::AppHandle, id: &str, operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let g = app.try_state::<gateway::Gateway>();
    if let Some(g) = g.as_ref() { g.invalidate_vault(id); }
    let result = operation();
    if let Some(g) = g.as_ref() { g.invalidate_vault(id); }
    result
}

pub fn import_for_gateway(app: &tauri::AppHandle, store: &Arc<Mutex<Store>>, cfg: &Config) -> Vec<String> {
    let gw = app.try_state::<gateway::Gateway>().map(|g| g.inner().clone());
    let revision = gw.as_ref().map(|g| g.begin_sync("import"));
    let imported = import_channel_keys(app, store, cfg);
    for id in &imported.changed_ids { gateway::spawn_vault_sync(app, id.clone()); }
    if let (Some(g), Some(revision)) = (gw, revision) {
        let result = imported.error.map_or(Ok(()), |error| Err(format!("默认密钥部分导入失败：{error}")));
        let state = g.finish_sync(revision, result);
        let _ = app.emit("gateway-sync-status", state);
    } else if let Some(error) = imported.error {
        gateway::report_sync_error(app, error);
    }
    imported.changed_ids
}

pub fn sync_channel_to_gateway(app: &tauri::AppHandle, channel_id: &str) {
    if !matches!(channel_id, "4sapi" | "hapi" | "opencode-go") {
        return;
    }
    let h = app.clone();
    let store = app.state::<AppState>().store.clone();
    let cfg = lock_rw(&app.state::<AppState>().config).clone();
    tauri::async_runtime::spawn(async move {
        let worker = h.clone();
        if tokio::task::spawn_blocking(move || import_for_gateway(&worker, &store, &cfg)).await.is_err() {
            gateway::report_sync_error(&h, "渠道密钥已保存，但网关导入任务失败".into());
        }
    });
}
