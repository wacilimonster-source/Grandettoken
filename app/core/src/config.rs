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
    /// 胶囊形态固定显示哪些渠道(按顺序轮播);空 = 自动显示最紧张的一个
    pub pill_channels: Vec<String>,
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
            pill_channels: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::Config;

    /// 老库里的 settings JSON 没有 pillChannels 字段,必须能按默认值加载
    /// (容器上的 serde default 保证前向兼容),否则升级后配置整体读不出来。
    #[test]
    fn old_config_without_pill_channels_still_loads() {
        let old = r#"{"activeIntervalSec":60,"idleIntervalSec":300,"backoffIntervalSec":900,
            "warnPercent":40.0,"critPercent":15.0,"notify":true,"autostart":false,
            "collapseOnBlur":false,"form":"compact","sort":"percent"}"#;
        let c: Config = serde_json::from_str(old).unwrap();
        assert!(c.pill_channels.is_empty());
        assert_eq!(c.form, "compact");
    }

    #[test]
    fn pill_channels_roundtrip() {
        let c = Config {
            pill_channels: vec!["hapi".into(), "deepseek".into()],
            ..Config::default()
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pill_channels, vec!["hapi".to_string(), "deepseek".to_string()]);
    }
}
