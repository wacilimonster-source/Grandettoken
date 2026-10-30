//! 首页四种形态的**统一视图模型** —— 一处判定,四处渲染。
//!
//! 来源:产品设计第 6.1 节与第 7 节(`docs/design/design-home-forms-2026-10-21.html`)。
//! 那篇设计的一句话结论是:「四种形态各自算各自的真相」——同一个渠道在面板里是红色的、
//! 在胶囊里是绿色的、在紧凑条里连名字都被去掉了。根因是同一个判断
//! (`toneOf` / `dotCls` / `remainRatio` / `autoPick`)在前端被写了四遍。
//!
//! 本模块把这些判断一次性算完,前端四个渲染器只负责"画"。三条纪律:
//!
//!   1. **纯函数** —— 不碰网络、不碰 DOM、除了传入的 `now` 不读时钟;`cargo test` 能全覆盖;
//!   2. **算不出来就承认不知道** —— 拿不到分母的金额不算"健康",它进 T2 跟同类比绝对值;
//!   3. **同一个渠道,四种形态必须同一个结论** —— 结论在这里定死,形态之间不再有差异。
//!
//! 本模块产出顺序与状态之后,前端的 `sortChannels`、`toneOf`、`remainRatio() ?? 2`
//! 都要退位(P0 的验收里有一条:同一个渠道在四种形态里 `state` 必须完全一致)。

use crate::config::Config;
use crate::fetch::ChannelView;
use crate::providers::{Kind, Window};
use serde::Serialize;

// ───────────────────────── 紧迫度五桶 ─────────────────────────
//
// 百分比、金额、积分是三种不同的量纲,硬塞进一条数轴(以前前端那三个魔法数
// 4000 / 5000 / 9999)的后果是:"算不出比例的金额"和"积分"被当成同一类,
// 而"只剩 ¥0.03"被判成健康。改成先分桶、桶内再比 —— 桶与桶之间不比大小,
// 因为"T1 里剩 90%"和"T2 里剩 ¥500"谁更紧是一个没有答案的问题。

/// T0 · 需要动作:限流 / 鉴权失效 / 低于余额底线 / 取数失败。都是"现在就得做点什么"。
pub const TIER_ACTION: u8 = 0;
/// T1 · 有比例:配额窗口的百分比,或有 `claim.amount` 作分母的金额。唯一可跨渠道比的量。
pub const TIER_RATIO: u8 = 1;
/// T2 · 只有绝对额:没有分母的充值型渠道。没有分母 ≠ 健康,比不了比例就比钱。
pub const TIER_MONEY: u8 = 2;
/// T3 · 积分:风险是过期,不是余额低(沿用现有口径:近 7 天到期占比)。
pub const TIER_POINTS: u8 = 3;
/// T4 · 其余:未配置 / 没数可算的 / 不参与"最紧"评选的。
pub const TIER_REST: u8 = 4;

/// T0 桶内的先后顺序(越小越靠前):先把正在挡请求的排前面。
const T0_LIMITED: f64 = 0.0;
const T0_AUTH: f64 = 1.0;
const T0_FLOOR: f64 = 2.0;
const T0_FAILED: f64 = 3.0;

/// 七态。每种状态都同时有自己的**形状、颜色与写法** —— 形状优先于颜色,灰度下也成立
/// (设计原则 P4 · 颜色不单独承重)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewState {
    /// 新鲜:正常颜色,不显示"更新于" —— 不旧就不需要说
    Fresh,
    /// 陈旧:数值降透明度 + 前缀 `~` + 副行"12 分钟前的快照"
    Stale,
    /// 鉴权失效:写"需登录",整行可点 → 设置;旧快照仍在展开区里
    Auth,
    /// 取数失败:不写 0、不画绿,副行"取数失败 · 点开看原因"
    Error,
    /// 未配置:空心环。没启用不是故障,不参与告警统计
    NoKey,
    /// 已用尽:实心方块,写 0 而不是 `—` —— "确定是 0"和"不知道"是两件事
    Exhausted,
    /// 限流:副行说明绑定窗与恢复时间;Codex 的主数字仍取周窗,其它渠道取绑定窗。
    Limited,
}

/// 状态严重度序号:同桶同紧迫度时的兜底比较依据。
fn severity(s: ViewState) -> i64 {
    match s {
        ViewState::Limited => 0,
        ViewState::Auth => 1,
        ViewState::Exhausted => 2,
        ViewState::Error => 3,
        ViewState::Stale => 4,
        ViewState::Fresh => 5,
        ViewState::NoKey => 6,
    }
}

/// 色调。与前端 `TONE_COLOR` 一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Ok,
    Warn,
    Bad,
    Off,
}

/// 主数字。四种形态都画它 —— 不再各自从 `windows` / `remaining` 里挑。
///
/// 用扁平结构而不是枚举:前端拿到的是 `{percent}` / `{amount, unit}` / `{points}`
/// 三种形状之一,全空 = 「——」。这样 JS 里既不用判标签,也不会把"没有"读成 0。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Headline {
    /// 百分比(0–100)。已用尽时这里是**确定的** 0
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// 金额数值;非人民币原样保留,不折算
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    /// 币种(CNY / USD …)。与 `amount` 同进同出
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// 积分数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub points: Option<f64>,
}

impl Headline {
    pub fn percent(v: f64) -> Self {
        Self { percent: Some(if v.is_finite() { v.clamp(0.0, 100.0) } else { 0.0 }), ..Default::default() }
    }
    pub fn money(v: f64, unit: impl Into<String>) -> Self {
        Self {
            amount: Some(if v.is_finite() { v } else { 0.0 }),
            unit: Some(unit.into()),
            ..Default::default()
        }
    }
    pub fn points(v: f64) -> Self {
        Self { points: Some(if v.is_finite() { v } else { 0.0 }), ..Default::default() }
    }
    /// 「——」:没数字可言。与"确定为 0"严格区分(设计里反复强调的一处)
    pub fn unknown() -> Self {
        Self::default()
    }
    pub fn is_unknown(&self) -> bool {
        self.percent.is_none() && self.amount.is_none() && self.points.is_none()
    }
}

/// 这一行的签到参与情况。
///
/// 只做搬运:`done_today` 连"哪一天"都由后端判定(Qoder 的周期日从 10:00 起算,
/// 前端拿 `new Date()` 算本地日必然算错),这里不许再算一遍。
#[derive(Debug, Clone, Copy, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CheckinRow {
    pub enabled: bool,
    pub done: bool,
}

/// 传给"四种形态"的一行。**这一份对象被四个渲染器共用**,所以形态之间不可能不一致。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormRow {
    pub id: String,
    /// fresh / stale / auth / error / noKey / exhausted / limited
    pub state: ViewState,
    /// ok / warn / bad / off
    pub tone: Tone,
    /// 五桶中的哪一桶(见 TIER_* 常量)
    pub tier: u8,
    /// 桶内可比的高低值;**跨桶不比较**。None = 这一项不参与排序
    pub urgency: Option<f64>,
    /// 主数字取自哪个窗(Limited 时就是正在被拒的那个)
    pub binding_window: Option<String>,
    /// 主数字:四种形态必须一致地画它
    pub headline: Headline,
    /// 胶囊 / 徽标用的限定语,只显示一个,取优先级最高的那个
    pub qualifier: Option<String>,
    /// 副行:"2h 后恢复" / "12 分钟前的快照" / "按当前速度约 3 天"
    pub secondary: Option<String>,
    /// 距下一个可预期事件还有多少秒(限流恢复 / 余额耗尽)
    pub eta_secs: Option<i64>,
    /// 这份数据多老了(秒)
    pub age_secs: i64,
    pub checkin: CheckinRow,
}

/// 非人民币的余额:**并列,不折算**。
///
/// 静默换算等于在用户不知道汇率的情况下改了他的钱数(设计「明确不做」里点名过),
/// 所以这里只把"另外还有多少钱、是什么币种"原样报出去。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OtherMoney {
    pub unit: String,
    pub amount: f64,
}

/// 风险带第一格与第二格要用的合计。
///
/// 口径沿用"汇总只累加金额型渠道、非人民币不参与、隐藏的不参与"这三条老规矩,
/// 只是从"三个花掉的钱"换成了"还剩多少钱"—— 总余额才是这个挂件存在的理由(D04)。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HomeTotals {
    /// 人民币余额合计;一个 CNY 渠道都没有时 None(不要报 0)
    pub cny: Option<f64>,
    /// 非人民币余额,按币种并列
    pub others: Vec<OtherMoney>,
    /// 今日消耗(含订阅型套餐按天摊的那一笔,与底部汇总条同口径)
    pub today: Option<f64>,
    /// 昨日同时段:环比基线。None = 没有基线,界面写「——」而不是 0%(老契约)
    pub today_prev: Option<f64>,
    /// 被隐藏的渠道个数(「另有 N 个已隐藏」用得上)
    pub hidden: usize,
}

/// 列表级结论:顺序、最紧的那一个、以及几个计数(分组标题与失败横幅要靠它们)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeView {
    /// 按最终顺序排好的行。前端直接照抄,不再自己排
    pub rows: Vec<FormRow>,
    /// 与 rows 同序的渠道 id(前端用它替换 `sortChannels`)
    pub order: Vec<String>,
    /// 最紧的那一个(没有需要关注的就 None)
    pub risk: Option<String>,
    /// T0(需要处理)的个数 —— 「需要处理 · N」分组标题用
    pub action_count: usize,
    /// 陈旧 / 取数失败与鉴权失效的个数 —— 底栏与"大面积失败"横幅用
    pub stale_count: usize,
    pub error_count: usize,
    /// 首页级的签到进度:**隐藏的与没开的不进分母**(与前端 `ckActiveIds` 同一口径)
    pub checkin: Option<HomeCheckin>,
    /// 风险带第一/二格要的合计;空列表时各项都是 None
    pub totals: HomeTotals,
    /// 这一份视图是照哪个时刻算的(同一份 view 里所有 `age_secs` 的基准)
    pub now: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeCheckin {
    pub total: usize,
    pub done: usize,
    /// 还没签的渠道 id(紧凑条的"还差谁"用得上)
    pub pending: Vec<String>,
}

/// 参与签到统计的一行。签到状态不在 `ChannelView` 里,所以要单独传进来。
#[derive(Debug, Clone)]
pub struct CheckinInput {
    pub id: String,
    pub enabled: bool,
    pub done_today: bool,
}

/// 判定要用的一组输入。全部由调用方给 —— 本模块不读时钟、不读配置。
#[derive(Debug, Clone)]
pub struct ViewInput {
    pub now: i64,
    pub warn_percent: f64,
    pub crit_percent: f64,
    /// 「余额底线」(元):充值型渠道低于它就算需要动作,不能因为算不出分母而缺席
    pub money_floor: f64,
    /// 超过多少秒没更新算陈旧
    pub stale_after_sec: i64,
    /// urgency | percent | balance | dayUsage | custom
    pub sort: String,
    /// `sort = custom` 时的完整顺序(未列入的附在末尾)
    pub order: Vec<String>,
}

/// 多久没更新算「陈旧」的默认线。
///
/// 轮询间隔默认是 60 秒(面板展开)/ 300 秒(收起),取 900 秒 = 三倍最大间隔:
/// 正常轮询下永远碰不到,一旦碰到了就是真的很久没取到数。
pub const DEFAULT_STALE_SEC: i64 = 900;

/// 「余额底线」默认 ¥10。
///
/// 起点意义 > 精确意义:不同渠道量级差很多(DeepSeek 一次充 ¥50,4SAPI 一次充 ¥500),
/// 但"只剩 3 分钱"在任何量级上都是用户该知道的。
pub const DEFAULT_MONEY_FLOOR: f64 = 10.0;

impl ViewInput {
    /// 从配置出一份输入。`now` 由调用方给 —— 测试要能把时间钉住。
    pub fn from_cfg(cfg: &Config, now: i64) -> Self {
        Self {
            now,
            warn_percent: cfg.warn_percent,
            crit_percent: cfg.crit_percent,
            money_floor: if cfg.money_floor > 0.0 {
                cfg.money_floor
            } else {
                DEFAULT_MONEY_FLOOR
            },
            stale_after_sec: if cfg.stale_after_sec > 0 {
                cfg.stale_after_sec
            } else {
                DEFAULT_STALE_SEC
            },
            sort: cfg.sort.clone(),
            order: cfg.channel_order.clone(),
        }
    }
}

/// 窗口之间的先后:剩余越少越"绑定";剩余一样时,先重置的那个更值得盯。
fn tighter(a: &Window, b: &Window) -> std::cmp::Ordering {
    a.remain_percent
        .partial_cmp(&b.remain_percent)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| reset_secs(a).cmp(&reset_secs(b)))
}

fn reset_secs(w: &Window) -> i64 {
    w.resets_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
        .unwrap_or(i64::MAX)
}

/// 现在的绑定窗:正在被限流的那个优先,其余取剩余最少的那个。
///
/// 绑定窗用于风险、排序与恢复时间。Codex 的主数字另按周优先显示。
/// 长窗打满时短窗跟着显示 0%;短窗限流时长窗退到副行 —— 一个渠道在任何时刻
/// 只有一个"最紧的窗",四种形态都报它(D03)。
pub fn binding_window(v: &ChannelView) -> Option<&Window> {
    let wins = &v.windows;
    if wins.is_empty() {
        return None;
    }
    if let Some(w) = wins
        .iter()
        .filter(|w| w.status == "rate-limited")
        .min_by(|a, b| tighter(a, b))
    {
        return Some(w);
    }
    wins.iter().min_by(|a, b| tighter(a, b))
}

fn is_limited(v: &ChannelView) -> bool {
    v.limited || v.windows.iter().any(|w| w.status == "rate-limited")
}

/// 通知沿用各渠道既有口径。Codex 周窗是展示重点,通知仍看实际最紧的窗。
pub fn notification_window(v: &ChannelView) -> Option<&Window> {
    if v.id == "codex" {
        binding_window(v)
    } else {
        v.windows.iter().find(|w| w.main).or_else(|| v.windows.first())
    }
}

fn clamp01(x: f64) -> f64 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// 有分母的比例;拿不到分母就是 None —— **None 绝不参与"健康"判定**。
fn ratio(v: &ChannelView) -> Option<f64> {
    if !v.has_key {
        return None;
    }
    match v.kind {
        Kind::Percent => binding_window(v).map(|w| clamp01(w.remain_percent / 100.0)),
        Kind::Amount => {
            let denom = v.claim.as_ref().map(|c| c.amount).filter(|a| *a > 0.0)?;
            let rem = v.remaining?;
            Some(clamp01(rem / denom))
        }
        Kind::Points => None,
    }
}

fn below_floor(v: &ChannelView, floor: f64) -> bool {
    v.kind == Kind::Amount && matches!(v.remaining, Some(r) if r <= floor)
}

/// 是不是"确定用完了":有数字,而且是 0。「确定是 0」与「不知道」必须分开写。
fn exhausted(v: &ChannelView) -> bool {
    match v.kind {
        Kind::Percent => matches!(ratio(v), Some(r) if r <= 0.0),
        Kind::Amount | Kind::Points => matches!(v.remaining, Some(r) if r <= 0.0),
    }
}

/// 七态判定。**唯一的入口** —— 前端不再各自解释 `valid / limited / updatedAt`。
pub fn state_of(v: &ChannelView, input: &ViewInput) -> ViewState {
    if !v.has_key {
        return ViewState::NoKey;
    }
    // 网页登录 / 复用本机登录态的渠道:取不到数 = 登录态没了,要给人一条出路。
    // 手填密钥的渠道(keyring)才是"取数失败" —— 这两件事在界面上是两种出路。
    if !v.valid {
        return match v.auth_source.as_str() {
            "web" | "app" => ViewState::Auth,
            _ => ViewState::Error,
        };
    }
    if is_limited(v) {
        return ViewState::Limited;
    }
    if exhausted(v) {
        return ViewState::Exhausted;
    }
    if input.now - v.updated_at > input.stale_after_sec {
        return ViewState::Stale;
    }
    ViewState::Fresh
}

fn tone_of_state(s: ViewState, v: &ChannelView, input: &ViewInput) -> Tone {
    match s {
        ViewState::NoKey | ViewState::Auth | ViewState::Error => Tone::Off,
        ViewState::Limited | ViewState::Exhausted => Tone::Bad,
        _ => {
            // 积分型的风险是过期,不是余额低 —— 这一段沿用前端 `toneOf` 里已经验过的口径:
            // 近 **7 天**到期的量占剩余的比例(30 天会误报:签到积分本来就是 31 天有效)。
            if v.kind == Kind::Points {
                let share = expiring_share(v, 7, input.now);
                return if share >= 0.3 {
                    Tone::Bad
                } else if share >= 0.1 {
                    Tone::Warn
                } else {
                    Tone::Ok
                };
            }
            match ratio(v) {
                Some(r) if r * 100.0 < input.crit_percent => Tone::Bad,
                Some(r) if r * 100.0 < input.warn_percent => Tone::Warn,
                Some(_) => Tone::Ok,
                // 没有分母:不许因为算不出比例就判"健康"。这一处的旧 bug(D02)
                // 让"只剩 ¥0.03"长期显示为绿色——那是一个余额挂件在余额上的核心失效。
                None if below_floor(v, input.money_floor) => Tone::Bad,
                None if v.kind == Kind::Amount && v.remaining.is_some() => Tone::Warn,
                None => Tone::Off,
            }
        }
    }
}

/// 近 `days` 天内到期的量占剩余的比例。
fn expiring_share(v: &ChannelView, days: i64, now: i64) -> f64 {
    let Some(rem) = v.remaining else { return 0.0 };
    if rem <= 0.0 {
        return 0.0;
    }
    let horizon = now + days * 86_400;
    let soon: f64 = v
        .expiring
        .iter()
        .filter(|e| e.at > 0 && e.at <= horizon)
        .map(|e| e.amount.max(0.0))
        .sum();
    clamp01(soon / rem)
}

#[derive(Debug, Clone, Copy)]
struct Score {
    tier: u8,
    urgency: Option<f64>,
}

fn score_of(v: &ChannelView, state: ViewState, input: &ViewInput) -> Score {
    // T0 · 需要动作:限流 → 鉴权 → 低于底线 → 取数失败
    match state {
        ViewState::Limited => return Score { tier: TIER_ACTION, urgency: Some(T0_LIMITED) },
        ViewState::Auth => return Score { tier: TIER_ACTION, urgency: Some(T0_AUTH) },
        ViewState::Error => return Score { tier: TIER_ACTION, urgency: Some(T0_FAILED) },
        _ => {}
    }
    if below_floor(v, input.money_floor) && matches!(state, ViewState::Fresh | ViewState::Stale) {
        return Score { tier: TIER_ACTION, urgency: Some(T0_FLOOR) };
    }
    match state {
        ViewState::NoKey => Score { tier: TIER_REST, urgency: None },
        _ => match v.kind {
            // T1 · 有比例
            Kind::Percent | Kind::Amount if ratio(v).is_some() => {
                Score { tier: TIER_RATIO, urgency: ratio(v) }
            }
            // T2 · 只有绝对额(没有分母 ≠ 健康)
            Kind::Amount if v.remaining.is_some() => {
                Score { tier: TIER_MONEY, urgency: v.remaining.map(|r| r.max(0.0)) }
            }
            // T3 · 积分:风险是过期,不是余额低
            Kind::Points => Score {
                tier: TIER_POINTS,
                urgency: Some(1.0 - expiring_share(v, 7, input.now)),
            },
            _ => Score { tier: TIER_REST, urgency: None },
        },
    }
}

fn opt_cmp(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// 界面上的限定语,取优先级最高的那一个(文案表见设计附录 A)。
fn qualifier_of(state: ViewState, v: &ChannelView, input: &ViewInput) -> Option<String> {
    match state {
        ViewState::Limited => Some("限流".into()),
        ViewState::Auth => Some("需登录".into()),
        ViewState::Error => Some("取数失败".into()),
        ViewState::NoKey => Some("未配置".into()),
        ViewState::Exhausted => Some("已用尽".into()),
        ViewState::Stale => Some("~".into()),
        ViewState::Fresh if below_floor(v, input.money_floor) => Some("低于底线".into()),
        ViewState::Fresh => None,
    }
}

fn fmt_hours(secs: i64) -> String {
    if secs < 60 {
        return "不到 1 分钟".into();
    }
    if secs < 3600 {
        return format!("{} 分钟", secs / 60);
    }
    if secs < 86_400 {
        let h = secs as f64 / 3600.0;
        return if h < 10.0 {
            format!("{:.1} 小时", h)
        } else {
            format!("{} 小时", h.round() as i64)
        };
    }
    format!("{} 天", secs / 86_400)
}

/// 副行。文案口径照抄设计附录 A:先"哪个窗",再"多久",最后"其它窗"。
fn secondary_of(v: &ChannelView, state: ViewState, input: &ViewInput) -> Option<String> {
    match state {
        ViewState::NoKey => None,
        ViewState::Auth => Some("点开去重登一次".into()),
        ViewState::Error => match v.error.as_ref().map(|e| e.trim()).filter(|e| !e.is_empty()) {
            Some(e) => Some(format!("取数失败 · {e}")),
            None => Some("取数失败 · 点开看原因".into()),
        },
        ViewState::Stale => Some(format!("{} 前的快照", fmt_hours(input.now - v.updated_at))),
        ViewState::Limited => {
            let Some(w) = binding_window(v) else { return None };
            let mut parts = vec![format!("{}窗", w.label)];
            if let Some(secs) = until_reset(w, input.now) {
                parts.push(format!("{} 后恢复", fmt_hours(secs)));
            }
            // 其它窗的比例留在最后给用户一个参照(「周期 62%」)
            let others: Vec<String> = v
                .windows
                .iter()
                .filter(|o| o.label != w.label && o.status != "rate-limited")
                .map(|o| format!("{} {:.0}%", o.label, o.remain_percent))
                .collect();
            if !others.is_empty() {
                parts.push(others.join(" · "));
            }
            Some(parts.join(" · "))
        }
        _ => {
            // 金额型:把"还能用几天"摆出来 —— 这个数后端早就算好了(daysOfBalance),
            // 以前它只出现在 `shortageRisk` 为真时的展开区里(D13)。
            match claim_days(v) {
                Some(d) if d >= 1 => Some(format!("按当前速度约可用 {d} 天")),
                Some(_) => Some("按当前速度撑不过 1 天".into()),
                None => None,
            }
        }
    }
}

fn until_reset(w: &Window, now: i64) -> Option<i64> {
    let ts = chrono::DateTime::parse_from_rfc3339(w.resets_at.as_deref()?)
        .ok()?
        .timestamp();
    let left = ts - now;
    (left > 0).then_some(left)
}

fn claim_days(v: &ChannelView) -> Option<i64> {
    let d = v.claim.as_ref()?.days_of_balance?;
    if d.is_finite() && d > 0.0 {
        Some(d.round().max(1.0) as i64)
    } else {
        None
    }
}

/// 组装主数字。四种形态都画它 —— 不再各自挑窗。
///
/// 未配置 / 鉴权失效 / 取数失败一律写「——」:这三种是"不知道",
/// 跟"确定剩下 0"是两件事,不能共用一种写法(设计 P3 / 七态表)。
fn headline_of(v: &ChannelView, state: ViewState) -> Headline {
    if matches!(state, ViewState::NoKey | ViewState::Auth | ViewState::Error) {
        return Headline::unknown();
    }
    match v.kind {
        Kind::Percent => match if v.id == "codex" {
            v.windows.iter().find(|w| w.main).or_else(|| binding_window(v))
        } else {
            binding_window(v)
        } {
            Some(w) => Headline::percent(w.remain_percent),
            None => match v.remaining {
                Some(r) => Headline::percent(r),
                None => Headline::unknown(),
            },
        },
        Kind::Points => match v.remaining {
            Some(r) => Headline::points(r),
            None => Headline::unknown(),
        },
        Kind::Amount => match v.remaining {
            Some(r) => Headline::money(
                r,
                if v.unit.is_empty() { "CNY".to_string() } else { v.unit.clone() },
            ),
            None => Headline::unknown(),
        },
    }
}

fn eta_secs_of(v: &ChannelView, state: ViewState, input: &ViewInput) -> Option<i64> {
    if state == ViewState::Limited {
        return binding_window(v).and_then(|w| until_reset(w, input.now));
    }
    claim_days(v).map(|d| d * 86_400)
}

/// 单行判定。托盘、管理页卡片、测试都可以只调这一个。
pub fn row_of(v: &ChannelView, ck: Option<&CheckinInput>, input: &ViewInput) -> FormRow {
    let state = state_of(v, input);
    let score = score_of(v, state, input);
    FormRow {
        id: v.id.clone(),
        state,
        tone: tone_of_state(state, v, input),
        tier: score.tier,
        urgency: score.urgency,
        binding_window: binding_window(v).map(|w| w.label.clone()),
        headline: headline_of(v, state),
        qualifier: qualifier_of(state, v, input),
        secondary: secondary_of(v, state, input),
        eta_secs: eta_secs_of(v, state, input),
        age_secs: (input.now - v.updated_at).max(0),
        checkin: ck
            .map(|c| CheckinRow { enabled: c.enabled, done: c.done_today })
            .unwrap_or_default(),
    }
}

/// 风险带的合计:总余额(CNY 合计 + 其它币种并列)与今日消耗。
///
/// 只数"可见、已配置、确实有余额"的金额型渠道 —— 隐藏的与取不到数的都不进。
/// 订阅型套餐(`plan`)是**已经花掉的钱**:它进今日消耗,不进余额。
fn totals_of(vs: &[ChannelView]) -> HomeTotals {
    let mut cny: Option<f64> = None;
    let mut others: Vec<OtherMoney> = Vec::new();
    let mut today: Option<f64> = None;
    let mut today_prev: Option<f64> = None;
    let mut hidden = 0usize;

    for v in vs {
        if v.hidden {
            hidden += 1;
            continue;
        }
        if !v.has_key {
            continue;
        }
        if v.kind == Kind::Amount {
            if let Some(r) = v.remaining {
                if r.is_finite() {
                    let unit = if v.unit.is_empty() { "CNY" } else { v.unit.as_str() };
                    if unit.eq_ignore_ascii_case("CNY") {
                        *cny.get_or_insert(0.0) += r;
                    } else {
                        match others.iter_mut().find(|o| o.unit == unit) {
                            Some(o) => o.amount += r,
                            None => others.push(OtherMoney { unit: unit.to_string(), amount: r }),
                        }
                    }
                }
            }
        }
        // 今日消耗:金额型由快照推算,订阅型按天摊 —— 两者口径不同但都算"今天掏出去的钱"
        let day = v.day.unwrap_or(0.0) + v.plan.as_ref().map(|p| p.day).unwrap_or(0.0);
        if v.day.is_some() || v.plan.is_some() {
            *today.get_or_insert(0.0) += day;
        }
        let prev = v.day_prev.unwrap_or(0.0) + v.plan.as_ref().map(|p| p.day_prev).unwrap_or(0.0);
        if v.day_prev.is_some() || v.plan.is_some() {
            // 上期要有基线才算环比:两边口径对称,否则界面会拿 0 当分母(老规矩)
            if v.day_prev.is_some() || v.plan.is_some() {
                *today_prev.get_or_insert(0.0) += prev;
            }
        }
    }

    HomeTotals {
        cny,
        others,
        today,
        today_prev,
        hidden,
    }
}

fn custom_rank(id: &str, input: &ViewInput) -> i64 {
    input
        .order
        .iter()
        .position(|x| x == id)
        .map(|i| i as i64)
        .unwrap_or(9999)
}

/// 列表级视图:排好序的行 + 最紧的那一个 + 几个计数。
///
/// 排序沿用现有的四种模式,默认改成 `urgency`(设计附录 B:`percent` 读到时映射为
/// `urgency`,老配置仍然可用)。**隐藏的渠道不进 `order`** —— 它们在管理页里以
/// 另一种身份出现。
pub fn build_home(vs: &[ChannelView], cks: &[CheckinInput], input: &ViewInput) -> HomeView {
    let map: std::collections::HashMap<&str, &CheckinInput> =
        cks.iter().map(|c| (c.id.as_str(), c)).collect();

    let idx: std::collections::HashMap<&str, &ChannelView> =
        vs.iter().map(|v| (v.id.as_str(), v)).collect();

    let mut rows: Vec<FormRow> = vs
        .iter()
        .filter(|v| !v.hidden)
        .map(|v| row_of(v, map.get(v.id.as_str()).copied(), input))
        .collect();

    match input.sort.as_str() {
        "custom" => rows.sort_by(|a, b| {
            custom_rank(&a.id, input)
                .cmp(&custom_rank(&b.id, input))
                .then(a.id.cmp(&b.id))
        }),
        "balance" => rows.sort_by(|a, b| {
            let r = |id: &str| idx.get(id).and_then(|v| v.remaining).unwrap_or(f64::MAX);
            r(&a.id)
                .partial_cmp(&r(&b.id))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        }),
        "dayUsage" => rows.sort_by(|a, b| {
            let d = |id: &str| idx.get(id).and_then(|v| v.day).unwrap_or(f64::MIN);
            d(&b.id)
                .partial_cmp(&d(&a.id))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        }),
        // urgency(以及读到就映射过来的 percent)
        _ => rows.sort_by(|a, b| {
            (a.tier, severity(a.state))
                .cmp(&(b.tier, severity(b.state)))
                .then_with(|| opt_cmp(a.urgency, b.urgency))
                .then(a.id.cmp(&b.id))
        }),
    }

    let order: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    // 最紧的那一个:T0 里有谁就是谁,否则取第一个"非 T4"的;都不够格就 None。
    let risk = rows
        .iter()
        .find(|r| r.tier == TIER_ACTION)
        .or_else(|| rows.iter().find(|r| r.tier != TIER_REST))
        .map(|r| r.id.clone());

    let action_count = rows.iter().filter(|r| r.tier == TIER_ACTION).count();
    let stale_count = rows.iter().filter(|r| r.state == ViewState::Stale).count();
    let error_count = rows
        .iter()
        .filter(|r| r.state == ViewState::Error || r.state == ViewState::Auth)
        .count();

    // 签到:只数"开了 + 没被隐藏 + 确实在列表里"的 —— 与前端 `ckActiveIds` 同一口径
    let mut total = 0usize;
    let mut done = 0usize;
    let mut pending = Vec::new();
    for r in rows.iter() {
        if !r.checkin.enabled {
            continue;
        }
        total += 1;
        if r.checkin.done {
            done += 1;
        } else {
            pending.push(r.id.clone());
        }
    }

    HomeView {
        rows,
        order,
        risk,
        action_count,
        stale_count,
        error_count,
        checkin: (total > 0).then_some(HomeCheckin { total, done, pending }),
        totals: totals_of(vs),
        now: input.now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::{ChannelView, ClaimState};
    use crate::providers::{Expiring, Window};
    use chrono::TimeZone;

    // ── 夹具 ────────────────────────────────────────────────────────

    const NOW: i64 = 1_000_000;

    fn base_input() -> ViewInput {
        ViewInput {
            now: NOW,
            warn_percent: 40.0,
            crit_percent: 15.0,
            money_floor: DEFAULT_MONEY_FLOOR,
            stale_after_sec: DEFAULT_STALE_SEC,
            sort: "urgency".into(),
            order: Vec::new(),
        }
    }

    fn win(label: &str, remain: f64, status: &str, resets_in: Option<i64>) -> Window {
        Window {
            label: label.into(),
            percent: 100.0 - remain,
            remain_percent: remain,
            status: status.into(),
            resets_at: resets_in.map(|s| {
                chrono::Utc.timestamp_opt(NOW + s, 0).unwrap().to_rfc3339()
            }),
            main: label == "周期",
        }
    }

    /// 一个"全空"的 ChannelView。字段多,测试只改自己关心的那几个。
    fn dummy() -> ChannelView {
        ChannelView {
            id: String::new(),
            name: String::new(),
            short: String::new(),
            color: String::new(),
            kind: Kind::Amount,
            unstable: false,
            has_key: false,
            valid: false,
            remaining: None,
            used: None,
            total: None,
            unit: String::new(),
            windows: Vec::new(),
            reset_credits: None,
            expiring: Vec::new(),
            expiry_complete: false,
            expiring_soon: None,
            extra: Vec::new(),
            error: None,
            limited: false,
            auth_source: "keyring".into(),
            auth_label: String::new(),
            day: None,
            week: None,
            month: None,
            day_prev: None,
            week_prev: None,
            month_prev: None,
            plan: None,
            estimated: false,
            stale: false,
            updated_at: NOW,
            claim: None,
            hidden: false,
            auto_hidden: false,
            peak: None,
            gap_day_from: None,
            gap_week_from: None,
            gap_month_from: None,
            gap_days: Vec::new(),
            accounts: 0,
            grant_today: None,
            grant_week: None,
        }
    }

    fn amount(id: &str, remaining: f64, claim_amount: Option<f64>) -> ChannelView {
        ChannelView {
            id: id.into(),
            name: id.into(),
            kind: Kind::Amount,
            has_key: true,
            valid: true,
            remaining: Some(remaining),
            unit: "CNY".into(),
            claim: claim_amount.map(|amount| ClaimState {
                amount,
                last_claim_at: None,
                source: "auto".into(),
                days_until_eligible: None,
                eligible: false,
                daily_burn: Some(1.0),
                days_of_balance: Some(remaining),
                shortage_risk: false,
            }),
            ..dummy()
        }
    }

    fn percent_ch(id: &str, windows: Vec<Window>) -> ChannelView {
        ChannelView {
            id: id.into(),
            name: id.into(),
            kind: Kind::Percent,
            has_key: true,
            valid: true,
            remaining: windows.iter().map(|w| w.remain_percent).next(),
            windows,
            unit: "percent".into(),
            ..dummy()
        }
    }

    fn points_ch(id: &str, remaining: f64, expiring: Vec<Expiring>) -> ChannelView {
        ChannelView {
            id: id.into(),
            name: id.into(),
            kind: Kind::Points,
            has_key: true,
            valid: true,
            remaining: Some(remaining),
            expiring,
            unit: "credits".into(),
            ..dummy()
        }
    }

    // ── 七态 ────────────────────────────────────────────────────────

    #[test]
    fn seven_states_are_distinct() {
        let input = base_input();

        // 没配密钥 = 未配置,不是故障
        let mut v = amount("ds", 0.0, None);
        v.has_key = false;
        assert_eq!(state_of(&v, &input), ViewState::NoKey);

        // 手填密钥取数失败
        let mut v = amount("ds", 3.0, None);
        v.valid = false;
        assert_eq!(state_of(&v, &input), ViewState::Error);

        // 网页 / 本机登录态:取不到数 = 鉴权失效(要给用户一条出路)
        let mut v = amount("wb", 3.0, None);
        v.valid = false;
        v.auth_source = "web".into();
        assert_eq!(state_of(&v, &input), ViewState::Auth);
        v.auth_source = "app".into();
        assert_eq!(state_of(&v, &input), ViewState::Auth);

        // 任一窗口被限流
        let v = percent_ch("oc", vec![
            win("周期", 62.0, "ok", None),
            win("5 小时", 0.0, "rate-limited", Some(7200)),
        ]);
        assert_eq!(state_of(&v, &input), ViewState::Limited);

        // 确定为 0(没限流)= 已用尽,要写 0 而不是 ——
        let v = percent_ch("oc", vec![win("周期", 0.0, "ok", None)]);
        assert_eq!(state_of(&v, &input), ViewState::Exhausted);

        // 数据老了
        let mut v = amount("ds", 50.0, Some(500.0));
        v.updated_at = NOW - DEFAULT_STALE_SEC - 1;
        assert_eq!(state_of(&v, &input), ViewState::Stale);

        // 正常
        assert_eq!(state_of(&amount("ds", 88.0, Some(100.0)), &input), ViewState::Fresh);
    }

    /// P0 验收①:断网 3 分钟后,四种形态都要能看出"这是旧的"。
    /// 这里保证的是**同一个 `state`** —— 四种形态的差异只能存在于"怎么画"。
    #[test]
    fn offline_three_minutes_is_stale_in_every_form() {
        let mut input = base_input();
        input.stale_after_sec = 180;
        let mut v = amount("ds", 300.0, Some(500.0));
        v.updated_at = input.now - 200;
        let row = row_of(&v, None, &input);
        assert_eq!(row.state, ViewState::Stale);
        assert_eq!(row.qualifier.as_deref(), Some("~"));
        assert_eq!(
            row.headline,
            Headline::money(300.0, "CNY"),
            "旧数据仍然是旧数据的内容,不许改写成 0 或 ——"
        );
        assert!(row.secondary.as_deref().unwrap().contains("前的快照"));
        // 陈旧不等于翻红:颜色仍由真实余量决定,只是多了一层"不确定"的写法
        assert_eq!(row.tone, Tone::Ok);
    }

    // ── 五桶 ────────────────────────────────────────────────────────

    /// P0 验收②:只剩 ¥0.03、又没有分母的渠道 —— 必须不是绿的,且排在最前面。
    #[test]
    fn pennyless_channel_without_denominator_is_the_most_urgent() {
        let input = base_input();
        let poor = amount("poor", 0.03, None);
        let rich = amount("rich", 88.0, Some(100.0));
        let home = build_home(&[rich, poor], &[], &input);

        assert_eq!(home.rows[0].id, "poor", "排在还剩 ¥88 的前面");
        assert_eq!(home.risk.as_deref(), Some("poor"));
        assert_eq!(home.rows[0].tier, TIER_ACTION, "低于底线 → T0");
        assert_eq!(home.rows[0].tone, Tone::Bad, "不许因为算不出比例就判健康");
        assert_eq!(home.rows[0].qualifier.as_deref(), Some("低于底线"));
    }

    #[test]
    fn no_denominator_money_still_competes_by_absolute_amount() {
        let input = base_input();
        // 都高于底线(¥10)时才轮到"比钱"
        let poor = amount("a", 30.0, None);
        let rich = amount("b", 500.0, None);
        let home = build_home(&[rich, poor], &[], &input);
        assert_eq!(home.order, vec!["a".to_string(), "b".to_string()], "钱少的在前面");
        assert!(home.rows.iter().all(|r| r.tier == TIER_MONEY));
    }

    #[test]
    fn buckets_never_mix_units() {
        let input = base_input();
        let tight_pct = percent_ch("pct", vec![win("周期", 8.0, "ok", None)]);
        let money = amount("money", 500.0, None);
        let pts = points_ch("pts", 1000.0, vec![]);
        let healthy = percent_ch("ok", vec![win("周期", 90.0, "ok", None)]);
        let home = build_home(&[healthy, pts, money, tight_pct], &[], &input);

        let tier_of = |id: &str| home.rows.iter().find(|r| r.id == id).unwrap().tier;
        assert_eq!(tier_of("money"), TIER_MONEY);
        assert_eq!(tier_of("pts"), TIER_POINTS);
        assert_eq!(tier_of("pct"), TIER_RATIO);
        assert_eq!(tier_of("ok"), TIER_RATIO);
        // 桶与桶之间不比大小:¥500 不会因为"500 > 8"就被推到 T1 后面去看不到的地方,
        // 但 T1(有比例)整体排在 T2(只有绝对额)之前 —— 这是设计里定死的桶序。
        assert_eq!(home.order.join(","), "pct,ok,money,pts");
    }

    /// 积分型的风险是过期,不是余额低:近 7 天到期占比大的排前面。
    #[test]
    fn points_sort_by_expiring_share() {
        let input = base_input();
        let near = points_ch("near", 1000.0, vec![
            Expiring { at: NOW + 3 * 86_400, amount: 600.0, label: "签到".into() },
            Expiring { at: NOW + 300 * 86_400, amount: 400.0, label: "活动".into() },
        ]);
        let far = points_ch("far", 1000.0, vec![
            Expiring { at: NOW + 300 * 86_400, amount: 1000.0, label: "活动".into() },
        ]);
        let home = build_home(&[far, near], &[], &input);
        assert_eq!(home.order, vec!["near".to_string(), "far".to_string()]);
        let near_row = home.rows.iter().find(|r| r.id == "near").unwrap();
        assert_eq!(near_row.tone, Tone::Bad, "7 天内到期占比 60% ≥ 30% → 红");
    }

    // ── 绑定窗(D03)─────────────────────────────────────────────────

    /// P0 验收③:短窗限流时,主数字必须切到那个窗,四种形态都报它。
    #[test]
    fn limited_window_binds_the_headline() {
        let input = base_input();
        let v = percent_ch("oc", vec![
            win("周期", 62.0, "ok", Some(5 * 86_400)),
            win("5 小时", 0.0, "rate-limited", Some(7200)),
        ]);
        let row = row_of(&v, None, &input);
        assert_eq!(row.state, ViewState::Limited);
        assert_eq!(row.binding_window.as_deref(), Some("5 小时"));
        assert_eq!(row.headline, Headline::percent(0.0), "大数字必须是这个窗的 0%");
        assert_eq!(row.tone, Tone::Bad);
        assert_eq!(row.qualifier.as_deref(), Some("限流"));
        let sec = row.secondary.as_deref().unwrap();
        assert!(sec.contains("5 小时窗"), "先说哪个窗:{sec}");
        assert!(sec.contains("2.0 小时 后恢复"), "再说多久:{sec}");
        assert!(sec.contains("周期 62%"), "最后是其它窗:{sec}");
        assert_eq!(row.eta_secs, Some(7200));
    }

    #[test]
    fn binding_window_is_the_tightest_one_even_without_rate_limit() {
        let input = base_input();
        let v = percent_ch("oc", vec![
            win("周期", 62.0, "ok", None),
            win("5 小时", 4.0, "ok", None),
        ]);
        let row = row_of(&v, None, &input);
        assert_eq!(row.binding_window.as_deref(), Some("5 小时"));
        assert_eq!(row.headline, Headline::percent(4.0));
        assert_eq!(row.tone, Tone::Bad, "4% < crit 15%");
    }

    #[test]
    fn codex_weekly_headline_preserves_short_window_risk_and_recovery() {
        let input = base_input();
        let mut week = win("本周", 98.0, "ok", Some(7 * 86_400));
        week.main = true;
        let mut v = percent_ch("codex", vec![
            week, win("5 小时", 0.0, "rate-limited", Some(7200)),
        ]);
        let row = row_of(&v, None, &input);
        assert_eq!(row.headline, Headline::percent(98.0));
        assert_eq!(row.binding_window.as_deref(), Some("5 小时"));
        assert_eq!(row.state, ViewState::Limited);
        assert_eq!(row.tone, Tone::Bad);
        assert_eq!(row.eta_secs, Some(7200));
        assert!(row.secondary.unwrap().contains("5 小时窗"));
        v.windows[1].status = "ok".into();
        v.windows[1].remain_percent = 4.0;
        let row = row_of(&v, None, &input);
        assert_eq!(row.headline, Headline::percent(98.0));
        assert_eq!(row.tone, Tone::Bad); // 短窗告急仍参与风险判断
    }

    #[test]
    fn codex_notifications_follow_actual_risk_instead_of_weekly_headline() {
        let mut week = win("本周", 87.0, "ok", None);
        week.main = true;
        let mut v = percent_ch("codex", vec![week, win("5 小时", 4.0, "ok", None)]);
        assert_eq!(notification_window(&v).unwrap().label, "5 小时");
        assert_eq!(notification_window(&v).unwrap().remain_percent, 4.0);
        v.windows[0].remain_percent = 0.0;
        v.windows[0].status = "rate-limited".into();
        assert_eq!(notification_window(&v).unwrap().label, "本周");
        // 非 Codex 仍按原有主窗判定,不因这次展示调整改变通知口径。
        v.id = "opencode-go".into();
        v.windows[0].remain_percent = 87.0;
        v.windows[0].status = "ok".into();
        assert_eq!(notification_window(&v).unwrap().label, "本周");
    }

    // ── 颜色与"确定是 0" ───────────────────────────────────────────

    #[test]
    fn tone_follows_thresholds_and_treats_unknown_as_off() {
        let input = base_input();
        assert_eq!(row_of(&amount("a", 30.0, Some(100.0)), None, &input).tone, Tone::Warn,
            "30% 落在 warn 40 与 crit 15 之间");
        assert_eq!(row_of(&amount("a", 10.0, Some(100.0)), None, &input).tone, Tone::Bad);
        let nameless = ChannelView { remaining: None, ..amount("a", 0.0, None) };
        assert_eq!(row_of(&nameless, None, &input).tone, Tone::Off, "没数字 = 不知道,不是绿");
    }

    /// 未配置 / 鉴权 / 取数失败写「——」,都要与"确定为 0"区分开。
    #[test]
    fn unknown_is_not_zero() {
        let input = base_input();
        let mut nokey = amount("ds", 0.0, None);
        nokey.has_key = false;
        assert!(row_of(&nokey, None, &input).headline.is_unknown());

        let zeroed = percent_ch("oc", vec![win("周期", 0.0, "ok", None)]);
        let row = row_of(&zeroed, None, &input);
        assert_eq!(row.headline, Headline::percent(0.0), "确定是 0 就写 0");
        assert!(!row.headline.is_unknown(), "0 不是「没有数字」");
        assert_eq!(row.state, ViewState::Exhausted);
    }

    // ── 排序模式与签到 ──────────────────────────────────────────────

    #[test]
    fn legacy_sort_modes_still_work() {
        let mut input = base_input();
        let a = ChannelView { day: Some(1.0), ..amount("a", 9.0, None) };
        let b = ChannelView { day: Some(5.0), ..amount("b", 3.0, None) };

        input.sort = "balance".into();
        assert_eq!(build_home(&[a.clone(), b.clone()], &[], &input).order,
            vec!["b".to_string(), "a".to_string()], "余额升序");

        input.sort = "dayUsage".into();
        assert_eq!(build_home(&[a.clone(), b.clone()], &[], &input).order,
            vec!["b".to_string(), "a".to_string()], "今日消耗降序");

        input.sort = "custom".into();
        input.order = vec!["a".into(), "b".into()];
        assert_eq!(build_home(&[b, a], &[], &input).order,
            vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn percent_mode_maps_to_urgency() {
        let mut input = base_input();
        input.sort = "percent".into(); // 老配置
        let healthy = percent_ch("ok", vec![win("周期", 90.0, "ok", None)]);
        let limited = percent_ch("lim", vec![win("周期", 0.0, "rate-limited", None)]);
        let home = build_home(&[healthy, limited], &[], &input);
        assert_eq!(home.order[0], "lim", "读到 percent 时按 urgency 排");
    }

    /// 签到:参与了才进分母;全签完也要留得下"✓ N/N"(D09 的后端那一半)。
    #[test]
    fn checkin_counts_only_active_channels() {
        let input = base_input();
        let a = amount("a", 50.0, Some(100.0));
        let b = amount("b", 50.0, Some(100.0));
        let hidden = ChannelView { hidden: true, ..amount("c", 50.0, Some(100.0)) };
        let cks = vec![
            CheckinInput { id: "a".into(), enabled: true, done_today: true },
            CheckinInput { id: "b".into(), enabled: true, done_today: false },
            CheckinInput { id: "c".into(), enabled: true, done_today: false },
            CheckinInput { id: "z".into(), enabled: false, done_today: false },
        ];
        let home = build_home(&[a.clone(), b.clone(), hidden], &cks, &input);
        let ck = home.checkin.clone().unwrap();
        assert_eq!((ck.total, ck.done), (2, 1), "隐藏的与没开的不进分母");
        assert_eq!(ck.pending, vec!["b".to_string()]);

        // 全签完:仍然报 2/2,而不是"这个东西不存在了"
        let cks2 = vec![
            CheckinInput { id: "a".into(), enabled: true, done_today: true },
            CheckinInput { id: "b".into(), enabled: true, done_today: true },
        ];
        let home2 = build_home(&[a, b], &cks2, &input);
        let ck2 = home2.checkin.unwrap();
        assert_eq!((ck2.total, ck2.done), (2, 2));
        assert!(ck2.pending.is_empty());
    }

    #[test]
    fn hidden_channels_stay_out_of_the_order() {
        let input = base_input();
        let mut hidden = amount("hide", 0.0, None);
        hidden.hidden = true;
        let shown = amount("show", 900.0, None);
        let home = build_home(&[hidden.clone(), shown], &[], &input);
        assert_eq!(home.order, vec!["show".to_string()]);
        // 但管理页还能查它自己的状态(不该因为隐藏就失去结论)
        assert_eq!(row_of(&hidden, None, &input).state, ViewState::Exhausted);
    }

    // ── 契约 / 整体 ────────────────────────────────────────────────

    /// P0 验收④:同一个渠道在四种形态里必须是同一份数据 —— 因为四种形态
    /// 现在共用同一个 `FormRow`,它们之间不可能不一致。
    #[test]
    fn one_row_serializes_once() {
        let input = base_input();
        let v = percent_ch("oc", vec![win("周期", 62.0, "ok", Some(86_400 * 5))]);
        let row = row_of(&v, None, &input);
        let j = serde_json::to_value(&row).unwrap();
        assert_eq!(j["state"], "fresh");
        assert_eq!(j["tone"], "ok", "62% 高于 warn 线 40%");
        assert_eq!(j["tier"], TIER_RATIO);
        assert_eq!(j["bindingWindow"], "周期");
        // 前端直接看 `headline.percent` / `headline.amount` / `headline.points`,不用判标签
        assert_eq!(j["headline"], serde_json::json!({ "percent": 62.0 }));
        assert_eq!(j["checkin"]["enabled"], false);
        assert!(j["ageSecs"].is_i64());
    }

    #[test]
    fn counts_feed_the_group_headers() {
        let input = base_input();
        let ok = percent_ch("ok", vec![win("周期", 90.0, "ok", None)]);
        let mut failed = amount("bad", 5.0, None);
        failed.valid = false;
        let mut old = amount("old", 50.0, Some(100.0));
        old.updated_at = NOW - DEFAULT_STALE_SEC - 10;
        let home = build_home(&[ok, failed, old], &[], &input);
        assert_eq!(home.action_count, 1, "取数失败 = 需要动作");
        assert_eq!(home.error_count, 1);
        assert_eq!(home.stale_count, 1);
        assert_eq!(home.risk.as_deref(), Some("bad"));
    }

    /// 风险带第一格:人民币合计 + 其它币种并列(不折算)。
    #[test]
    fn totals_keep_currencies_side_by_side() {
        let input = base_input();
        let mut cny1 = amount("a", 1200.0, None);
        cny1.unit = "CNY".into();
        cny1.day = Some(10.0);
        cny1.day_prev = Some(8.0);
        let mut cny2 = amount("b", 84.6, None);
        cny2.unit = "CNY".into();
        cny2.day = Some(2.4);
        cny2.day_prev = Some(4.0);
        let mut usd = amount("c", 42.1, None);
        usd.unit = "USD".into();
        let hidden = ChannelView { hidden: true, ..amount("d", 999.0, None) };
        let pts = points_ch("e", 1000.0, vec![]);

        let home = build_home(&[cny1, cny2, usd, hidden, pts], &[], &input);
        let t = &home.totals;
        assert_eq!(t.cny, Some(1284.6), "¥1200 + ¥84.6");
        assert_eq!(t.others.len(), 1);
        assert_eq!(t.others[0].unit, "USD");
        assert_eq!((t.others[0].amount * 10.0).round(), 421.0, "42.1 并列,不折算");
        assert_eq!((t.today.unwrap() * 10.0).round(), 124.0, "今日 ¥12.4");
        assert_eq!((t.today_prev.unwrap() * 10.0).round(), 120.0, "昨日同时段 ¥12.0");
        assert_eq!(t.hidden, 1, "隐藏的既不进余额也不消失");
    }

    /// 一个 CNY 渠道都没有时,总余额是 None —— 不要拿 0 冒充"没钱了"。
    #[test]
    fn totals_are_none_when_there_is_nothing_to_sum() {
        let input = base_input();
        let home = build_home(&[points_ch("e", 1000.0, vec![])], &[], &input);
        assert_eq!(home.totals.cny, None);
        assert!(home.totals.others.is_empty());
        assert_eq!(home.totals.today, None);
    }

    #[test]
    fn days_of_balance_surface_in_the_secondary_line() {
        let input = base_input();
        let row = row_of(&amount("ds", 30.0, Some(100.0)), None, &input);
        assert_eq!(row.eta_secs, Some(30 * 86_400));
        assert_eq!(row.secondary.as_deref(), Some("按当前速度约可用 30 天"));
    }
}
