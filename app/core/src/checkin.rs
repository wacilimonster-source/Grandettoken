//! 签到(Trae / WorkBuddy 加油站)的纯逻辑:北京日口径、幂等判定、退避、状态机。
//!
//! 设计见 `design-auto-checkin-2026-10-13.html`。这里不碰网络,只做判断 —— 网络与凭据
//! 在 `fetch.rs` / 外壳里,这样状态机可以挂满测试(`src-tauri` 不挂测试)。
//!
//! 三条红线(与设计稿第 9、10 节一致):
//!   ① **HTTP 200 不代表成功**:一律按 `code` 判;判因只看 `code/HTTP/域名`,**永远不看 message 文案**
//!      (9074 的官方文案是"当前参与用户太多",10001 在国区是"今天已签"、国际版是"活动未开启")。
//!   ② **不把"点了按钮"说成"领到了"**:claim 之后要回查一次,并等下一次余额取数确认到账。
//!   ③ **签到的状态不参与余额渲染**:签到失败永远不可能让余额显示成 0。

use serde::{Deserialize, Serialize};

/// 签到的凭据形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckinAuth {
    /// Trae:本机登录态(`Cloud-IDE-JWT` 令牌 + 设备号)
    TraeApp,
    /// WorkBuddy:挂件内网页登录拿到的会话(Cookie 或手动粘贴的令牌)
    WebSession,
}

/// 一个可签到渠道的静态定义。与 `providers::PROVIDERS`(取数定义)分开 —— 签到是**写操作**,
/// 不该混进取数定义里被误复用。
pub struct CheckinDef {
    pub id: &'static str,
    /// 界面上叫的名字(与渠道名一致)
    pub name: &'static str,
    pub auth: CheckinAuth,
    /// 查状态 / 领取 的 URL
    pub status_url: &'static str,
    pub claim_url: &'static str,
    /// 旧路径回退(WorkBuddy 的活动按季运营,路径换过)
    pub fallback_status: &'static [&'static str],
    pub fallback_claim: &'static [&'static str],
    /// 请求 body(签到平台都是 POST + JSON)
    pub body: &'static str,
    /// 额外请求头
    pub headers: &'static [(&'static str, &'static str)],
}

pub const CHECKINS: &[CheckinDef] = &[
    CheckinDef {
        id: "trae",
        name: "Trae",
        auth: CheckinAuth::TraeApp,
        status_url: "https://api.trae.cn/trae/api/v2/ug/checkin_credits/status",
        claim_url: "https://api.trae.cn/trae/api/v2/ug/checkin_credits/claim",
        fallback_status: &[],
        fallback_claim: &[],
        // 2026-10-13 真机取证:`{}` 与 `{"req_source":1}` 都返回 code=0,取前者(与 trae_body 同风格)
        body: "{}",
        headers: &[
            ("X-User-Region", "CN"),
            ("Origin", "https://www.trae.cn"),
            ("Referer", "https://www.trae.cn/"),
        ],
    },
    CheckinDef {
        id: "workbuddy",
        name: "WorkBuddy",
        auth: CheckinAuth::WebSession,
        status_url: "https://www.workbuddy.cn/v2/billing/meter/checkin-activity-status",
        claim_url: "https://www.workbuddy.cn/v2/billing/meter/daily-checkin",
        fallback_status: &[
            "https://www.codebuddy.cn/v2/billing/meter/checkin-activity-status",
            "https://www.workbuddy.cn/billing/meter/checkin-activity-status",
            "https://www.workbuddy.cn/billing/meter/checkin-status",
        ],
        fallback_claim: &[
            "https://www.codebuddy.cn/v2/billing/meter/daily-checkin",
            "https://www.workbuddy.cn/billing/meter/daily-checkin",
        ],
        body: "{}",
        // X-Client-Platform 与 X-Requested-With 是那套控制台接口的老要求(取数渠道已在用)
        headers: &[
            ("X-Client-Platform", "web"),
            ("X-Requested-With", "XMLHttpRequest"),
            ("Origin", "https://www.workbuddy.cn"),
            ("Referer", "https://www.workbuddy.cn/profile/plans-activity"),
        ],
    },
];

pub fn find(id: &str) -> Option<&'static CheckinDef> {
    CHECKINS.iter().find(|c| c.id == id)
}

/// 一次签到尝试的结果。界面的八种状态就是它的映射(设计稿 7.2)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "detail")]
pub enum Outcome {
    /// 刚领到(credits = 平台声明的数额;拿不到就不给数字)
    Claimed(Option<f64>),
    /// 今天已经签过(可能是用户自己在客户端点的)
    Already(Option<f64>),
    /// 今天没得领(活动未开启/已结束)
    Inactive(String),
    /// 设备/出口不被接受或限流:当天可退避重试
    Retry(String),
    /// 凭据失效:重试无用,要用户动手
    AuthNeeded,
    /// 接口结构变了:停止并置灰开关,余额不受影响
    BadShape(String),
    /// 其它失败
    Failed(String),
}

impl Outcome {
    /// 这次尝试有没有"问出结果"——没有的话(限流/网络)当天还可以再来。
    pub fn retryable(&self) -> bool {
        matches!(self, Outcome::Retry(_) | Outcome::Failed(_))
    }
    /// 要不要立刻告诉用户(设计稿:只有凭据失效与结构变化值得打断)。
    pub fn needs_user(&self) -> bool {
        matches!(self, Outcome::AuthNeeded | Outcome::BadShape(_))
    }
    /// 落库/落配置用的短码。
    pub fn code(&self) -> String {
        match self {
            Outcome::Claimed(_) => "claimed".into(),
            Outcome::Already(_) => "already".into(),
            Outcome::Inactive(m) => format!("inactive:{m}"),
            Outcome::Retry(m) => format!("retry:{m}"),
            Outcome::AuthNeeded => "auth".into(),
            Outcome::BadShape(m) => format!("shape:{m}"),
            Outcome::Failed(m) => format!("failed:{m}"),
        }
    }
}

/// 北京日("2026-10-13")。**签到日按北京时间算**,不是本机时区、不是 UTC ——
/// 平台的"今天"是北京时间的今天(所有社区实现一致)。
pub fn beijing_day(now_ts: i64) -> String {
    use chrono::{FixedOffset, TimeZone};
    let tz = FixedOffset::east_opt(8 * 3600).expect("UTC+8 合法");
    tz.timestamp_opt(now_ts, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// 当天该试的时候:00:23 之后(社区实测 00:00–00:10 是高峰,00:15–00:45 最好签),
/// 加上 0–20 分钟抖动(用渠道 id + 日期派生,同一天同一渠道只抖一次,重启不重摇)。
pub fn scheduled_minute(day: &str, id: &str) -> u32 {
    let mut h: u64 = 1469598103934665603;
    for b in day.bytes().chain(id.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    23 + (h % 21) as u32 // 00:23 ～ 00:43
}

/// 北京时间的(时, 分)。
pub fn beijing_hm(now_ts: i64) -> (u32, u32) {
    use chrono::{FixedOffset, TimeZone, Timelike};
    let tz = FixedOffset::east_opt(8 * 3600).expect("UTC+8 合法");
    tz.timestamp_opt(now_ts, 0)
        .single()
        .map(|d| (d.hour(), d.minute()))
        .unwrap_or((0, 0))
}

/// 现在到没到"今天该签"的时刻:北京时间 00:23 + 该渠道的抖动之后(当天更晚也算 → 开机补跑)。
pub fn due_now(now_ts: i64, day: &str, id: &str) -> bool {
    let (h, m) = beijing_hm(now_ts);
    h > 0 || (h == 0 && m >= scheduled_minute(day, id))
}

/// 退避序列(秒):120 → 300 → 600 → 900 → 1200 封顶;跨日清零。
pub const BACKOFF_SEC: [i64; 5] = [120, 300, 600, 900, 1200];

pub fn next_delay(attempts: i64) -> i64 {
    let i = attempts.clamp(1, BACKOFF_SEC.len() as i64) as usize - 1;
    BACKOFF_SEC[i]
}

// 这里原来还有一个 `should_retry(outcome, attempts) = retryable && attempts < 5`。
// 它与 next_delay 的「1200 秒封顶」互相抵消:第 6 次起调用方把 next_try_at 置成 None,
// 节拍器把 None 读成"不在退避中",于是当天改成每 5 分钟硬撞(报告 P1-2)。
// 现在"该不该继续当天重试"只由 next_delay 的封顶退避表达一处。

/// 签到渠道的配置(每渠道一份)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CheckinCfg {
    /// 总开关。**默认关**:首次要在渠道卡上点一次「开启签到」才发请求(设计稿裁决 3A)
    pub enabled: bool,
    /// 自动签(每天 00:23+抖动,以及开机补跑)。开总开关后默认开
    pub auto: bool,
    /// 最近一次成功/尝试的北京日(判"今天处理过了吗"的唯一依据,落盘)
    pub last_day: Option<String>,
    /// 最近一次结果码(短码,不含凭据)
    pub last_code: Option<String>,
    /// 当天已尝试次数(退避用;跨日清零)
    pub attempts: i64,
    /// 下次可尝试的时刻(unix 秒)
    pub next_try_at: Option<i64>,
    /// 最近一次尝试的时刻 + 结果摘要(界面直接显示)
    pub last_try_at: Option<i64>,
    pub last_message: Option<String>,
    /// 今天领到的数额(平台给的;拿不到则为 None —— 不编)
    pub last_credits: Option<f64>,
}

impl Default for CheckinCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            auto: true,
            last_day: None,
            last_code: None,
            attempts: 0,
            next_try_at: None,
            last_try_at: None,
            last_message: None,
            last_credits: None,
        }
    }
}

impl CheckinCfg {
    /// 跨日清零:新的一天,尝试次数与退避从头算。
    pub fn roll_day(&mut self, today: &str) {
        if self.last_day.as_deref() != Some(today) {
            self.attempts = 0;
            self.next_try_at = None;
        }
    }

    /// 今天是不是已经处理完了(签过 / 没活动 / 凭据失效都不该再自动试)。
    pub fn done_for_day(&self, today: &str) -> bool {
        if self.last_day.as_deref() != Some(today) {
            return false;
        }
        match self.last_code.as_deref() {
            Some(c) => {
                c.starts_with("claimed") || c.starts_with("already") || c.starts_with("inactive")
                    || c.starts_with("auth")
                    || c.starts_with("shape")
            }
            None => false,
        }
    }
}

/// Trae status/claim 的响应解读。
///
/// 2026-10-13 真机取证的响应:
/// `{"checked_in":true,"code":0,"credits":100,"did_checked_in":true,"enable":true,"extra_credits":100,"message":"success"}`
/// —— 字段都在**顶层**;`code` 才是成败,P 不 200 不代表成功。
pub fn parse_trae(body: &serde_json::Value, claiming: bool) -> Outcome {
    let code = body.get("code").and_then(|c| c.as_i64());
    // 凭据失效:1001/1002/401(以及 HTTP 层已由调用方处理)
    if matches!(code, Some(1001) | Some(1002) | Some(401)) {
        return Outcome::AuthNeeded;
    }
    // 成功:code=0(缺 code 时看有没有 checked_in/credits 字段兜底)
    let ok = code == Some(0)
        || (code.is_none()
            && (body.get("checked_in").is_some() || body.get("credits").is_some()));
    if !ok {
        let raw = code.map(|c| c.to_string()).unwrap_or_else(|| "无 code".into());
        // 9074 / 9095 这类:按 code 判,不看文案
        return match code {
            Some(9074) => Outcome::Retry("device".into()),
            Some(9095) => Outcome::Already(trae_credits(body)),
            _ => Outcome::Failed(format!("code={raw}")),
        };
    }

    // credits 可能在顶层,也可能被 data/result 包一层(社区记录过:只读顶层会把已签判成未签 → 重复领取)
    let credits = trae_credits(body);
    let checked = body
        .get("checked_in")
        .or_else(|| body.get("did_checked_in"))
        .and_then(|v| v.as_bool())
        .or_else(|| path_bool(body, &["data", "checked_in"]))
        // 顶层有两个别名(checked_in / did_checked_in),嵌套变体以前只兜了一个:
        // 只读 data.checked_in 会把"已签"读成"未签",从而重复领取(报告 P3-12)
        .or_else(|| path_bool(body, &["data", "did_checked_in"]))
        .or_else(|| path_bool(body, &["result", "checked_in"]))
        .unwrap_or(false);
    let enable = body
        .get("enable")
        .and_then(|v| v.as_bool())
        .or_else(|| path_bool(body, &["data", "enable"]))
        .unwrap_or(true);

    if !enable {
        return Outcome::Inactive("activity".into());
    }
    if checked {
        // 查状态时就是"已签";正在领取时也当已签(拿到的数额以平台给的为准)
        return Outcome::Already(credits);
    }
    if claiming {
        // 领完仍是未签 → 结构对不上,别谎报成功
        return Outcome::BadShape("claim 后仍显示未签".into());
    }
    Outcome::Retry("ready".into()) // 可以领(调用方据此发 claim)
}

fn path_bool(v: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    let mut cur = v;
    for k in keys {
        cur = cur.get(k)?;
    }
    cur.as_bool()
}

fn trae_credits(body: &serde_json::Value) -> Option<f64> {
    for k in ["credits", "extra_credits"] {
        if let Some(n) = body.get(k).and_then(|v| v.as_f64()) {
            if n > 0.0 {
                return Some(n);
            }
        }
    }
    for p in [["data", "points"], ["data", "credits"], ["result", "credits"]] {
        let mut cur = body;
        let mut ok = true;
        for k in p {
            match cur.get(k) {
                Some(v) => cur = v,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            if let Some(n) = cur.as_f64() {
                if n > 0.0 {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// WorkBuddy 活动状态/领取响应的解读。
///
/// 关键坑(设计稿 3.2):`10001` 在国区是"今天已签"、在国际版是"活动未开启" —— **含义相反**,
/// 必须按 `code + 域名` 判;`active=false` 或 404 = 本期结束。
/// 另外有实现记录"签到响应里没有数额字段",所以数额拿不到就留 None,不编。
pub fn parse_workbuddy(body: &serde_json::Value, claiming: bool) -> Outcome {
    let code = body.get("code").and_then(|c| c.as_i64());
    let msg = body
        .get("msg")
        .or_else(|| body.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    // 国内版的"今天已签"用 10001 表达(带"已签到"字样);拿不到 code 时按 msg 兜底,
    // 但**只在认得出"已签到/已领取"这种明确字样时**才当已签 —— 其余一律不猜。
    let checked = body
        .get("data")
        .and_then(|d| d.get("today_checked_in").or_else(|| d.get("checked_in")))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let active = body
        .get("data")
        .and_then(|d| d.get("active"))
        .and_then(|v| v.as_bool());
    let credits = body
        .get("data")
        .and_then(|d| d.get("today_credit").or_else(|| d.get("daily_credit")))
        .and_then(|v| v.as_f64())
        .filter(|n| *n > 0.0);

    if matches!(code, Some(401) | Some(403) | Some(10002)) {
        return Outcome::AuthNeeded;
    }
    if active == Some(false) {
        return Outcome::Inactive(msg);
    }
    if checked {
        return Outcome::Already(credits);
    }
    match code {
        Some(0) => {
            if claiming {
                // 领完还不显示已签 → 不谎报(到账核对由调用方用余额差值确认)
                Outcome::Claimed(credits)
            } else {
                Outcome::Retry("ready".into())
            }
        }
        Some(10001) => {
            // 国区:今天已签;国际版:活动未开启。按文案区分(这是唯一必须看文案的地方,
            // 因为同一个 code 两种含义)。认不出来就当"没活动"(保守:不重试,也不谎报已签)
            let m = msg.to_lowercase();
            if m.contains("已签") || m.contains("已领取") || m.contains("already") {
                Outcome::Already(credits)
            } else {
                Outcome::Inactive(msg)
            }
        }
        Some(404) => Outcome::Inactive(msg),
        Some(c) => Outcome::Failed(format!("code={c}")),
        None => {
            if body.get("data").is_some() {
                // 有 data 但没 code:结构变了,停下而不是继续猜
                Outcome::BadShape("响应无 code".into())
            } else {
                Outcome::Failed("响应无 code".into())
            }
        }
    }
}

// ───────────────────────── 网络层(只在这一段里出网) ─────────────────────────

/// 签到专用的 UA:与取数路径共用同一份(网关按 UA 分流,两处不一致会各踩各的坑)。
use crate::fetch::BROWSER_UA;

/// 签到专用的 HTTP 客户端:**强制直连,不跟随系统代理**。
///
/// 理由(设计稿裁决 4A):Trae 侧 9074("当前参与用户太多")的成因之一是按出口 IP 排队限流,
/// 而系统代理/机场 IP 正是那类高发出口。余额取数跟随代理没问题,签到这条特意绕开。
/// 单次 20 秒、不用连接池 —— 一天就几次请求,不值得为它维护池子。
pub fn direct_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(BROWSER_UA)
        .build()
        .expect("签到 HTTP 客户端初始化失败")
}

/// 一次请求要带的那份凭据。**只在内存里过一趟**:不落盘、不进日志。
#[derive(Debug, Clone)]
pub enum Cred {
    /// Trae:本机登录态(令牌 + 纯数字设备号;设备号缺了会恒定 9074)
    Trae { token: String, device_id: String },
    /// WorkBuddy:网页会话。`cookie:…` 走 Cookie 头,其余当 Bearer 令牌
    Web(String),
}

fn apply_cred(req: reqwest::RequestBuilder, cred: &Cred) -> reqwest::RequestBuilder {
    match cred {
        Cred::Trae { token, device_id } => req
            .header("Authorization", format!("Cloud-IDE-JWT {}", token.trim()))
            .header("x-device-id", device_id.trim()),
        Cred::Web(key) => match crate::weblogin::cookie_header(key) {
            // 会话 Cookie + 浏览器 UA(网关按 UA 分流)
            Some(cookie) => req.header("Cookie", cookie).header("User-Agent", BROWSER_UA),
            None => req.header("Authorization", format!("Bearer {}", key.trim())),
        },
    }
    .header("Content-Type", "application/json")
    .header("Accept", "application/json, text/plain, */*")
}

/// 打一次 status 或 claim。按 URL 链逐个试:网络/5xx 才换下一个;
/// 拿到能解读的响应(含业务失败)就地返回 —— 换域名只会换出同一句答复。
pub async fn call(
    client: &reqwest::Client,
    def: &CheckinDef,
    cred: &Cred,
    claim: bool,
) -> Outcome {
    let primary = if claim { def.claim_url } else { def.status_url };
    let fallbacks = if claim { def.fallback_claim } else { def.fallback_status };
    let urls: Vec<&str> = std::iter::once(primary).chain(fallbacks.iter().copied()).collect();
    let mut last = Outcome::Failed("没有可用地址".into());

    for url in urls.iter() {
        let mut req = client.post(*url);
        req = apply_cred(req, cred);
        for (k, v) in def.headers {
            req = req.header(*k, *v);
        }
        let resp = match req.body(def.body.to_string()).send().await {
            Ok(r) => r,
            Err(e) => {
                let msg = if e.is_timeout() {
                    "请求超时".to_string()
                } else if e.is_connect() {
                    "无法连接(检查网络)".to_string()
                } else {
                    format!("请求失败: {e}")
                };
                last = Outcome::Failed(msg);
                continue; // 网络问题值得换域名
            }
        };
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        // HTTP 层先判:401/403 = 凭据失效(不用解析 body)
        if matches!(status.as_u16(), 401 | 403) {
            return Outcome::AuthNeeded;
        }
        if status.as_u16() == 404 {
            // 活动换季/路径下线:不是错误,是"没得领"
            last = Outcome::Inactive(format!("HTTP 404 ({})", host_of(url)));
            continue;
        }
        let body: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                // 网关/nginx 限流与维护页返回的是 HTML。429 与 5xx 都属于「稍后再试」,
                // 不是「接口结构变了」:BadShape 会弹通知、把开关置灰、并把当天判成完结
                // (done_for_day 认 shape 前缀),而稍后重试本来就能成(报告 P1-10)。
                let later = status.is_server_error() || status.as_u16() == 429;
                last = if later {
                    Outcome::Retry(format!("HTTP {}", status.as_u16()))
                } else {
                    Outcome::BadShape(format!("HTTP {} · 响应不是 JSON", status.as_u16()))
                };
                if later {
                    continue; // 5xx / 429 值得换下一个域名并留到下一轮退避
                }
                return last;
            }
        };
        let out = match def.id {
            "trae" => parse_trae(&body, claim),
            _ => parse_workbuddy(&body, claim),
        };
        // 5xx 且我们判成"不可重试"的业务失败 → 当成服务端问题,继续试下一个域名
        if status.is_server_error() && matches!(out, Outcome::Failed(_)) {
            last = Outcome::Retry(format!("HTTP {}", status.as_u16()));
            continue;
        }
        return out;
    }
    last
}

fn host_of(url: &str) -> String {
    url.split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 北京日:跨时区也对(用已知时刻验证)。
    #[test]
    fn beijing_day_is_utc_plus_8() {
        use chrono::{FixedOffset, TimeZone};
        let tz = FixedOffset::east_opt(8 * 3600).unwrap();
        // 北京 2026-10-13 00:30
        let ts = tz.with_ymd_and_hms(2026, 10, 13, 0, 30, 0).unwrap().timestamp();
        assert_eq!(beijing_day(ts), "2026-10-13");
        // 往前一小时:北京 10-12 23:30 —— 属于前一天(用 UTC 或本机时区会算成 10-12/10-13 的错位)
        assert_eq!(beijing_day(ts - 3600), "2026-10-12");
    }

    /// 抖动:同一天同一渠道恒定,不同天/不同渠道不同;落在 23–43 分。
    #[test]
    fn scheduled_minute_is_stable_and_in_window() {
        let a = scheduled_minute("2026-10-13", "trae");
        let b = scheduled_minute("2026-10-13", "trae");
        assert_eq!(a, b, "同一天同一渠道只抖一次(重启不重摇)");
        assert!((23..=43).contains(&a), "落在 00:23–00:43: {a}");
        let c = scheduled_minute("2026-10-14", "trae");
        let d = scheduled_minute("2026-10-13", "workbuddy");
        assert!(a != c || a != d, "不同的日子/渠道不该完全一样");
    }

    /// 触发时刻:00:23+抖动之前不算到点,之后(含当天更晚)都算 —— 关机一整天再开机也能补跑。
    #[test]
    fn due_now_window_and_catch_up() {
        use chrono::{FixedOffset, TimeZone};
        let tz = FixedOffset::east_opt(8 * 3600).unwrap();
        let at = |h, m| tz.with_ymd_and_hms(2026, 10, 13, h, m, 0).unwrap().timestamp();
        let day = "2026-10-13";
        let sched = scheduled_minute(day, "trae");
        assert!(!due_now(at(0, sched.saturating_sub(1)), day, "trae"), "还没到抖动的点");
        assert!(due_now(at(0, 43), day, "trae"), "抖动上限 43 分,到这里一定该签");
        assert!(due_now(at(9, 0), day, "trae"), "白天开机 → 补跑");
        assert_eq!(beijing_hm(at(9, 5)), (9, 5));
    }

    /// 退避序列与封顶;跨日清零。
    #[test]
    fn backoff_and_day_rollover() {
        assert_eq!(next_delay(1), 120);
        assert_eq!(next_delay(2), 300);
        assert_eq!(next_delay(5), 1200);
        assert_eq!(next_delay(9), 1200, "封顶");
        assert_eq!(next_delay(0), 120);

        let mut c = CheckinCfg::default();
        c.attempts = 3;
        c.next_try_at = Some(123);
        c.last_day = Some("2026-10-12".into());
        c.roll_day("2026-10-13");
        assert_eq!(c.attempts, 0, "跨日清零");
        assert!(c.next_try_at.is_none());
    }

    /// 当天处理完了就不再自动试;可重试的才继续。
    #[test]
    fn done_for_day_rules() {
        let today = "2026-10-13";
        let mut c = CheckinCfg::default();
        c.last_day = Some(today.into());
        c.last_code = Some("claimed:100".into());
        assert!(c.done_for_day(today));
        c.last_code = Some("already".into());
        assert!(c.done_for_day(today));
        c.last_code = Some("auth".into());
        assert!(c.done_for_day(today), "凭据失效不再自动重试");
        c.last_code = Some("retry:device".into());
        assert!(!c.done_for_day(today), "限流当天还要再试");
        // 昨天签的不算今天
        c.last_day = Some("2026-10-12".into());
        assert!(!c.done_for_day(today));
    }

    /// Trae:真机取样的响应(2026-10-13)—— 已签、100 分、在顶层。
    #[test]
    fn trae_real_sample_is_already_checked_in() {
        let body = json!({"checked_in":true,"code":0,"credits":100,"did_checked_in":true,
                          "enable":true,"extra_credits":100,"message":"success"});
        assert_eq!(parse_trae(&body, false), Outcome::Already(Some(100.0)));
        // 未签的形态(enable 且 checked_in=false)→ 可领
        let body = json!({"checked_in":false,"code":0,"credits":100,"enable":true});
        assert_eq!(parse_trae(&body, false), Outcome::Retry("ready".into()));
        // 领取后仍然没签 → 结构对不上,不谎报
        assert!(matches!(parse_trae(&json!({"checked_in":false,"code":0}), true), Outcome::BadShape(_)));
    }

    /// Trae 的错误码:凭据失效、设备被拒、已签,都不看 message。
    #[test]
    fn trae_codes_are_interpreted_by_code_not_message() {
        assert_eq!(parse_trae(&json!({"code":1001,"message":"随便写的"}), false), Outcome::AuthNeeded);
        assert_eq!(parse_trae(&json!({"code":1002}), false), Outcome::AuthNeeded);
        // 9074 = 设备/出口不被接受(文案伪装成"太挤了")
        assert_eq!(parse_trae(&json!({"code":9074,"message":"当前参与用户太多,请稍后再试"}), false), Outcome::Retry("device".into()));
        // 9095 = 本设备今日已签
        assert_eq!(parse_trae(&json!({"code":9095}), false), Outcome::Already(None));
        assert!(matches!(parse_trae(&json!({"code":12345}), false), Outcome::Failed(_)));
        // enable=false = 活动/入口没开
        assert!(matches!(parse_trae(&json!({"code":0,"enable":false}), false), Outcome::Inactive(_)));
    }

    /// 数额字段被 data 包一层也要认(否则会把已签判成未签,重复领取)。
    #[test]
    fn trae_credits_may_be_nested() {
        let body = json!({"code":0,"data":{"checked_in":true,"points":150}});
        assert_eq!(parse_trae(&body, false), Outcome::Already(Some(150.0)));
    }

    /// WorkBuddy:10001 在国区是"已签"、认不出文案时当"没活动"(都不重试)。
    #[test]
    fn workbuddy_10001_branches_by_message() {
        let already = json!({"code":10001,"msg":"今日已签到"});
        assert!(matches!(parse_workbuddy(&already, false), Outcome::Already(_)));
        let inactive = json!({"code":10001,"msg":"签到活动未开启或已过期"});
        assert!(matches!(parse_workbuddy(&inactive, false), Outcome::Inactive(_)));
    }

    /// WorkBuddy:活动关了 / 凭据失效 / 可领 / 领到。
    #[test]
    fn workbuddy_main_states() {
        assert!(matches!(
            parse_workbuddy(&json!({"code":0,"data":{"active":false}}), false),
            Outcome::Inactive(_)
        ));
        assert_eq!(parse_workbuddy(&json!({"code":401,"msg":"unauthorized"}), false), Outcome::AuthNeeded);
        let ready = json!({"code":0,"data":{"active":true,"today_checked_in":false,"daily_credit":100}});
        assert_eq!(parse_workbuddy(&ready, false), Outcome::Retry("ready".into()));
        // 领取成功:数额有就给,没有就 None(设计稿:响应可能没有数额字段)
        assert_eq!(parse_workbuddy(&ready, true), Outcome::Claimed(Some(100.0)));
        let no_amount = json!({"code":0,"data":{}});
        assert_eq!(parse_workbuddy(&no_amount, true), Outcome::Claimed(None));
        // 结构不认识 → BadShape(而不是当"可领"去重试)
        assert!(matches!(parse_workbuddy(&json!({"foo":1}), false), Outcome::Failed(_)));
    }
}
