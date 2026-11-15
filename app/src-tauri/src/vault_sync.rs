use std::sync::{Arc, Mutex};
use tauri::Manager;
use tokenscope_core::{config::Config, fetch::now_ts, models, secrets, store::Store};

use crate::{gateway, lock_rw, source_base, AppState};

pub fn vault_defaults_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub fn vault_default_channel(store: &Store, vault_id: &str) -> Result<Option<String>, String> {
    for id in ["4sapi", "hapi", "opencode-go"] {
        if store.get_setting(&format!("vault_default_channel:{id}"))?.as_deref() == Some(vault_id) {
            return Ok(Some(id.to_string()));
        }
    }
    Ok(None)
}

pub fn ensure_vault_mutable(store: &Store, vault_id: &str) -> Result<(), String> {
    if vault_default_channel(store, vault_id)?.is_some() {
        return Err("渠道默认密钥仅供查看，请在渠道管理中修改".into());
    }
    Ok(())
}

/// 复用旧版自动导入的默认条目；其他用户创建的条目不会被标记为只读。
/// 返回本次发生密钥或调用端点变化的 `vault_id` 列表，供网关自动同步。
pub fn import_channel_keys(store: &Arc<Mutex<Store>>, cfg: &Config) -> Result<Vec<String>, String> {
    let _guard = lock_rw(vault_defaults_lock());
    let mut changed_vids = Vec::new();
    for (id, platform, website) in [
        ("4sapi", "4SAPI", "https://4sapi.com"),
        ("hapi", "Hapi", "https://ai.yuchuantest.com"),
        ("opencode-go", "OpenCode Go", "https://opencode.ai"),
    ] {
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
                        secrets::vault_delete(&item.id)?;
                        item.hint.clear();
                        item.updated_at = now_ts().max(item.updated_at.saturating_add(1));
                        lock_rw(store).vault_upsert(&item)?;
                        changed_vids.push(item.id.clone());
                    }
                }
                continue;
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
        let changed = old_key.as_deref() != Some(key.as_str()) || item.api_endpoint != endpoint;
        if changed || is_new {
            if old_key.as_deref() != Some(key.as_str()) {
                secrets::vault_set(&item.id, &key)?;
            }
            item.hint = secrets::mask_of(&key);
            item.api_endpoint = endpoint;
            if !is_new {
                item.updated_at = now_ts().max(item.updated_at.saturating_add(1));
            }
            if let Err(e) = lock_rw(store).vault_upsert(&item) {
                if is_new {
                    let _ = secrets::vault_delete(&item.id);
                }
                return Err(e);
            }
            changed_vids.push(item.id.clone());
        }
        lock_rw(store).set_setting(&mapping, &item.id)?;
    }
    Ok(changed_vids)
}

pub fn sync_channel_to_gateway(app: &tauri::AppHandle, channel_id: &str) {
    if !matches!(channel_id, "4sapi" | "hapi" | "opencode-go") {
        return;
    }
    let h = app.clone();
    let store = app.state::<AppState>().store.clone();
    let cfg = lock_rw(&app.state::<AppState>().config).clone();
    tauri::async_runtime::spawn(async move {
        if let Ok(Ok(vids)) =
            tokio::task::spawn_blocking(move || import_channel_keys(&store, &cfg)).await
        {
            for vid in vids {
                gateway::spawn_vault_sync(&h, vid);
            }
        }
    });
}
