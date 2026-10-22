//! 签到(Trae / WorkBuddy 加油站)的纯逻辑:北京日口径、幂等判定、退避、状态机。
//!
//! 设计见 `docs/design/design-auto-checkin-2026-10-13.html`。这里不碰网络,只做判断 —— 网络与凭据
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
    /// Qoder:本机桌面端的 `auth.v1.dat`(Electron safeStorage 信封)。
    /// 与 Trae 同为"读本机登录态",但解密链完全不同,所以单独一档。
    QoderApp,
}

/// 签到请求的 HTTP 方法。
///
/// Trae / WorkBuddy 的查状态与领取**都是 POST**(平台那套接口就是这么设计的);
/// **Qoder 是 GET**(查活动列表)+ POST(领取)—— 2026-10-20 真机踩到:
/// 用 POST 打 Qoder 的 `/sash/api/v1/me/campaigns` 直接 404,
/// 而 GET 是 200。所以方法必须逐渠道声明,不能写死。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckinMethod {
    Get,
    Post,
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
    /// 领取地址。**含 `{id}` 占位符时,由 status 响应里的活动 id 替换** ——
    /// Qoder 的领取路径是 `/campaigns/{campaignId}/claim`,id 只能从查状态那一步拿到,
    /// 写不进静态定义。Trae / WorkBuddy 的领取地址是固定的,不含占位符。
    pub claim_url: &'static str,
    /// 旧路径回退(WorkBuddy 的活动按季运营,路径换过)
    pub fallback_status: &'static [&'static str],
    pub fallback_claim: &'static [&'static str],
    /// 查状态 / 领取 的请求方式(逐渠道声明,见 [`CheckinMethod`])
    pub status_method: CheckinMethod,
    pub claim_method: CheckinMethod,
    /// 请求 body(只有 POST 才发;GET 一律不发 body)
    pub body: &'static str,
    /// 额外请求头
    pub headers: &'static [(&'static str, &'static str)],
    /// 平台的「今天」从几点算起(北京时间的小时)。
    /// **0 = 自然日 00:00 跨日**;Qoder 是 **10** —— 官方活动描述原文
    /// 「每日 10:00（UTC+8）刷新」,实测 `startAt`/`endAt` 正好是
    /// 10:00:00 → 次日 09:59:00。
    ///
    /// 为什么必须逐渠道配:若对 Qoder 沿用 00:23 去签,那时活动还没跨日,
    /// 查到的是**昨天**的状态,「昨天已领」会被误判成「今天已领」→ 当天静默丢分。
    pub day_roll_hour: u32,
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
        status_method: CheckinMethod::Post,
        claim_method: CheckinMethod::Post,
        body: "{}",
        headers: &[
            ("X-User-Region", "CN"),
            ("Origin", "https://www.trae.cn"),
            ("Referer", "https://www.trae.cn/"),
        ],
        day_roll_hour: 0,
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
        status_method: CheckinMethod::Post,
        claim_method: CheckinMethod::Post,
        body: "{}",
        // X-Client-Platform 与 X-Requested-With 是那套控制台接口的老要求(取数渠道已在用)
        headers: &[
            ("X-Client-Platform", "web"),
            ("X-Requested-With", "XMLHttpRequest"),
            ("Origin", "https://www.workbuddy.cn"),
            ("Referer", "https://www.workbuddy.cn/profile/plans-activity"),
        ],
        day_roll_hour: 0,
    },
    // ───────────── Qoder(国际版 / 国内版)─────────────
    //
    // 2026-10-20 真机取证(本机桌面端 0.4.8,region=global):
    //   ① `GET /sash/api/v1/me/campaigns` → 200,活动带 actionType/claimStatus/benefit
    //   ② `POST /sash/api/v1/me/campaigns/{id}/claim` → 200,重复领回 `replayed:true`(幂等)
    //   ③ 活动描述原文「每日 10:00（UTC+8）刷新,领取后 30 天有效」
    //   ④ 基础头为 Authorization + Cosy-ClientType + User-Agent;国际版另需 Qoder 自带 UMID helper
    //      返回的 Cosy-MachineToken / Cosy-MachineType / Cosy-MachineCode,否则活动可能被静默过滤
    //
    // 两版是**两套账号**(官方明确不支持跨区迁移),所以是两个独立渠道,不合并。
    CheckinDef {
        id: "qoder",
        name: "Qoder",
        auth: CheckinAuth::QoderApp,
        status_url: "https://openapi.qoder.sh/sash/api/v1/me/campaigns",
        // {id} 由 status 响应里的活动 id 替换(见 claim_url 的字段注释)
        claim_url: "https://openapi.qoder.sh/sash/api/v1/me/campaigns/{id}/claim",
        fallback_status: &[],
        fallback_claim: &[],
        // 查活动列表是 **GET**(POST 会 404 —— 真机踩到过);领取是 POST
        status_method: CheckinMethod::Get,
        claim_method: CheckinMethod::Post,
        body: "{}",
        headers: &[
            ("Cosy-ClientType", "10"),
            ("User-Agent", "Qoder"),
        ],
        day_roll_hour: 10,
    },
    CheckinDef {
        id: "qoder-cn",
        name: "Qoder 国内版",
        auth: CheckinAuth::QoderApp,
        status_url: "https://openapi.qoder.com.cn/sash/api/v1/me/campaigns",
        claim_url: "https://openapi.qoder.com.cn/sash/api/v1/me/campaigns/{id}/claim",
        fallback_status: &[],
        fallback_claim: &[],
        status_method: CheckinMethod::Get,
        claim_method: CheckinMethod::Post,
        body: "{}",
        headers: &[
            ("Cosy-ClientType", "10"),
            ("User-Agent", "Qoder"),
        ],
        day_roll_hour: 10,
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
    /// **有活动,但只能在客户端里领**:Qoder 的 `VIEW_DETAILS` 类活动
    /// (claimStatus = CLAIMABLE,actionType 不是 CLAIM_BENEFIT)——
    /// 平台把领取放在活动页里(客户端打开 `growth-page/activity-iframe`),
    /// 没有给"直接领"的语义。这条要如实说出来,不能并进 Inactive:
    /// 并进去就是"本期无打卡活动",而用户明明在客户端看得到一个可领的活动
    /// (2026-10-22 实证:1024 活动 claimable=true,旧口径报"无活动")。
    Manual(String),
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
            Outcome::Manual(m) => format!("manual:{m}"),
            Outcome::Retry(m) => format!("retry:{m}"),
            Outcome::AuthNeeded => "auth".into(),
            Outcome::BadShape(m) => format!("shape:{m}"),
            Outcome::Failed(m) => format!("failed:{m}"),
        }
    }
    /// 结论的**类别**,不含任何文案。
    ///
    /// 为什么单独有它:`code()` 里嵌着平台给的 message —— 那是第三方原样返回的
    /// 任意文本(项目自己的纪律就是"判因只看 code,不看 message 文案"),
    /// **绝不能进日志**(日志是要贴给 support 看的)。
    /// 要落结论时用这个,只留类别;平台的业务码另记。
    pub fn verdict(&self) -> &'static str {
        match self {
            Outcome::Claimed(_) => "claimed",
            Outcome::Already(_) => "already",
            Outcome::Inactive(_) => "inactive",
            Outcome::Manual(_) => "manual",
            Outcome::Retry(_) => "retry",
            Outcome::AuthNeeded => "auth",
            Outcome::BadShape(_) => "shape",
            Outcome::Failed(_) => "failed",
        }
    }
    /// 平台声明的数额(已领 / 刚领都算)。界面据此显示 "+100 分"。
    pub fn credits(&self) -> Option<f64> {
        match self {
            Outcome::Claimed(c) | Outcome::Already(c) => *c,
            _ => None,
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

/// **签到周期日**:平台的"今天"从 `roll_hour` 点起算。
///
/// 自然日 00:00 跨日的渠道(Trae / WorkBuddy,`roll_hour = 0`)就是 [`beijing_day`];
/// Qoder 的 `roll_hour = 10` —— 北京时间 10:00 之前仍属于**前一天的活动周期**。
///
/// 为什么幂等键必须用它:Qoder 的活动窗口是 10:00 → 次日 09:59(实测 startAt/endAt)。
/// 若拿自然日当键,10-21 09:00 查到的"已领"其实是 10-20 那一期领的,
/// 会被判成"今天已领"而跳过 10:00 之后该领的那一期 → 当天静默丢 100 分。
pub fn cycle_day(now_ts: i64, roll_hour: u32) -> String {
    if roll_hour == 0 {
        return beijing_day(now_ts);
    }
    // 把时间轴往前挪 roll_hour 小时,再取自然日 —— 等价于"以 roll_hour 点为日界"
    beijing_day(now_ts - (roll_hour as i64) * 3600)
}

/// 当天该试的时候:跨日时刻之后(社区实测 00:00–00:10 是高峰,00:15–00:45 最好签),
/// 加上 0–20 分钟抖动(用渠道 id + 日期派生,同一天同一渠道只抖一次,重启不重摇)。
///
/// `roll_hour = 0` 的渠道沿用老口径(00:23–00:43);非 0 的渠道从 `roll_hour:00` 起抖
/// (Qoder 是 10:00–10:20)。
pub fn scheduled_minute(day: &str, id: &str, roll_hour: u32) -> u32 {
    let mut h: u64 = 1469598103934665603;
    for b in day.bytes().chain(id.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    let jitter = (h % 21) as u32; // 0 ～ 20 分钟
    if roll_hour == 0 {
        23 + jitter // 00:23 ～ 00:43
    } else {
        roll_hour * 60 + jitter // 10:00 ～ 10:20
    }
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

/// 北京时间的「当天已过分钟数」(0 ～ 1439)。
///
/// 用分钟而不是(时,分)来比:`due_now` 要判的是"现在到没到 `roll_hour:jitter` 这一刻",
/// 拆成两个数去比会写出 `h > roll || (h == roll && m >= jitter)` 这种容易漏掉
/// "已经过了跨日点、现在是下午"的分支。整数比较一次说完。
pub fn beijing_minute_of_day(now_ts: i64) -> u32 {
    let (h, m) = beijing_hm(now_ts);
    h * 60 + m
}

/// 现在到没到"本周期该签"的时刻:从 `roll_hour:00` + 该渠道的抖动之后开始
/// (当天更晚也算 → 开机补跑)。
///
/// `roll_hour = 0` 的渠道等价于老口径(00:23+抖动之后)。
pub fn due_now(now_ts: i64, day: &str, id: &str, roll_hour: u32) -> bool {
    beijing_minute_of_day(now_ts) >= scheduled_minute(day, id, roll_hour)
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
    /// 连续签到天数(只有 Qoder 的接口给 `streak_days`;拿不到就是 None,不编)
    pub last_streak_days: Option<i64>,
    /// 产生 `auth` / `inactive` / `manual` 结论时运行的版本。用来判断那条结论是不是**陈旧**的。
    ///
    /// 为什么需要它(v0.1.47 真机踩到):v0.1.47 修好了「WorkBuddy 签到只读网页 Cookie、
    /// 取数却用桌面端凭据」这件事,但升级后当天界面仍显示"凭据失效"——
    /// 因为 `auth` 让 `done_for_day` 判当天完结,而那条 `auth` 是**升级前**记下的
    /// (本机 lastTryAt=00:37,升级是 22:48),凭据链路早就变了却没人重新判。
    ///
    /// v0.1.57 起也覆盖 `inactive` / `manual`:它们是**我们的分类器**给出的结论
    /// ("哪些活动算打卡"),规则一改就得重问 —— 与凭据判决是同一类问题。
    ///
    /// `None` = 这条记录**早于本字段存在** → 一律视为陈旧:产生它的代码已经不是现在这份了。
    pub last_auth_version: Option<String>,
    /// 最近一次查询 Qoder 活动列表的时刻。签到开关开着时,即使自动领取关闭也会监测活动。
    pub last_activity_check_at: Option<i64>,
    /// 最近看到的 Qoder 活动快照(最多保留 100 条,用于识别新活动和状态变化)。
    pub activity_history: Vec<QoderActivity>,
    /// 最近一次活动列表中的优先活动,供渠道详情和首页徽标展示。
    pub latest_activity: Option<QoderActivity>,
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
            last_streak_days: None,
            last_auth_version: None,
            last_activity_check_at: None,
            activity_history: Vec::new(),
            latest_activity: None,
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
                    || c.starts_with("manual")
                    || c.starts_with("auth")
                    || c.starts_with("shape")
            }
            None => false,
        }
    }

    /// 今天这个渠道**没有待办了**(已签 / 平台说已签 / 本期无活动)。
    ///
    /// 与 [`done_for_day`](Self::done_for_day) 的区别:那个还含 `auth` / `shape`
    /// —— 它们的意思是"今天别再试了",**不是**"签完了"。首页徽标要的是正面结论,
    /// 所以两者不能混用。
    ///
    /// `inactive`(本期无打卡活动)算"没有待办":不算进去的话,聚合行会永远说
    /// "还有 N 个没签",用户会一直等一个不会到来的签到。
    ///
    /// `manual`(有活动,但只能在客户端领)同理:挂件做不了这件事,天天提醒
    /// "还有 1 个没签"只会变成一个关不掉的噪音 —— 它照样有徽标(「有活动」),
    /// 只是不进"待签"计数。
    pub fn signed_today(&self, today: &str) -> bool {
        if self.last_day.as_deref() != Some(today) {
            return false;
        }
        match self.last_code.as_deref() {
            Some(c) => {
                c.starts_with("claimed")
                    || c.starts_with("already")
                    || c.starts_with("inactive")
                    || c.starts_with("manual")
            }
            None => false,
        }
    }

    /// 这条 `auth` 是不是陈旧的(记录它的版本与当前版本不同)。
    ///
    /// 只对 `auth` 有意义:`claimed` / `already` 是**平台给的既成事实**(真领到了),
    /// 换版本也不会变成没领;而 `auth` 是**对某一套凭据的判决**,凭据链路一改就该重判。
    pub fn auth_is_stale(&self, version: &str) -> bool {
        match self.last_code.as_deref() {
            Some(c) if c.starts_with("auth") => {
                self.last_auth_version.as_deref() != Some(version)
            }
            _ => false,
        }
    }

    /// 今天是不是**真的**不用再试了。
    ///
    /// 与 [`done_for_day`](Self::done_for_day) 只差一处:`auth` 只在**同一版本内**才算完结。
    /// 换了版本说明凭据链路可能已经变了(比如 v0.1.47 把 WorkBuddy 签到改成用桌面端凭据),
    /// 这时候还拿旧版本的 `auth` 挡住当天,用户就会看到一条早已不成立的"凭据失效"。
    ///
    /// 不担心变成无限重试:`auth` 不可重试(`next_try_at` 为 None),且下次真判出 `auth`
    /// 时会带上当前版本,同一版本内照样当天完结 —— 只有"换版本"这一次会多给一枪。
    pub fn blocks_today(&self, today: &str, version: &str) -> bool {
        if !self.done_for_day(today) {
            return false;
        }
        // 换了版本:凭据判决(auth)与**我们自己的分类器结论**(inactive / manual)
        // 都要重问一次。后者是"哪些活动算打卡"这条规则给的,规则一改,旧结论就可能
        // 是假阴性 —— v0.1.57 就撞上了:Qoder 把当日活动换成 VIEW_DETAILS + CLAIMABLE,
        // 旧版本记下"本期无打卡活动",升级后当天若不重判,用户看到的还是"软件里没有"。
        if self.auth_is_stale(version) || self.inactive_is_stale(version) {
            return false;
        }
        true
    }

    /// 这条 `inactive` / `manual` 是不是旧版本判的。
    ///
    /// 与 [`auth_is_stale`](Self::auth_is_stale) 同一个理由,只是对象换成了
    /// "活动分类"而不是"凭据"。`claimed` / `already` 不看版本 —— 那是平台事实。
    pub fn inactive_is_stale(&self, version: &str) -> bool {
        match self.last_code.as_deref() {
            Some(c) if c.starts_with("inactive") || c.starts_with("manual") => {
                self.last_auth_version.as_deref() != Some(version)
            }
            _ => false,
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

// ───────────────────────── Qoder ─────────────────────────
//
// 2026-10-20 真机取证(桌面端 0.4.8,region=global):
//   查:`{"uid":…,"showCampaign":true,"claimable":false,
//        "campaigns":[{ "campaignId":…, "campaignKey":"act-20261020-910",
//                       "actionType":"CLAIM_BENEFIT"|"VIEW_DETAILS",
//                       "startAt":…, "endAt":…,
//                       "claimStatus":"CLAIMABLE"|"CLAIMED",
//                       "benefit":{"kind":"CREDITS","amount":100,
//                                  "validity":{"mode":"RELATIVE_DAYS","days":30}} }]}`
//   领:`{"grantId":…,"status":"CLAIMED","replayed":true|false,
//        "benefit":{"kind":"CREDITS","amount":100}, "campaignId":…, "expiresAt":…}`

/// Qoder 的活动类型。只有 `CLAIM_BENEFIT` 是"可以领的活动";
/// `VIEW_DETAILS` 是纯展示(本机实测就有这么一条),**不参与领取**。
///
/// 2026-10-20 真机取证的原始响应逐字为 `"actionType":"CLAIM_BENEFIT"` ——
/// 注意是完整的 `BENEFIT`(带 E)。**不要**照抄某些社区实现/混淆 bundle 里
/// 拼错的 `CLAIM_BENEFIT`:那样 `has_campaign` 恒为 false,会把"今天可领"
/// 静默判成"本期无打卡活动",用户当天直接丢 100 分。
const QODER_CLAIM_ACTION: &str = "CLAIM_BENEFIT";

/// 上游活动类型的**别名表**(用于兼容可能的历史拼写)。
///
/// 主名是 [`QODER_CLAIM_ACTION`];这里额外认那个少一个 E 的写法 ——
/// 万一上游某天真的发出来,不至于把"可领"读成"没活动"。
/// 只做**识别**用,不参与任何写入。
const QODER_CLAIM_ACTION_ALIASES: &[&str] = &["CLAIM_BEFIT"];

/// Qoder 活动的一份轻量快照,只存用于展示与去重的公开活动信息。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct QoderActivity {
    /// campaignId;响应未提供时退到 campaignKey 或标题作为稳定去重键。
    pub id: String,
    pub title: String,
    pub action_type: String,
    pub claim_status: String,
    pub observed_at: Option<i64>,
}

/// 从活动响应解析所有活动,不是只挑可签到项。
///
/// 返回 `None` 代表响应结构不认识;空 `Vec` 则是合法的空活动列表。
pub fn qoder_activities(body: &serde_json::Value) -> Option<Vec<QoderActivity>> {
    let list = body.get("campaigns")?.as_array()?;
    let mut activities: Vec<QoderActivity> = Vec::with_capacity(list.len());
    for c in list {
        let title = qoder_campaign_title(c);
        let id = c
            .get("campaignId")
            .and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
            })
            .or_else(|| {
                c.get("campaignKey")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| title.clone());
        let action_type = c
            .get("actionType")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let claim_status = c
            .get("claimStatus")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let activity = QoderActivity {
            id,
            title,
            action_type,
            claim_status,
            observed_at: None,
        };
        if let Some(index) = activities.iter().position(|a| a.id == activity.id) {
            activities[index] = activity;
        } else {
            activities.push(activity);
        }
    }
    Some(activities)
}

/// 这个 actionType 算不算"可领的活动"。
fn is_claim_action(action: &str) -> bool {
    action == QODER_CLAIM_ACTION || QODER_CLAIM_ACTION_ALIASES.contains(&action)
}

pub fn qoder_action_claimable(action: &str, status: &str) -> bool {
    is_claim_action(action) && status == "CLAIMABLE"
}

/// 从查状态响应里挑出"这一期该领的活动"。
///
/// 判定顺序很重要:
///   ① `claimable == true` 且存在 `CLAIMABLE` 的 CLAIM_BENEFIT 活动 → 可以领
///   ② 存在 `CLAIMED` 的 CLAIM_BENEFIT 活动 → 今天已经领过
///   ③ 只有 `VIEW_DETAILS` / 压根没有 CLAIM_BENEFIT → **本期没有打卡活动**(Inactive)
///
/// ②与③必须分开:把③也报成"今天已签"是**谎报**(用户会以为领到了),
/// 而报成失败又会天天报警。设计稿 §3.2 把这两条单列,理由在此。
pub struct QoderPick {
    /// 可领活动的 id(拿去拼 claim URL);没有可领的就是 None
    pub claim_id: Option<String>,
    /// 该活动的数额(平台声明;拿不到就不给数字)
    pub amount: Option<f64>,
    /// 有没有"今天已领"的证据
    pub already: bool,
    /// 有没有任何 CLAIM_BENEFIT 活动(有=活动在;没有=换季了)
    pub has_campaign: bool,
    /// 有**可领但只能手动领**的活动时,带上它的标题(用于界面直说"有活动:X")。
    ///
    /// 2026-10-22 实证:Qoder 把当日活动换成 `VIEW_DETAILS` + `CLAIMABLE`
    /// (「1024 | Community Boost」),顶层 `claimable` 已经是 true,
    /// 而旧口径只认 CLAIM_BENEFIT → 报"本期无打卡活动",用户看到的是"软件里没有"。
    pub manual: Option<String>,
}

/// 活动标题:优先中文,退英文,再退 campaignKey。
///
/// 只在 `placements[].content.{zh,en}.title` 里取 —— 那是客户端弹窗上
/// 显示的那句话,拿它来告诉用户"是哪个活动"最不容易对错号。
fn qoder_campaign_title(c: &serde_json::Value) -> String {
    let mut en = String::new();
    if let Some(ps) = c.get("placements").and_then(|p| p.as_array()) {
        for p in ps {
            let content = p.get("content");
            if let Some(t) = content
                .and_then(|x| x.get("zh"))
                .and_then(|x| x.get("title"))
                .and_then(|x| x.as_str())
            {
                let t = t.trim();
                if !t.is_empty() {
                    return t.to_string();
                }
            }
            if en.is_empty() {
                if let Some(t) = content
                    .and_then(|x| x.get("en"))
                    .and_then(|x| x.get("title"))
                    .and_then(|x| x.as_str())
                {
                    let t = t.trim();
                    if !t.is_empty() {
                        en = t.to_string();
                    }
                }
            }
        }
    }
    if en.is_empty() {
        c.get("campaignKey")
            .and_then(|k| k.as_str())
            .unwrap_or("有活动")
            .to_string()
    } else {
        en
    }
}

fn qoder_benefit_amount(c: &serde_json::Value) -> Option<f64> {
    c.get("benefit")
        .and_then(|b| b.get("amount"))
        .and_then(|a| a.as_f64())
        .filter(|n| n.is_finite() && *n > 0.0)
}

pub fn qoder_pick(body: &serde_json::Value) -> QoderPick {
    let mut pick = QoderPick {
        claim_id: None,
        amount: None,
        already: false,
        has_campaign: false,
        manual: None,
    };
    let Some(list) = body.get("campaigns").and_then(|c| c.as_array()) else {
        return pick;
    };
    for c in list {
        let action = c.get("actionType").and_then(|a| a.as_str()).unwrap_or("");
        let status = c.get("claimStatus").and_then(|s| s.as_str()).unwrap_or("");
        if !is_claim_action(action) {
            // VIEW_DETAILS 之类不参与领取,但"可领"这件事要记下来 ——
            // 它是"有活动,只能在客户端点",不是"没活动"。
            if status == "CLAIMABLE" && pick.manual.is_none() {
                pick.manual = Some(qoder_campaign_title(c));
            }
            continue;
        }
        pick.has_campaign = true;
        match status {
            "CLAIMABLE" => {
                if pick.claim_id.is_none() {
                    pick.claim_id = c
                        .get("campaignId")
                        .and_then(|i| i.as_str())
                        .map(String::from);
                    pick.amount = qoder_benefit_amount(c);
                }
            }
            "CLAIMED" => {
                pick.already = true;
                // 已领的那条也把数额带上(界面显示"今日已领 +100")
                if pick.amount.is_none() {
                    pick.amount = qoder_benefit_amount(c);
                }
            }
            _ => {}
        }
    }
    pick
}

/// Qoder 查状态 / 领取响应的解读。
///
/// **判因只看 code / HTTP / 字段结构,不看 message 文案**(三条红线之一)。
/// 文案在 Qoder 这边尤其不可信:活动描述会随季节改,`msg` 也可能是英文。
pub fn parse_qoder(body: &serde_json::Value, claiming: bool) -> Outcome {
    // ① 业务错误码:上游用 `code` 字符串(UNAUTHORIZED / TOKEN_INVALID / TOKEN_EXPIRE)
    if let Some(code) = body.get("code").and_then(|c| c.as_str()) {
        let c = code.to_ascii_uppercase();
        if c.contains("UNAUTHORIZED") || c.contains("TOKEN_INVALID") || c.contains("TOKEN_EXPIRE") {
            return Outcome::AuthNeeded;
        }
        // 其他字符串 code 一律当失败,并把 code 原样带出来(排查时它才是能查的东西)
        return Outcome::Failed(format!("code={code}"));
    }

    if claiming {
        // 领取响应:`status` 是终态;`replayed:true` = 这次之前就已经领过了(幂等回执)
        let status = body.get("status").and_then(|s| s.as_str()).unwrap_or("");
        let amount = body
            .get("benefit")
            .and_then(|b| b.get("amount"))
            .and_then(|a| a.as_f64())
            .filter(|n| n.is_finite() && *n > 0.0);
        let replayed = body.get("replayed").and_then(|r| r.as_bool()).unwrap_or(false);
        return match status {
            "CLAIMED" => {
                if replayed {
                    Outcome::Already(amount)
                } else {
                    Outcome::Claimed(amount)
                }
            }
            // 平台明确说"已经被领过" —— 幂等,不是错误
            "ALREADY_CLAIMED" => Outcome::Already(amount),
            // 仍在处理中:没问出结果,当天还可以再来看一次
            "PENDING" => Outcome::Retry("pending".into()),
            "REVOKED" => Outcome::Inactive("活动已撤销".into()),
            // 结构里没有 status = 接口变了,别谎报成功
            "" => Outcome::BadShape("claim 响应里没有 status".into()),
            other => {
                // 上游把失败原因放在 failureCode(实测字段名)
                let fc = body
                    .get("failureCode")
                    .and_then(|f| f.as_str())
                    .unwrap_or(other);
                match fc {
                    "REDEMPTION_CODE_OUT_OF_STOCK" => Outcome::Inactive("奖励已发完".into()),
                    "ACHIEVEMENT_NOT_COMPLETED" => Outcome::Inactive("任务未完成".into()),
                    "CAMPAIGN_NOT_ACTIVE" => Outcome::Inactive("活动未开启".into()),
                    // 风控:重试无用,要用户自己去看(不是凭据问题,别指向重新登录)
                    "RISK_BLOCKED" | "RISK_DEPENDENCY_UNAVAILABLE" => {
                        Outcome::Inactive(format!("平台风控拦截({fc})"))
                    }
                    // 认不出来的码 → Failed,并把 **failureCode** 带出来
                    // (它才是能拿去查的东西;status 通常只是 "FAILED",没信息量)
                    _ => Outcome::Failed(format!("failureCode={fc}")),
                }
            }
        };
    }

    // ② 查状态:必须能拿到 campaigns 数组,否则是结构变了
    if body.get("campaigns").and_then(|c| c.as_array()).is_none() {
        return Outcome::BadShape("响应里没有 campaigns".into());
    }
    let pick = qoder_pick(body);
    if let Some(id) = pick.claim_id {
        // 可领:把活动 id 与数额带回调用方(它据此拼 claim URL)
        return Outcome::Retry(format!("ready:{id}"));
    }
    if pick.already {
        return Outcome::Already(pick.amount);
    }
    if !pick.has_campaign {
        // 只有 VIEW_DETAILS 或空列表:先看有没有"可领但只能手动领"的活动
        if let Some(title) = pick.manual {
            return Outcome::Manual(title);
        }
        // 否则才是真的本期没有打卡活动(不是失败,也不是已签)
        return Outcome::Inactive("本期无打卡活动".into());
    }
    // 有活动但状态既不是 CLAIMABLE 也不是 CLAIMED(如未来状态)→ 结构变了
    Outcome::BadShape("活动状态不认识".into())
}

/// 从 `retry:ready:{id}` 里取出活动 id。取不到返回 None。
pub fn ready_claim_id(code: &str) -> Option<&str> {
    code.strip_prefix("ready:")?.split(':').next()
}

/// Qoder 查状态响应里的连续签到天数(`streak_days`)。
///
/// **只有 Qoder 有这个字段** —— Trae/WorkBuddy 的接口不给,所以它在
/// `CheckinCfg` 里是可选的:拿不到就是 None,界面不显示,绝不编一个数。
/// 放在 `campaigns` 之外的顶层(本机实测样本)。
pub fn qoder_streak(body: &serde_json::Value) -> Option<i64> {
    body.get("streak_days")
        .or_else(|| body.get("streakDays"))
        .and_then(|v| v.as_i64())
        .filter(|n| *n >= 0)
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
#[derive(Clone)]
pub enum Cred {
    /// Trae:本机登录态(令牌 + 纯数字设备号;设备号缺了会恒定 9074)
    Trae { token: String, device_id: String },
    /// WorkBuddy:网页会话。`cookie:…` 走 Cookie 头,其余当 Bearer 令牌。
    ///
    /// **也承载桌面端凭据**:`workbuddy::desktop_tokens()` 给的是裸 JWT(不带
    /// `desktop:` 前缀 —— 那是取数层的标记),走到这里正好落进"其余当 Bearer"这一支。
    /// 2026-10-20 实测:该令牌打签到接口 200,且换哪种 UA 都行。
    Web(String),
    /// Qoder:本机桌面端 `auth.v1.dat` 解出的访问令牌,一律走 `Authorization: Bearer`。
    Qoder(String),
    /// Qoder 国际版:在 Bearer 之外带上 Qoder 自己生成的设备身份头。
    QoderMachine {
        token: String,
        identity: crate::qoder::QoderMachineIdentity,
    },
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
        Cred::Qoder(token) => req.header("Authorization", format!("Bearer {}", token.trim())),
        Cred::QoderMachine { token, identity } => {
            let mut req = req
                .header("Authorization", format!("Bearer {}", token.trim()))
                .header("Cosy-MachineToken", identity.machine_token.trim())
                .header("Cosy-MachineType", identity.machine_type.trim())
                .header("Cosy-MachineCode", identity.machine_code.trim());
            if let Some(version) = identity.client_version.as_deref() {
                req = req.header("Cosy-Version", version);
            }
            if let Some(machine_os) = identity.machine_os.as_deref() {
                req = req.header("Cosy-MachineOS", machine_os);
            }
            if let Some(hostname) = identity.machine_hostname.as_deref() {
                req = req.header("Cosy-MachineHostname", hostname);
            }
            if let Some(machine_id) = identity.machine_id.as_deref() {
                req = req.header("Cosy-MachineId", machine_id);
            }
            req
        }
    }
    .header("Content-Type", "application/json")
    .header("Accept", "application/json, text/plain, */*")
}

/// 与 [`call`] 同一条路,但**额外把原始 body 带回来**。
///
/// 为什么需要:Qoder 的连续签到天数(`streak_days`)在**顶层**,而 `Outcome` 只承载
/// 判定结果、不承载原始响应。与其为了一个展示字段再打一次接口(多一跳网络、
/// 多一次被限流的机会),不如让第一次调用就把 body 捎回来。
///
/// `call()` 保持原样转发到这里 —— 它只是丢掉了 body。
pub async fn call_with_body(
    client: &reqwest::Client,
    def: &CheckinDef,
    cred: &Cred,
    claim: bool,
    claim_id: Option<&str>,
) -> (Outcome, Option<serde_json::Value>) {
    let primary = if claim { def.claim_url } else { def.status_url };
    let fallbacks = if claim { def.fallback_claim } else { def.fallback_status };
    let urls: Vec<&str> = std::iter::once(primary).chain(fallbacks.iter().copied()).collect();
    let mut last = Outcome::Failed("没有可用地址".into());

    let subst: Vec<String> = urls
        .iter()
        .map(|u| {
            if u.contains("{id}") {
                match claim_id {
                    Some(id) => u.replace("{id}", id),
                    None => String::new(),
                }
            } else {
                (*u).to_string()
            }
        })
        .collect();
    if subst.iter().all(|u| u.is_empty()) {
        return (
            Outcome::Failed("领取地址需要活动 id,但调用方没给".into()),
            None,
        );
    }

    for url in subst.iter().filter(|u| !u.is_empty()) {
        // 方法逐渠道声明(见 CheckinMethod):Qoder 的查状态是 GET,POST 会 404
        let method = if claim { def.claim_method } else { def.status_method };
        let mut req = match method {
            CheckinMethod::Get => client.get(url),
            CheckinMethod::Post => client.post(url),
        };
        req = apply_cred(req, cred);
        for (k, v) in def.headers {
            req = req.header(*k, *v);
        }
        // GET **不发 body**(带了反而可能被网关当成非法请求)
        if method == CheckinMethod::Post {
            req = req.body(def.body.to_string());
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                let msg = if e.is_timeout() {
                    "请求超时".to_string()
                } else if e.is_connect() {
                    "无法连接(检查网络)".to_string()
                } else {
                    format!("请求失败: {e}")
                };
                let code = if e.is_timeout() {
                    crate::errcodes::TIMEOUT
                } else {
                    crate::errcodes::CONNECT
                };
                crate::diaglog::wire(
                    "checkin",
                    Some(def.id),
                    "http",
                    Some(code),
                    &[
                        ("url", url),
                        ("host", &host_of(url)),
                        ("method", if method == CheckinMethod::Get { "GET" } else { "POST" }),
                        ("action", if claim { "claim" } else { "status" }),
                    ],
                );
                last = Outcome::Failed(msg);
                continue;
            }
        };
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if matches!(status.as_u16(), 401 | 403) {
            // L2:凭据被拒 —— 用户只会说"没领到",日志要能指出是哪一环。
            crate::diaglog::wire(
                "checkin",
                Some(def.id),
                "http",
                Some(crate::errcodes::CRED_EXPIRED),
                &[
                    ("url", url),
                    ("host", &host_of(url)),
                    ("status", &status.as_u16().to_string()),
                    ("action", if claim { "claim" } else { "status" }),
                ],
            );
            return (Outcome::AuthNeeded, None);
        }
        if status.as_u16() == 404 {
            crate::diaglog::wire(
                "checkin",
                Some(def.id),
                "http",
                Some(crate::errcodes::HTTP_404),
                &[
                    ("url", url),
                    ("host", &host_of(url)),
                    ("action", if claim { "claim" } else { "status" }),
                ],
            );
            last = Outcome::Inactive(format!("HTTP 404 ({})", host_of(url)));
            continue;
        }
        let body: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                let later = status.is_server_error() || status.as_u16() == 429;
                last = if later {
                    Outcome::Retry(format!("HTTP {}", status.as_u16()))
                } else {
                    Outcome::BadShape(format!("HTTP {} · 响应不是 JSON", status.as_u16()))
                };
                crate::diaglog::wire(
                    "checkin",
                    Some(def.id),
                    "http",
                    Some(if later {
                        crate::errcodes::HTTP_5XX
                    } else {
                        crate::errcodes::BODY_NOT_JSON
                    }),
                    &[
                        ("url", url),
                        ("status", &status.as_u16().to_string()),
                        ("bytes", &text.len().to_string()),
                        ("action", if claim { "claim" } else { "status" }),
                    ],
                );
                if later {
                    continue;
                }
                return (last, None);
            }
        };
        let out = match def.id {
            "trae" => parse_trae(&body, claim),
            "qoder" | "qoder-cn" => parse_qoder(&body, claim),
            _ => parse_workbuddy(&body, claim),
        };
        // L1:平台给的**业务码**(不看 message 文案 —— 那条红线就是"判因只看 code")。
        // 平台不返 code 时就空着,绝不编。码也限长:它是第三方给的字符串。
        let biz = body
            .get("code")
            .map(|c| match c {
                serde_json::Value::String(s) => s.chars().take(24).collect::<String>(),
                other => other.to_string(),
            })
            .unwrap_or_default();
        crate::diaglog::write_keyed(
            "checkin",
            &format!("{}:resp", def.id),
            &format!(
                "签到响应 · 动作 {} · HTTP {} · 平台码 {} · 结论 {}",
                if claim { "claim" } else { "status" },
                status.as_u16(),
                if biz.is_empty() { "(没给)" } else { &biz },
                // ⚠ 只记**类别**,不记 `out.code()` —— 后者嵌着平台给的 message,
                // 那是第三方原样返回的任意文本,不能进要贴给 support 的日志。
                out.verdict()
            ),
        );
        if status.is_server_error() && matches!(out, Outcome::Failed(_)) {
            last = Outcome::Retry(format!("HTTP {}", status.as_u16()));
            continue;
        }
        return (out, Some(body));
    }
    (last, None)
}

/// 打一次 status 或 claim。按 URL 链逐个试:网络/5xx 才换下一个;
/// 拿到能解读的响应(含业务失败)就地返回 —— 换域名只会换出同一句答复。
///
/// `claim_id`:领取地址里 `{id}` 占位符的取值(只有 Qoder 用得上 ——
/// 它的领取路径是 `/campaigns/{campaignId}/claim`,而 id 只能从查状态那一步拿到)。
/// 占位符存在但 `claim_id` 为 None = 调用方没把 id 带过来,这是**调用方的错**,
/// 直接报出来而不是拿一个空 id 去撞平台。
pub async fn call(
    client: &reqwest::Client,
    def: &CheckinDef,
    cred: &Cred,
    claim: bool,
    claim_id: Option<&str>,
) -> Outcome {
    call_with_body(client, def, cred, claim, claim_id).await.0
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
        let a = scheduled_minute("2026-10-13", "trae", 0);
        let b = scheduled_minute("2026-10-13", "trae", 0);
        assert_eq!(a, b, "同一天同一渠道只抖一次(重启不重摇)");
        assert!((23..=43).contains(&a), "落在 00:23–00:43: {a}");
        let c = scheduled_minute("2026-10-14", "trae", 0);
        let d = scheduled_minute("2026-10-13", "workbuddy", 0);
        assert!(a != c || a != d, "不同的日子/渠道不该完全一样");
    }

    /// Qoder 的跨日时刻是 **10:00**(官方活动描述 + 实测 startAt/endAt),
    /// 抖动窗口因此落在 10:00–10:20,而不是 00:23–00:43。
    #[test]
    fn qoder_schedules_at_ten_am() {
        let a = scheduled_minute("2026-10-20", "qoder", 10);
        let b = scheduled_minute("2026-10-20", "qoder", 10);
        assert_eq!(a, b, "同一天同一渠道只抖一次");
        assert!((600..=620).contains(&a), "落在 10:00–10:20 = 600–620 分: {a}");
        // 国内版是另一个渠道 id,抖动值与它无关(各抖各的)
        let cn = scheduled_minute("2026-10-20", "qoder-cn", 10);
        assert!((600..=620).contains(&cn));
    }

    /// **签到周期日**:Qoder 的"今天"从 10:00 起算。
    ///
    /// 这条是本次改动的核心 —— 拿自然日当幂等键会让 09:59 与 10:01 落进同一天,
    /// 于是 10:00 那一期永远不会被领(「昨天已领」被读成「今天已领」)。
    #[test]
    fn qoder_cycle_day_rolls_at_ten_am_beijing() {
        use chrono::{FixedOffset, TimeZone};
        let tz = FixedOffset::east_opt(8 * 3600).unwrap();
        let at = |d, h, m| {
            tz.with_ymd_and_hms(2026, 10, d, h, m, 0)
                .unwrap()
                .timestamp()
        };
        // 10-21 09:59(还没跨日)→ 仍属于 10-20 那一期
        assert_eq!(cycle_day(at(21, 9, 59), 10), "2026-10-20");
        // 10-21 10:00(刚跨日)→ 已经是 10-21 那一期
        assert_eq!(cycle_day(at(21, 10, 0), 10), "2026-10-21");
        // 10-21 23:30 → 10-21
        assert_eq!(cycle_day(at(21, 23, 30), 10), "2026-10-21");
        // 00:23 的渠道不受影响:仍是自然日
        assert_eq!(cycle_day(at(21, 9, 59), 0), "2026-10-21");
        assert_eq!(cycle_day(at(21, 0, 30), 0), "2026-10-21");
    }

    /// 触发时刻:抖动点之前不算到点,之后(含当天更晚)都算 —— 关机一整天再开机也能补跑。
    #[test]
    fn due_now_window_and_catch_up() {
        use chrono::{FixedOffset, TimeZone};
        let tz = FixedOffset::east_opt(8 * 3600).unwrap();
        let at = |h, m| tz.with_ymd_and_hms(2026, 10, 13, h, m, 0).unwrap().timestamp();
        let day = "2026-10-13";
        let sched = scheduled_minute(day, "trae", 0);
        assert!(!due_now(at(0, sched.saturating_sub(1)), day, "trae", 0), "还没到抖动的点");
        assert!(due_now(at(0, 43), day, "trae", 0), "抖动上限 43 分,到这里一定该签");
        assert!(due_now(at(9, 0), day, "trae", 0), "白天开机 → 补跑");
        assert_eq!(beijing_hm(at(9, 5)), (9, 5));
    }

    /// Qoder 的触发窗口:10:00 之前**不该**试(那时还没跨日,查到的是昨天那期),
    /// 10:20 之后一定该试。
    #[test]
    fn qoder_due_now_is_after_ten_am() {
        use chrono::{FixedOffset, TimeZone};
        let tz = FixedOffset::east_opt(8 * 3600).unwrap();
        let at = |h, m| tz.with_ymd_and_hms(2026, 10, 20, h, m, 0).unwrap().timestamp();
        let day = "2026-10-20";
        // 09:59 还没到(哪怕 00:23 那套早就该跑了)
        assert!(!due_now(at(9, 59), day, "qoder", 10), "10:00 之前不算到点");
        // 10:20 是抖动上限,到这里一定该签
        assert!(due_now(at(10, 20), day, "qoder", 10));
        // 下午开机 → 补跑
        assert!(due_now(at(15, 0), day, "qoder", 10));
        // 而 00:23 的渠道在 09:59 早就该跑了(两套口径互不影响)
        assert!(due_now(at(9, 59), day, "trae", 0));
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
    /// 首页徽标要的是"今天没有待办了"这个**正面**结论,与 done_for_day(含 auth/shape
    /// —— 那是"别再试了",不是"签完了")不能混用。2026-10-21 首页徽标上线时刻意分开。
    #[test]
    fn signed_today_is_a_positive_conclusion_not_just_stop_trying() {
        let today = "2026-10-21";
        let mk = |code: &str| {
            let mut c = CheckinCfg::default();
            c.last_day = Some(today.into());
            c.last_code = Some(code.into());
            c
        };

        // 真签完的三种
        for code in ["claimed", "claimed:100", "already", "inactive:本期无活动"] {
            assert!(mk(code).signed_today(today), "{code} 应算今天没有待办");
        }
        // auth / shape:今天不该再试,但**不是签完了** —— 徽标要说"需处理",不能说"已签"
        for code in ["auth", "shape:缺字段", "retry:ready", "retry:device", "failed:boom"] {
            assert!(!mk(code).signed_today(today), "{code:?} 不是'签完了'");
        }
        // 昨天签的不算今天
        let mut y = CheckinCfg::default();
        y.last_day = Some("2026-10-20".into());
        y.last_code = Some("claimed".into());
        assert!(!y.signed_today(today), "跨日了就得重新签");
    }

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

    /// v0.1.47 真机踩到的那件事:`auth` 记在升级**之前**,升级后凭据链路已经修好,
    /// 但当天仍被那条旧 `auth` 挡住,界面一直显示"凭据失效"。
    /// 判据是「记录 auth 的版本 ≠ 当前版本」 → 陈旧 → 放开当天。
    #[test]
    fn stale_auth_does_not_block_the_day_after_version_change() {
        let today = "2026-10-20";
        let mut c = CheckinCfg::default();
        c.last_day = Some(today.into());
        c.last_code = Some("auth".into());

        // ① 老数据:字段还不存在(None) → 一定陈旧
        assert!(c.auth_is_stale("0.1.47"));
        assert!(c.done_for_day(today), "done_for_day 本身不变:auth 仍算当天完结");
        assert!(!c.blocks_today(today, "0.1.47"), "但换版本后不该再挡当天");

        // ② 同一版本判出的 auth → 不陈旧,当天照旧完结(不担心无限重试)
        c.last_auth_version = Some("0.1.47".into());
        assert!(!c.auth_is_stale("0.1.47"));
        assert!(c.blocks_today(today, "0.1.47"));
        // 但换到 0.1.48 又该放行
        assert!(c.auth_is_stale("0.1.48"));
        assert!(!c.blocks_today(today, "0.1.48"));

        // ③ claimed / already 是平台给的既成事实,任何版本都不重判
        for code in ["claimed", "already"] {
            let mut d = CheckinCfg::default();
            d.last_day = Some(today.into());
            d.last_code = Some(code.into());
            d.last_auth_version = Some("0.1.0".into());
            assert!(!d.auth_is_stale("0.1.47"), "{code} 不该因版本变化被重判");
            assert!(!d.inactive_is_stale("0.1.47"), "{code} 不该因版本变化被重判");
            assert!(d.blocks_today(today, "0.1.47"), "{code} 仍应挡住当天");
        }

        // ③b `inactive` / `manual` 是**我们的分类器**给的结论(v0.1.57 起):
        // 旧版本判的"本期无活动"要重问 —— Qoder 把活动换成 VIEW_DETAILS + CLAIMABLE 后,
        // 旧结论就是假阴性,不重判的话升级当天仍然显示"软件里没有"。
        for code in ["inactive:本期无活动", "manual:1024 活动"] {
            let mut d = CheckinCfg::default();
            d.last_day = Some(today.into());
            d.last_code = Some(code.into());
            d.last_auth_version = Some("0.1.0".into());
            assert!(!d.auth_is_stale("0.1.47"), "{code} 不是凭据判决");
            assert!(d.inactive_is_stale("0.1.47"), "{code} 换版本必须重判");
            assert!(!d.blocks_today(today, "0.1.47"), "{code} 不该挡住新版本当天");
            // 同一版本内照样当天完结,不会无限重试
            d.last_auth_version = Some("0.1.47".into());
            assert!(d.blocks_today(today, "0.1.47"), "{code} 同版本内当天完结");
        }

        // ④ 不是今天的 auth 不影响今天
        let mut e = CheckinCfg::default();
        e.last_day = Some("2026-10-19".into());
        e.last_code = Some("auth".into());
        assert!(!e.blocks_today(today, "0.1.47"));
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

    // ───────────── Qoder(2026-10-20 真机样本)─────────────

    /// **真机原样的 actionType 是 `CLAIM_BENEFIT`(带 E)** —— 这条回归用例守着
    /// 一个真实踩过的坑:早先照混淆 bundle 写成了 `CLAIM_BEFIT`,
    /// 结果 `has_campaign` 恒为 false,把"今天可领"静默判成"本期无打卡活动",
    /// 用户当天直接丢 100 分(2026-10-20 真机用例抓到的)。
    #[test]
    fn qoder_action_type_is_claim_benefit_with_e() {
        assert_eq!(QODER_CLAIM_ACTION, "CLAIM_BENEFIT");
        assert!(is_claim_action("CLAIM_BENEFIT"), "真机上的拼写必须被认");
        // 少一个 E 的历史写法也认(别名表),但主名是带 E 的
        assert!(is_claim_action("CLAIM_BEFIT"));
        assert!(!is_claim_action("VIEW_DETAILS"));
        assert!(!is_claim_action(""));

        // 用真机原样的 actionType 走一遍:必须认出"有活动"
        let body = json!({
            "campaigns":[
                {"campaignId":"a","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMED",
                 "benefit":{"kind":"CREDITS","amount":100}},
                {"campaignId":"b","actionType":"VIEW_DETAILS","claimStatus":"CLAIMED"}
            ]
        });
        let pick = qoder_pick(&body);
        assert!(pick.has_campaign, "CLAIM_BENEFIT 必须被认成打卡活动");
        assert!(pick.already, "那条 CLAIMED 的应判为已领");
    }

    /// 真机抓到的**可领**形态(把 claimStatus 换成 CLAIMABLE):
    /// 必须回 `retry:ready:{活动id}` —— 调用方据此拼 claim URL。
    #[test]
    fn qoder_claimable_yields_ready_with_id() {
        let body = json!({
            "uid":"01a068b6-c10f-7658-9045-5084b2acb9f2",
            "showCampaign":true,"claimable":true,
            "campaigns":[{
                "campaignId":"01a15721-d469-7a24-abd5-a81b73dc68f9",
                "campaignKey":"act-20261020-910",
                "actionType":"CLAIM_BENEFIT",
                "startAt":1792461600,"endAt":1792547940,
                "claimStatus":"CLAIMABLE",
                "benefit":{"kind":"CREDITS","amount":100,
                           "validity":{"mode":"RELATIVE_DAYS","days":30}}
            }]
        });
        let out = parse_qoder(&body, false);
        assert_eq!(
            out,
            Outcome::Retry("ready:01a15721-d469-7a24-abd5-a81b73dc68f9".into())
        );
        // 调用方要能从短码里取回活动 id
        assert_eq!(
            ready_claim_id("ready:01a15721-d469-7a24-abd5-a81b73dc68f9"),
            Some("01a15721-d469-7a24-abd5-a81b73dc68f9")
        );
        assert_eq!(ready_claim_id("already"), None);
        assert_eq!(ready_claim_id("ready:"), Some(""));
    }

    /// **2026-10-22 真机样本**:当天的活动换成了 `VIEW_DETAILS` + `CLAIMABLE`
    /// (「1024 | Community Boost」,顶层 `claimable` 已经是 true),
    /// 旧口径只认 CLAIM_BENEFIT → 回 `Inactive("本期无打卡活动")`,
    /// 而用户在自己的客户端里明明看得到一个可领的活动 —— 这就是"软件里显示没有"。
    ///
    /// 现在的口径:`Manual(标题)` —— 有活动、但只能在客户端点开领,如实说出来。
    #[test]
    fn qoder_view_details_claimable_is_manual_not_inactive() {
        let body = json!({
            "uid":"01a068b6-c10f-7658-9045-5084b2acb9f2",
            "showCampaign":true,
            "claimable":true,
            "campaigns":[
                {"campaignId":"01a15d6a-c6df-7e4c-ac3d-b46f98d053f7",
                 "campaignKey":"act-20261021-273",
                 "actionType":"VIEW_DETAILS",
                 "startAt":1792634400,"endAt":1793375940,
                 "claimStatus":"CLAIMABLE",
                 "placements":[{"type":"POPUP","campaignUrl":"https://openapi.qoder.sh/growth-page/activity-iframe",
                   "content":{"en":{"title":"1024 | Community Boost"},
                              "zh":{"title":"1024 | 创造力集结"}}}]},
                {"campaignId":"01a159af-8a04-76b8-b0f4-59d8e2b60583",
                 "campaignKey":"act-20261020-944",
                 "actionType":"VIEW_DETAILS",
                 "claimStatus":"CLAIMED"}
            ]
        });
        let pick = qoder_pick(&body);
        assert!(!pick.has_campaign, "VIEW_DETAILS 不是 CLAIM_BENEFIT");
        assert_eq!(pick.manual.as_deref(), Some("1024 | 创造力集结"));
        assert_eq!(parse_qoder(&body, false), Outcome::Manual("1024 | 创造力集结".into()));

        // 没有可领的 VIEW_DETAILS(只有 CLAIMED 的)→ 仍然是"本期无活动",别谎报有活动
        let quiet = json!({"campaigns":[{"campaignId":"x","actionType":"VIEW_DETAILS","claimStatus":"CLAIMED"}]});
        assert_eq!(parse_qoder(&quiet, false), Outcome::Inactive("本期无打卡活动".into()));

        // 空列表同理
        assert_eq!(parse_qoder(&json!({"campaigns":[]}), false), Outcome::Inactive("本期无打卡活动".into()));

        // CLAIM_BENEFIT 可领时仍然优先走"能直接领",不能被 manual 抢走
        let both = json!({"campaigns":[
            {"campaignId":"m","actionType":"VIEW_DETAILS","claimStatus":"CLAIMABLE",
             "placements":[{"content":{"zh":{"title":"活动页"}}}]},
            {"campaignId":"c","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMABLE",
             "benefit":{"kind":"CREDITS","amount":100}}
        ]});
        assert_eq!(parse_qoder(&both, false), Outcome::Retry("ready:c".into()));
    }

    /// `manual` 与 `inactive` 一样算"今天没有待办":挂件做不了这件事,
    /// 天天在聚合行里喊"还有 N 个没签"只会变成关不掉的噪音。
    #[test]
    fn qoder_manual_blocks_the_day_but_keeps_its_code() {
        let c = CheckinCfg {
            last_day: Some("2026-10-22".into()),
            last_code: Some("manual:1024 | 创造力集结".into()),
            ..Default::default()
        };
        assert!(c.done_for_day("2026-10-22"));
        assert!(c.signed_today("2026-10-22"));
        // 换一天照样要重新查
        assert!(!c.done_for_day("2026-10-23"));
        // 但"同一天完结"要带版本:这条 manual 是旧版本判的(last_auth_version = None)
        // → 不算完结,节拍器会重问一次(v0.1.57 升级后当天就能看到新结论)
        assert!(!c.blocks_today("2026-10-22", "0.1.57"));
        let mut fresh = c.clone();
        fresh.last_auth_version = Some("0.1.57".into());
        assert!(fresh.blocks_today("2026-10-22", "0.1.57"), "同一版本内当天完结");
        assert!(!fresh.blocks_today("2026-10-22", "0.1.58"), "换版本重判");
        assert_eq!(Outcome::Manual("x".into()).verdict(), "manual");
        assert_eq!(Outcome::Manual("x".into()).code(), "manual:x");
        assert_eq!(Outcome::Manual("x".into()).credits(), None);
    }

    /// 真机原样的**已领**形态(本机 10-20 就是这个):Already + 数额。
    #[test]
    fn qoder_claimed_is_already_with_amount() {
        let body = json!({
            "uid":"01a068b6","showCampaign":true,"claimable":false,
            "campaigns":[{
                "campaignId":"01a15721","campaignKey":"act-20261020-910",
                "actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMED",
                "benefit":{"kind":"CREDITS","amount":100}
            }]
        });
        assert_eq!(parse_qoder(&body, false), Outcome::Already(Some(100.0)));
    }

    /// **只有 VIEW_DETAILS 活动 = 本期没有打卡活动**,不能报成"今天已签"。
    ///
    /// 这是设计稿 §3.2 单列的一条:把 Inactive 谎报成 Already,用户会以为领到了;
    /// 报成 Failed 又会天天报警。真机日志里就有这么一条 VIEW_DETAILS。
    #[test]
    fn qoder_view_details_only_is_inactive_not_already() {
        let body = json!({
            "uid":"01a068b6","showCampaign":true,"claimable":false,
            "campaigns":[{
                "campaignId":"01a05bce","campaignKey":"act-20260901-493",
                "actionType":"VIEW_DETAILS","claimStatus":"CLAIMED"
            }]
        });
        let out = parse_qoder(&body, false);
        assert!(matches!(out, Outcome::Inactive(_)), "实际: {out:?}");
        assert!(!matches!(out, Outcome::Already(_)), "绝不能谎报成已领");
        // 空列表同理
        let empty = json!({"showCampaign":false,"campaigns":[]});
        assert!(matches!(parse_qoder(&empty, false), Outcome::Inactive(_)));
    }

    /// 已领 + 可领同时存在(上一期领过、这一期可领)→ 必须优先报"可领"。
    #[test]
    fn qoder_claimable_wins_over_older_claimed() {
        let body = json!({
            "claimable":true,
            "campaigns":[
                {"campaignId":"old","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMED",
                 "benefit":{"kind":"CREDITS","amount":100}},
                {"campaignId":"new","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMABLE",
                 "benefit":{"kind":"CREDITS","amount":200}}
            ]
        });
        assert_eq!(parse_qoder(&body, false), Outcome::Retry("ready:new".into()));
    }

    /// 领取响应的三种终态 + 幂等回执。真机实测 `replayed:true` 就是"之前已领"。
    #[test]
    fn qoder_claim_response_states() {
        // 刚领到
        let fresh = json!({"grantId":"g1","status":"CLAIMED","replayed":false,
                           "benefit":{"kind":"CREDITS","amount":100}});
        assert_eq!(parse_qoder(&fresh, true), Outcome::Claimed(Some(100.0)));
        // 幂等回执:之前已领,不重复发币
        let replay = json!({"grantId":"g1","status":"CLAIMED","replayed":true,
                            "benefit":{"kind":"CREDITS","amount":100}});
        assert_eq!(parse_qoder(&replay, true), Outcome::Already(Some(100.0)));
        // 平台明确说已被领过
        assert_eq!(
            parse_qoder(&json!({"status":"ALREADY_CLAIMED"}), true),
            Outcome::Already(None)
        );
        // 还在处理中 → 当天可以再来看一次
        assert!(matches!(parse_qoder(&json!({"status":"PENDING"}), true), Outcome::Retry(_)));
        // 撤销 → 没得领
        assert!(matches!(parse_qoder(&json!({"status":"REVOKED"}), true), Outcome::Inactive(_)));
        // 没有 status = 结构变了,绝不谎报成功
        assert!(matches!(parse_qoder(&json!({"grantId":"g1"}), true), Outcome::BadShape(_)));
    }

    /// 领取失败的 `failureCode` 要映射到"用户能理解的处置",而不是一律 Failed。
    #[test]
    fn qoder_failure_codes_map_to_actionable_states() {
        let with = |fc: &str| {
            json!({"status":"FAILED","failureCode":fc})
        };
        assert!(matches!(parse_qoder(&with("REDEMPTION_CODE_OUT_OF_STOCK"), true), Outcome::Inactive(_)));
        assert!(matches!(parse_qoder(&with("ACHIEVEMENT_NOT_COMPLETED"), true), Outcome::Inactive(_)));
        assert!(matches!(parse_qoder(&with("CAMPAIGN_NOT_ACTIVE"), true), Outcome::Inactive(_)));
        // 风控:重试无用 —— 报成 Inactive(要用户自己去看),不是 AuthNeeded(别指错方向去重新登录)
        assert!(matches!(parse_qoder(&with("RISK_BLOCKED"), true), Outcome::Inactive(_)));
        // 认不出来的码 → Failed,但要把 code 原样带出来(排查时它才是能查的东西)
        match parse_qoder(&with("SOMETHING_NEW"), true) {
            Outcome::Failed(m) => assert!(m.contains("SOMETHING_NEW"), "{m}"),
            other => panic!("期望 Failed,实际 {other:?}"),
        }
    }

    /// 凭据失效的三个字符串码 + HTTP 层已由调用方处理。
    #[test]
    fn qoder_auth_codes() {
        for code in ["UNAUTHORIZED", "TOKEN_INVALID", "TOKEN_EXPIRE"] {
            assert_eq!(
                parse_qoder(&json!({"code": code, "message":"whatever"}), false),
                Outcome::AuthNeeded,
                "code={code}"
            );
        }
        // 其它字符串 code → Failed(并把 code 带出来)
        match parse_qoder(&json!({"code":"SOMETHING_ELSE"}), false) {
            Outcome::Failed(m) => assert!(m.contains("SOMETHING_ELSE"), "{m}"),
            other => panic!("期望 Failed,实际 {other:?}"),
        }
    }

    /// 结构不认识(没有 campaigns 数组)→ BadShape,停止自动签到但不谎报。
    #[test]
    fn qoder_bad_shape_stops_but_does_not_lie() {
        assert!(matches!(parse_qoder(&json!({"foo":1}), false), Outcome::BadShape(_)));
        assert!(matches!(parse_qoder(&json!({"campaigns":"not-an-array"}), false), Outcome::BadShape(_)));
        // 有 CLAIM_BENEFIT 但状态是没见过的值 → 也是结构变化
        let weird = json!({"campaigns":[{"campaignId":"x","actionType":"CLAIM_BENEFIT",
                                         "claimStatus":"FUTURE_STATE"}]});
        assert!(matches!(parse_qoder(&weird, false), Outcome::BadShape(_)));
    }

    /// 数额缺失时不给数字(不编) —— 与 Trae/WorkBuddy 同口径。
    #[test]
    fn qoder_missing_amount_gives_none() {
        let body = json!({
            "campaigns":[{"campaignId":"c1","actionType":"CLAIM_BENEFIT",
                          "claimStatus":"CLAIMABLE","benefit":{"kind":"CREDITS"}}]
        });
        assert_eq!(parse_qoder(&body, false), Outcome::Retry("ready:c1".into()));
        // 已领但没数额 → Already(None),界面不显示"+?"
        let no_amt = json!({
            "campaigns":[{"campaignId":"c1","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMED"}]
        });
        assert_eq!(parse_qoder(&no_amt, false), Outcome::Already(None));
        // amount=0 视为"没有数额"(0 会被渲染成"没领到")
        let zero = json!({
            "campaigns":[{"campaignId":"c1","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMED",
                          "benefit":{"kind":"CREDITS","amount":0}}]
        });
        assert_eq!(parse_qoder(&zero, false), Outcome::Already(None));
    }

    /// **真机:WorkBuddy 签到走桌面端凭据**(2026-10-20 修的那个分叉)。
    ///
    /// 这条用例就是修复的验收标准:以前签到只读网页 Cookie,而桌面端凭据明明可用,
    /// 于是"余额天天出数、打卡天天报 auth"。现在把顺序对齐后,
    /// 桌面端 token 必须能打通签到接口(实测 200,且不挑 UA / X-Client-Platform)。
    ///
    /// 默认 ignore:`cargo test --lib -- --ignored wb_checkin_desktop_cred`。
    #[test]
    #[ignore]
    fn wb_checkin_desktop_cred_reaches_the_endpoint() {
        let now_ms = chrono::Local::now().timestamp_millis();
        let tokens = crate::workbuddy::desktop_tokens(now_ms);
        assert!(
            !tokens.is_empty(),
            "本机读不到 WorkBuddy 桌面端凭据(没装/没登录),这条用例需要真机环境"
        );
        eprintln!("桌面端候选凭据 {} 份", tokens.len());

        let client = direct_client();
        let def = find("workbuddy").expect("workbuddy 签到定义在表里");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        // 逐份试(取数那条路也是 401 换下一份 —— 签到照抄,别只试第一份就放弃)
        let mut last = Outcome::AuthNeeded;
        for (i, tok) in tokens.iter().enumerate() {
            // **不带 `desktop:` 前缀**:那是取数层的标记,带进来会变成
            // `Bearer desktop:xxx`,必然 401
            let cred = Cred::Web(tok.clone());
            let out = rt.block_on(call(&client, def, &cred, false, None));
            eprintln!("第 {} 份凭据 → {:?}", i + 1, out);
            if !matches!(out, Outcome::AuthNeeded) {
                // 打通了:要么可领(ready)、要么已签(already)、要么没活动(inactive)
                eprintln!("✅ 桌面端凭据能打通签到接口: {out:?}");
                return;
            }
            last = out;
        }
        panic!("所有桌面端凭据都被签到接口拒绝(最后一份:{last:?})");
    }
}
