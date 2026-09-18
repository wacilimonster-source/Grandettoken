//! 应用设置。存 SQLite 的 settings 表,单条 JSON。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 申请制额度渠道的规则(4SAPI 这类:定期申请,把余额补到固定上限)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClaimConfig {
    pub enabled: bool,
    /// 单次额度:申请后余额补到这个数,同时也是剩余百分比的分母
    /// (接口给的累计发放只增不减,拿它当分母会越算越低,没有决策价值)
    pub amount: f64,
    /// 两次申请之间至少间隔多少天(用户口径:超过 2 周可再申请)
    pub min_interval_days: i64,
    /// 手动修正的上次申请时间(unix 秒);None = 完全用自动检测
    pub manual_last_at: Option<i64>,
    /// 手动值写入的时刻。自动检测到比它更晚的一次跳升 = 又申请了一次,自动值接管
    pub manual_set_at: Option<i64>,
}

impl Default for ClaimConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            amount: 200.0,
            min_interval_days: 14,
            manual_last_at: None,
            manual_set_at: None,
        }
    }
}

impl ClaimConfig {
    /// 4SAPI 是典型的申请制渠道,默认开启;其他渠道默认关,需要时在设置里打开
    pub fn for_channel(id: &str) -> Self {
        Self {
            enabled: id == "4sapi",
            ..Default::default()
        }
    }
}

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
    /// 全局快捷键(显示/隐藏面板),Tauri accelerator 格式如 "Alt+KeyG";
    /// 空串 = 不注册。老配置缺字段时兜成空,不会抢占别人的热键。
    pub hotkey: String,
    pub autostart: bool,
    /// 置顶(全局开关)。折叠形态没有置顶按钮,所以置顶状态要能持久化;
    /// 面板的 📌 按钮与设置项共用这一个值,避免两处状态打架。
    pub always_on_top: bool,
    /// panel | compact | pill(贴边不是独立形态,是这三种形态之上的吸附态)
    pub form: String,
    /// percent | balance | dayUsage
    pub sort: String,
    /// 字号档位:md(标准) | lg(大) | xl(特大)。倍率映射在前端(FS_U),
    /// 这里只存档位本身 —— 老配置缺字段时 serde default 兜成 md,行为不变。
    pub font_scale: String,
    /// 胶囊形态固定显示哪些渠道(按顺序轮播);空 = 自动显示最紧张的一个
    pub pill_channels: Vec<String>,
    /// 被隐藏的渠道 id:主面板不显示、**不发请求**(App 型也不再读本机登录凭据),
    /// 快照历史保留,随时可恢复。serde default 保证老配置自动兼容。
    pub hidden_channels: Vec<String>,
    /// 申请制额度规则:渠道 id -> 规则
    pub claim_channels: HashMap<String, ClaimConfig>,
    /// 「自定义排序」下的渠道顺序(渠道 id 数组);未列入的渠道附在末尾
    pub channel_order: Vec<String>,
    /// 拖到屏幕边缘自动吸附。**默认关** —— 挂件随手放在哪里都可能,自动吸附
    /// 会让人措手不及(往右上角一拖就变成一条竖条),所以做成显式开启。
    pub dock_enabled: bool,
    /// 是否自动检查更新。关掉后只能靠管理页里的「检查更新」按钮手动触发。
    pub auto_check_update: bool,
    /// 托盘图标直接画余额概览数(74 / 1.2k / 82,底色=状态色)。
    /// 默认开 —— 扫一眼托盘就是它的主要用途;关掉回到品牌角点样式。
    pub tray_show_number: bool,
    /// 红色告急的状态点做 2s 一次的红晕呼吸 —— 全产品唯一允许的自我循环动画,
    /// 挂件 7×24 常开,循环即噪点,所以**默认关**,由设置里显式打开。
    pub alert_pulse: bool,
    /// 上次检查更新的 unix 秒。自动检查的节流依据(24 小时一次),
    /// 检查失败也写它 —— 不让网络故障变成重试风暴。
    pub last_check_at: Option<i64>,
    /// 用户在更新提示里选了「跳过此版本」的版本号。等于它就不提示。
    pub skipped_version: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            active_interval_sec: 60,
            idle_interval_sec: 300,
            backoff_interval_sec: 900,
            warn_percent: 40.0,
            crit_percent: 15.0,
            hotkey: String::new(),
            autostart: false,
            always_on_top: true,
            form: "panel".into(),
            sort: "percent".into(),
            font_scale: "md".into(),
            pill_channels: Vec::new(),
            hidden_channels: Vec::new(),
            claim_channels: HashMap::from([(
                "4sapi".to_string(),
                ClaimConfig::for_channel("4sapi"),
            )]),
            channel_order: Vec::new(),
            dock_enabled: false,
            auto_check_update: true,
            tray_show_number: true,
            alert_pulse: false,
            last_check_at: None,
            skipped_version: None,
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
    use super::{ClaimConfig, Config};

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
        // 老配置同样没有 fontScale:必须兜成「标准」,不能报错也不能变空串
        assert_eq!(c.font_scale, "md");
        // 老配置没有 hiddenChannels:默认全部显示
        assert!(c.hidden_channels.is_empty());
        // 老配置没有 alertPulse:呼吸动画默认关
        assert!(!c.alert_pulse);
    }

    #[test]
    fn hidden_channels_roundtrip() {
        let mut c = Config::default();
        c.hidden_channels = vec!["trae".into(), "workbuddy".into()];
        let back: Config = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.hidden_channels, vec!["trae".to_string(), "workbuddy".to_string()]);
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

    /// 老配置里没有 claimChannels,加载后必须拿到"4SAPI 默认开启申请制"。
    #[test]
    fn missing_claim_channels_defaults_to_four_s_api_enabled() {
        let old = r#"{"activeIntervalSec":60,"form":"panel","sort":"percent"}"#;
        let c: Config = serde_json::from_str(old).unwrap();
        let s = c.claim_channels.get("4sapi").expect("4sapi 应有默认规则");
        assert!(s.enabled);
        assert_eq!(s.amount, 200.0);
        assert_eq!(s.min_interval_days, 14);
        // 其他渠道默认不开
        assert!(!c.claim_channels.contains_key("hapi"));
    }

    #[test]
    fn claim_channels_roundtrip() {
        let mut c = Config::default();
        c.claim_channels.insert(
            "hapi".into(),
            ClaimConfig {
                enabled: true,
                amount: 100.0,
                min_interval_days: 7,
                manual_last_at: Some(1_700_000_000),
                manual_set_at: Some(1_700_000_100),
            },
        );
        let json = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        let h = back.claim_channels.get("hapi").unwrap();
        assert!(h.enabled);
        assert_eq!(h.amount, 100.0);
        assert_eq!(h.min_interval_days, 7);
        assert_eq!(h.manual_last_at, Some(1_700_000_000));
    }
}
