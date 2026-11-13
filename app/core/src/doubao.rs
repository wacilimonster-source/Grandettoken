//! 豆包(doubao.com)网页会话渠道:体验额度读数 + 每日签到。
//!
//! 口径来自 CreditDaddy 的 doubaoClient.js(issue #32,2026-10-19/20 用调试端口
//! CDP 实测,已签 + 未签两个场景):
//!
//! ```text
//! POST https://www.doubao.com/alice/ug/campaign/v3/detail/      任务面板(一次给全)
//!   body {"campaign_group_code":"pc_incentive_task_mvp_v3"}
//!   → data.progress.total_amount = 体验额度余量
//!   → data.tasks[] 里 task_kind === "daily_signin" 那项 = 签到状态
//! POST https://www.doubao.com/alice/ug/campaign/landing/checkin  领取(+200/天)
//!   body {"scene":"pc_incentive_task_mvp_v3"}
//!   → code 0 + data.idempotent:true = 重复领取(服务端幂等,不重复发放)
//! ```
//!
//! 鉴权:**纯 doubao.com 网页会话 Cookie**,没有 a_bogus / msToken 请求签名,
//! device_id 空可过 —— 但**客户端环境要像样**(issue #32 实测:裸 UA 打活动接口
//! 吃「请求非法」)。指纹 = 豆包桌面版 webview 的请求形状:
//! UA 用 `DoubaoDesktop/2.32.6 Chrome/147`,URL 必带 version_code / aid 等平台参数,
//! web_id / tea_uuid 按 Cookie 里的 sessionid 确定性派生(每号一台稳定「设备」;
//! CreditDaddy 按 uid 派生 —— cookie 里的 sessionid 是同一账号的稳定标识,等价且
//! 不需要额外接口)。Golden 向量钉住派生与查询串(见测试)。
//!
//! 参与门槛:仅个人账号(飞书企业号被排除,detail 里没有签到任务,如实报 Inactive)。
//! 活动页 UI 有「仅支持豆包电脑版」前端墙,但接口不吃这套 —— cookie 直打即通。
//!
//! 凭据:网页会话住在 WebView2 的 profile 里(与 WorkBuddy 同一条裁决,不落盘),
//! 登录窗捕获后取数时现读;手动粘贴的 Cookie 走凭据管理器兜底。都不进日志。

use crate::checkin::Outcome;
use crate::providers::{FetchResult, Kind};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// 网页会话的主域名(登录窗、Cookie 罐都对着它)。
pub const HOST: &str = "www.doubao.com";
pub const DETAIL_PATH: &str = "/alice/ug/campaign/v3/detail/";
pub const CHECKIN_PATH: &str = "/alice/ug/campaign/landing/checkin";
/// 任务面板所属活动组(pc 端激励任务 v3;签到是其中的 daily_signin 任务)。
pub const CAMPAIGN_GROUP: &str = "pc_incentive_task_mvp_v3";
/// detail 的请求体(取数与签到状态共用)。
pub const DETAIL_BODY: &str = "{\"campaign_group_code\":\"pc_incentive_task_mvp_v3\"}";
/// claim 的请求体。
pub const CLAIM_BODY: &str = "{\"scene\":\"pc_incentive_task_mvp_v3\"}";

/// 豆包桌面版 webview 的 UA(CDP 抓包原样)。活动接口按它放行客户端。
pub const APP_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) DoubaoDesktop/2.32.6 Chrome/147.0.7727.149 Safari/537.36";
pub const REFERER: &str = "https://www.doubao.com/pc/agent-explore";
pub const ORIGIN: &str = "https://www.doubao.com";

/// 会话失效的业务码(实测:匿名 / 过期 cookie 返 710012001「登录已过期」)。
const LOGIN_INVALID: f64 = 710012001.0;

/// detail 的完整地址(签到/取数共用;取数侧由 providers 表给,签到侧由 call_with_body 拼)。
pub fn detail_url() -> String {
    format!("https://{HOST}{DETAIL_PATH}")
}

/// claim 的完整地址。
pub fn checkin_url() -> String {
    format!("https://{HOST}{CHECKIN_PATH}")
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
/// 参数顺序与参考实现逐字节一致(URLSearchParams 插入序),Golden 钉死。
pub fn decorate_url(url: &str, cookie: &str) -> String {
    let base = url.split('?').next().unwrap_or(url);
    let seed = seed_of(cookie);
    let (web_id, tea_uuid) = device_ids(&seed);
    let pairs: [(&str, &str); 18] = [
        ("version_code", "20800"),
        ("language", "zh"),
        ("device_platform", "web"),
        ("doubao_device_platform", "web"),
        ("aid", "497858"),
        ("real_aid", "497858"),
        ("pkg_type", "release_version"),
        ("pc_version", "2.32.6"),
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

// ---------------------------------------------------------------- 签到解析

/// detail 响应里的签到任务(tasks[].task_kind === "daily_signin")。
fn checkin_task<'a>(body: &'a Value) -> Option<&'a Value> {
    body.get("data")
        .and_then(|d| d.get("tasks"))
        .and_then(Value::as_array)?
        .iter()
        .find(|t| t.get("task_kind").and_then(Value::as_str) == Some("daily_signin"))
}

/// 签到状态机(detail = 查状态,checkin = 领取)。
/// 判因只看 code / 字段结构,不看 message 文案(全仓红线)。
pub fn parse_doubao(body: &Value, claiming: bool) -> Outcome {
    match biz_code(body) {
        Some(c) if c == LOGIN_INVALID => return Outcome::AuthNeeded,
        Some(c) if c != 0.0 => return Outcome::Retry(format!("biz:{c}")),
        None => return Outcome::BadShape("响应缺 code 字段".into()),
        _ => {}
    }
    if claiming {
        let Some(data) = body.get("data").filter(|d| d.is_object()) else {
            return Outcome::BadShape("claim 响应缺 data".into());
        };
        // 服务端幂等:重复领取 code 0 + idempotent:true —— 归 already,不算错误
        if data.get("idempotent").and_then(Value::as_bool) == Some(true) {
            return Outcome::Already(data.get("reward_amount").and_then(fnum));
        }
        return match data.get("reward_amount").and_then(fnum) {
            Some(amount) if amount > 0.0 => Outcome::Claimed(Some(amount)),
            Some(_) => Outcome::BadShape("claim 成功但 reward_amount 为 0".into()),
            None => Outcome::BadShape("claim 响应缺 reward_amount".into()),
        };
    }
    // status 分支:detail 里挖签到任务
    let Some(task) = checkin_task(body) else {
        // 活动下线 / 飞书企业号被排除:detail 里没有签到任务 —— 不是失败,是没得签
        return Outcome::Inactive("当前账号无签到任务(活动未开放或飞书企业账号不支持)".into());
    };
    let c = task.get("daily_checkin");
    let checked = c.and_then(|x| x.get("checked_in_today")).and_then(Value::as_bool);
    match checked {
        Some(true) => Outcome::Already(c.and_then(|x| x.get("reward")).and_then(|rw| rw.get("reward_amount")).and_then(fnum)),
        Some(false) => Outcome::Retry("ready".into()),
        // 任务在,却没有 checked_in_today 字段 —— 口径未知,不猜
        _ => Outcome::BadShape("签到任务缺 checked_in_today".into()),
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkin::Outcome;

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

    #[test]
    fn decorate_url_matches_reference_query() {
        let url = decorate_url(detail_url().as_str(), "");
        let (base, q) = url.split_once('?').unwrap();
        assert_eq!(base, "https://www.doubao.com/alice/ug/campaign/v3/detail/");
        assert_eq!(
            q,
            "version_code=20800&language=zh&device_platform=web&doubao_device_platform=web&aid=497858&real_aid=497858&pkg_type=release_version&pc_version=2.32.6&doubao_pc_version=5.72.66&samantha_web=1&web_platform=desktop&use-olympus-account=1&web_tab_id=0&web_id=7639479694&tea_uuid=2888214177777777&device_id=&region=&sys_region="
        );
    }

    #[test]
    fn decorate_url_strips_existing_query() {
        let u = decorate_url("https://www.doubao.com/x?old=1", "");
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

    fn detail_body(checked: bool) -> Value {
        serde_json::json!({
            "code": 0,
            "data": {"tasks": [
                {"task_kind": "other", "daily_checkin": {"checked_in_today": true}},
                {"task_kind": "daily_signin", "daily_checkin": {
                    "checked_in_today": checked,
                    "continuous_days": 3,
                    "reward": {"reward_amount": 200.0}
                }}
            ]}
        })
    }

    #[test]
    fn signin_status_states() {
        assert_eq!(parse_doubao(&detail_body(false), false), Outcome::Retry("ready".into()));
        assert_eq!(parse_doubao(&detail_body(true), false), Outcome::Already(Some(200.0)));
    }

    #[test]
    fn signin_no_task_is_inactive() {
        let body = serde_json::json!({"code": 0, "data": {"tasks": [{"task_kind": "other"}]}});
        assert!(matches!(parse_doubao(&body, false), Outcome::Inactive(_)));
        let body = serde_json::json!({"code": 0, "data": {}});
        assert!(matches!(parse_doubao(&body, false), Outcome::Inactive(_)));
    }

    #[test]
    fn signin_claim_results() {
        let ok = serde_json::json!({"code": 0, "data": {"reward_amount": 200.0, "continuous_days": 4}});
        assert_eq!(parse_doubao(&ok, true), Outcome::Claimed(Some(200.0)));
        let idem = serde_json::json!({"code": 0, "data": {"idempotent": true, "reward_amount": 200.0}});
        assert_eq!(parse_doubao(&idem, true), Outcome::Already(Some(200.0)));
        let no_amount = serde_json::json!({"code": 0, "data": {}});
        assert!(matches!(parse_doubao(&no_amount, true), Outcome::BadShape(_)));
    }

    #[test]
    fn signin_auth_and_biz_codes() {
        assert_eq!(parse_doubao(&serde_json::json!({"code": 710012001}), false), Outcome::AuthNeeded);
        assert_eq!(parse_doubao(&serde_json::json!({"code": 710012001}), true), Outcome::AuthNeeded);
        assert_eq!(parse_doubao(&serde_json::json!({"code": 42}), true), Outcome::Retry("biz:42".into()));
        assert!(matches!(parse_doubao(&serde_json::json!({}), false), Outcome::BadShape(_)));
    }
}
