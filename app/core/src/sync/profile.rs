//! 独立保存同步设置,避免其他功能用旧的整份 Config 覆盖同步组。
//!
//! 这里只存组标识和发布设置;主密钥仍只在系统凭据库中。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{config::Config, store::Store};

pub const PROFILE_KEY: &str = "sync:profile:v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncProfile {
    version: u8,
    enabled: bool,
    relay_kind: String,
    relay_id: Option<String>,
    source: Option<String>,
    key_epoch: u8,
    publish_interval_sec: u64,
    channels: Vec<String>,
    platform_labels: HashMap<String, String>,
    last_upload_at: Option<i64>,
    last_error: Option<String>,
    last_error_at: Option<i64>,
    next_allowed_at: Option<i64>,
    oauth_client_id: Option<String>,
    github_login: Option<String>,
    github_user_id: Option<i64>,
}

impl SyncProfile {
    pub fn from_config(c: &Config) -> Self {
        Self {
            version: 1,
            enabled: c.sync_enabled,
            relay_kind: c.sync_relay_kind.clone(),
            relay_id: c.sync_relay_id.clone(),
            source: c.sync_source.clone(),
            key_epoch: c.sync_key_epoch,
            publish_interval_sec: c.sync_publish_interval_sec,
            channels: c.sync_channels.clone(),
            platform_labels: c.sync_platform_labels.clone(),
            last_upload_at: c.sync_last_upload_at,
            last_error: c.sync_last_error.clone(),
            last_error_at: c.sync_last_error_at,
            next_allowed_at: c.sync_next_allowed_at,
            oauth_client_id: c.sync_oauth_client_id.clone(),
            github_login: c.sync_github_login.clone(),
            github_user_id: c.sync_github_user_id,
        }
    }

    pub fn apply_to(&self, c: &mut Config) {
        c.sync_enabled = self.enabled;
        c.sync_relay_kind.clone_from(&self.relay_kind);
        c.sync_relay_id.clone_from(&self.relay_id);
        c.sync_source.clone_from(&self.source);
        c.sync_key_epoch = self.key_epoch;
        c.sync_publish_interval_sec = self.publish_interval_sec;
        c.sync_channels.clone_from(&self.channels);
        c.sync_platform_labels.clone_from(&self.platform_labels);
        c.sync_last_upload_at = self.last_upload_at;
        c.sync_last_error.clone_from(&self.last_error);
        c.sync_last_error_at = self.last_error_at;
        c.sync_next_allowed_at = self.next_allowed_at;
        c.sync_oauth_client_id.clone_from(&self.oauth_client_id);
        c.sync_github_login.clone_from(&self.github_login);
        c.sync_github_user_id = self.github_user_id;
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.relay_kind != "gist" {
            return Err("同步配置版本或中继类型不受支持".into());
        }
        if self.relay_id.as_deref().is_some_and(|id| !super::code::valid_gist_id(id)) {
            return Err("同步配置中的中继标识无效".into());
        }
        if self.source.as_deref().is_some_and(|s| super::crypto::normalize_source(s).is_none()) {
            return Err("同步配置中的发布来源无效".into());
        }
        if !(30..=3600).contains(&self.publish_interval_sec) {
            return Err("同步配置中的发布间隔无效".into());
        }
        Ok(())
    }
}

/// 升级时只迁移一次,绝不以旧 Config 覆盖已经存在的独立同步配置。
pub fn migrate(store: &Store, legacy: &Config) -> Result<(), String> {
    if store.get_setting(PROFILE_KEY)?.is_none() {
        save(store, &SyncProfile::from_config(legacy))?;
    }
    load(store).map(|_| ())
}

pub fn load(store: &Store) -> Result<SyncProfile, String> {
    let raw = store.get_setting(PROFILE_KEY)?
        .ok_or("同步配置尚未初始化,请重启应用后重试")?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|_| "同步配置无法读取,已保留原始数据,请重试或恢复设置")?;
    // Option 字段缺失时 serde 默认按 None 处理;同步组 ID 缺失不能误判成未建组。
    let required = serde_json::to_value(SyncProfile::from_config(&Config::default()))
        .map_err(|_| "同步配置校验失败")?;
    let present = value.as_object().ok_or("同步配置格式不正确,已保留原始数据")?;
    if required.as_object().unwrap().keys().any(|key| !present.contains_key(key)) {
        return Err("同步配置不完整,已保留原始数据,请恢复设置".into());
    }
    let profile: SyncProfile = serde_json::from_value(value)
        .map_err(|_| "同步配置无法读取,已保留原始数据,请重试或恢复设置")?;
    profile.validate()?;
    Ok(profile)
}

pub fn save(store: &Store, profile: &SyncProfile) -> Result<(), String> {
    profile.validate()?;
    let json = serde_json::to_string(profile).map_err(|_| "同步配置序列化失败")?;
    store.set_setting(PROFILE_KEY, &json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_preserves_existing_group_and_ignores_stale_general_settings() {
        let store = Store::open_memory().unwrap();
        let mut existing = Config::default();
        existing.sync_enabled = true;
        existing.sync_relay_id = Some("00112233445566778899aabbccddeeff".into());
        existing.sync_source = Some("s_00112233445566778899aabbccddeeff".into());
        existing.sync_channels = vec!["one".into()];
        existing.sync_key_epoch = 7;
        migrate(&store, &existing).unwrap();

        // 普通设置的旧快照无同步组：再保存它也不能改写独立同步配置。
        let mut stale = Config::default();
        stale.font_scale = "xl".into();
        stale.save(&store).unwrap();
        migrate(&store, &stale).unwrap();
        let mut reloaded = Config::try_load(&store).unwrap();
        load(&store).unwrap().apply_to(&mut reloaded);
        assert_eq!(reloaded.font_scale, "xl");
        assert_eq!(SyncProfile::from_config(&reloaded), SyncProfile::from_config(&existing));
        let relay = super::super::code::parse_gist_id_hex(reloaded.sync_relay_id.as_deref().unwrap()).unwrap();
        let mut before = super::super::code::SyncCode::new(relay, [42; 32]);
        before.key_epoch = existing.sync_key_epoch;
        let mut after = super::super::code::SyncCode::new(relay, [42; 32]);
        after.key_epoch = reloaded.sync_key_epoch;
        assert_eq!(before.encode(), after.encode(), "迁移及普通设置保存不能改变同步码");
    }

    #[test]
    fn corrupted_profile_is_not_silently_replaced() {
        let store = Store::open_memory().unwrap();
        store.set_setting(PROFILE_KEY, "{broken").unwrap();
        assert!(migrate(&store, &Config::default()).is_err());
        assert_eq!(store.get_setting(PROFILE_KEY).unwrap().as_deref(), Some("{broken"));
        let mut incomplete = serde_json::to_value(SyncProfile::from_config(&Config::default())).unwrap();
        incomplete.as_object_mut().unwrap().remove("relayId");
        store.set_setting(PROFILE_KEY, &incomplete.to_string()).unwrap();
        assert!(load(&store).is_err());
    }
}
