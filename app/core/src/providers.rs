//! 渠道定义与响应解析。
//!
//! 两类渠道,渲染方式不同:
//!   Amount  金额型 —— 有 remaining/used/total,可算钱、可推"预计可用天数"
//!   Percent 配额型 —— 接口只给已消耗百分比 + 重置时间,无法折算金额
//!
//! 所有取不到的值一律 None,不要用 0 顶替 —— 0 会被界面渲染成"已耗尽",
//! 而请求失败和余额归零是两回事。

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Amount,
    Percent,
}

/// OpenCode Go 的三个限流窗口。任一触顶都会限流,所以三个都要显示。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub label: String,
    /// 已消耗百分比
    pub percent: f64,
    pub remain_percent: f64,
    /// "ok" | "rate-limited"
    pub status: String,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FetchResult {
    pub valid: bool,
    pub kind: Kind,
    pub remaining: Option<f64>,
    pub used: Option<f64>,
    pub total: Option<f64>,
    pub unit: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<Window>,
    /// 界面展开区里展示的键值对,顺序敏感故用 Vec
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub limited: bool,
}

impl Default for Kind {
    fn default() -> Self {
        Kind::Amount
    }
}

fn round2(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

fn round1(n: f64) -> f64 {
    (n * 10.0).round() / 10.0
}

/// f64 是 C 的 double,与 JS 的 Number 精度一致,直接复用原 extractor 的算术。
fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn path<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for k in keys {
        cur = cur.get(k)?;
    }
    Some(cur)
}

/// 4SAPI:积分制,500000 积分 = 1 元。接口标 USD,实际结算人民币。
pub fn extract_4sapi(body: &Value) -> FetchResult {
    const RATE: f64 = 500_000.0;

    let granted = path(body, &["data", "total_granted"]).and_then(num);
    let available = path(body, &["data", "total_available"]).and_then(num);

    let (Some(granted), Some(available)) = (granted, available) else {
        // 兜底:部分部署直接返回余额字段
        let remaining = ["remaining", "balance"]
            .iter()
            .find_map(|k| body.get(*k).and_then(num))
            .or_else(|| path(body, &["quota", "remaining"]).and_then(num));
        return FetchResult {
            valid: remaining.is_some(),
            kind: Kind::Amount,
            remaining,
            unit: "CNY".into(),
            error: if remaining.is_none() {
                Some("无可识别的余额字段".into())
            } else {
                None
            },
            ..Default::default()
        };
    };

    let used = path(body, &["data", "total_used"])
        .and_then(num)
        .unwrap_or(granted - available);

    // 两者任一显式为 false 就是业务失败。之前写成 `||`,而正常响应里没有
    // is_active,右侧恒为 true —— 整个表达式永远为真,`code:false` 被吞掉,
    // 失败会被当成成功渲染出来。
    let code_ok = body.get("code").and_then(|c| c.as_bool()).unwrap_or(true);
    let active_ok = path(body, &["is_active"]).and_then(|c| c.as_bool()).unwrap_or(true);

    FetchResult {
        valid: code_ok && active_ok,
        kind: Kind::Amount,
        remaining: Some(round2(available / RATE)),
        used: Some(round2(used / RATE)),
        total: Some(round2(granted / RATE)),
        unit: "CNY".into(),
        ..Default::default()
    }
}

/// OpenCode Go:未公开接口,只返回百分比。没有稳定保证,失败应静默降级。
pub fn extract_opencode_go(body: &Value) -> FetchResult {
    // 错误形如 {type:"error",error:{type:"AuthError"|"EntitlementError",message}}
    if body.get("type").and_then(|t| t.as_str()) == Some("error") {
        let etype = path(body, &["error", "type"])
            .and_then(|t| t.as_str())
            .unwrap_or("");
        return FetchResult {
            valid: false,
            kind: Kind::Percent,
            error: Some(
                match etype {
                    "EntitlementError" => "未订阅 Go 套餐",
                    "AuthError" => "密钥无效或缺失",
                    _ => "请求失败",
                }
                .into(),
            ),
            ..Default::default()
        };
    }

    let Some(usage) = body.get("usage") else {
        return FetchResult {
            valid: false,
            kind: Kind::Percent,
            error: Some("响应结构异常".into()),
            ..Default::default()
        };
    };

    let win = |key: &str, label: &str| -> Option<Window> {
        let w = usage.get(key)?;
        let pct = w.get("percent").and_then(num).unwrap_or(0.0);
        Some(Window {
            label: label.into(),
            percent: pct,
            remain_percent: round1(100.0 - pct),
            status: w
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("ok")
                .into(),
            resets_at: w
                .get("resetsAt")
                .and_then(|s| s.as_str())
                .map(String::from),
        })
    };

    let windows: Vec<Window> = [
        win("rolling", "5 小时"),
        win("weekly", "本周"),
        win("monthly", "本月"),
    ]
    .into_iter()
    .flatten()
    .collect();

    if windows.is_empty() {
        return FetchResult {
            valid: false,
            kind: Kind::Percent,
            error: Some("无可用配额窗口".into()),
            ..Default::default()
        };
    }

    // 卡片主数值取三窗中最紧张的一个
    let tightest = windows
        .iter()
        .max_by(|a, b| a.percent.partial_cmp(&b.percent).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();

    FetchResult {
        valid: true,
        kind: Kind::Percent,
        remaining: Some(tightest.remain_percent),
        used: Some(tightest.percent),
        total: Some(100.0),
        unit: "%".into(),
        limited: windows.iter().any(|w| w.status == "rate-limited"),
        windows,
        ..Default::default()
    }
}

/// DeepSeek 官方:充值型,接口不给累计消耗,全靠本地快照差值推算。
pub fn extract_deepseek(body: &Value) -> FetchResult {
    let Some(info) = body
        .get("balance_infos")
        .and_then(|b| b.as_array())
        .and_then(|a| a.first())
    else {
        return FetchResult {
            valid: false,
            kind: Kind::Amount,
            error: Some("无余额信息".into()),
            ..Default::default()
        };
    };

    let total_balance = info.get("total_balance").and_then(num);
    // 缺字段就是"未知",不能拿 0.00 顶替 —— 界面会把"没这个数据"读成"余额是 0"
    let mut extra = Vec::new();
    if let Some(g) = info.get("granted_balance").and_then(num) {
        extra.push(("赠送余额".to_string(), format!("¥{:.2}", g)));
    }
    if let Some(t) = info.get("topped_up_balance").and_then(num) {
        extra.push(("充值余额".to_string(), format!("¥{:.2}", t)));
    }

    FetchResult {
        valid: body
            .get("is_available")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        kind: Kind::Amount,
        remaining: total_balance.map(round2),
        used: None,
        total: None, // 充值型无"限额"概念,不画进度条
        unit: "CNY".into(),
        extra,
        ..Default::default()
    }
}

/// Hapi:积分制,20 积分 = 1 元。
pub fn extract_hapi(body: &Value) -> FetchResult {
    let d = body.get("data").filter(|v| v.is_object()).unwrap_or(body);

    if d.get("error").map(|e| !e.is_null()).unwrap_or(false) {
        return FetchResult {
            valid: false,
            kind: Kind::Amount,
            error: Some(
                d.get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("接口返回错误")
                    .into(),
            ),
            ..Default::default()
        };
    }

    let Some(balance) = d.get("balance").and_then(num) else {
        return FetchResult {
            valid: false,
            kind: Kind::Amount,
            error: Some("无 balance 字段".into()),
            ..Default::default()
        };
    };

    FetchResult {
        valid: d.get("isValid").and_then(|v| v.as_bool()).unwrap_or(true),
        kind: Kind::Amount,
        remaining: Some(round2(balance / 20.0)),
        used: None,
        total: None,
        unit: "CNY".into(),
        extra: vec![
            ("原始积分".into(), format!("{:.2}", balance)),
            ("换算".into(), "20 积分 = ¥1".into()),
            ("消耗来源".into(), "本地流水推算".into()),
        ],
        ..Default::default()
    }
}

/// 渠道静态定义。加新渠道只需在这里追加一条 + 一个 extractor。
pub struct ProviderDef {
    pub id: &'static str,
    pub name: &'static str,
    pub short: &'static str,
    pub color: &'static str,
    pub url: &'static str,
    /// 同渠道的备用入口。主地址失败且失败类型"值得换域名"时按顺序重试。
    /// 4SAPI 有四个域名,某个挂了不至于让这个渠道瞎掉。
    pub fallback_urls: &'static [&'static str],
    /// 额外请求头,除 Authorization 外
    pub extra_headers: &'static [(&'static str, &'static str)],
    /// 未公开接口,失败要静默降级而非报警
    pub unstable: bool,
    pub extract: fn(&Value) -> FetchResult,
}

pub const PROVIDERS: &[ProviderDef] = &[
    ProviderDef {
        id: "4sapi",
        name: "4SAPI",
        short: "4S",
        color: "#e8622c",
        url: "https://4sapi.com/api/usage/token",
        fallback_urls: &[
            "https://4sapi.cn/api/usage/token",
            "https://4sapi.ai/api/usage/token",
            "https://4sapi.org/api/usage/token",
        ],
        extra_headers: &[],
        unstable: false,
        extract: extract_4sapi,
    },
    ProviderDef {
        id: "opencode-go",
        name: "OpenCode Go",
        short: "OC",
        color: "#f5a623",
        url: "https://opencode.ai/zen/go/v1/usage",
        fallback_urls: &[],
        extra_headers: &[], // 只认 Bearer,x-api-key 无效
        unstable: true,
        extract: extract_opencode_go,
    },
    ProviderDef {
        id: "deepseek",
        name: "DeepSeek",
        short: "DS",
        color: "#4d6bfe",
        url: "https://api.deepseek.com/user/balance",
        fallback_urls: &[],
        extra_headers: &[],
        unstable: false,
        extract: extract_deepseek,
    },
    ProviderDef {
        id: "hapi",
        name: "Hapi",
        short: "HA",
        color: "#22a06b",
        url: "https://ai.yuchuantest.com/v1/usage",
        fallback_urls: &[],
        extra_headers: &[("User-Agent", "cc-switch/1.0")],
        unstable: false,
        extract: extract_hapi,
    },
];

pub fn find(id: &str) -> Option<&'static ProviderDef> {
    PROVIDERS.iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn four_s_api_converts_points() {
        let r = extract_4sapi(&json!({
            "code": true,
            "data": {
                "total_granted": "250000000",
                "total_available": "93500000",
                "total_used": "156500000"
            }
        }));
        assert!(r.valid);
        assert_eq!(r.remaining, Some(187.0));
        assert_eq!(r.used, Some(313.0));
        assert_eq!(r.total, Some(500.0));
    }

    #[test]
    fn four_s_api_falls_back_when_fields_missing() {
        // 缺字段时必须报错,不能默默返回 0 —— 那会被渲染成"已耗尽"
        let r = extract_4sapi(&json!({ "code": true, "data": {} }));
        assert!(!r.valid);
        assert!(r.remaining.is_none());
        assert!(r.error.is_some());
    }

    /// 业务失败(HTTP 200 但 code:false)必须判为无效 —— 之前 `||` 写法会把它吞掉。
    #[test]
    fn four_s_api_business_failure_is_invalid() {
        let r = extract_4sapi(&json!({
            "code": false,
            "data": { "total_granted": 1000, "total_available": 200, "total_used": 800 }
        }));
        assert!(!r.valid, "code:false 必须判为无效");
        // is_active:false 同理(老的部署用它表示停用)
        let r = extract_4sapi(&json!({
            "is_active": false,
            "data": { "total_granted": 1000, "total_available": 200, "total_used": 800 }
        }));
        assert!(!r.valid, "is_active:false 必须判为无效");
        // 正常响应(两个字段都没有)仍然是有效的
        let r = extract_4sapi(&json!({
            "data": { "total_granted": 1000, "total_available": 200, "total_used": 800 }
        }));
        assert!(r.valid);
        assert_eq!(r.remaining, Some(0.0)); // 200/500000 四舍五入到分
    }

    #[test]
    fn opencode_picks_tightest_window() {
        let r = extract_opencode_go(&json!({
            "usage": {
                "rolling": {"status": "ok", "percent": 12.4, "resetsAt": "a"},
                "weekly":  {"status": "ok", "percent": 33.1, "resetsAt": "b"},
                "monthly": {"status": "rate-limited", "percent": 98.7, "resetsAt": "c"}
            }
        }));
        assert!(r.valid);
        assert_eq!(r.remaining, Some(1.3)); // 最紧张的月度窗口
        assert!(r.limited);
        assert_eq!(r.windows.len(), 3);
    }

    #[test]
    fn opencode_reports_entitlement_error() {
        let r = extract_opencode_go(&json!({
            "type": "error",
            "error": {"type": "EntitlementError", "message": "OpenCode Go subscription required."}
        }));
        assert!(!r.valid);
        assert_eq!(r.error.as_deref(), Some("未订阅 Go 套餐"));
    }

    #[test]
    fn deepseek_splits_balance() {
        let r = extract_deepseek(&json!({
            "is_available": true,
            "balance_infos": [{
                "currency": "CNY",
                "total_balance": "256.00",
                "granted_balance": "0.00",
                "topped_up_balance": "256.00"
            }]
        }));
        assert_eq!(r.remaining, Some(256.0));
        assert!(r.total.is_none()); // 充值型不画进度条
    }

    #[test]
    fn hapi_divides_points_by_20() {
        let r = extract_hapi(&json!({ "data": { "balance": 1974.5, "isValid": true } }));
        assert_eq!(r.remaining, Some(98.73));
    }

    #[test]
    fn hapi_error_is_surfaced() {
        let r = extract_hapi(&json!({ "error": "invalid key" }));
        assert!(!r.valid);
        assert!(r.error.is_some());
    }
}
