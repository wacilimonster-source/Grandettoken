//! 美元 → 人民币牌价。
//!
//! 存在的唯一理由:套餐型渠道(OpenCode Go 是 $10/期的包月)接口只回百分比用量、
//! 不回金额,要进「今日 / 本周 / 本月消耗」的合计就必须先折成人民币。
//!
//! 三条口径(2026-10-14 产品裁决):
//! 1. **每月取一次最新的** —— 当月已有新数就不再打网络;跨月后的第一轮重取。
//! 2. 取不到就**沿用上月的数**,并在界面上写明它是几月的(宁可旧,不要没有)。
//! 3. 一次都没取到过就**不参与合计**(宁可少加,不能拿猜的数当"花掉的钱")。
//!
//! 数值合理性:美元兑人民币几十年没出过 4~12 之外,超出即视为接口坏了或被劫持,
//! 直接丢弃这个源 —— 与「单位可疑就不显示」同一条纪律(报告 P1-1)。

use crate::store::Store;
use serde::{Deserialize, Serialize};

/// 缓存键(存在 settings 表里,与取数缓存同一去处)。
pub const SETTINGS_KEY: &str = "fx_usd_cny";

/// 依次尝试的公开牌价接口。都不需要密钥,返回里都只带一个 CNY 数。
/// 第二个源的地址实测过:老的 `api.frankfurter.app` 现在 301 到 `.dev/v1`,
/// 参数名也从 `from/to` 换成了 `base/symbols`(写错的版本会一直拿到 HTML)。
const SOURCES: &[(&str, &str)] = &[
    ("https://open.er-api.com/v6/latest/USD", "er-api"),
    (
        "https://api.frankfurter.dev/v1/latest?base=USD&symbols=CNY",
        "frankfurter",
    ),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FxRate {
    /// 1 美元 = rate 人民币
    pub rate: f64,
    /// 这份牌价属于哪个北京月("2026-10"):既用于"每月取一次",也用于界面说明
    pub month: String,
    /// 来自哪个接口,便于排查"这个数哪来的"
    pub src: String,
}

/// 北京月标签。刻意用 UTC+8 而不是机器时区:"牌价是哪个月公布的"不该随用户
/// 机器时区变;与 [`crate::checkin::beijing_day`] 同一把尺子。
pub fn month_tag(now_ts: i64) -> String {
    crate::checkin::beijing_day(now_ts)
        .chars()
        .take(7)
        .collect()
}

/// 区间外的数一律不采信(接口坏 / 被劫持 / 单位搞错)。
pub fn sane(r: f64) -> bool {
    r > 4.0 && r < 12.0
}

/// 两个接口的返回形状都是 `{"rates": {"CNY": 7.x}}`。
pub fn parse_rate(v: &serde_json::Value) -> Option<f64> {
    let r = v.get("rates")?.get("CNY")?.as_f64()?;
    sane(r).then_some(r)
}

/// 读缓存。库里存的是坏 JSON 时按"没有缓存"处理(不阻断取数)。
pub fn load(store: &Store) -> Option<FxRate> {
    store
        .get_setting(SETTINGS_KEY)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub fn save(store: &Store, f: &FxRate) -> Result<(), String> {
    let txt = serde_json::to_string(f).map_err(|e| format!("序列化牌价失败: {e}"))?;
    store.set_setting(SETTINGS_KEY, &txt)
}

/// 本轮该用哪个牌价。`cached` 由调用方在**锁库段**读出来传进来(这个函数会 await,
/// 不能持着 `std::sync::MutexGuard` 跨 await)。
///
/// 当月已有数 → 直接返回,零网络。跨月后逐个试接口;全失败则回退到上月的旧数。
pub async fn refresh(http: &reqwest::Client, cached: Option<FxRate>, now_ts: i64) -> Option<FxRate> {
    let want = month_tag(now_ts);
    if let Some(c) = &cached {
        if c.month == want && sane(c.rate) {
            return Some(c.clone());
        }
    }
    for (url, src) in SOURCES {
        match http
            .get(*url)
            .timeout(std::time::Duration::from_secs(6))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                match resp.json::<serde_json::Value>().await {
                    Ok(body) => {
                        if let Some(rate) = parse_rate(&body) {
                            let fresh = FxRate {
                                rate,
                                month: want.clone(),
                                src: (*src).to_string(),
                            };
                            return Some(fresh);
                        }
                        eprintln!("汇率接口 {src} 返回不可用的数(HTTP {status}),换下一个源");
                    }
                    Err(e) => eprintln!("汇率接口 {src} 解析失败: {e}"),
                }
            }
            Err(e) => eprintln!("汇率接口 {src} 请求失败: {e}"),
        }
    }
    // 一个源都没通:沿用旧数(哪怕是一个月前的),界面会写明它是几月的
    cached
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_tag_is_beijing_month_not_local() {
        // 2026-10-01 07:30 UTC = 北京 10-01 15:30 → 2026-10;
        // 同一时刻在 UTC-8 还是 09-30 23:30 —— 用机器时区就会把"哪个月的牌价"算错。
        assert_eq!(month_tag(1_790_839_800), "2026-10");
        // 月初边界:北京 10-01 00:30(= UTC 09-30 16:30)已算 10 月,
        // 再早一小时的北京 09-30 23:30 还算 9 月
        assert_eq!(month_tag(1_790_785_800), "2026-10");
        assert_eq!(month_tag(1_790_782_200), "2026-09");
    }

    #[test]
    fn parses_both_response_shapes() {
        let er: serde_json::Value =
            serde_json::from_str(r#"{"result":"ok","rates":{"CNY":7.18,"JPY":150.0}}"#).unwrap();
        assert_eq!(parse_rate(&er), Some(7.18));
        let fk: serde_json::Value =
            serde_json::from_str(r#"{"base":"USD","rates":{"CNY":7.23}}"#).unwrap();
        assert_eq!(parse_rate(&fk), Some(7.23));
    }

    #[test]
    fn refuses_silently_when_number_is_impossible_or_missing() {
        // 接口把单位写成"分",或干脆没有 CNY 这一档 —— 都不能当成 7.x 用
        for bad in [
            r#"{"rates":{"CNY":718.0}}"#,
            r#"{"rates":{"CNY":0.72}}"#,
            r#"{"rates":{"EUR":0.9}}"#,
            r#"{"error":"quota exceeded"}"#,
            r#"{"rates":"not-an-object"}"#,
        ] {
            let v: serde_json::Value = serde_json::from_str(bad).unwrap();
            assert_eq!(parse_rate(&v), None, "不该采信: {bad}");
        }
    }

    #[test]
    fn roundtrips_through_settings_json() {
        let f = FxRate {
            rate: 7.18,
            month: "2026-10".into(),
            src: "er-api".into(),
        };
        let txt = serde_json::to_string(&f).unwrap();
        let back: FxRate = serde_json::from_str(&txt).unwrap();
        assert_eq!(back, f);
        // 库里躺着坏 JSON(手工改过/版本不同)时按"没有缓存"处理,不能 panic
        assert!(serde_json::from_str::<FxRate>("not json").is_err());
    }
}
