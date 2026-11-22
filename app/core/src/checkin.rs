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
    /// MiniMax Code:本机 `~/.minimax` 的明文 OAuth 凭据(与取数同一条读取链,
    /// 见 `crate::minimax`;签名头在请求时现算,不能进静态 headers)。
    MiniMaxApp,
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
    CheckinDef {
        id: "minimax",
        name: "MiniMax",
        auth: CheckinAuth::MiniMaxApp,
        // 状态 GET / 领取 POST(官方卡片的这一对;动词错了是裸 404,不是签名错)
        status_url: "https://agent.minimax.cn/minimax-cloud/api/v1/signin/status",
        claim_url: "https://agent.minimax.cn/minimax-cloud/api/v1/signin/claim",
        // 域名跟凭据区域走(signed_checkin 换 host),不设跨区回退:
        // 错区的令牌打哪个区都 401,而 401 是"就地返回 AuthNeeded"的语义
        fallback_status: &[],
        fallback_claim: &[],
        status_method: CheckinMethod::Get,
        claim_method: CheckinMethod::Post,
        body: "{}",
        headers: &[("User-Agent", "MiniMaxCode")],
        // 服务端按请求里的 timezone_id(系统 IANA 名)决定"今天是哪天";对
        // 国内用户(UTC+8)与 beijing_day 一致,幂等键沿用自然日
        day_roll_hour: 0,
    },
    // 豆包工作的**自动签到已移除**(2026-11-14,随 v0.3.47):服务端自 2026-10-24 起
    // 对签到写路径按客户端签名/版本收紧,直连一律 710010702「系统错误」,连官方
    // 客户端都领不动(复盘与证据链见 docs/report/doubao-checkin-710010702-2026-11-14.md)。
    // 渠道保留**取数**(体验额度读数正常);判断矩阵的 Retry/AuthNeeded 分级、
    // 「回查是唯一成功证据」等纪律来自这一渠道的实战,随签到一并删除前留此存照。
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
    /// 领取请求已发出，但状态回查没有给出明确结果；继续回查，不提前宣称成功。
    Confirming(Option<f64>),
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

/// WorkBuddy 的业务码 10001 会随接口区域变化含义，解析时必须显式带上来源区域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkBuddyRegion {
    Mainland,
    International,
    Unknown,
}

/// 从实际请求域名推断 WorkBuddy 接口区域。未知域名不猜状态。
pub fn workbuddy_region_for_host(host: &str) -> WorkBuddyRegion {
    let host = host.to_ascii_lowercase();
    if host == "workbuddy.cn"
        || host.ends_with(".workbuddy.cn")
        || host == "codebuddy.cn"
        || host.ends_with(".codebuddy.cn")
    {
        WorkBuddyRegion::Mainland
    } else if host == "workbuddy.com"
        || host.ends_with(".workbuddy.com")
        || host == "codebuddy.com"
        || host.ends_with(".codebuddy.com")
    {
        WorkBuddyRegion::International
    } else {
        WorkBuddyRegion::Unknown
    }
}

impl Outcome {
    /// 这次尝试有没有"问出结果"——没有的话(限流/网络)当天还可以再来。
    pub fn retryable(&self) -> bool {
        matches!(self, Outcome::Retry(_) | Outcome::Failed(_) | Outcome::Confirming(_))
    }
    /// 要不要立刻告诉用户(设计稿:只有凭据失效与结构变化值得打断)。
    pub fn needs_user(&self) -> bool {
        matches!(self, Outcome::AuthNeeded | Outcome::BadShape(_))
    }
    /// 落库/落配置用的短码。
    pub fn code(&self) -> String {
        match self {
            Outcome::Claimed(_) => "claimed".into(),
            Outcome::Confirming(_) => "confirming".into(),
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
            Outcome::Confirming(_) => "confirming",
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
            Outcome::Claimed(c) | Outcome::Confirming(c) | Outcome::Already(c) => *c,
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

/// 一天的签到结论(签到中心日历带的格子,前端只认这几个词)。
///
/// 有流水看**当天最后一行**的短码(与 `CheckinCfg::done_for_day` /
/// `checkin_of_day` 判"今天处理过了吗"同一口径);没流水的空槽按"该不该签"分:
/// `pending`(今天还没到结论)/ `missed`(该签没签)/ `no_data`(开启之前,
/// 记录里本来就不该有 —— 绝不能画成"没签")。有流水但没成功的(重试没成 /
/// 凭据失效 / 接口变动 / 确认中)一律 `failed`,因为它们都是" attempted 没签成"。
pub fn day_status(last_outcome: Option<&str>, expected: bool, is_current: bool) -> &'static str {
    let o = last_outcome.filter(|s| !s.is_empty());
    match o {
        Some(o) if o.starts_with("claimed") => "claimed",
        Some(o) if o.starts_with("already") => "already",
        Some(o) if o.starts_with("inactive") => "inactive",
        Some(o) if o.starts_with("manual") => "manual",
        Some(_) => "failed",
        None if is_current => "pending",
        None if expected => "missed",
        None => "no_data",
    }
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

// ───────────── 到账核对(claim 之后的余额二次校验)─────────────
//
// 设计见 docs/design/design-arrival-confirm-2026-10-24.html。定位是**只见证,不裁决**:
//
//   ① 回查状态仍是唯一成功证据 —— 余额核对只发生在判成 `claimed` **之后**,
//      永远不能用它把 `confirming` 升级成 `claimed`(余额增量分不清"我们领的"和"充值");
//   ② 推算的东西不当事实说 —— 增量在界面上永远带「按余额推算」标注,
//      与 `grants.rs` 的 `source: "delta"` 同一条纪律;
//   ③ 核对结论绝不参与判定:不碰 `last_code` / `done_for_day` / `blocks_today`,
//      徽标颜色与"今天已处理"的口径一概不读它。
//
// 单向性:判定只有 confirmed / pending / unseen 三态,**没有"确认失败"**。
// 取数间隔里的正常消耗会把 +100 压成负增量(grants.rs 已论证过),
// 所以 `delta ≤ 0` 只能落 unseen("未见增量"),文案明说"不代表没领到"。

/// 正增量才算到账迹象。与 `grants::GRANT_EPS` 同值:低于它当浮点噪声。
pub const ARRIVAL_EPS: f64 = 0.5;

/// 一次到账核对。挂在 `CheckinCfg::arrival` 上,claim 前取基线快照,
/// 之后每份新快照喂给 [`ArrivalInfo::observe`] 直到三态之一落定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrivalInfo {
    /// `pending`(还没见到正增量)→ `confirmed`(见到了)/ `unseen`(跨日还没见到)。
    pub status: String,
    /// claim 前最后一份成功快照的余额与时刻。
    pub baseline: f64,
    pub baseline_at: i64,
    /// 确认时观察到的增量(confirmed 才有)。
    pub delta: Option<f64>,
    pub observed_at: Option<i64>,
    /// 签到周期日(与 `last_day` 同口径,含 Qoder 的 10:00 跨日)。
    /// 核对的截止线:跨了这一天还没见到增量就落 unseen,不隔天追账。
    pub day: String,
}

impl ArrivalInfo {
    /// claim 前调用:以当时的最新快照为基线开一张核对单。
    pub fn start(baseline: f64, baseline_at: i64, day: String) -> Self {
        Self {
            status: "pending".into(),
            baseline,
            baseline_at,
            delta: None,
            observed_at: None,
            day,
        }
    }

    /// 拿一份新快照喂给核对。
    ///
    /// 判序:先看跨日(过截止线落 unseen),再看增量(正的落 confirmed),
    /// 都不满足就维持 pending 等下一份快照。**已落定的单子是事实,不再改写**。
    pub fn observe(&mut self, balance: f64, now: i64, today: &str) {
        if self.status != "pending" {
            return;
        }
        if today != self.day {
            // 跨日了还没见到:落 unseen。不判失败 —— 发放可能延迟、也可能被消耗抵消。
            self.status = "unseen".into();
            return;
        }
        let d = balance - self.baseline;
        // `>=` 与 grants::detect 同口径:正好到阈值也算(detect 是 `d < EPS → None`)
        if d >= ARRIVAL_EPS {
            self.status = "confirmed".into();
            self.delta = Some(d);
            self.observed_at = Some(now);
        }
    }
}

/// 签到渠道的配置(每渠道一份)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CheckinCfg {
    /// 活动监测总开关。关闭后不查询状态也不发领取请求；自动领取由 `auto` 单独控制。
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
    /// 最近一次活动状态查询的时刻。监测开着时,即使自动领取关闭也会继续只读检查。
    pub last_activity_check_at: Option<i64>,
    /// 最近一次跨渠道活动/签到状态，用于渠道卡和监测提示。
    pub activity_state: Option<ActivityState>,
    /// 最近已提示的“活动键 + 状态”，防止每小时重复提醒。
    pub last_activity_notice_key: Option<String>,
    /// 最近已提示过的「结果码 + 周期日」。签到告警(`auth` / `shape`)按它去重:
    /// 同一天同一个问题只弹一次,状态变了或到了新的一天才允许再提醒。
    ///
    /// 与 `last_activity_notice_key` 分开:那条管“有活动可领”,这条管“需要用户动手”,
    /// 共用一个键会让两类提示互相吞掉。
    pub last_alert_key: Option<String>,
    /// 最近看到的 Qoder 活动快照(最多保留 100 条,用于识别新活动和状态变化)。
    pub activity_history: Vec<QoderActivity>,
    /// 最近一次活动列表中的优先活动,供渠道详情和首页徽标展示。
    pub latest_activity: Option<QoderActivity>,
    /// claim 之后的余额到账核对(**纯证据,不参与判定**,见本文件「到账核对」一节)。
    /// 只在 `last_code` 是 `claimed` 的那一次写入;`already`(平台侧已签)不核对 ——
    /// 那不是我们领的,没有"到账"可确认。
    pub arrival: Option<ArrivalInfo>,
    /// 活动监测**本次开启**的时刻(unix 秒)。签到中心靠它区分"没签"
    /// (开启之后该签而没签)与"无数据"(开启之前,记录里本来就不该有)。
    /// 由后端在 set_config 合并处记(`enabled` false→true 的那一跳),关闭时清空;
    /// 老配置是 `None`,由查询侧用首条流水时刻兜底。
    pub enabled_since: Option<i64>,
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
            activity_state: None,
            last_activity_notice_key: None,
            last_alert_key: None,
            activity_history: Vec::new(),
            latest_activity: None,
            arrival: None,
            enabled_since: None,
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
    parse_workbuddy_for_region(body, claiming, WorkBuddyRegion::Mainland)
}

pub fn parse_workbuddy_for_region(
    body: &serde_json::Value,
    claiming: bool,
    region: WorkBuddyRegion,
) -> Outcome {
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
            // 同一个 code 的语义由实际请求域名决定；未知区域停在待确认，不能猜成已签或无活动。
            match region {
                WorkBuddyRegion::Mainland => Outcome::Already(credits),
                WorkBuddyRegion::International => Outcome::Inactive(msg),
                WorkBuddyRegion::Unknown => Outcome::BadShape("WorkBuddy 区域未知(code=10001)".into()),
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

/// 跨渠道的最近一次活动判定。不同平台仍使用各自的解析器，前端只消费统一语义。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ActivityState {
    /// 活动 ID 或“渠道:周期:范围”组成的稳定键。
    pub key: String,
    pub title: String,
    /// claimable / manual / confirming / claimed / device_used / inactive / unknown
    pub status: String,
    /// account / device
    pub scope: String,
    /// CN / Global / Unknown
    pub region: String,
    /// 平台周期键(例如北京时间签到日)。
    pub period: String,
    pub reward: Option<f64>,
    pub observed_at: Option<i64>,
    pub detail: Option<String>,
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
///
/// 连接池:**进程内复用一份**。这里原写「一天就几次请求,不用连接池」,但同一个客户端
/// 现在也被 5 分钟一次的签到节拍器与活动监测调用(`lib.rs` 的 `monitor_*` / `run_checkin`),
/// 一天是几百次 —— 每次重建 TLS 上下文与连接池纯属浪费。`no_proxy` 的**语义不变**,
/// 只是把「每次新建」改成「首次构造、之后复用」。
pub fn direct_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(20))
                .user_agent(BROWSER_UA)
                .build()
                .expect("签到 HTTP 客户端初始化失败")
        })
        .clone()
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
    /// MiniMax:本机客户端凭据(键形如 `mm:<region>:<token>`,区域决定域名;
    /// 签名 URL 与三个签名头在 call_with_body 的 minimax 分支现算,见 `crate::minimax`)。
    MiniMax { key: String, uid: Option<String> },
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
        Cred::MiniMax { key, .. } => {
            // 令牌从键里解出;签名头与实际域名在 call_with_body 里处理
            let token = crate::minimax::parse_key(key)
                .map(|(_, t)| t.trim())
                .unwrap_or(key.trim());
            req.header("Authorization", format!("Bearer {}", token))
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
        // MiniMax:账户端点要官方客户端签名,查询串带 IANA 时区名,域名跟凭据
        // 区域走 —— 全部在发请求前现算,静态表表达不了(见 core::minimax 头注)
        let mm = match cred {
            Cred::MiniMax { key, uid } => {
                crate::minimax::signed_checkin(url, method == CheckinMethod::Post, key, uid.as_deref())
            }
            _ => None,
        };
        let url: &str = mm.as_ref().map(|s| s.url.as_str()).unwrap_or(url);
        let mut req = match method {
            CheckinMethod::Get => client.get(url),
            CheckinMethod::Post => client.post(url),
        };
        req = apply_cred(req, cred);
        for (k, v) in def.headers {
            req = req.header(*k, *v);
        }
        if let Some(s) = &mm {
            for (k, v) in &s.headers {
                req = req.header(*k, v);
            }
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
                    // 原始 reqwest 错误只进日志,用户面给一句可读的
                    crate::diaglog::write_keyed(def.id, "reqwest", &format!("请求失败: {e}"));
                    "请求没发出去(检查网络)".to_string()
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
            "minimax" => crate::minimax::parse_minimax(&body, claim),
            "qoder" | "qoder-cn" => parse_qoder(&body, claim),
            _ => parse_workbuddy_for_region(
                &body,
                claim,
                workbuddy_region_for_host(&host_of(url)),
            ),
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
#[path = "checkin_tests.rs"]
mod tests;
