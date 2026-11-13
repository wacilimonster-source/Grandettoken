//! 豆包(doubao.com)网页会话渠道:体验额度读数。
//!
//! 口径来自 CreditDaddy 的 doubaoClient.js(issue #32,2026-10-19/20 用调试端口
//! CDP 实测):
//!
//! ```text
//! POST https://www.doubao.com/alice/ug/campaign/v3/detail/      任务面板(一次给全)
//!   body {"campaign_group_code":"pc_incentive_task_mvp_v3"}
//!   → data.progress.total_amount = 体验额度余量
//!   → data.tasks[] 里 task_kind === "daily_signin" 那项 = 签到状态(只在展示里带一句)
//! ```
//!
//! 鉴权:**纯 doubao.com 网页会话 Cookie**,没有 a_bogus / msToken 请求签名,
//! device_id 空可过 —— 但**客户端环境要像样**(issue #32 实测:裸 UA 打活动接口
//! 吃「请求非法」)。指纹 = 豆包桌面版 webview 的请求形状:
//! UA 是 `DoubaoDesktop/{version} Chrome/{kernel}`,URL 必带 version_code / aid 等
//! 平台参数;**version / version_code / kernel 运行时从本机真实客户端探测**
//! ([`crate::doubao_client`])—— 2026-10-24 起服务端对活动接口按客户端形状收紧,
//! 钉死的版本参数一腐烂就会被拒(2026-11-14 实测,复盘见
//! docs/report/doubao-checkin-710010702-2026-11-14.md)。
//! web_id / tea_uuid 按 Cookie 里的 sessionid 确定性派生(每号一台稳定「设备」;
//! CreditDaddy 按 uid 派生 —— cookie 里的 sessionid 是同一账号的稳定标识,等价且
//! 不需要额外接口)。Golden 向量钉住派生与查询串(见测试)。
//!
//! **自动签到已移除**(2026-11-14,随 v0.3.44):签到写路径被服务端整体拒绝
//! (业务码 710010702,连官方客户端都领不动),直连路线短期无解,渠道只留取数。
//! 判断矩阵的 Retry/AuthNeeded 分级、「回查是唯一成功证据」等纪律来自签到实战,
//! 删除前留此存照(完整记录在同目录 report 里)。
//!
//! 凭据:网页会话住在 WebView2 的 profile 里(与 WorkBuddy 同一条裁决,不落盘),
//! 登录窗捕获后取数时现读;手动粘贴的 Cookie 走凭据管理器兜底。都不进日志。

use crate::providers::{FetchResult, Kind};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// 网页会话的主域名(登录窗、Cookie 罐都对着它)。
pub const HOST: &str = "www.doubao.com";
pub const DETAIL_PATH: &str = "/alice/ug/campaign/v3/detail/";
/// 任务面板所属活动组(pc 端激励任务 v3)。
pub const CAMPAIGN_GROUP: &str = "pc_incentive_task_mvp_v3";
/// detail 的请求体。
pub const DETAIL_BODY: &str = "{\"campaign_group_code\":\"pc_incentive_task_mvp_v3\"}";

/// 桌面版 webview 的 UA 家族(**兜底占位值**:2026-10-19 CDP 抓包原样)。
/// 静态表(providers)引用它当占位;实际发送前取数路径会用 [`app_ua`] 运行时替换
/// —— 真实版本来自本机客户端探测([`crate::doubao_client`]),这个钉值只在
/// 不装客户端的机器上发出去,可能被服务端拒绝(见模块头注)。
pub const APP_UA: &str = crate::doubao_client::FALLBACK_UA;
pub const REFERER: &str = "https://www.doubao.com/pc/agent-explore";
pub const ORIGIN: &str = "https://www.doubao.com";

/// 会话失效的业务码(实测:匿名 / 过期 cookie 返 710012001「登录已过期」)。
const LOGIN_INVALID: f64 = 710012001.0;

/// detail 的完整地址。
pub fn detail_url() -> String {
    format!("https://{HOST}{DETAIL_PATH}")
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

/// 顶层业务码。detail / checkin 都是 `code` 字段 + HTTP 200 的信封,HTTP 码不作数。
fn biz_code(body: &Value) -> Option<f64> {
    body.get("code").and_then(fnum)
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

/// 体验额度读数(detail 的 `data.progress.total_amount`)。
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
    // 额度读数:progress.total_amount。缺失 = 结构变了,如实报错,不用 0 顶替
    let Some(total) = body.get("data").and_then(|d| d.get("progress")).and_then(|p| p.get("total_amount")).and_then(fnum) else {
        r.error = Some("响应里没有额度汇总字段(data.progress.total_amount)".into());
        return r;
    };
    r.valid = true;
    r.remaining = Some(total);
    push_signin_extra(&mut r, body);
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
        let url = decorate_url_with(&captured_fp(), detail_url().as_str(), "");
        let (base, q) = url.split_once('?').unwrap();
        assert_eq!(base, "https://www.doubao.com/alice/ug/campaign/v3/detail/");
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
        let url = decorate_url_with(&fresh, detail_url().as_str(), "");
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
