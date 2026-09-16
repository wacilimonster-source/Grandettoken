//! 应用设置。存 SQLite 的 settings 表,单条 JSON。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// 面板展开时的轮询间隔。查询余额不消耗 token,这四个接口都是账务接口,
    /// 所以可以放心高频轮询,唯一约束是对方的限流礼貌性。
    pub active_interval_sec: u64,
    /// 折叠 / 托盘态
    pub idle_interval_sec: u64,
    /// 连续失败后的退避间隔
    pub backoff_interval_sec: u64,
    /// 剩余百分比低于此值标黄
    pub warn_percent: f64,
    /// 低于此值标红
    pub crit_percent: f64,
    pub notify: bool,
    pub autostart: bool,
    pub collapse_on_blur: bool,
    /// panel | compact | pill | edge
    pub form: String,
    /// percent | balance | dayUsage
    pub sort: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            active_interval_sec: 60,
            idle_interval_sec: 300,
            backoff_interval_sec: 900,
            warn_percent: 40.0,
            crit_percent: 15.0,
            notify: true,
            autostart: false,
            collapse_on_blur: false,
            form: "panel".into(),
            sort: "percent".into(),
        }
    }
}

impl Config {
    pub fn load(store: &crate::store::Store) -> Self {
        store
            .get_setting("config")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, store: &crate::store::Store) -> Result<(), String> {
        let json = serde_json::to_string(self).map_err(|e| e.to_string())?;
        store.set_setting("config", &json)
    }
}
