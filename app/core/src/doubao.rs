//! 豆包(doubao.com)网页会话渠道:订阅额度 + 活动赠送额度读数。
//!
//! 取数走官方客户端启动时自己调用的只读汇总接口:
//!
//! ```text
//! POST https://www.doubao.com/alice/commerce/sale/subscription/quota/summary/
//!   body {"product_line":"membership"}
//!   → window_limit_section.window_limit_groups[]      套餐额度与旧版短窗
//!   → incentive_quota_section.window_limit_groups[]   活动赠送额度
//! ```
//!
//! 窗口类型:`4` 套餐积分、`3` 活动赠送、`1/2` 旧版 5 小时/周窗(2026-11 起
//! 个人版已取消 5 小时限制,但旧套餐仍可能返回,不能写死)。任务面板接口
//! (`campaign/v3/detail`)只返回体验额度,已降为兼容解析分支,不再作为主取数。
//!
//! 鉴权:**纯 doubao.com 网页会话 Cookie**,没有 a_bogus / msToken 请求签名。
//! 请求身份必须和登录窗一致:登录窗现在也用同一份桌面版 UA,取数继续带
//! version_code / aid / 派生设备号;网页 Cookie 配另一套 UA 是 710022004 的典型
//! 触发条件,不能混用。**version / version_code / kernel 运行时从本机真实客户端探测**
//! ([`crate::doubao_client`])—— 钉死的版本参数一腐烂就会被拒(2026-11-14 实测,
//! 复盘见 docs/report/doubao-checkin-710010702-2026-11-14.md)。
//! web_id / tea_uuid 按 Cookie 里的 sessionid 确定性派生(每号一台稳定「设备」;
//! CreditDaddy 按 uid 派生 —— cookie 里的 sessionid 是同一账号的稳定标识,等价且
//! 不需要额外接口)。Golden 向量钉住派生与查询串(见测试)。
//!
//! **自动签到已移除**(2026-11-14,随 v0.3.47):签到写路径被服务端整体拒绝
//! (业务码 710010702,连官方客户端都领不动),直连路线短期无解,渠道只留取数。
//! 710022004 是风控验证要求,不是普通限流:一旦收到就停止自动重试 30 分钟,
//! 由用户在官方客户端手动完成验证,程序不尝试绕过。
//!
//! 凭据:网页会话住在 WebView2 的 profile 里(与 WorkBuddy 同一条裁决,不落盘),
//! 登录窗捕获后取数时现读;手动粘贴的 Cookie 走凭据管理器兜底。都不进日志。

use crate::providers::{FetchResult, Kind};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 网页会话的主域名(登录窗、Cookie 罐都对着它)。
pub const HOST: &str = "www.doubao.com";
pub const QUOTA_PATH: &str = "/alice/commerce/sale/subscription/quota/summary/";
/// 订阅额度汇总的请求体(与官方客户端一致)。
pub const QUOTA_BODY: &str = "{\"product_line\":\"membership\"}";

/// 桌面版 webview 的 UA 家族(**兜底占位值**:2026-10-19 CDP 抓包原样)。
/// 静态表(providers)引用它当占位;实际发送前取数路径会用 [`app_ua`] 运行时替换
/// —— 真实版本来自本机客户端探测([`crate::doubao_client`]),这个钉值只在
/// 不装客户端的机器上发出去,可能被服务端拒绝(见模块头注)。
pub const APP_UA: &str = crate::doubao_client::FALLBACK_UA;
pub const REFERER: &str = "https://www.doubao.com/pc/agent-explore";
pub const ORIGIN: &str = "https://www.doubao.com";

/// 会话失效的业务码(实测:匿名 / 过期 cookie 返 710012001「登录已过期」)。
const LOGIN_INVALID: f64 = 710012001.0;
/// 风控验证要求(滑块/人机验证)。它不是带宽限流,重试只会加深标记。
const RISK_CHALLENGE: f64 = 710022004.0;
/// 收到风控码后的本地静默期。官方客户端完成验证后再手动刷新即可。
const RISK_COOLDOWN: Duration = Duration::from_secs(30 * 60);

fn risk_until() -> &'static Mutex<Option<Instant>> {
    static UNTIL: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    UNTIL.get_or_init(|| Mutex::new(None))
}

/// 风控冷却期内返回用户可见说明;不发任何请求。
pub fn risk_cooldown_message() -> Option<String> {
    let mut slot = risk_until().lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();
    match *slot {
        Some(until) if until > now => {
            let mins = until.duration_since(now).as_secs().div_ceil(60).max(1);
            Some(format!(
                "豆包触发风控验证（710022004）· 已暂停自动重试，约 {mins} 分钟后可再试；请在官方客户端完成验证后手动刷新"
            ))
        }
        Some(_) => {
            *slot = None;
            None
        }
        None => None,
    }
}

/// 响应体顶层业务码是风控验证时，记下本地静默期。
/// 只读业务码，不解析或保存响应正文。
pub fn note_risk_if_present(body: &Value) {
    if biz_code(body) == Some(RISK_CHALLENGE) {
        *risk_until().lock().unwrap_or_else(|p| p.into_inner()) =
            Some(Instant::now() + RISK_COOLDOWN);
    }
}

#[cfg(test)]
fn clear_risk_cooldown() {
    *risk_until().lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// 订阅额度汇总的完整地址。
pub fn quota_url() -> String {
    format!("https://{HOST}{QUOTA_PATH}")
}

/// 从凭据键里解出 Cookie 串。允许带 `cookie:` 前缀(网页会话罐的传输格式)也允许裸
/// Cookie(手动粘贴的自救路径);没有 Bearer 概念 —— 豆包只有这一种凭据。
/// 形状不对返回 None(调用方如实报错,别拿去当 Bearer 撞)。
pub fn cookie_header_of(key: &str) -> Option<&str> {
    let rest = key.trim().strip_prefix("cookie:").unwrap_or(key.trim());
    if rest.is_empty() || !rest.contains('=') {
        return None;
    }
    // 换行/回车会拆出第二条请求头,绝不能放行(与 core::weblogin 同一条红线)
    if rest.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(rest)
}

/// 设备指纹的种子:Cookie 里的 `sessionid`(登录一次内恒定,换号才变)。
/// 取不到就退化为 "anonymous"(CreditDaddy 同款兜底)—— 匿名也有稳定指纹,
/// 只是登录后服务端按真实会话记账,种子只影响「这台设备长什么样」。
fn seed_of(cookie: &str) -> String {
    for name in ["sessionid", "sessionid_ss"] {
        for pair in cookie.split(';') {
            let pair = pair.trim();
            if let Some(v) = pair.strip_prefix(&format!("{name}=")) {
                let v = v.trim();
                if !v.is_empty() {
                    return v.to_string();
                }
            }
        }
    }
    "anonymous".into()
}

/// 按 seed 派生稳定设备标识(web_id 18 位数字、tea_uuid 16 位数字)。
/// 算法与 CreditDaddy 逐字节一致:sha256("doubao-dev:" + seed) 取字节模 10。
fn device_ids(seed: &str) -> (String, String) {
    let mut h = Sha256::new();
    h.update(format!("doubao-dev:{seed}").as_bytes());
    let d = h.finalize();
    let web_id = format!(
        "76{}",
        d[..8].iter().map(|b| (b % 10).to_string()).collect::<String>()
    );
    let mut tea: String = d[8..16].iter().map(|b| (b % 10).to_string()).collect();
    // padEnd(16, '7'):派生只出 8 位,后面补 '7'(参考实现原样)
    while tea.len() < 16 {
        tea.push('7');
    }
    (web_id, tea)
}

/// 组装带客户端指纹的接口 URL:固定平台参数 + 派生设备号 + 空设备/区域。
/// 版本段(version_code / pc_version)来自本机客户端探测 —— 钉死会腐烂,见模块头注。
/// 参数顺序与参考实现逐字节一致(URLSearchParams 插入序),Golden 钉死。
pub fn decorate_url(url: &str, cookie: &str) -> String {
    decorate_url_with(crate::doubao_client::detect(), url, cookie)
}

/// 同 [`decorate_url`],指纹可注入(单测用,Golden 不依赖装机状态)。
pub fn decorate_url_with(fp: &crate::doubao_client::ClientFingerprint, url: &str, cookie: &str) -> String {
    let base = url.split('?').next().unwrap_or(url);
    let seed = seed_of(cookie);
    let (web_id, tea_uuid) = device_ids(&seed);
    let pairs: [(&str, &str); 18] = [
        ("version_code", &fp.version_code),
        ("language", "zh"),
        ("device_platform", "web"),
        ("doubao_device_platform", "web"),
        ("aid", "497858"),
        ("real_aid", "497858"),
        ("pkg_type", "release_version"),
        ("pc_version", &fp.version),
        ("doubao_pc_version", "5.72.66"),
        ("samantha_web", "1"),
        ("web_platform", "desktop"),
        ("use-olympus-account", "1"),
        ("web_tab_id", "0"),
        ("web_id", &web_id),
        ("tea_uuid", &tea_uuid),
        ("device_id", ""),
        ("region", ""),
        ("sys_region", ""),
    ];
    format!(
        "{base}?{}",
        pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&")
    )
}

/// 当前应发送的 UA(运行时探测,进程内缓存;静态表的占位值在两条请求路径
/// 上都会被它替换 —— 调用方是 checkin::call_with_body 与 fetch::fetch_once_inner)。
pub fn app_ua() -> &'static str {
    let fp: &'static crate::doubao_client::ClientFingerprint = crate::doubao_client::detect();
    &fp.ua
}

// ---------------------------------------------------------------- 取数解析

fn fnum(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|x| x.is_finite())
}

/// 顶层业务码。额度/任务接口都是 `code` 字段 + HTTP 200 的信封,HTTP 码不作数。
fn biz_code(body: &Value) -> Option<f64> {
    body.get("code").and_then(fnum)
}

/// 金额字段可能是 `"9,000"` 这类格式化字符串;只保留数字和小数点。
fn amount(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '.')
            .collect::<String>()
            .parse::<f64>()
            .ok(),
        _ => None,
    }
    .filter(|x| x.is_finite() && *x >= 0.0)
}

fn epoch_seconds(v: f64) -> i64 {
    if v > 1e11 { (v / 1000.0) as i64 } else { v as i64 }
}

fn reset_label(v: f64) -> Option<String> {
    if v <= 0.0 {
        return None;
    }
    DateTime::<Utc>::from_timestamp(epoch_seconds(v), 0)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
}

/// 额度数值的显示:整数不留 ".0"(活动额度都是整数,200.0 要显示成 200)。
fn fmt_quota(v: f64) -> String {
    if v.fract().abs() < 1e-9 {
        format!("{}", v as i64)
    } else {
        format!("{v:.2}")
    }
}

/// detail 里顺带把签到摘要塞进展开区。**不额外发请求** —— 这份响应用户已经付过一次
/// 网络往返,签到状态、连续天数、今日奖励都在里面;再单独打一次接口只会多一条风控面。
fn push_signin_extra(r: &mut FetchResult, body: &Value) {
    let Some(tasks) = body.get("data").and_then(|d| d.get("tasks")).and_then(Value::as_array) else {
        return;
    };
    let Some(task) = checkin_task(body) else { return; };
    let Some(c) = task.get("daily_checkin") else { return; };

    let checked = c.get("checked_in_today").and_then(Value::as_bool);
    let days = c.get("continuous_days").and_then(fnum);
    let reward = c
        .get("reward")
        .and_then(|rw| rw.get("reward_amount"))
        .and_then(fnum);
    match checked {
        Some(true) => {
            let mut s = String::from("已签");
            if let Some(d) = days {
                s.push_str(&format!(" · 连续 {} 天", fmt_quota(d)));
            }
            r.extra.push(("今日签到".into(), s));
            if let Some(a) = reward {
                r.extra.push(("今日奖励".into(), format!("+{} 额度", fmt_quota(a))));
            }
        }
        Some(false) => {
            let mut s = String::from("未签");
            if let Some(a) = reward {
                s.push_str(&format!(" · 可领 +{} 额度", fmt_quota(a)));
            }
            r.extra.push(("今日签到".into(), s));
        }
        // 字段缺失 = 口径未知,不猜也不编
        None => {}
    }
    r.extra.push(("任务".into(), format!("{} 项", tasks.len())));
}

#[derive(Clone)]
struct SummaryWindow {
    window_type: i64,
    label: String,
    used_percent: Option<f64>,
    total: Option<f64>,
    used: Option<f64>,
    reset: Option<String>,
    starts_on_use: bool,
    exhausted: bool,
}

fn summary_label(window_type: i64, group: &str) -> String {
    match window_type {
        4 => {
            if group.trim().is_empty() {
                "套餐额度".into()
            } else {
                group.trim().to_string()
            }
        }
        3 => "活动赠送额度".into(),
        1 => "5 小时".into(),
        2 => "本周".into(),
        _ => "额度".into(),
    }
}

fn summary_window(raw: &Value, group: &str, exhausted: bool) -> Option<SummaryWindow> {
    let window_type = raw.get("window_type").and_then(fnum)?.round() as i64;
    if !(1..=4).contains(&window_type) {
        return None;
    }
    let total = raw.get("total_amount").and_then(amount);
    let used = raw.get("used_amount").and_then(amount);
    let mut used_percent = raw
        .get("used_percent")
        .and_then(fnum)
        .map(|v| v.clamp(0.0, 100.0));
    if used_percent.is_none() {
        used_percent = total
            .zip(used)
            .filter(|(t, _)| *t > 0.0)
            .map(|(t, u)| (u / t * 100.0).clamp(0.0, 100.0));
    }
    // 没有任何可解释的数值就不要构造 0%,否则会把「字段缺失」画成「已耗尽」。
    if total.is_none() && used.is_none() && used_percent.is_none() {
        return None;
    }
    let end = raw.get("end_time").and_then(fnum).unwrap_or(0.0);
    let start = raw.get("start_time").and_then(fnum).unwrap_or(0.0);
    Some(SummaryWindow {
        window_type,
        label: summary_label(window_type, group),
        used_percent,
        total,
        used,
        reset: reset_label(end),
        starts_on_use: end <= 0.0 || start <= 0.0,
        exhausted,
    })
}

fn summary_windows(data: &Value) -> Vec<SummaryWindow> {
    let mut out = Vec::new();
    for key in ["window_limit_section", "incentive_quota_section"] {
        let Some(section) = data.get(key) else { continue };
        let exhausted = section
            .get("usage_exhausted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let Some(groups) = section.get("window_limit_groups").and_then(Value::as_array) else {
            continue;
        };
        for group in groups {
            let group_name = group
                .get("feature_group_name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let Some(limits) = group.get("window_limits").and_then(Value::as_array) else {
                continue;
            };
            for raw in limits {
                if let Some(window) = summary_window(raw, group_name, exhausted) {
                    out.push(window);
                }
            }
        }
    }
    out
}

fn summary_window_text(w: &SummaryWindow) -> String {
    let mut text = match (w.total, w.used) {
        (Some(total), Some(used)) => format!(
            "剩余 {} / {}",
            fmt_quota((total - used).max(0.0)),
            fmt_quota(total)
        ),
        _ => w
            .used_percent
            .map(|used| format!("已用 {:.1}%", used))
            .unwrap_or_else(|| "额度已读取".into()),
    };
    if let Some(reset) = &w.reset {
        text.push_str(&format!(" · 重置 {reset}"));
    } else if w.starts_on_use {
        text.push_str(" · 开始使用后计时");
    }
    text
}

/// 订阅额度汇总:套餐积分是主口径,活动赠送和旧版短窗放展开区。
/// 不把多个套餐窗口相加 —— 不同 feature group 的额度不是同一份额度池。
fn extract_summary(body: &Value) -> FetchResult {
    let Some(data) = body.get("data") else {
        return FetchResult {
            kind: Kind::Points,
            unit: "额度".into(),
            error: Some("响应里没有额度数据(data)".into()),
            ..Default::default()
        };
    };
    let windows = summary_windows(data);
    if windows.is_empty() {
        return FetchResult {
            kind: Kind::Points,
            unit: "额度".into(),
            error: Some("额度接口没有返回可识别的额度窗口".into()),
            ..Default::default()
        };
    }
    let main_idx = windows
        .iter()
        .position(|w| w.window_type == 4 && w.total.is_some() && w.used.is_some())
        .or_else(|| windows.iter().position(|w| w.window_type == 4))
        .or_else(|| windows.iter().position(|w| w.window_type == 2))
        .or_else(|| windows.iter().position(|w| w.window_type == 1))
        .or_else(|| windows.iter().position(|w| w.window_type == 3))
        .unwrap_or(0);
    let main = &windows[main_idx];
    let (remaining, used, total, unit) = match (main.total, main.used) {
        (Some(total), Some(used)) => (
            Some((total - used).max(0.0)),
            Some(used),
            Some(total),
            "额度",
        ),
        _ => match main.used_percent {
            Some(used) => (Some((100.0 - used).max(0.0)), Some(used), Some(100.0), "%"),
            None => {
                return FetchResult {
                    kind: Kind::Points,
                    unit: "额度".into(),
                    error: Some("额度窗口缺少可换算的数值".into()),
                    ..Default::default()
                }
            }
        },
    };
    let mut result = FetchResult {
        valid: true,
        kind: Kind::Points,
        remaining,
        used,
        total,
        unit: unit.into(),
        limited: main.exhausted || remaining.is_some_and(|v| v <= 0.0),
        expiry_complete: false,
        ..Default::default()
    };
    if let Some(plan) = data
        .get("current_subscription")
        .and_then(|v| v.get("display"))
        .and_then(|v| v.get("short_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        result.extra.push(("套餐".into(), plan.to_string()));
    }
    if let Some(active) = data
        .get("member_info")
        .and_then(|v| v.get("hasActiveSubscription"))
        .and_then(Value::as_bool)
    {
        result
            .extra
            .push(("订阅状态".into(), if active { "生效" } else { "未订阅" }.into()));
    }
    for (index, window) in windows.iter().enumerate() {
        if index == main_idx {
            continue;
        }
        result
            .extra
            .push((window.label.clone(), summary_window_text(window)));
    }
    result
}

/// 旧任务面板兼容:只把 `progress.total_amount` 当体验额度,不再作为主接口。
fn extract_campaign(body: &Value) -> FetchResult {
    let Some(total) = body
        .get("data")
        .and_then(|d| d.get("progress"))
        .and_then(|p| p.get("total_amount"))
        .and_then(fnum)
    else {
        return FetchResult {
            kind: Kind::Points,
            unit: "额度".into(),
            error: Some("响应里没有额度汇总字段(data.progress.total_amount)".into()),
            ..Default::default()
        };
    };
    let mut result = FetchResult {
        valid: true,
        kind: Kind::Points,
        remaining: Some(total),
        unit: "额度".into(),
        expiry_complete: false,
        ..Default::default()
    };
    push_signin_extra(&mut result, body);
    result
}

/// 豆包额度读数:优先订阅汇总,兼容旧任务面板。
pub fn extract_doubao(body: &Value) -> FetchResult {
    let mut r = FetchResult {
        kind: Kind::Points,
        // 界面按 unit 渲染单位(前端:unit != "credits" 照显示),豆包管这叫「额度」
        unit: "额度".into(),
        ..Default::default()
    };
    match biz_code(body) {
        Some(c) if c == LOGIN_INVALID => {
            r.error = Some("登录已过期 · 在渠道卡点「登录豆包账号」".into());
            return r;
        }
        Some(c) if c == RISK_CHALLENGE => {
            r.error = Some(
                "豆包触发风控验证（710022004）· 已暂停自动重试，请在官方客户端完成验证后再手动刷新"
                    .into(),
            );
            return r;
        }
        Some(c) if c != 0.0 => {
            r.error = Some(format!("豆包业务错误(code {c})"));
            return r;
        }
        None => {
            r.error = Some("响应缺 code 字段".into());
            return r;
        }
        _ => {}
    }
    let data = body.get("data");
    if data
        .and_then(|d| d.get("window_limit_section"))
        .is_some()
        || data
            .and_then(|d| d.get("incentive_quota_section"))
            .is_some()
    {
        return extract_summary(body);
    }
    if data
        .and_then(|d| d.get("progress"))
        .and_then(|p| p.get("total_amount"))
        .is_some()
    {
        return extract_campaign(body);
    }
    r.error = Some("响应里没有可识别的额度字段".into());
    r
}

// ---------------------------------------------------------------- 签到任务(只进展示)

/// detail 响应里的签到任务(tasks[].task_kind === "daily_signin")。
/// 自动签到已移除,这个摘要只随取数进「今日签到」展示,让用户知道去客户端领。
fn checkin_task<'a>(body: &'a Value) -> Option<&'a Value> {
    body.get("data")
        .and_then(|d| d.get("tasks"))
        .and_then(Value::as_array)?
        .iter()
        .find(|t| t.get("task_kind").and_then(Value::as_str) == Some("daily_signin"))
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden:与 CreditDaddy doubaoClient.js 的派生逐字节一致(node sha256 算出)。
    #[test]
    fn device_ids_match_reference() {
        let (w, t) = device_ids("anonymous");
        assert_eq!(w, "7639479694");
        assert_eq!(t, "2888214177777777");
        let (w, t) = device_ids("sess_abc123");
        assert_eq!(w, "7608085871");
        assert_eq!(t, "0216664577777777");
    }

    /// 双向验证的"红"侧:种子变了,指纹必须变。
    #[test]
    fn device_ids_differ_by_seed() {
        let a = device_ids("a");
        let b = device_ids("b");
        assert_ne!(a.0, b.0);
        assert_ne!(a.1, b.1);
    }

    /// 2026-10-19 CDP 抓包时的指纹 —— Golden 用它注入,与装机状态无关。
    fn captured_fp() -> crate::doubao_client::ClientFingerprint {
        crate::doubao_client::ClientFingerprint {
            version: "2.32.6".into(),
            version_code: "20800".into(),
            chrome: "147.0.7727.149".into(),
            ua: crate::doubao_client::FALLBACK_UA.into(),
            source: "test".into(),
        }
    }

    #[test]
    fn decorate_url_matches_reference_query() {
        let url = decorate_url_with(&captured_fp(), quota_url().as_str(), "");
        let (base, q) = url.split_once('?').unwrap();
        assert_eq!(
            base,
            "https://www.doubao.com/alice/commerce/sale/subscription/quota/summary/"
        );
        assert_eq!(
            q,
            "version_code=20800&language=zh&device_platform=web&doubao_device_platform=web&aid=497858&real_aid=497858&pkg_type=release_version&pc_version=2.32.6&doubao_pc_version=5.72.66&samantha_web=1&web_platform=desktop&use-olympus-account=1&web_tab_id=0&web_id=7639479694&tea_uuid=2888214177777777&device_id=&region=&sys_region="
        );
    }

    /// 版本段跟指纹走:探测到新客户端,查询串里的 version_code / pc_version 同步换,
    /// 其余参数逐字节不动(2026-11-14:20800 已被服务端拒绝,见 docs/report)。
    #[test]
    fn decorate_url_follows_detected_fingerprint() {
        let fresh = crate::doubao_client::ClientFingerprint {
            version: "2.36.15".into(),
            version_code: "23615".into(),
            chrome: "158.0.1.2".into(),
            ua: String::new(),
            source: "test".into(),
        };
        let url = decorate_url_with(&fresh, quota_url().as_str(), "");
        assert!(url.contains("version_code=23615&"));
        assert!(url.contains("pc_version=2.36.15&"));
        assert!(url.contains("doubao_pc_version=5.72.66&"), "派生来源未知的参数维持钉值");
    }

    #[test]
    fn decorate_url_strips_existing_query() {
        let u = decorate_url_with(&captured_fp(), "https://www.doubao.com/x?old=1", "");
        assert!(u.contains("/x?version_code=20800"));
        assert!(!u.contains("old=1"));
    }

    #[test]
    fn seed_prefers_sessionid() {
        assert_eq!(seed_of("a=1; sessionid=xyz; b=2"), "xyz");
        assert_eq!(seed_of("sessionid_ss=ss1"), "ss1");
        assert_eq!(seed_of("nothing=here"), "anonymous");
        assert_eq!(seed_of("sessionid="), "anonymous");
    }

    #[test]
    fn cookie_header_accepts_raw_and_prefixed() {
        assert_eq!(cookie_header_of("a=1; b=2"), Some("a=1; b=2"));
        assert_eq!(cookie_header_of("cookie:a=1"), Some("a=1"));
        assert_eq!(cookie_header_of(""), None);
        assert_eq!(cookie_header_of("noequals"), None);
        assert_eq!(cookie_header_of("a=1\nX: 2"), None, "控制字符要拦");
    }

    #[test]
    fn extract_subscription_summary_prefers_package_and_keeps_bonus_separate() {
        let body = serde_json::json!({
            "code": 0,
            "data": {
                "window_limit_section": {
                    "usage_exhausted": false,
                    "window_limit_groups": [{
                        "feature_group_name": "标准套餐",
                        "window_limits": [{
                            "window_type": 4,
                            "used_percent": 4.99,
                            "start_time": 1_764_000_000_000i64,
                            "end_time": 1_764_604_800_000i64,
                            "total_amount": "9,000",
                            "used_amount": "449"
                        }]
                    }]
                },
                "incentive_quota_section": {
                    "usage_exhausted": false,
                    "window_limit_groups": [{
                        "feature_group_name": "活动赠送额度",
                        "window_limits": [{
                            "window_type": 3,
                            "total_amount": "2,100",
                            "used_amount": "0",
                            "end_time": 1_764_604_800_000i64
                        }]
                    }]
                },
                "current_subscription": {"display": {"short_name": "标准套餐"}},
                "member_info": {"hasActiveSubscription": true}
            }
        });
        let r = extract_doubao(&body);
        assert!(r.valid, "{:?}", r.error);
        assert_eq!(r.remaining, Some(8551.0));
        assert_eq!(r.used, Some(449.0));
        assert_eq!(r.total, Some(9000.0));
        assert_eq!(r.unit, "额度");
        assert!(!r.limited);
        assert!(r.extra.iter().any(|(k, v)| k == "套餐" && v == "标准套餐"));
        assert!(r
            .extra
            .iter()
            .any(|(k, v)| k == "活动赠送额度" && v.contains("剩余 2100 / 2100")));
    }

    #[test]
    fn extract_legacy_percentage_window_uses_percent_unit() {
        let body = serde_json::json!({
            "code": 0,
            "data": {
                "window_limit_section": {
                    "window_limit_groups": [{
                        "feature_group_name": "旧版",
                        "window_limits": [{"window_type": 1, "used_percent": 40.0}]
                    }]
                }
            }
        });
        let r = extract_doubao(&body);
        assert!(r.valid);
        assert_eq!(r.remaining, Some(60.0));
        assert_eq!(r.used, Some(40.0));
        assert_eq!(r.total, Some(100.0));
        assert_eq!(r.unit, "%");
    }

    #[test]
    fn extract_summary_without_numbers_is_not_rendered_as_zero() {
        // 场景一:窗口有部分数值(只有 total 没有 used/percent)→ 明确报「缺少可换算」。
        let body = serde_json::json!({
            "code": 0,
            "data": {
                "window_limit_section": {
                    "window_limit_groups": [{
                        "feature_group_name": "标准套餐",
                        "window_limits": [{"window_type": 4, "total_amount": "9,000"}]
                    }]
                }
            }
        });
        let r = extract_doubao(&body);
        assert!(!r.valid);
        assert!(r.remaining.is_none());
        assert!(r.error.unwrap().contains("缺少可换算"));

        // 场景二:窗口完全没有任何数值 → summary_window 会丢弃该窗口,
        // 报「没有可识别的额度窗口」,同样不许渲染成 0 / 已耗尽。
        let body = serde_json::json!({
            "code": 0,
            "data": {
                "window_limit_section": {
                    "window_limit_groups": [{
                        "feature_group_name": "标准套餐",
                        "window_limits": [{"window_type": 4}]
                    }]
                }
            }
        });
        let r = extract_doubao(&body);
        assert!(!r.valid);
        assert!(r.remaining.is_none());
        assert!(r.error.unwrap().contains("可识别的额度窗口"));
    }

    #[test]
    fn risk_code_sets_local_cooldown_without_network() {
        clear_risk_cooldown();
        assert!(risk_cooldown_message().is_none());
        note_risk_if_present(&serde_json::json!({"code": 710022004}));
        assert!(risk_cooldown_message().unwrap().contains("710022004"));
        clear_risk_cooldown();
        assert!(risk_cooldown_message().is_none());
    }

    #[test]
    fn extract_happy_path() {
        let body = serde_json::json!({
            "code": 0,
            "data": {"progress": {"total_amount": 3267.0}}
        });
        let r = extract_doubao(&body);
        assert!(r.valid);
        assert_eq!(r.kind, Kind::Points);
        assert_eq!(r.remaining, Some(3267.0));
        assert_eq!(r.unit, "额度");
        assert!(!r.expiry_complete, "豆包没有逐笔到期数据,不能谎称完整");
    }

    #[test]
    fn extract_adds_signin_summary_from_same_response() {
        let body = serde_json::json!({
            "code": 0,
            "data": {
                "progress": {"total_amount": 950.0},
                "tasks": [
                    {"task_kind": "chat_activity", "title": "网页任务"},
                    {"task_kind": "daily_signin", "daily_checkin": {
                        "checked_in_today": true,
                        "continuous_days": 1,
                        "reward": {"reward_amount": 200.0}
                    }}
                ]
            }
        });
        let r = extract_doubao(&body);
        assert!(r.valid);
        assert_eq!(r.remaining, Some(950.0));
        assert!(r
            .extra
            .iter()
            .any(|(k, v)| k == "今日签到" && v == "已签 · 连续 1 天"));
        assert!(r
            .extra
            .iter()
            .any(|(k, v)| k == "今日奖励" && v == "+200 额度"));
        assert!(r.extra.iter().any(|(k, v)| k == "任务" && v == "2 项"));
    }

    #[test]
    fn extract_unchecked_signin_shows_claimable_without_guessing() {
        let body = serde_json::json!({
            "code": 0,
            "data": {
                "progress": {"total_amount": 0.0},
                "tasks": [
                    {"task_kind": "daily_signin", "daily_checkin": {
                        "checked_in_today": false,
                        "reward": {"reward_amount": 200.0}
                    }},
                    {"task_kind": "chat_activity"}
                ]
            }
        });
        let r = extract_doubao(&body);
        assert!(r.valid);
        assert!(r
            .extra
            .iter()
            .any(|(k, v)| k == "今日签到" && v == "未签 · 可领 +200 额度"));
    }

    #[test]
    fn extract_without_signin_task_keeps_extra_empty() {
        let body = serde_json::json!({
            "code": 0,
            "data": {"progress": {"total_amount": 100.0}, "tasks": [{"task_kind": "chat_activity"}]}
        });
        let r = extract_doubao(&body);
        assert!(r.valid);
        assert!(r.extra.is_empty(), "没有签到任务就不要编签到摘要");
    }

    #[test]
    fn extract_login_invalid_is_auth_error() {
        let r = extract_doubao(&serde_json::json!({"code": 710012001}));
        assert!(!r.valid);
        assert!(r.error.unwrap().contains("登录已过期"));
    }

    #[test]
    fn extract_other_code_and_missing_total() {
        let r = extract_doubao(&serde_json::json!({"code": 500}));
        assert!(!r.valid);
        let r = extract_doubao(&serde_json::json!({"code": 0, "data": {}}));
        assert!(!r.valid, "缺额度字段要如实报错,不能渲染成 0");
    }
}
