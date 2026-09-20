//! 渠道定义与响应解析。
//!
//! 三类渠道,渲染方式不同:
//!   Amount  金额型 —— 有 remaining/used/total,可算钱、可推"预计可用天数"
//!   Percent 配额型 —— 接口只给已消耗百分比 + 重置时间,无法折算金额
//!   Points  积分型 —— 有剩余量,但每一笔都有各自的到期日,量最大的风险是过期
//!
//! 所有取不到的值一律 None,不要用 0 顶替 —— 0 会被界面渲染成"已耗尽",
//! 而请求失败和余额归零是两回事。

use crate::appauth::App;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Amount,
    Percent,
    Points,
}

/// 一笔会在某时刻过期的积分。积分型渠道的核心信息就是这个列表 ——
/// "还剩多少"只是总数,"哪一笔什么时候过期"才决定该先花哪笔。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Expiring {
    /// 到期时刻(unix 秒)
    pub at: i64,
    /// 到期时还没用掉的量(过期损失 = 这一笔的剩余量)
    pub amount: f64,
    /// 这一笔的来源(签到奖励 / 活动赠送 / 每日积分…)
    pub label: String,
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
    /// 逐笔到期(升序)。只有积分型渠道才有。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub expiring: Vec<Expiring>,
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
        // 实测 resetsAt 锚定开通日(如 10-13)而非日历月,叫「周期」才准确
        win("monthly", "周期"),
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
    /// 凭据来源。Keyring = 用户填的 API Key;App = 复用本机已登录应用的凭据
    pub auth: AuthKind,
    /// Authorization 头的前缀(Trae 不是 Bearer)
    pub auth_prefix: &'static str,
    /// 请求方式。积分平台都是 POST,body 里带动态时间窗,所以用函数生成
    pub method: Method,
    /// 未公开接口,失败要静默降级而非报警
    pub unstable: bool,
    /// 拿不到凭据时的占位类型(渠道还没启用时的展示口径)
    pub default_kind: Kind,
    pub extract: fn(&Value) -> FetchResult,
}

/// 凭据来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// Windows 凭据管理器里的 API Key(用户自己填)
    Keyring,
    /// 本机已登录应用的凭据(只读复用,用户不用填任何东西)
    App(App),
}

/// 请求方式。
#[derive(Clone, Copy)]
pub enum Method {
    Get,
    /// POST + JSON body(函数生成:窗口时间要按当前时刻算)
    PostJson(fn() -> String),
}

/// 北京时间。两个平台的日期都不带时区,且都是中国版服务,统一按 UTC+8 解释。
fn beijing(offset_sec: i64) -> String {
    use chrono::{Duration, FixedOffset, Utc};
    let tz = FixedOffset::east_opt(8 * 3600).expect("UTC+8 合法");
    (Utc::now() + Duration::seconds(offset_sec))
        .with_timezone(&tz)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// 把 "2026-09-22 10:59:25" 这种北京时间字符串转成 unix 秒。
fn parse_beijing_time(s: &str) -> Option<i64> {
    use chrono::{FixedOffset, NaiveDateTime, TimeZone};
    let tz = FixedOffset::east_opt(8 * 3600)?;
    let dt = NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    tz.from_local_datetime(&dt).single().map(|x| x.timestamp())
}

/// 一笔记入到期表的最小剩余量。低于这个数按"已经用完"处理,免得列表里
/// 排一堆 0.00 的行。
const POINTS_EPS: f64 = 0.01;

/// Trae:积分制。总量在 usage_summary,逐笔在 user_entitlement_pack_list ——
/// 每笔的数量(quota.credits_limit)、已用(usage.credits_amount)、到期
/// (expire_time,unix 秒)都齐全,所以"哪天过期多少"能精确到笔。
pub fn extract_trae(body: &Value) -> FetchResult {
    let Some(packs) = body.get("user_entitlement_pack_list").and_then(|v| v.as_array()) else {
        return FetchResult {
            valid: false,
            kind: Kind::Points,
            unit: "credits".into(),
            error: Some("响应结构异常".into()),
            ..Default::default()
        };
    };

    let mut expiring: Vec<Expiring> = Vec::new();
    let mut remaining = 0.0;
    let mut granted = 0.0;
    let mut used = 0.0;
    let mut counted = 0usize;

    for p in packs {
        // 免费订阅包没有 credits_limit(它靠 no_bonus_quota 之类的标志描述),
        // 额度不在这套计数里,跳过 —— 硬按 0 算会把它记成"0 积分已用完"。
        let Some(limit) = path(p, &["entitlement_base_info", "quota", "credits_limit"]).and_then(num)
        else {
            continue;
        };
        let pack_used = path(p, &["usage", "credits_amount"]).and_then(num).unwrap_or(0.0);
        counted += 1;
        granted += limit;
        used += pack_used;
        let left = (limit - pack_used).max(0.0);
        remaining += left;

        let Some(at) = p.get("expire_time").and_then(num).filter(|t| *t > 0.0) else {
            continue;
        };
        if left < POINTS_EPS {
            continue;
        }
        let label = path(
            p,
            &[
                "entitlement_base_info",
                "product_extra",
                "package_extra",
                "package_name",
            ],
        )
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("积分包")
        .to_string();
        expiring.push(Expiring {
            at: at as i64,
            amount: round2(left),
            label,
        });
    }

    if counted == 0 {
        return FetchResult {
            valid: false,
            kind: Kind::Points,
            unit: "credits".into(),
            error: Some("无可用额度包".into()),
            ..Default::default()
        };
    }
    expiring.sort_by_key(|e| e.at);

    // 汇总口径用接口给的(它才是官方口径);拿不到时退回逐笔求和。
    let total_granted = path(body, &["usage_summary", "total_amount"]).and_then(num);
    let total_used = path(body, &["usage_summary", "consumed_amount"]).and_then(num);

    FetchResult {
        valid: true,
        kind: Kind::Points,
        remaining: Some(round2(remaining)),
        used: Some(round2(total_used.unwrap_or(used))),
        total: None, // 积分型没有"额度上限"这个概念,不画进度条
        unit: "credits".into(),
        expiring,
        extra: vec![
            ("累计发放".into(), format!("{:.0} 积分", total_granted.unwrap_or(granted))),
            ("已使用".into(), format!("{:.2} 积分", total_used.unwrap_or(used))),
            ("额度包".into(), format!("{} 笔", counted)),
        ],
        ..Default::default()
    }
}

/// WorkBuddy(腾讯 CodeBuddy 的积分)。逐笔在 get-user-resource 的
/// Accounts 数组里:数量/剩余是字符串小数,到期时间三种字段各有各的格式。
pub fn extract_workbuddy(body: &Value) -> FetchResult {
    let accounts = path(body, &["data", "Response", "Data", "Accounts"]).and_then(|v| v.as_array());
    let Some(accounts) = accounts else {
        let msg = body
            .get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or("响应结构异常");
        return FetchResult {
            valid: false,
            kind: Kind::Points,
            unit: "credits".into(),
            error: Some(msg.into()),
            ..Default::default()
        };
    };

    let mut expiring: Vec<Expiring> = Vec::new();
    let mut remaining = 0.0;
    let mut granted = 0.0;
    let mut used = 0.0;
    let mut counted = 0usize;

    for a in accounts {
        // Status:0 有效,1 退款,2 已过期,3 已用完。退款/已过期的包不该计入余额。
        let status = a.get("Status").and_then(num).unwrap_or(0.0) as i64;
        if status == 1 || status == 2 {
            continue;
        }
        // 切片包的额度在 SlicePeriodUsageDetails 里,主字段为 0。
        let slice = a
            .get("SlicePeriodUsageDetails")
            .and_then(|v| v.as_array())
            .and_then(|l| l.first());
        let pick = |key: &str| -> Option<f64> {
            slice
                .and_then(|s| s.get(format!("SlicePeriodCapacity{}", key)))
                .and_then(num)
                .or_else(|| a.get(format!("CycleCapacity{}", key)).and_then(num))
        };
        let Some(size) = pick("SizePrecise") else {
            continue;
        };
        let left = pick("RemainPrecise").unwrap_or(0.0).max(0.0);
        counted += 1;
        granted += size;
        used += (size - left).max(0.0);
        remaining += left;

        let Some(at) = wb_expiry_at(a) else { continue };
        if left < POINTS_EPS {
            continue;
        }
        expiring.push(Expiring {
            at,
            amount: round2(left),
            label: wb_pack_label(a.get("PackageCode").and_then(|c| c.as_str()).unwrap_or("")),
        });
    }

    if counted == 0 {
        return FetchResult {
            valid: false,
            kind: Kind::Points,
            unit: "credits".into(),
            error: Some("无可用的积分包".into()),
            ..Default::default()
        };
    }
    expiring.sort_by_key(|e| e.at);

    FetchResult {
        valid: true,
        kind: Kind::Points,
        remaining: Some(round2(remaining)),
        used: Some(round2(used)),
        total: None,
        unit: "credits".into(),
        expiring,
        extra: vec![
            ("本周期发放".into(), format!("{:.0} 积分", granted)),
            ("已使用".into(), format!("{:.2} 积分", used)),
            ("积分包".into(), format!("{} 笔", counted)),
        ],
        ..Default::default()
    }
}

/// 到期时间:优先 "扣减截止"(DeductionEndTime,毫秒时间戳字符串),
/// 其次是 ExpiredTime,最后是 CycleEndTime(本周期结束),都是北京时间字符串。
/// 与官方前端 `DeductionEndTime || ExpiredTime || CycleEndTime` 的取值顺序一致。
fn wb_expiry_at(a: &Value) -> Option<i64> {
    if let Some(ms) = a.get("DeductionEndTime").and_then(num).filter(|v| *v > 0.0) {
        return Some((ms / 1000.0) as i64);
    }
    for key in ["ExpiredTime", "CycleEndTime"] {
        if let Some(s) = a.get(key).and_then(|v| v.as_str()) {
            if let Some(ts) = parse_beijing_time(s) {
                return Some(ts);
            }
        }
    }
    None
}

/// 商品码 → 中文名。码形如 `TCACA_code_007_nzdH5h4Nl0`,数字段与官方前端的
/// 商品表一一对应;认不出来就退回"积分包",不猜。
fn wb_pack_label(code: &str) -> String {
    let idx = code
        .split("code_")
        .nth(1)
        .and_then(|s| s.split('_').next())
        .and_then(|s| s.parse::<u32>().ok());
    match idx {
        Some(1) => "每日积分".into(),
        Some(2) => "Pro 月度".into(),
        Some(3) => "Pro 年度".into(),
        Some(5) => "Pro 月度 Plus".into(),
        Some(6) => "赠送积分".into(),
        Some(7) => "活动赠送".into(),
        Some(8) => "免费月度".into(),
        Some(9) => "加量包".into(),
        _ => "积分包".into(),
    }
}

/// Trae 的取数 body。两个端点(ide_user_ent_usage / user_current_entitlement_list)
/// 返回同一份数据,带上 require_usage 才是逐笔明细。
pub fn trae_body() -> String {
    r#"{"require_usage":true,"full_data":true}"#.to_string()
}

/// WorkBuddy 的取数 body。分页取 200 条(实测 30 条足够覆盖);时间窗与官方前端
/// 一致(从当前时刻起)—— 窗口开太宽会把上几个周期用剩的旧包也捞回来,
/// "本周期发放 / 已使用"就会比官方口径大(实测宽 7 天多出 4 个包、400 积分)。
pub fn workbuddy_body() -> String {
    format!(
        r#"{{"PageNumber":1,"PageSize":200,"ProductCode":"p_tcaca","Status":[0,1,2,3],"PackageEndTimeRangeBegin":"{}","PackageEndTimeRangeEnd":"{}"}}"#,
        beijing(0),
        beijing(100 * 365 * 86_400)
    )
}

/// Codex(ChatGPT 订阅)的用量。`wham/usage` 返回 `rate_limit.primary_window`
/// (+ 可选 secondary_window),字段为本机实抓样本(见 codex-research.html)。
/// 窗口标签按时长生成 —— 窗型由服务端按套餐决定,绝不能写死「5 小时」
/// (OpenAI 2026-07 曾临时取消过 5 小时窗又恢复,结构是会变的)。
pub fn extract_codex(body: &Value) -> FetchResult {
    let rl = match body.get("rate_limit") {
        Some(v) if v.is_object() => v,
        _ => {
            return FetchResult {
                valid: false,
                kind: Kind::Percent,
                error: Some("响应无 rate_limit(登录态或接口结构变了)".into()),
                ..Default::default()
            }
        }
    };

    let win = |key: &str| -> Option<Window> {
        let w = rl.get(key)?;
        let used = w.get("used_percent").and_then(num)?;
        let secs = w.get("limit_window_seconds").and_then(num).unwrap_or(0.0);
        Some(Window {
            label: codex_window_label(secs),
            percent: round1(used),
            remain_percent: round1((100.0 - used).max(0.0)),
            status: if used >= 100.0 { "rate-limited".into() } else { "ok".into() },
            resets_at: w
                .get("reset_at")
                .and_then(num)
                .filter(|s| *s > 0.0)
                .and_then(|s| {
                    chrono::DateTime::from_timestamp(s as i64, 0)
                        .map(|d| d.to_rfc3339())
                }),
        })
    };

    let Some(primary) = win("primary_window") else {
        return FetchResult {
            valid: false,
            kind: Kind::Percent,
            error: Some("无可用配额窗口".into()),
            ..Default::default()
        };
    };
    let mut windows = vec![primary.clone()];
    if let Some(s) = win("secondary_window") {
        windows.push(s);
    }

    let limited = rl
        .get("limit_reached")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let plan = body
        .get("plan_type")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();
    let mut extra = Vec::new();
    if !plan.is_empty() {
        extra.push(("套餐".into(), plan));
    }

    // 裁决①:大数字固定取 primary 窗(不是最紧窗);副标题会写明窗名。
    FetchResult {
        valid: true,
        kind: Kind::Percent,
        remaining: Some(primary.remain_percent),
        used: Some(primary.percent),
        total: Some(100.0),
        unit: "%".into(),
        limited: limited || primary.status == "rate-limited",
        windows,
        extra,
        ..Default::default()
    }
}

/// 窗长 → 中文标签。阈值取整档上沿,服务端怎么改窗型都能落进合理名字。
/// 0(字段缺失)落兜底名「周期」,不能猜成「5 小时」—— 那是编造服务端没给的信息。
fn codex_window_label(secs: f64) -> String {
    if secs <= 0.0 {
        "周期".into()
    } else if secs <= 6.0 * 3600.0 {
        "5 小时".into()
    } else if secs <= 36.0 * 3600.0 {
        "当日".into()
    } else if secs <= 9.0 * 86_400.0 {
        "本周".into()
    } else if secs <= 35.0 * 86_400.0 {
        "本月".into()
    } else {
        "周期".into()
    }
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
        auth: AuthKind::Keyring,
        auth_prefix: "Bearer ",
        method: Method::Get,
        unstable: false,
        default_kind: Kind::Amount,
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
        auth: AuthKind::Keyring,
        auth_prefix: "Bearer ",
        method: Method::Get,
        unstable: true,
        default_kind: Kind::Percent,
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
        auth: AuthKind::Keyring,
        auth_prefix: "Bearer ",
        method: Method::Get,
        unstable: false,
        default_kind: Kind::Amount,
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
        auth: AuthKind::Keyring,
        auth_prefix: "Bearer ",
        method: Method::Get,
        unstable: false,
        default_kind: Kind::Amount,
        extract: extract_hapi,
    },
    ProviderDef {
        id: "trae",
        name: "Trae",
        short: "TR",
        color: "#6d5ef8",
        // 两个端点返回同一份数据,互为备份(某个被摘掉时不至于整个渠道瞎掉)
        url: "https://api.trae.cn/trae/api/v2/pay/user_current_entitlement_list",
        fallback_urls: &["https://api.trae.cn/trae/api/v2/pay/ide_user_ent_usage"],
        // Origin 可以不发,但一旦发错就是 403 —— 带着正确值最稳
        extra_headers: &[
            ("Origin", "https://www.trae.cn"),
            ("Referer", "https://www.trae.cn/"),
        ],
        auth: AuthKind::App(App::Trae),
        auth_prefix: "Cloud-IDE-JWT ",
        method: Method::PostJson(trae_body),
        unstable: true,
        default_kind: Kind::Points,
        extract: extract_trae,
    },
    ProviderDef {
        id: "workbuddy",
        name: "WorkBuddy",
        short: "WB",
        color: "#19b8c4",
        url: "https://www.workbuddy.cn/billing/meter/get-user-resource",
        fallback_urls: &["https://www.codebuddy.cn/billing/meter/get-user-resource"],
        // X-Client-Platform 是硬要求:缺了直接 403 code 10085(实测)
        extra_headers: &[
            ("X-Client-Platform", "web"),
            ("Origin", "https://www.workbuddy.cn"),
            ("Referer", "https://www.workbuddy.cn/profile/plans-usage"),
        ],
        auth: AuthKind::App(App::WorkBuddy),
        auth_prefix: "Bearer ",
        method: Method::PostJson(workbuddy_body),
        unstable: true,
        default_kind: Kind::Points,
        extract: extract_workbuddy,
    },
    ProviderDef {
        id: "codex",
        name: "Codex",
        short: "CX",
        color: "#10a37f",
        // ChatGPT 后端内部接口,Codex CLI 自身每 60s 轮询同一个地址。
        // 实测只带 Authorization 即可(account-id 头可省);无备用入口。
        url: "https://chatgpt.com/backend-api/wham/usage",
        fallback_urls: &[],
        extra_headers: &[],
        auth: AuthKind::App(App::Codex),
        auth_prefix: "Bearer ",
        method: Method::Get,
        unstable: true,
        default_kind: Kind::Percent,
        extract: extract_codex,
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

    /// Trae:逐笔到期。每笔的数量/已用/到期都要落到 expiring 里,按到期升序;
    /// 没有 credits_limit 的免费订阅包要跳过(不能按 0 记一笔)。
    #[test]
    fn trae_fills_per_pack_expiry() {
        let body = json!({
            "usage_summary": { "total_amount": 850, "consumed_amount": 344.724 },
            "user_entitlement_pack_list": [
                {   // 免费订阅包:没有 credits_limit,必须跳过
                    "expire_time": 1790783999_i64,
                    "entitlement_base_info": { "quota": { "no_bonus_quota": true } }
                },
                {   // 已用完的签到包:剩余 0,不进到期表,但计入已用
                    "expire_time": 1789716889_i64,
                    "usage": { "credits_amount": 150 },
                    "entitlement_base_info": {
                        "quota": { "credits_limit": 150 },
                        "product_extra": { "package_extra": { "package_name": "签到奖励" } }
                    }
                },
                {   // 还有 5.276 的签到包,9-18 到期
                    "expire_time": 1789716889_i64,
                    "usage": { "credits_amount": 194.724 },
                    "entitlement_base_info": {
                        "quota": { "credits_limit": 200 },
                        "product_extra": { "package_extra": { "package_name": "签到奖励" } }
                    }
                },
                {   // 500 分的每月赠送,9-30 到期
                    "expire_time": 1790783999_i64,
                    "entitlement_base_info": {
                        "quota": { "credits_limit": 500 },
                        "product_extra": { "package_extra": { "package_name": "每月登录赠送" } }
                    }
                }
            ]
        });
        let r = extract_trae(&body);
        assert!(r.valid, "{:?}", r.error);
        assert_eq!(r.kind, Kind::Points);
        // 150 + 200 + 500 = 850 发放;已用 150 + 194.724 → 剩 505.276
        assert_eq!(r.remaining, Some(505.28));
        assert_eq!(r.used, Some(344.72));
        assert!(r.total.is_none(), "积分型不设总额上限");
        assert_eq!(r.expiring.len(), 2, "用完的那笔不进到期表");
        assert_eq!(r.expiring[0].at, 1789716889); // 升序:9-18 在前
        assert_eq!(r.expiring[0].amount, 5.28);
        assert_eq!(r.expiring[0].label, "签到奖励");
        assert_eq!(r.expiring[1].at, 1790783999);
        assert_eq!(r.expiring[1].amount, 500.0);
    }

    /// 结构不对必须报无效,而不是给一个"剩余 0"的假结果。
    #[test]
    fn trae_reports_bad_shape() {
        let r = extract_trae(&json!({ "code": 401, "msg": "unauthorized" }));
        assert!(!r.valid);
        assert!(r.error.is_some());
        // 只有免费包(没有 credits_limit)= 没有可用额度包
        let r = extract_trae(&json!({
            "user_entitlement_pack_list": [{ "expire_time": 1, "entitlement_base_info": {} }]
        }));
        assert!(!r.valid);
    }

    /// WorkBuddy:数量/剩余是字符串小数,到期优先取 DeductionEndTime(毫秒),
    /// 退回 CycleEndTime(北京时间字符串);退款/已过期的包不计入。
    #[test]
    fn workbuddy_reads_packs_and_expiry() {
        let body = json!({
            "code": 0, "msg": "OK",
            "data": { "Response": { "Data": { "Accounts": [
                {
                    "PackageCode": "TCACA_code_007_nzdH5h4Nl0", "Status": 0,
                    "CapacityType": 1,
                    "CycleCapacitySizePrecise": "100",
                    "CycleCapacityRemainPrecise": "9.4000001",
                    "CycleCapacityUsedPrecise": "90.5999999",
                    "CycleEndTime": "2026-09-22 10:59:25",
                    "DeductionEndTime": "1790045965000"
                },
                {
                    "PackageCode": "TCACA_code_008_cfWoLwvjU4", "Status": 0,
                    "CycleCapacitySizePrecise": "500", "CycleCapacityRemainPrecise": "500",
                    "CycleEndTime": "2026-09-30 23:59:59"
                },
                {   // 已过期:不计入
                    "PackageCode": "TCACA_code_007_nzdH5h4Nl0", "Status": 2,
                    "CycleCapacitySizePrecise": "100", "CycleCapacityRemainPrecise": "100",
                    "CycleEndTime": "2026-09-18 15:33:13"
                },
                {   // 已用完:剩余 0,只计入发放/已用
                    "PackageCode": "TCACA_code_007_nzdH5h4Nl0", "Status": 3,
                    "CycleCapacitySizePrecise": "100", "CycleCapacityRemainPrecise": "0",
                    "CycleEndTime": "2026-09-18 15:33:13"
                }
            ]}}}
        });
        let r = extract_workbuddy(&body);
        assert!(r.valid, "{:?}", r.error);
        assert_eq!(r.kind, Kind::Points);
        // 9.4000001 + 500 + 0 = 509.4(过期的 100 不算)
        assert_eq!(r.remaining, Some(509.4));
        assert_eq!(r.expiring.len(), 2);
        assert_eq!(r.expiring[0].amount, 9.4);
        assert_eq!(r.expiring[0].label, "活动赠送");
        // DeductionEndTime 是毫秒,要按秒算
        assert_eq!(r.expiring[0].at, 1790045965);
        // 没有 DeductionEndTime 时退回 CycleEndTime(北京时间 → unix)
        assert_eq!(r.expiring[1].label, "免费月度");
        assert_eq!(
            r.expiring[1].at,
            parse_beijing_time("2026-09-30 23:59:59").unwrap()
        );
    }

    #[test]
    fn workbuddy_reports_business_error() {
        let r = extract_workbuddy(&json!({ "code": 10085, "msg": "请求不合法,如有疑问请联系客服" }));
        assert!(!r.valid);
        assert_eq!(r.error.as_deref(), Some("请求不合法,如有疑问请联系客服"));
    }

    #[test]
    fn beijing_time_roundtrip() {
        // 2026-09-18 15:34:49(UTC+8)= 1789716889
        assert_eq!(parse_beijing_time("2026-09-18 15:34:49"), Some(1789716889));
        assert_eq!(parse_beijing_time("  2026-09-18 15:34:49 "), Some(1789716889));
        assert_eq!(parse_beijing_time("2026/09/18"), None);
    }

    /// 本机实抓的 free 号响应:单个 30 天窗、0% 已用。大数字取 primary。
    #[test]
    fn codex_free_single_month_window() {
        let r = extract_codex(&json!({
            "plan_type": "free",
            "rate_limit": {
                "allowed": true, "limit_reached": false,
                "primary_window": {
                    "used_percent": 0, "limit_window_seconds": 2592000,
                    "reset_after_seconds": 2592000, "reset_at": 1792378201
                },
                "secondary_window": null
            }
        }));
        assert!(r.valid);
        assert_eq!(r.kind, Kind::Percent);
        assert_eq!(r.windows.len(), 1);
        assert_eq!(r.windows[0].label, "本月");
        assert_eq!(r.windows[0].remain_percent, 100.0);
        assert_eq!(r.remaining, Some(100.0));
        assert!(!r.limited);
        assert_eq!(r.extra[0], ("套餐".into(), "free".into()));
        assert!(r.windows[0].resets_at.as_deref().unwrap().ends_with("+00:00"));
    }

    /// 付费号预期:5 小时 primary + 每周 secondary 两窗,大数字仍取 primary。
    #[test]
    fn codex_paid_two_windows_headline_is_primary() {
        let r = extract_codex(&json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true, "limit_reached": false,
                "primary_window": { "used_percent": 76.5, "limit_window_seconds": 18000, "reset_at": 1792378201 },
                "secondary_window": { "used_percent": 39.0, "limit_window_seconds": 604800, "reset_at": 1792378201 }
            }
        }));
        assert!(r.valid);
        assert_eq!(r.windows.len(), 2);
        assert_eq!(r.windows[0].label, "5 小时");
        assert_eq!(r.windows[1].label, "本周");
        assert_eq!(r.remaining, Some(23.5));
    }

    #[test]
    fn codex_rate_limited_marks_limited() {
        let r = extract_codex(&json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": false, "limit_reached": true,
                "primary_window": { "used_percent": 100, "limit_window_seconds": 18000, "reset_at": 1792378201 },
                "secondary_window": null
            }
        }));
        assert!(r.valid);
        assert!(r.limited);
        assert_eq!(r.windows[0].status, "rate-limited");
        assert_eq!(r.windows[0].remain_percent, 0.0);
    }

    #[test]
    fn codex_missing_rate_limit_is_invalid() {
        let r = extract_codex(&json!({ "detail": "Not authenticated" }));
        assert!(!r.valid);
        assert!(r.error.is_some());
    }

    #[test]
    fn codex_window_labels_scale_with_seconds() {
        assert_eq!(codex_window_label(18000.0), "5 小时");
        assert_eq!(codex_window_label(86_400.0), "当日");
        assert_eq!(codex_window_label(604_800.0), "本周");
        assert_eq!(codex_window_label(2_592_000.0), "本月");
        assert_eq!(codex_window_label(9_000_000.0), "周期");
        // 字段缺失(limit_window_seconds 不在/为 0)必须落兜底名,不能猜「5 小时」
        assert_eq!(codex_window_label(0.0), "周期");
    }

}
