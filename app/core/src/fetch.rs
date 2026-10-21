//! 取数与视图组装。
//!
//! 核心原则:**失败不降级成 0**。取数失败时继续显示上次成功快照并标注时间,
//! 因为余额类工具最危险的 bug 就是把请求失败渲染成"余额归零",用户会以为欠费了。

use crate::appauth::{self};
use crate::config::{ClaimConfig, Config};
use crate::custom;
use crate::fx::{self, FxRate};
use crate::peak;
use crate::providers::{self, AuthKind, Expiring, FetchResult, Kind, Method};
use crate::store::Store;
use crate::secrets;
use crate::weblogin;
use chrono::{Datelike, Duration, Local, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// 判"数据有缺口"的空档线。定义在 store(归档/曲线/文案三处共用同一个数字),
/// 这里转发一次,调用方继续用 `fetch::OBSERVE_GAP_SEC`。
pub use crate::store::OBSERVE_GAP_SEC;

/// 当前这把密钥的指纹(曲线上只画"现在这本账";换 key 后旧账不进曲线)。
/// 非 Keyring 型的凭据会轮换(网页会话/本机登录态),一律空串 = 不分段。
/// **阻塞**(要读凭据管理器),只能在非主线程上调。
pub fn current_key_fp(def: &providers::ProviderDef) -> String {
    if !matches!(def.auth, AuthKind::Keyring) {
        return String::new();
    }
    // 走 resolve 而不是 get:引用模式的渠道,值在密钥箱里(设计稿 product-vault-channel-bridge)
    match secrets::resolve(def.id).map(|r| r.key().map(|k| k.to_string())) {
        Ok(Some(k)) if !k.trim().is_empty() => key_fp(&k),
        _ => String::new(),
    }
}

/// 密钥指纹:同一把 API Key 恒定,换 key 就换一本账(见 store::record_fp)。
///
/// 为什么用 sha256 前 12 字节的十六进制:
///   ① 它只用来给快照分段,不承担任何认证职责,截断够用;
///   ② 不落明文也不可逆(与"密钥只进凭据管理器"同一条纪律);
///   ③ 稳定 —— 换台机器同一把 key 也是同一段。
pub fn key_fp(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.trim().as_bytes());
    let d = h.finalize();
    d.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

/// 距到期多久算"近期"。界面上的"近 30 天将过期"用的就是它。
pub const EXPIRING_SOON_DAYS: i64 = 30;

/// Cookie 型凭据要冒充的浏览器 UA。见 fetch_once 里的注释:网关按 UA 分流,
/// 非浏览器 UA 一律 401。版本号写死没关系(网关只看"像不像浏览器"),
/// 但别改成 `TokenScope/…`。
pub(crate) const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/154.0.0.0 Safari/537.36 Edg/154.0.0.0";

/// 桌面端凭据在凭据串里的前缀:带它的走 `Authorization: Bearer`,且**不带 Cookie**。
/// 为什么不靠"长得像不像令牌"来分:同一个渠道可能有三种凭据(Cookie 会话 / 桌面端令牌 /
/// 手动粘贴的密钥),光看字符串分不出来,前缀是唯一无歧义的标记。
pub const DESKTOP_TOKEN_PREFIX: &str = "desktop:";

/// 桌面端请求的 UA(与 dsh-connect-workbuddy 同口径:CLI 形态的 CodeBuddy 身份)。
pub(crate) const DESKTOP_UA: &str = "CLI/2.63.2 CodeBuddy/2.63.2";

/// 传给界面的一行。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelView {
    pub id: String,
    pub name: String,
    pub short: String,
    pub color: String,
    pub kind: Kind,
    /// 未公开接口,失败时应静默降级而非报警
    pub unstable: bool,
    pub has_key: bool,
    pub valid: bool,
    pub remaining: Option<f64>,
    pub used: Option<f64>,
    pub total: Option<f64>,
    pub unit: String,
    pub windows: Vec<providers::Window>,
    /// 逐笔到期(升序)。积分型渠道才有内容。
    pub expiring: Vec<Expiring>,
    /// 近 30 天内会到期的合计;没有则 None
    pub expiring_soon: Option<Expiring>,
    pub extra: Vec<(String, String)>,
    pub error: Option<String>,
    pub limited: bool,
    /// 凭据来源:"keyring"(用户填的密钥) / "app"(复用本机应用的登录态) /
    /// "web"(挂件内网页登录拿到的令牌,界面要给它一个登录入口)
    pub auth_source: String,
    /// 凭据来源的说明文字,直接显示在界面上
    pub auth_label: String,
    /// 今日 / 本周 / 本月消耗。None 表示数据窗口有空洞,不能报 0
    pub day: Option<f64>,
    pub week: Option<f64>,
    pub month: Option<f64>,
    /// 上一周期「同时段」消耗(汇总条环比用):昨日 0 点到昨天此刻 / 上周同时段 /
    /// 上月同时段。None = 基线不足,界面显示「——」而不是 0(与 day 同一契约)。
    pub day_prev: Option<f64>,
    pub week_prev: Option<f64>,
    pub month_prev: Option<f64>,
    /// 订阅型套餐(OpenCode Go $10/期)按天摊出来的消耗。None = 该渠道没有套餐成本,
    /// 或**一次都没取到过汇率**(宁可少加一笔,不拿猜出来的数当"花掉的钱")。
    /// 汇总条把这三格与上面的 day/week/month 相加,所以这里单独一套字段,
    /// 界面上才能既写"3 个渠道"又写"含套餐"。
    pub plan: Option<PlanCost>,
    /// 消耗是否为本地快照推算值(充值型渠道接口不给累计消耗)
    pub estimated: bool,
    /// 本次取数失败,以上数值来自上次成功快照
    pub stale: bool,
    pub updated_at: i64,
    /// 申请制额度状态;None = 该渠道没启用申请制(见 ClaimConfig)
    pub claim: Option<ClaimState>,
    /// 用户隐藏了该渠道:不显示、不取数(见 Config::hidden_channels)。
    /// 视图仍会返回,管理页要靠它列出「已隐藏」的卡片。
    pub hidden: bool,
    /// 峰谷计价渠道(DeepSeek)的时段状态;其它渠道 None。
    pub peak: Option<PeakView>,
    /// 观测缺口:今天/本周/本月**从几点起才有数据**(超过 [`OBSERVE_GAP_SEC`] 才算)。
    /// None = 起点没缺口(或这个窗口压根没数据)。
    pub gap_day_from: Option<i64>,
    pub gap_week_from: Option<i64>,
    pub gap_month_from: Option<i64>,
    /// 有缺口的日子(本地日零点,升序,最近 92 天)—— 柱图/趋势给这些天打斜纹
    pub gap_days: Vec<i64>,
    /// 这个渠道用过几把密钥(> 1 = 还有历史账户的数据;与"当前是否在用"无关)
    pub accounts: i64,
}

/// 峰谷时段状态。倒计时**不在这里算**:只给"下一次切换的时刻"和"判定时刻",
/// 前端按本地时钟每秒重算 —— 这样一条消息管一天,不用为倒计时加压轮询频率。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeakView {
    pub peak: bool,
    /// 下一次峰↔谷切换的时刻;None = 当前规则下扫不到
    pub next_change_at: Option<i64>,
    /// 此刻整日谷价是不是因为法定节假日 / 周末(界面文案据此说"为什么便宜")
    pub holiday: bool,
    pub weekend: bool,
    /// 节假日表已过期(进入表最大年份的 11 月之后)—— 如实提示,不假装知道
    pub table_stale: bool,
    /// 判定时刻(前端据此在本地推算倒计时)
    pub now: i64,
}

/// 申请制额度渠道(4SAPI 这类:定期申请,把余额补到固定上限)的展示状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimState {
    /// 单次额度,也是剩余百分比的分母
    pub amount: f64,
    /// 生效的上次申请时间;None = 还没记录到
    pub last_claim_at: Option<i64>,
    /// auto(快照跳升检测) | manual(手动修正) | none
    pub source: String,
    /// 距可再次申请还有几天;None = 未记录申请时间
    pub days_until_eligible: Option<i64>,
    pub eligible: bool,
    /// 近 7 天日均消耗推算(数据覆盖不足 1 天时为 None)
    pub daily_burn: Option<f64>,
    /// 按当前速度,余额还能用几天
    pub days_of_balance: Option<f64>,
    /// 余额可能撑不到下次可申请日
    pub shortage_risk: bool,
}

/// 订阅型套餐($10/期)折成人民币后**按天摊**出来的三份消耗。
///
/// 口径:每天固定 `usd × 汇率 ÷ 周期天数`,今日/本周/本月各按"该窗口已经过的秒数"
/// 计,上期同时段同式。所以它的环比天然接近 ±0% —— 这是诚实的:订阅成本不随用量波动,
/// 把它当"消耗"只是为了让汇总条等于**这个周期你真掏出去的钱**。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanCost {
    pub day: f64,
    pub week: f64,
    pub month: f64,
    pub day_prev: f64,
    pub week_prev: f64,
    pub month_prev: f64,
    /// 套餐原价(美元)
    pub usd: f64,
    /// 一个周期多少天(学到的或先按 30 估的)
    pub days: f64,
    /// 折算用的汇率与它所属的月份(界面上要能回答"这个 ¥ 数哪来的")
    pub rate: f64,
    pub rate_month: String,
    /// days 是从连续两次真实重置学到的(true),还是还没跨过重置、先按 30 天估(false)
    pub learned: bool,
    /// 摊到每天是多少钱(界面直接显示,不再让人心算)
    pub per_day: f64,
}

/// 周期长度学习的记忆体。存在 settings 里(键见 [`CYCLE_KEY_PREFIX`])。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CycleMemo {
    /// 最近一次看到的"周期窗重置"时刻(UTC 秒)
    pub last_reset: i64,
    /// 学到的周期长度(天);None = 还没观察到第二次重置
    pub days: Option<f64>,
}

/// 没学到周期之前的假设。OpenCode Go 是包月,30 天最接近真值;
/// 一旦观察到真的跨期重置, [`learn_cycle`] 会用实际间隔替换它。
pub const ASSUMED_CYCLE_DAYS: f64 = 30.0;

/// 从"连续两次周期窗重置的间隔"学周期长度。
///
/// 同一周期内 `resets_at` 不该变,所以:
/// - 没变 / 变小(时钟被往前校正、服务端小幅调表)→ 原样返回,绝不改锚点;
/// - 只往前挪几天(< 3 天)→ 当成服务端微调,也原样返回,否则会学出个"3 天周期";
/// - 真的跨了一次重置 → 间隔落在 7~45 天才采信(账单周期可能是 28/30/31 天),
///   区间外(改套餐、补历史数据)保留旧值,只把锚点挪到新重置时刻。
pub fn learn_cycle(prev: Option<CycleMemo>, reset: i64) -> CycleMemo {
    let Some(p) = prev else {
        return CycleMemo {
            last_reset: reset,
            days: None,
        };
    };
    let gap = reset - p.last_reset;
    if gap < 3 * 86_400 {
        return p;
    }
    let days = gap as f64 / 86_400.0;
    CycleMemo {
        last_reset: reset,
        days: if (7.0..=45.0).contains(&days) {
            Some(days)
        } else {
            p.days
        },
    }
}

/// 摊销计算。窗口起点全部由调用方给(和本地快照消耗用同一把尺子:本地零点/周一/月初),
/// 否则"今日消耗"这一格里会混进两种日起点。
pub fn plan_cost(
    usd: f64,
    days: f64,
    rate: &FxRate,
    learned: bool,
    now: i64,
    today0: i64,
    week0: i64,
    month0: i64,
    prev0: i64,
) -> PlanCost {
    let days = if days.is_finite() && days > 0.0 { days } else { ASSUMED_CYCLE_DAYS };
    let per_day = usd * rate.rate / days;
    let span = |a: i64, b: i64| (b.max(a) - a) as f64 / 86_400.0;
    PlanCost {
        day: per_day * span(today0, now),
        week: per_day * span(week0, now),
        month: per_day * span(month0, now),
        day_prev: per_day * span(today0 - 86_400, now - 86_400),
        week_prev: per_day * span(week0 - 7 * 86_400, now - 7 * 86_400),
        // 终点钳在上月之内:与金额型渠道同一处理(报告 P2-4),短月不会串期
        month_prev: per_day * span(prev0, (prev0 + (now - month0)).min(month0 - 1)),
        usd,
        days,
        rate: rate.rate,
        rate_month: rate.month.clone(),
        learned,
        per_day,
    }
}

/// 周期记忆体的 settings 键前缀(每个订阅渠道各一份,以后加第二家不用改结构)。
pub const CYCLE_KEY_PREFIX: &str = "plan_cycle:";

pub fn cycle_key(id: &str) -> String {
    format!("{CYCLE_KEY_PREFIX}{id}")
}

/// 读周期记忆。库里是坏 JSON 时按"没有"处理(与汇率缓存同一口径:缓存不该阻断取数)。
pub fn load_cycle(store: &Store, id: &str) -> Option<CycleMemo> {
    store
        .get_setting(&cycle_key(id))
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub fn save_cycle(store: &Store, id: &str, memo: &CycleMemo) -> Result<(), String> {
    let txt = serde_json::to_string(memo).map_err(|e| format!("序列化周期失败: {e}"))?;
    store.set_setting(&cycle_key(id), &txt)
}

/// 按 **PROVIDERS 下标**装一整套周期记忆 —— 长度与顺序必须与 `views` 完全一致,
/// 因为 [`attach_plan_costs`] 是按下标读的。以前只装"有套餐的那几条"(长度 1),
/// 而 opencode-go 在 PROVIDERS[1] → `cycles.get(1)` 恒为 None:周期永远学不到,
/// 每轮还把同一份锚点重写回 SQLite(P1)。装的时候顺手过滤成 None,读的地方不用再判。
fn load_cycles(store: &Store) -> Vec<Option<CycleMemo>> {
    providers::PROVIDERS
        .iter()
        .map(|d| {
            if providers::plan_usd(d.id).is_some() {
                load_cycle(store, d.id)
            } else {
                None
            }
        })
        .collect()
}

/// 周期窗的重置时刻:**只认「主窗」**的 `resets_at`(RFC3339)。没有就返回 None ——
/// 宁可不学,也不能拿 5 小时窗或周窗的间隔去当包月周期。以前这里兜了个
/// `v.windows.first()`,等于把非主窗的间隔学成"包月周期"(P2)。
fn main_window_reset(v: &ChannelView) -> Option<i64> {
    let w = v.windows.iter().find(|w| w.main)?;
    let txt = w.resets_at.as_ref()?;
    chrono::DateTime::parse_from_rfc3339(txt)
        .ok()
        .map(|d| d.timestamp())
}

/// 给订阅型渠道挂上按天摊销的消耗。`cycles` 与 `views` 都按 PROVIDERS 的下标对齐。
///
/// - 隐藏的渠道不挂(汇总条本来就不算它,挂上等于偷偷把用户关掉的东西加回钱);
/// - 一次都没取到汇率就不挂,界面会显示"套餐未折算"而不是拿猜的数当花掉的钱;
/// - 取数失败/沿用旧快照也照挂 —— 订阅费是时间过的,不是 token 用的,
///   跟这一轮网络成不成功无关(与"消耗不因取数失败而清零"同一条纪律,P1-4)。
fn attach_plan_costs(
    views: &mut [Option<ChannelView>],
    store: &Store,
    cycles: &[Option<CycleMemo>],
    rate: Option<&FxRate>,
) {
    let now = now_ts();
    // 与金额型渠道同一把尺子:本地零点 / 本地周一 / 本地月初,否则"今日"这一格里
    // 会混进两种日起点
    let (today0, week0, month0, prev0) = (
        local_midnight_today(),
        local_week_start(),
        local_month_start(),
        local_prev_month_start(),
    );
    for (idx, slot) in views.iter_mut().enumerate() {
        let Some(v) = slot else { continue };
        if v.hidden {
            continue;
        }
        let Some(usd) = providers::plan_usd(&v.id) else {
            continue;
        };
        let Some(rate) = rate else {
            // 一次都没取到汇率:明说"没折进合计",而不是默默少一笔让人以为合计是对的
            v.extra
                .push(("套餐".into(), format!("${usd:.0}/期 · 未折进消耗(汇率还没取到)")));
            continue;
        };
        let reset = main_window_reset(v);
        let before = cycles.get(idx).copied().flatten();
        let memo = match (before, reset) {
            (m, Some(reset)) => {
                let fresh = learn_cycle(m, reset);
                if Some(fresh) != m {
                    if let Err(e) = save_cycle(store, &v.id, &fresh) {
                        eprintln!("写入周期记忆失败 {}: {e}", v.id);
                    }
                }
                Some(fresh)
            }
            (m, None) => m,
        };
        // 周期长度三级口径:真实观测学到的 > 从重置日往前推一个整月 > 30 天兜底
        let (days, note) = match memo.and_then(|m| m.days) {
            Some(d) => (d, ""),
            None => match reset.and_then(inferred_cycle_days) {
                Some(d) => (d, "(按上一期长度)"),
                None => (ASSUMED_CYCLE_DAYS, "(周期按 30 天估)"),
            },
        };
        let p = plan_cost(usd, days, rate, note.is_empty(), now, today0, week0, month0, prev0);
        let ds = if (days - days.round()).abs() < 0.05 {
            format!("{}", days.round() as i64)
        } else {
            format!("{days:.1}")
        };
        v.extra.push((
            format!("套餐摊销{note}"),
            format!(
                "¥{:.2}/天 · ${:.0}/{ds} 天 · 汇率 {:.2}({})",
                p.per_day, p.usd, p.rate, p.rate_month
            ),
        ));
        v.plan = Some(p);
    }
}

/// 没学到周期时的第一版估计:套餐是**按月**订的,所以"重置时刻往前推一个整月"
/// 就是本期的长度(11-03 重置 → 10-03 起 → 31 天;3-03 重置 → 2-03 起 → 28/29 天)。
/// 比一律按 30 天估准,而且**第一天就准** —— 不用等整整一个月观察到真实重置;
/// 真实观测([`learn_cycle`])一旦拿到会覆盖它。
pub fn inferred_cycle_days(reset: i64) -> Option<f64> {
    use chrono::{Months, Utc};
    let end = Utc.timestamp_opt(reset, 0).single()?;
    let start = end.checked_sub_months(Months::new(1))?;
    let days = (end - start).num_seconds() as f64 / 86_400.0;
    (7.0..=45.0).contains(&days).then_some(days)
}

/// 一次尝试的失败类型 —— 它决定"要不要换个域名再试"。
/// 分错会让排障变难:密钥错(4xx)换域名只是白等,网络故障不换就白白瞎掉一个渠道。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    /// 连不上 / DNS / 超时:换域名
    Transport,
    /// 5xx、429、响应不是 JSON:换域名(可能是入口自身的问题或限流)
    Server,
    /// 401/403/407:服务器明确拒绝这份凭据 —— 换下一份凭据的信号
    Auth,
    /// 其余 4xx(404/400):路径或参数问题,换域名、换凭据都是同一答案
    Client,
    /// 业务失败(HTTP 200 但 code:false 之类):服务器明确答复了,不换
    Business,
}

impl FailKind {
    /// 值得换域名重试吗
    pub fn retryable(self) -> bool {
        matches!(self, FailKind::Transport | FailKind::Server)
    }
}

/// 值得换"下一份凭据"再试吗?
///
/// 只有服务器明确答复"这份凭据不行"(401/403)才是换凭据的信号:本机可能留着
/// 几份登录态(Trae 装了多个版本),其中一份可能已作废。
/// 网络不通、5xx、业务失败都跟身份无关,换凭据只是白等。
pub fn another_credential_may_help(kind: Option<FailKind>) -> bool {
    kind == Some(FailKind::Auth)
}

/// 尝试顺序:上次成功的入口优先(避免每轮都先撞那个挂掉的),
/// 其余按定义顺序补上,去重。
pub fn attempt_order(primary: &str, fallbacks: &[&str], last_good: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for u in last_good
        .into_iter()
        .chain(std::iter::once(primary))
        .chain(fallbacks.iter().copied())
    {
        if !out.iter().any(|x| x == u) {
            out.push(u.to_string());
        }
    }
    out
}

/// 被服务器拒过的凭据降权多久。**不是永久**:用户重新登录、平台侧恢复、
/// 或者那次 401 只是瞬时抖动,过了这段时间它就回到原位再试一次。
const REJECTED_TTL: i64 = 30 * 60;

/// 把"被 401/403 拒过"的凭据排到后面,其余保持原顺序。
///
/// 为什么需要:手动粘贴的令牌排在桌面端凭据之前(用户明确填的东西优先),
/// 而一份**早已作废**的手动令牌会让每轮都先白打一个 401。修好"401 要换下一份"
/// 之后已经能取到数,但那份废令牌仍会在每轮开头拖一个往返 —— 真机日志里
/// 用户就是这么被卡了三天(2026-10-20)。
///
/// **纯函数**(时间与拒因都由参数给),单测能钉住"降权而非丢弃"这条语义。
fn demote_rejected(
    creds: Vec<appauth::Credential>,
    rejected: &HashMap<String, i64>,
    now: i64,
    fp_of: impl Fn(&str) -> String,
) -> Vec<appauth::Credential> {
    let fresh = |c: &appauth::Credential| {
        rejected
            .get(&fp_of(&c.token))
            .map(|at| now - *at >= REJECTED_TTL)
            .unwrap_or(true)
    };
    let mut ok: Vec<appauth::Credential> = Vec::new();
    let mut bad: Vec<appauth::Credential> = Vec::new();
    for c in creds {
        if fresh(&c) {
            ok.push(c);
        } else {
            bad.push(c);
        }
    }
    // 被拒的全部沉底:顺序在各自组内保持不变
    ok.extend(bad);
    ok
}

/// 申请事件的最小跳升幅度(元)。低于这个数当四舍五入抖动,不记为一次申请。
const CLAIM_JUMP_MIN_DELTA: f64 = 20.0;
/// 回溯窗口:与快照保留期(90 天)对齐
const CLAIM_LOOKBACK_SEC: i64 = 90 * 86_400;

/// 合成"上次申请时间":手动修正优先,除非快照检测到了比手动写入时刻更晚的一次跳升
/// —— 那说明用户又申请了一次,自动值接管。
fn effective_last_claim(cfg: &ClaimConfig, auto_at: Option<i64>) -> Option<(i64, &'static str)> {
    match (auto_at, cfg.manual_last_at) {
        (Some(auto), Some(man)) => {
            let set_at = cfg.manual_set_at.unwrap_or(0);
            if auto > set_at && auto >= man {
                Some((auto, "auto"))
            } else {
                Some((man, "manual"))
            }
        }
        (Some(auto), None) => Some((auto, "auto")),
        (None, Some(man)) => Some((man, "manual")),
        (None, None) => None,
    }
}

/// 由原始输入算出申请制状态。纯函数,便于测试。
pub fn claim_state(
    cfg: &ClaimConfig,
    auto_at: Option<i64>,
    now: i64,
    remaining: Option<f64>,
    daily_burn: Option<f64>,
) -> Option<ClaimState> {
    if !cfg.enabled || cfg.amount <= 0.0 {
        return None;
    }

    let days_of_balance = match (remaining, daily_burn) {
        (Some(r), Some(b)) if b > 0.0 => Some(r / b),
        _ => None,
    };

    let Some((last, source)) = effective_last_claim(cfg, auto_at) else {
        // 还没记录到申请时间:只知道额度上限,给不出倒计时
        return Some(ClaimState {
            amount: cfg.amount,
            last_claim_at: None,
            source: "none".into(),
            days_until_eligible: None,
            eligible: false,
            daily_burn,
            days_of_balance,
            shortage_risk: false,
        });
    };

    let elapsed_days = (now - last).max(0) / 86_400;
    let until = (cfg.min_interval_days - elapsed_days).max(0);
    let eligible = until == 0;
    let shortage_risk = !eligible && days_of_balance.map_or(false, |d| d < until as f64);

    Some(ClaimState {
        amount: cfg.amount,
        last_claim_at: Some(last),
        source: source.into(),
        days_until_eligible: Some(until),
        eligible,
        daily_burn,
        days_of_balance,
        shortage_risk,
    })
}

/// 近 7 天日均消耗(推算)。快照覆盖不足 1 天时返回 None ——
/// 样本太短算出来的斜率会离谱,宁可不报。
/// `key_fp` 非空时只看同一把密钥的账:换 key 不该把另一个账户的历史算进斜率。
fn seven_day_burn(store: &Store, id: &str, key_fp: &str, now: i64) -> Option<f64> {
    let since = now - 7 * 86_400;
    let fp = if key_fp.is_empty() { None } else { Some(key_fp) };
    let first = if let Some(fp) = fp {
        store.first_ts_since_fp(id, fp, since).ok().flatten()?
    } else {
        store.first_ts_since(id, since).ok().flatten()?
    };
    let covered = now - first;
    if covered < 86_400 {
        return None;
    }
    let used = if let Some(fp) = fp {
        store.consumed_in_window_fp(id, fp, since, now).ok()?
    } else {
        store.consumed_in_window(id, since, now).ok()?
    };
    Some(used / (covered as f64 / 86_400.0))
}

pub fn now_ts() -> i64 {
    Local::now().timestamp()
}

/// 本地零点。按用户本地时区切,不按 UTC —— 否则跨时区时"今日消耗"会指错日子。
/// DST 区用 earliest()(0 点被重复时取第一段);0 点被跳过的区取 latest()(当天第一个
/// 存在的时刻);0 点根本不存在的区(哈瓦那等)退回**请求的那一天**的 UTC 零点 ——
/// 兜成 now_ts 会把窗口塌成零长度、再被 -86400 容差放大成"近 24 小时",数字静默偏大
/// (扫描报告 P2-9);而以前直接拿"今天"的 UTC 零点顶替,会把周一/月初/上月初全部算错。
fn local_date_start(y: i32, m: u32, d: u32) -> i64 {
    resolve_midnight(Local.with_ymd_and_hms(y, m, d, 0, 0, 0), y, m, d)
}

/// [`local_date_start`] 的决策体(抽出来单测:DST 的坑没法在测试里造时区)。
fn resolve_midnight(
    cand: chrono::LocalResult<chrono::DateTime<Local>>,
    y: i32,
    m: u32,
    d: u32,
) -> i64 {
    // 0 点被重复(DST 回拨):取第一段,窗口不会因此短一截
    if let Some(x) = cand.earliest() {
        return x.timestamp();
    }
    // 0 点被跳过(DST 前拨):取当天第一个存在的时刻
    if let Some(x) = cand.latest() {
        return x.timestamp();
    }
    // 本地这一天没有 0 点:退到**请求的 (y,m,d)** 的 UTC 零点
    if let Some(x) = Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).earliest() {
        return x.timestamp();
    }
    // 连 y/m/d 都不合法:最后的兜底才是今天的 UTC 零点
    let n = now_ts();
    n - n.rem_euclid(86_400)
}

pub fn local_midnight_today() -> i64 {
    let n = Local::now();
    local_date_start(n.year(), n.month(), n.day())
}

/// 当前本地时区偏移(秒,东为正)。归档分桶用:把明细压成日级摘要时,必须和界面
/// 用**同一个日历**切天。中国无夏令时,一年一个值就是精确的;有夏令时的区跨 DST
/// 那一天可能差一小时(那天摘要挪一格),这是可接受的近似 —— 如实写在这里,
/// 不假装它普适。
pub fn local_tz_offset() -> i64 {
    Local::now().offset().local_minus_utc() as i64
}

/// 本地日 YYYY-MM-DD(通知去重按它记账:同一天同一渠道只响一次)。
pub fn local_day_string() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

/// 本地时间 YYYY-MM-DD HH:MM(体检报告抬头用)。
pub fn local_datetime_string() -> String {
    Local::now().format("%Y-%m-%d %H:%M").to_string()
}

pub fn local_week_start() -> i64 {
    let n = Local::now();
    let back = match n.weekday() {
        Weekday::Mon => 0,
        Weekday::Tue => 1,
        Weekday::Wed => 2,
        Weekday::Thu => 3,
        Weekday::Fri => 4,
        Weekday::Sat => 5,
        Weekday::Sun => 6,
    };
    let d = n.date_naive() - Duration::days(back);
    local_date_start(d.year(), d.month(), d.day())
}

pub fn local_month_start() -> i64 {
    let n = Local::now();
    local_date_start(n.year(), n.month(), 1)
}

/// 上月 1 号零点(环比「上月同期」窗口的起点)。
pub fn local_prev_month_start() -> i64 {
    let n = Local::now();
    let (y, m) = if n.month() == 1 {
        (n.year() - 1, 12)
    } else {
        (n.year(), n.month() - 1)
    };
    local_date_start(y, m, 1)
}

/// 取不到数时的统一失败结果:带上渠道本来的类型,别让 Default 的 Amount 把
/// 百分比渠道渲染成金额卡(扫描报告 P1-2)。
fn err_result(
    def: &providers::ProviderDef,
    msg: String,
    fail: Option<FailKind>,
) -> (FetchResult, Option<FailKind>) {
    (
        FetchResult {
            valid: false,
            kind: def.default_kind,
            unit: unit_for(def.default_kind).to_string(),
            error: Some(msg),
            ..Default::default()
        },
        fail,
    )
}

/// kind → 单位只在这里定义一份(以前在三个地方各写一遍,漏了 Percent)。
pub fn unit_for(k: Kind) -> &'static str {
    match k {
        Kind::Amount => "CNY",
        Kind::Percent => "%",
        Kind::Points => "credits",
    }
}

/// 把 Origin/Referer 改写成与实际请求入口同域(报告 P2-3)。
/// 备用入口的网关看到别家的 Origin 会直接 403,而 403 会被界面说成"网页登录已过期"。
fn same_origin_header(url: &str, header: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split('/').next()?;
    if host.is_empty() {
        return None;
    }
    Some(if header.eq_ignore_ascii_case("origin") {
        format!("https://{host}")
    } else {
        format!("https://{host}/")
    })
}

/// 单次尝试:一个地址、一次请求。返回结果与"这次失败值不值得换域名"。
///
/// **这里是 9 个内置渠道取余额的唯一出口** —— 排查用的 L2 日志埋在这一层,
/// 一次覆盖全部渠道,不用去改 9 个 extractor(见 docs/product/product-logging-observability)。
async fn fetch_once(
    client: &reqwest::Client,
    def: &providers::ProviderDef,
    url: &str,
    key: &str,
) -> (FetchResult, Option<FailKind>) {
    let started = std::time::Instant::now();
    let (res, kind, info) = fetch_once_inner(client, def, url, key).await;
    log_wire(def, url, started.elapsed().as_millis(), &info, &res);
    (res, kind)
}

/// 一次请求的"线上事实" —— 只给日志用。
///
/// 排查"接口怎么了"最需要的就是这几样:状态码、字节数、响应的**结构**。
/// 只记结构不记值 —— 响应体里可能有余额、可能有令牌。
#[derive(Default, Clone)]
struct WireInfo {
    status: Option<u16>,
    bytes: usize,
    /// 顶层键名列表(JSON 时)或响应片段(非 JSON 时)。**只记结构,不记值**。
    shape: String,
    /// 这一层的错误码(传输层 / 状态码 / 业务失败)。
    code: Option<&'static str>,
}

/// 真正发请求的那一段(与 [`fetch_once`] 分开,是为了"每个请求只记一行 L2")。
async fn fetch_once_inner(
    client: &reqwest::Client,
    def: &providers::ProviderDef,
    url: &str,
    key: &str,
) -> (FetchResult, Option<FailKind>, WireInfo) {
    let mut info = WireInfo::default();
    let mut req = match def.method {
        Method::Get => client.get(url),
        Method::PostJson(body) => client
            .post(url)
            .header("Content-Type", "application/json")
            .body(body()),
    }
    .header("Accept", "application/json")
    // 单次尝试的预算:4 个入口最坏 32 秒,还收在 60 秒轮询周期内。
    // 用客户端的 20 秒默认值会让"某个域名挂着"直接拖垮整轮取数。
    .timeout(std::time::Duration::from_secs(8));

    // 网页登录型的凭据有两种形态(见 core::weblogin):
    // - `cookie:session=…; session_2=…` —— 宿主浏览器登录后留下的会话
    // - `desktop:<accessToken>` —— 从 WorkBuddy **桌面端**凭据里解出来的令牌
    // - 其余一律当令牌,走 `Authorization: Bearer …`(手动粘贴的访问密钥)
    //
    // Cookie 这条路**必须冒充浏览器 UA**:网关(openresty)按 UA 分流,同一批 Cookie
    // 用浏览器 UA 200、用客户端 UA 401(2026-10-08 实测,两种各打一次)。
    // 客户端默认那个 `TokenScope/0.1` 恰好是被拒的一类,所以这台机器上要靠逐请求覆盖。
    let desktop_token = key.trim().strip_prefix(DESKTOP_TOKEN_PREFIX);
    if let Some(token) = desktop_token {
        // 桌面端令牌走 Bearer,且**不能**带 Cookie;UA 用桌面端自己的形态
        req = req
            .header("Authorization", format!("Bearer {}", token.trim()))
            .header("User-Agent", DESKTOP_UA);
    } else {
        match weblogin::cookie_header(key) {
            Some(cookie) => {
                req = req.header("Cookie", cookie).header("User-Agent", BROWSER_UA);
            }
            None => {
                req = req.header(
                    "Authorization",
                    format!("{}{}", def.auth_prefix, key.trim()),
                );
            }
        }
    }

    for (k, v) in def.extra_headers {
        // 跨域名回退时 Origin/Referer 必须跟着走,别把主域的头发给备用域(P2-3)
        let val = match k.as_ref() {
            "Origin" | "Referer" => same_origin_header(url, k).unwrap_or_else(|| v.to_string()),
            _ => v.to_string(),
        };
        req = req.header(*k, val);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let text = e.to_string().to_lowercase();
            let (msg, kind, code) = if text.contains("certificate") || text.contains("cert") {
                // 证书/时钟问题换域名是同一个答案,别把四个入口各撞 8 秒(P2-24)
                (
                    "HTTPS 证书校验失败(检查代理拦截或系统时间)".to_string(),
                    FailKind::Business,
                    crate::errcodes::TLS,
                )
            } else if e.is_timeout() {
                ("请求超时".to_string(), FailKind::Transport, crate::errcodes::TIMEOUT)
            } else if e.is_connect() {
                (
                    "无法连接(检查网络或代理)".to_string(),
                    FailKind::Transport,
                    crate::errcodes::CONNECT,
                )
            } else {
                (
                    format!("请求失败: {e}"),
                    FailKind::Transport,
                    crate::errcodes::CONNECT,
                )
            };
            info.code = Some(code);
            let (r, k) = err_result(def, msg, Some(kind));
            return (r, k, info);
        }
    };

    let status = resp.status();
    info.status = Some(status.as_u16());
    // 上限 2 MB:入口被网关换成大 HTML 时不要整包读进内存再解析(P2-24)。
    // 先看 Content-Length,超了**在读之前**就拒;没有长度(分块传输)才按块累加,
    // 一超上限立刻停手 —— 以前 `resp.bytes()` 会先把整包缓进内存再判大小(P2)。
    const MAX_BODY: usize = 2_000_000;
    if let Some(len) = resp.content_length() {
        if len > MAX_BODY as u64 {
            info.code = Some(crate::errcodes::BODY_SHAPE);
            let (r, k) = err_result(def, format!("响应过大({len} 字节),已丢弃"), Some(FailKind::Server));
            return (r, k, info);
        }
    }
    let raw = match read_body_capped(resp, MAX_BODY).await {
        Ok(b) => {
            info.bytes = b.len();
            b
        }
        Err((msg, kind)) => {
            info.code = Some(crate::errcodes::CONNECT);
            let (r, k) = err_result(def, msg, Some(kind));
            return (r, k, info);
        }
    };
    let body: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(v) => {
            // 顶层键名列表:**只记结构,不记值** —— 值里可能有余额,可能有令牌。
            // 接口改字段名时,这是唯一能看出"原来叫 X 现在叫 Y"的线索。
            info.shape = shape_of(&v);
            v
        }
        Err(_) => {
            // 响应体不是 JSON 时,**失败类别要跟着状态码走**,不能一律当成 Server。
            //
            // 2026-10-20 真机踩到(用户 diag.log):WorkBuddy 的 401 由网关回了一页 HTML,
            // 这里判成 `FailKind::Server` → 只换域名、**不换下一份凭据** → 明明已经解出了
            // 桌面端令牌(fp=345578fe),循环却停在那份 401 的手动令牌上,
            // 日志写的是"凭据 [手动令牌→不通]",用户永远取不到数。
            // 401/403 说的是"这份凭据不行",与"响应长什么样"无关 —— 语义由状态码定。
            let code = status.as_u16();
            // 2xx 但不是 JSON = **接口结构变了**(网关挡了 / 返回了 HTML),
            // 不是"业务失败" —— 两者要分开,否则 grep BODY_NOT_JSON 找不到这里。
            info.code = Some(if status.is_success() {
                crate::errcodes::BODY_NOT_JSON
            } else {
                crate::errcodes::for_status(code)
            });
            // 非 JSON 时把响应**开头一小段**记下来(脱敏后)—— 网关回的是 HTML 还是
            // JSON 错误体,看这一段就知道。
            info.shape = snippet_of(&raw);
            let (r, k) = err_result(
                def,
                format!("响应不是合法 JSON (HTTP {code})"),
                Some(kind_for_status(code)),
            );
            return (r, k, info);
        }
    };

    let mut result = (def.extract)(&body);

    // 非 2xx 却解析成功,说明接口用 HTTP 状态码表达业务错误 —— 以状态码为准。
    // OpenCode 的 401/403 带 type:error 体,已被 extractor 处理,不覆盖它的文案。
    if !status.is_success() && result.valid {
        result.valid = false;
        result.error = Some(format!("HTTP {}", status.as_u16()));
    }
    // 复用登录态的渠道:401/403 不是"配置错了",而是那份凭据过期了 ——
    // 用户能做的只有重新登录一次,直接把话说清楚,并且要说对方向:
    // 本机登录型渠道(Trae/Codex)去开客户端;网页登录型(WorkBuddy)回挂件重新登录。
    if !status.is_success() && (status.as_u16() == 401 || status.as_u16() == 403) {
        if let Some(msg) = expired_credential_msg(def) {
            result.error = Some(msg);
        }
    }

    let kind = if result.valid {
        None
    } else if !status.is_success() {
        // 凭据被明确拒绝 → 换下一份登录态值得一试;404/400 → 路径/参数问题,换谁都没用。
        Some(kind_for_status(status.as_u16()))
    } else {
        Some(FailKind::Business) // HTTP 200 但业务失败
    };

    // 码:状态码优先(HTTP 200 但业务失败是另一回事)
    if info.code.is_none() {
        info.code = if !status.is_success() {
            Some(crate::errcodes::for_status(status.as_u16()))
        } else if !result.valid {
            Some(crate::errcodes::BIZ_FAIL)
        } else {
            None
        };
    }

    (result, kind, info)
}

/// 响应的**结构**摘要:顶层键名(最多 8 个)。**只记键名,不记值。**
fn shape_of(body: &serde_json::Value) -> String {
    let Some(obj) = body.as_object() else {
        return match body {
            serde_json::Value::Array(a) => format!("[{} 项]", a.len()),
            serde_json::Value::Null => "null".into(),
            other => format!("{}", type_name_of(other)),
        };
    };
    let keys: Vec<&str> = obj.keys().take(8).map(|s| s.as_str()).collect();
    if obj.len() > 8 {
        format!("{{{}+{}…}}", keys.join(","), obj.len() - 8)
    } else {
        format!("{{{}}}", keys.join(","))
    }
}

/// JSON 值的类型名(不记值本身)。
fn type_name_of(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// 非 JSON 响应的开头一小段(给日志看,脱敏由 [`crate::diaglog::redact`] 负责)。
///
/// ⚠ 这是**唯一一处原始第三方响应内容**进日志字段的地方,所以要额外收紧:
/// 只留可打印 ASCII、遇到 HTML 标签就截断、限长 80 字节。
/// 网关回的错误页有时会把请求行原样带回来,那里面可能有令牌。
fn snippet_of(raw: &[u8]) -> String {
    let head = String::from_utf8_lossy(&raw[..raw.len().min(200)]);
    let mut out = String::new();
    for ch in head.chars() {
        if ch == '<' {
            break; // HTML 开始 —— 后面是页面正文,没有排查价值
        }
        if ch.is_ascii_graphic() || ch == ' ' {
            out.push(ch);
        } else {
            out.push(' '); // 控制字符/非 ASCII 一律换成空格
        }
        if out.len() >= 80 {
            break;
        }
    }
    out.trim().to_string()
}

/// 把一次请求记成一行 L2。
///
/// **成功的事件不落盘**(只进内存环形缓冲);失败的落盘,并连带写出前后的上下文。
/// 这是"日志量"和"信息量"不矛盾的关键 —— 平时不占盘,出事才有现场。
fn log_wire(
    def: &providers::ProviderDef,
    url: &str,
    ms: u128,
    info: &WireInfo,
    res: &FetchResult,
) {
    let host = providers::url_host(url).unwrap_or_default();
    let path = url
        .split_once("://")
        .map(|(_, rest)| rest.find('/').map(|i| &rest[i..]).unwrap_or("/"))
        .unwrap_or("/")
        .to_string();
    let method = match def.method {
        Method::Get => "GET",
        Method::PostJson(_) => "POST",
    };
    let status_s = info.status.map(|s| s.to_string()).unwrap_or_default();
    let ms_s = ms.to_string();
    let bytes_s = info.bytes.to_string();
    let mut fields: Vec<(&str, &str)> = vec![
        ("url", url),
        ("host", &host),
        ("path", &path),
        ("method", method),
        ("ms", &ms_s),
    ];
    // 状态码只在真的拿到响应时才有(连接失败/超时时根本没有 HTTP 状态) ——
    // 写一个空的状态码会让人以为"服务器回了个空状态"。
    if info.status.is_some() {
        fields.push(("status", &status_s));
    }
    if info.bytes > 0 {
        fields.push(("bytes", &bytes_s));
    }
    if !info.shape.is_empty() {
        fields.push(("shape", &info.shape));
    }
    crate::diaglog::wire("fetch", Some(def.id), "http", info.code, &fields);
    // 成功的那次:把解析出的值也记下来(只进内存,除非开了详细日志)。
    // "余额显示不对"这类问题必须看成功响应,这是唯一能看到"接口到底返了什么"的地方。
    if res.valid {
        let rem = res
            .remaining
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into());
        let unit = res.unit.clone();
        crate::diaglog::result(
            "fetch",
            Some(def.id),
            "parse",
            &[("remaining", &rem), ("unit", &unit), ("kind", kind_str(res.kind))],
        );
    }
}

/// kind → 日志里用的短名(与 serde 的 lowercase 表示一致)。
fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::Amount => "amount",
        Kind::Percent => "percent",
        Kind::Points => "points",
    }
}

/// 非 2xx 的状态码 → 失败类别。**唯一口径**:正常分支与"响应体不是 JSON"那条
/// 错误页分支都必须走它,否则会出现"同一个 401 在两处被判成不同类型"的漂移
/// —— 2026-10-20 真机日志就是这么被坑的(HTML 错误页让 401 变成了 Server,
/// 于是只换域名、不换凭据,桌面端令牌永远轮不到)。
fn kind_for_status(code: u16) -> FailKind {
    if matches!(code, 401 | 403 | 407) {
        FailKind::Auth
    } else if code == 429 || code >= 500 {
        FailKind::Server
    } else {
        FailKind::Client
    }
}

/// 把下一块拼进缓冲,一旦超过 `max` 就报错(**纯函数**:上限逻辑靠它单测,
/// 真起 HTTP 服务端才能造出的 2 MB 响应不该进单元测试)。
/// `pub(crate)`:模型价目表那条取数路径(models.rs)用同一个上限口径,不另写一份。
pub(crate) fn push_chunk(out: &mut Vec<u8>, c: &[u8], max: usize) -> Result<(), String> {
    if out.len() + c.len() > max {
        return Err(format!("响应过大(超过 {max} 字节),已丢弃"));
    }
    out.extend_from_slice(c);
    Ok(())
}

/// 分块读响应体,超限**立刻停手** —— 不再把超大的包整个读进内存。
/// 返回 (缓冲, 失败类别),失败类别沿用 fetch_once 的口径。
async fn read_body_capped(
    mut resp: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, (String, FailKind)> {
    let mut out: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(c)) => {
                if let Err(msg) = push_chunk(&mut out, &c, max) {
                    return Err((msg, FailKind::Server));
                }
            }
            Ok(None) => return Ok(out),
            // 连接中断属于传输类 → 值得换下一个入口(以前 None 会让整条回退链就地停下)
            Err(e) => return Err((format!("读取响应失败: {e}"), FailKind::Transport)),
        }
    }
}

/// 要不要把"上次成功地址"的备忘清掉:**这次失败、且确实发过请求**时清。
/// 这个入口已经不行了却还被记着,每轮都要先等满 8 秒超时才切下一个(P2-22);
/// 成功那一支不清(备忘正要写进去),没发过请求(没凭据)也无从谈起。
fn should_clear_memo(result_valid: bool, attempted: bool) -> bool {
    !result_valid && attempted
}

/// 返回 (结果, **最后一次实际尝试的地址**, 最后一次失败的类别)。调用方据此记住
/// "哪个入口是通的",失败时也要知道"刚撞的是哪一个"好把那条备忘清掉。
pub async fn fetch_channel(
    client: &reqwest::Client,
    def: &providers::ProviderDef,
    key: &str,
    last_good: Option<&str>,
) -> (FetchResult, Option<String>, Option<FailKind>) {
    let order = attempt_order(def.url, def.fallback_urls, last_good);
    let mut last: Option<FetchResult> = None;
    let mut last_kind: Option<FailKind> = None;

    for (i, url) in order.iter().enumerate() {
        let (result, kind) = fetch_once(client, def, url, key).await;
        // 成功,或者失败类型不值得换域名(密钥错/业务失败)—— 就此打住
        let stop = match kind {
            None => true,
            Some(k) => !k.retryable(),
        };
        if stop || i + 1 == order.len() {
            // 失败也要把**刚试过的地址**报上去:调用方要靠它把"上次成功地址"清掉。
            // 以前只在成功时给 Some,于是失败分支永远不会清备忘 ——
            // 一个挂掉的备用镜像能把渠道永久钉在上面,每轮白等满超时(P2)。
            let used = Some(url.clone());
            return (result, used, kind);
        }
        // L1 决策:换下一个入口了 —— 记下"从哪换到哪、为什么换"。
        // 没有这一行,日志里只会看到"401"和"连不上"混在一起,看不出是哪个域名在坏。
        let why = match kind {
            Some(FailKind::Auth) => "凭据被拒",
            Some(FailKind::Transport) => "传输失败",
            Some(FailKind::Server) => "服务端错误",
            Some(FailKind::Client) => "请求有问题",
            Some(FailKind::Business) => "业务失败",
            None => "",
        };
        let from = providers::url_host(url).unwrap_or_default();
        let to = order
            .get(i + 1)
            .and_then(|u| providers::url_host(u))
            .unwrap_or_default();
        crate::diaglog::decision(
            "fetch",
            Some(def.id),
            "attempt",
            &format!("换下一个入口:{from} → {to}(原因:{why})"),
        );
        last_kind = kind;
        last = Some(result);
    }
    // order 至少含主地址,这里到不了;给个兜底避免 unwrap
    (
        last.unwrap_or(FetchResult {
            valid: false,
            kind: def.default_kind,
            error: Some("没有可用的接口地址".into()),
            ..Default::default()
        }),
        None,
        last_kind,
    )
}

/// 桌面端凭据读取器。**生产环境默认启用**;`cfg!(test)` 下默认**关闭** ——
/// 单测绝不能真去起 WorkBuddy 进程(那会让测试依赖本机装了什么,还会把并发测试拖慢,
/// 2026-10-19 就因此让两条无关用例挂掉)。要测这条路径的用例用 `set_desktop_reader` 显式打开。
type DesktopReader = dyn Fn() -> Vec<String> + Send + Sync;

static DESKTOP_READER: std::sync::RwLock<Option<&'static DesktopReader>> = std::sync::RwLock::new(None);

/// 测试里"永远读不到"的替身。只在测试构建里存在 —— 放外面会报"static is never used"
/// (v0.1.41 的发版记录说"0 警告",那是 `cargo test` 口径;`cargo check --all-targets`
/// 当时其实带着这一条)。
#[cfg(test)]
static NO_DESKTOP_READER: fn() -> Vec<String> = Vec::new;

/// 读**全部**桌面端候选(国内版优先,其次国际版,再其次历史备份)。
///
/// 为什么给全部:同一台机器可能同时登着国内版与国际版(两套凭据各自有效),
/// 而历史备份里的 token 可能已被上游作废(未过期但 401)—— 上层按 401 换下一份,
/// 比直接让用户重新登录更省事。
fn read_desktop_tokens() -> Vec<String> {
    if let Ok(g) = DESKTOP_READER.read() {
        if let Some(f) = *g {
            return f();
        }
    }
    if cfg!(test) {
        // 单测默认不碰本机凭据(见上面那段注释)
        return Vec::new();
    }
    let now_ms = chrono::Local::now().timestamp_millis();
    // desktop_tokens 内部已经处理:上次续期的成果 → 桌面端文件 → 过期时自动续期
    crate::workbuddy::desktop_tokens(now_ms)
        .into_iter()
        .map(|t| format!("{}{}", DESKTOP_TOKEN_PREFIX, t))
        .collect()
}

/// 测试专用:替换/关闭桌面端读取器。返回前一个(便于恢复)。
/// 目前没有用例需要打开它(桌面端链路由 `workbuddy` 模块的单测 + 一条默认 ignore 的
/// 真机测试覆盖),但保留入口:以后要测"凭据候选顺序"时不必再改生产代码。
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn set_desktop_reader(r: Option<&'static DesktopReader>) -> Option<&'static DesktopReader> {
    let mut g = DESKTOP_READER.write().ok()?;
    let prev = *g;
    *g = r;
    prev
}

/// 测试专用:恢复"读不到"的默认姿态。
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn disable_desktop_reader() {
    let _ = set_desktop_reader(Some(&NO_DESKTOP_READER));
}

/// 取一个渠道的凭据候选(按可信度排序)。空的第一个返回值 = 没有可用凭据;
/// 第二个返回值 = 凭据管理器本身坏了的说明文字(与"没配密钥"区分,P2-12)。
///
/// `web_src` = 网页登录型渠道的会话来源(外壳注入的"现从 webview 的 Cookie 罐里取",
/// 见 Fetcher::set_web_credential_source)。顺序上**手动粘贴的密钥排在它前面**:
/// 用户明确填进去的东西优先,填错了下一轮就会退回来用宿主的会话(401 时换候选)。
fn credentials(
    def: &providers::ProviderDef,
    web_src: Option<&(dyn Fn(&str) -> Option<String> + Send + Sync)>,
) -> (Vec<appauth::Credential>, Option<String>) {
    match def.auth {
        // Keyring 与 WebSession 都躺在 Windows 凭据管理器里;网页登录型还可能有一份
        // "宿主浏览器的会话"(不落盘)。
        AuthKind::Keyring | AuthKind::WebSession => {
            let mut out: Vec<appauth::Credential> = Vec::new();
            let mut err = None;
            match secrets::resolve(def.id) {
                Ok(secrets::Resolved::Inline(k)) | Ok(secrets::Resolved::Vault { key: k, .. }) => {
                    out.push(appauth::Credential {
                        token: k,
                        source: String::new(),
                        device_id: None,
                    })
                }
                Ok(secrets::Resolved::None) => {}
                // 断链:引用还在但箱子那条被删了。**发不了请求是必然的**,但不能一声不吭。
                //
                // N06(2026-10-21):以前这里什么都不说,于是兜底文案变成"还没填 XX 的密钥"——
                // 而"断链"和"没配"该去的地方**正好相反**:前者去密钥箱看,后者去渠道卡填。
                // 用户照着错的话去渠道卡重填,填完仍然不通(引用还在、优先级更高)。
                Ok(secrets::Resolved::Dangling(vid)) => {
                    err = Some(format!(
                        "引用的密钥箱条目已被删除(条目 id={vid})—— 去「密钥箱」检查,或在这里重新选一把"
                    ));
                }
                Err(e) => err = Some(e),
            }
            // WorkBuddy:**桌面端凭据优先**。它比网页会话活得久(accessToken 28 天、
            // refreshToken 30 天,且可续),而网页会话是约 7 天的绝对 TTL ——
            // 2026-10-15 之后连续三天取不到数,就是网页会话到期、而界面仍报"已登录"。
            // 读不到桌面端凭据不是错误(没装桌面端很常见),静默跳过、落到网页会话。
            if def.id == "workbuddy" {
                for tok in read_desktop_tokens() {
                    out.push(appauth::Credential {
                        token: tok,
                        source: "WorkBuddy 桌面端".into(),
                        device_id: None,
                    });
                }
            }
            if matches!(def.auth, AuthKind::WebSession) {
                if let Some(session) = web_src.and_then(|f| f(def.id)) {
                    out.push(appauth::Credential {
                        token: session,
                        source: String::new(),
                        device_id: None,
                    });
                }
            }
            (out, err)
        }
        // 本机应用的登录态:可能有几份(多个安装 / 旧备份),按可信度依次试
        AuthKind::App(app) => (appauth::candidates(app), None),
    }
}

/// 服务器明确说"这份凭据不行"(401/403)时的用户指引。密钥型返回 None ——
/// 那是密钥本身错了,由 HTTP 码/业务响应自己说话,不能瞎指"重新登录"。
///
/// 两条指引的方向不一样,别混:本机登录型渠道(Trae/Codex)去开一次客户端就行;
/// 网页登录型(WorkBuddy)**不能**让它去开桌面客户端 —— 桌面端 5.6.2 起把本机
/// 凭据文件里的 accessToken 改成加密信封(2026-10-02 取证),开多少次都读不到,
/// 只有挂件里那次网页登录有用。
pub fn expired_credential_msg(def: &providers::ProviderDef) -> Option<String> {
    match def.auth {
        AuthKind::App(app) => Some(format!("登录状态已失效 · 打开一次 {} 即可", app.label())),
        // WorkBuddy 现在有两条路:桌面端凭据(可续期)与网页会话(约 7 天绝对 TTL)。
        // 桌面端那条自己会刷新,所以先指向它;网页会话那条按老文案说清"7 天一次"。
        AuthKind::WebSession if def.id == "workbuddy" => Some(
            "WorkBuddy 凭据失效 · 打开一次桌面端(它会自动续期),或在本卡片点「重新登录」".into(),
        ),
        AuthKind::WebSession => Some(
            "网页登录已过期(平台会话约 7 天一次,属正常) · 在渠道卡上点「重新登录」即可".into(),
        ),
        AuthKind::Keyring => None,
    }
}

/// 凭据来源的说明文字,直接显示在界面上。
fn auth_label(def: &providers::ProviderDef, cred: Option<&appauth::Credential>) -> String {
    match def.auth {
        AuthKind::Keyring => "Windows 凭据管理器".into(),
        // 网页登录:不碰本机客户端的任何文件,也不写自己的凭据库 —— 会话就住在
        // 挂件的浏览器 profile 里(登录窗那个 webview 与主窗口共用一份)。
        // 三种形态分开报:桌面端凭据 / 浏览器会话 / 用户自己粘的令牌。
        AuthKind::WebSession => match cred {
            Some(c) if c.token.trim().starts_with(DESKTOP_TOKEN_PREFIX) => {
                "WorkBuddy 桌面端凭据(自动续期)".into()
            }
            Some(c) if weblogin::cookie_header(&c.token).is_some() => "浏览器网页会话".into(),
            Some(_) => "手动粘贴的密钥".into(),
            None => format!("未登录 {} 账号", def.name),
        },
        AuthKind::App(app) => match cred {
            Some(c) => {
                let src = c.source.trim_end_matches(".info");
                if src.is_empty() {
                    format!("本机 {}", app.label())
                } else {
                    format!("{} · {}", app.label(), src)
                }
            }
            None => format!("未检测到 {} 登录信息", app.label()),
        },
    }
}

/// 凭据候选的**可读标签**(诊断日志用)。只描述形态与来源,不含令牌本身。
fn cred_desc(def: &providers::ProviderDef, c: &appauth::Credential) -> String {
    let kind = if c.token.trim().starts_with(DESKTOP_TOKEN_PREFIX) {
        "桌面端"
    } else if weblogin::cookie_header(&c.token).is_some() {
        "网页会话"
    } else if matches!(def.auth, AuthKind::Keyring) {
        "密钥"
    } else {
        "手动令牌"
    };
    if c.source.is_empty() {
        kind.to_string()
    } else {
        format!("{kind}/{}", c.source)
    }
}

/// 汇总"近 30 天会过期多少"。
fn expiring_soon(expiring: &[Expiring], now: i64) -> Option<Expiring> {
    let limit = now + EXPIRING_SOON_DAYS * 86_400;
    let mut sum = 0.0;
    let mut soonest: Option<i64> = None;
    for e in expiring.iter().filter(|e| e.at >= now && e.at <= limit) {
        sum += e.amount;
        soonest = soonest.or(Some(e.at));
    }
    if sum < 0.01 {
        return None;
    }
    Some(Expiring {
        at: soonest.unwrap_or(limit),
        amount: (sum * 100.0).round() / 100.0,
        label: format!("近 {} 天", EXPIRING_SOON_DAYS),
    })
}

/// 申请制状态:只有开启了申请制的渠道才算,并且要查快照历史。
/// `key_fp` 非空时按同一把密钥分段 —— 换 key 写进来的余额变化不该被算成"申请到账"。
fn claim_for(
    def: &providers::ProviderDef,
    cfg: &Config,
    store: &Store,
    remaining: Option<f64>,
    key_fp: &str,
) -> Option<ClaimState> {
    let ccfg = cfg.claim_channels.get(def.id)?;
    if !ccfg.enabled {
        return None;
    }
    let now = now_ts();
    // 只有"补到本轮额度线附近"的量级变化才算一次申请,并且相邻两条快照不能隔太久
    // (合盖多日后第一轮会把几天的差值合成一个"跳升")—— P2-1
    let fp = if key_fp.is_empty() { None } else { Some(key_fp) };
    let auto_at = if let Some(fp) = fp {
        store.last_jump_since_fp(
            def.id,
            fp,
            now - CLAIM_LOOKBACK_SEC,
            CLAIM_JUMP_MIN_DELTA,
            ccfg.amount,
            // 2 小时:既要挡住"合盖几天后第一跳把几天的差值合成一次跳升"(那是几小时~几天),
            // 又不能紧到把正常补跑后的真申请也否掉(30 分钟太敏感)
            2 * 3600,
        )
    } else {
        store.last_jump_since(
            def.id,
            now - CLAIM_LOOKBACK_SEC,
            CLAIM_JUMP_MIN_DELTA,
            ccfg.amount,
            2 * 3600,
        )
    }
    .ok()
    .flatten();
    let burn = seven_day_burn(store, def.id, key_fp, now);
    claim_state(ccfg, auto_at, now, remaining, burn)
}

/// 日/周/月消耗与三个环比(金额型通用)。内置渠道与自定义渠道**共用这一份**,
/// 口径漂移的机会只留一个。
#[allow(clippy::type_complexity)]
fn amount_windows(
    store: &Store,
    id: &str,
    key_fp: &str,
    now: i64,
) -> (
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
) {
    // 消耗一律**只在当前这把密钥的账里**推算:换 key 后新旧账户的余额差
    // 不是消费(小鲸鱼 issue #163 的教训:数据没丢,但也不能混算)。
    let cons = |from: i64, to: i64| -> Option<f64> {
        let r = if key_fp.is_empty() {
            store.consumption_since(id, from, to)
        } else {
            store.consumption_since_fp(id, key_fp, from, to)
        };
        r.ok().flatten()
    };
    let today0 = local_midnight_today();
    let week0 = local_week_start();
    let month0 = local_month_start();
    // 只取一次:跨零点时两次调用可能差一个月份(P2-4)
    let prev0 = local_prev_month_start();
    // 环比口径 = 上一周期的「同时段」:昨日 0 点到昨天此刻、上周同时段、
    // 上月同时段 —— 拿整段上周期比会系统性偏大(它天然比本期多一段尾巴)
    (
        cons(today0, now),
        cons(week0, now),
        cons(month0, now),
        cons(today0 - 86_400, now - 86_400),
        cons(week0 - 7 * 86_400, now - 7 * 86_400),
        // 终点必须钳在上月之内:以前把"本月已过秒数"直接加到上月 1 号上,
        // 于是 3 月 31 日时窗口落到 3 月 3 日,本月的消耗被算进"上月同期"基线
        // (短月尤其明显)。同时只取一次 prev_month_start,免得跨零点两次不等。
        cons(prev0, (prev0 + (now - month0)).min(month0 - 1)),
    )
}

fn build_view(
    def: &providers::ProviderDef,
    mut result: FetchResult,
    stale: bool,
    updated_at: i64,
    store: &Store,
    cfg: &Config,
    cred: Option<&appauth::Credential>,
    key_fp: &str,
    accounts: i64,
) -> ChannelView {
    // 只有金额型才有"消耗"这个概念可算;配额型的用量由接口直接给出。
    // 注意:**不要**把 `result.valid` 串进来 —— 消耗是本地快照推算的,和这次网络请求
    // 成不成功无关。之前取数一失败就把三个窗口一起置 None,汇总里"今日/本周/本月"
    // 会整片显示"数据不足",明明本地有快照。
    let has_amount = result.kind == Kind::Amount && result.remaining.is_some();
    // 「现在」一律用调用方传进来的 `updated_at`,**不要**在这里再取一次 now_ts():
    // 生产调用方传的就是 now_ts()(见本文件 `let ts = now_ts();`),两者等价;
    // 而测试可以传一个合成时刻,把"今天几点"钉住 —— 之前这里偷偷用真实时钟,
    // 导致缺口类测试在刚过午夜时必挂(2026-10-21 00:07 真机撞到)。
    let now2 = updated_at;
    let (day, week, month, day_prev, week_prev, month_prev) = if has_amount {
        amount_windows(store, def.id, key_fp, now2)
    } else {
        (None, None, None, None, None, None)
    };

    let soon = expiring_soon(&result.expiring, now2);

    // 网页会话型(WorkBuddy):平台会话是约 7 天的**绝对 TTL**(证据链见
    // core::weblogin::web_session_hint 的注释)。在卡片变红之前把"用了几天"
    // 摆出来,用户能自己决定哪天顺手重新登录一次。只对"确实在用 Cookie 会话"
    // 的凭据说话:手动粘贴的密钥(Bearer 路线)没有这个生命周期。
    if matches!(def.auth, AuthKind::WebSession)
        && cred.map(|c| weblogin::cookie_header(&c.token).is_some()) == Some(true)
    {
        let at = store
            .get_setting(weblogin::WEB_SESSION_AT_KEY)
            .ok()
            .flatten()
            .and_then(|s| s.parse::<i64>().ok());
        if let Some(text) = weblogin::web_session_hint(at, now2) {
            result.extra.push(("网页会话".into(), text));
        }
    }

    // ── 观测缺口 / 密钥分段 / 峰谷 ──
    // 缺口与分段只对金额型算(百分比/积分型没有日周月消耗这回事,查了也没处显示)。
    let fp_opt = if key_fp.is_empty() { None } else { Some(key_fp) };
    let (gap_day_from, gap_week_from, gap_month_from, gap_days) = if has_amount {
        let tz = local_tz_offset();
        (
            gap_from(store, def.id, fp_opt, local_midnight_today(), now2),
            gap_from(store, def.id, fp_opt, local_week_start(), now2),
            gap_from(store, def.id, fp_opt, local_month_start(), now2),
            store
                .daily_coverage(def.id, fp_opt, now2 - 92 * 86_400, now2, tz)
                .unwrap_or_default()
                .into_iter()
                .filter(|d| gap_of_day(d, now2))
                .map(|d| d.day_start)
                .collect(),
        )
    } else {
        (None, None, None, Vec::new())
    };
    // 峰谷只看时间,不看数据 —— 但只在挂了"峰谷计价"的渠道上出现(DeepSeek)。
    let peak = if def.peak_pricing {
        let i = peak::info_at(now2);
        Some(PeakView {
            peak: i.peak,
            next_change_at: i.next_change_at,
            holiday: i.holiday,
            weekend: i.weekend,
            table_stale: peak::table_stale(now2),
            now: now2,
        })
    } else {
        None
    };

    ChannelView {
        id: def.id.into(),
        name: def.name.into(),
        short: def.short.into(),
        color: def.color.into(),
        kind: result.kind,
        unstable: def.unstable,
        has_key: true,
        valid: result.valid,
        remaining: result.remaining,
        used: result.used,
        total: result.total,
        unit: result.unit,
        windows: result.windows,
        expiring: result.expiring,
        expiring_soon: soon,
        extra: result.extra,
        error: result.error,
        limited: result.limited,
        auth_source: match def.auth {
            AuthKind::Keyring => "keyring".into(),
            AuthKind::WebSession => "web".into(),
            AuthKind::App(_) => "app".into(),
        },
        auth_label: auth_label(def, cred),
        day,
        week,
        month,
        day_prev,
        week_prev,
        month_prev,
        // 套餐摊销要等本轮汇率与周期记忆都到位,统一在 fetch_all 的同步段挂上
        plan: None,
        estimated: result.kind == Kind::Amount,
        stale,
        updated_at,
        claim: claim_for(def, cfg, store, result.remaining, key_fp),
        hidden: false,
        peak,
        gap_day_from,
        gap_week_from,
        gap_month_from,
        gap_days,
        accounts,
    }
}

/// 窗口里"从几点起才有数据":首次观测离窗口起点超过空档线才算缺口。
/// None = 起点没缺口,或这个窗口压根没有观测 —— 后者是"没数据",不是"有缺口"。
fn gap_from(store: &Store, id: &str, key_fp: Option<&str>, from_ts: i64, now: i64) -> Option<i64> {
    let cov = store.window_coverage(id, key_fp, from_ts, now).ok()?;
    let first = cov.first_ts?;
    if first - from_ts > OBSERVE_GAP_SEC {
        Some(first)
    } else {
        None
    }
}

/// 一天算不算"部分数据":起点晚于本地零点、或(已经过去的那天)末尾提前收工,
/// 超过空档线。判据与 peak.rs / store.rs 里的注释同一套数字。
fn gap_of_day(d: &crate::store::DayCoverage, now: i64) -> bool {
    let leading = d.first_ts - d.day_start;
    let finished = d.day_start + 86_400 <= now;
    let trailing = if finished {
        (d.day_start + 86_400 - d.last_ts).max(0)
    } else {
        0
    };
    leading > OBSERVE_GAP_SEC || trailing > OBSERVE_GAP_SEC
}

/// 没有可用凭据的渠道占位。这不是故障,只是没启用。
fn placeholder(def: &providers::ProviderDef) -> ChannelView {
    ChannelView {
        id: def.id.into(),
        name: def.name.into(),
        short: def.short.into(),
        color: def.color.into(),
        kind: def.default_kind,
        unstable: def.unstable,
        has_key: false,
        valid: false,
        remaining: None,
        used: None,
        total: None,
        unit: match def.default_kind {
            Kind::Amount => "CNY".into(),
            _ => "credits".into(),
        },
        windows: vec![],
        expiring: vec![],
        expiring_soon: None,
        extra: vec![],
        error: None,
        limited: false,
        auth_source: match def.auth {
            AuthKind::Keyring => "keyring".into(),
            AuthKind::WebSession => "web".into(),
            AuthKind::App(_) => "app".into(),
        },
        auth_label: auth_label(def, None),
        day: None,
        week: None,
        month: None,
        day_prev: None,
        week_prev: None,
        month_prev: None,
        plan: None,
        estimated: false,
        stale: false,
        updated_at: 0,
        claim: None,
        hidden: false,
        // 占位(没配密钥/已隐藏)不显示时段与缺口:没有数据可解释,少即是多
        peak: None,
        gap_day_from: None,
        gap_week_from: None,
        gap_month_from: None,
        gap_days: Vec::new(),
        accounts: 1,
    }
}

/// 自定义渠道的展示名/简称/颜色(没填的给一个体面的默认,不显示空字符串)。
fn custom_identity(ch: &custom::CustomChannel) -> (String, String, String) {
    let short = if ch.short.trim().is_empty() {
        ch.name.chars().take(2).collect::<String>()
    } else {
        ch.short.clone()
    };
    let color = if ch.color.trim().is_empty() {
        "#7f8ea3".to_string()
    } else {
        ch.color.clone()
    };
    (ch.name.clone(), short, color)
}

/// 自定义渠道的成品视图。与内置渠道共用消耗窗口/缺口/分段的全部口径。
fn build_custom_view(
    ch: &custom::CustomChannel,
    r: &custom::CustomResult,
    store: &Store,
    key_fp: &str,
    accounts: i64,
) -> ChannelView {
    let now = now_ts();
    // 探活模式没有数值:不装成"消耗"、不画日周月(它只回答"活着吗")
    let has_amount = r.valid && !r.probe_only && r.remaining.is_some();
    let (day, week, month, day_prev, week_prev, month_prev) = if has_amount {
        amount_windows(store, &ch.id, key_fp, now)
    } else {
        (None, None, None, None, None, None)
    };
    let fp_opt = if key_fp.is_empty() { None } else { Some(key_fp) };
    let (gap_day_from, gap_week_from, gap_month_from, gap_days) = if has_amount {
        let tz = local_tz_offset();
        (
            gap_from(store, &ch.id, fp_opt, local_midnight_today(), now),
            gap_from(store, &ch.id, fp_opt, local_week_start(), now),
            gap_from(store, &ch.id, fp_opt, local_month_start(), now),
            store
                .daily_coverage(&ch.id, fp_opt, now - 92 * 86_400, now, tz)
                .unwrap_or_default()
                .into_iter()
                .filter(|d| gap_of_day(d, now))
                .map(|d| d.day_start)
                .collect(),
        )
    } else {
        (None, None, None, Vec::new())
    };
    let (name, short, color) = custom_identity(ch);
    // 失败时"数据是什么时候的"要如实:沿用最近一次成功快照的时刻,不是此刻
    let updated_at = if r.valid {
        now
    } else {
        store
            .last_ts_since(&ch.id, fp_opt, 0)
            .ok()
            .flatten()
            .unwrap_or(0)
    };
    ChannelView {
        id: ch.id.clone(),
        name,
        short,
        color,
        kind: Kind::Amount,
        unstable: false,
        has_key: true,
        valid: r.valid,
        remaining: r.remaining,
        used: None,
        total: None,
        unit: ch.normalized_unit(),
        windows: Vec::new(),
        expiring: Vec::new(),
        expiring_soon: None,
        extra: vec![(
            "取数".to_string(),
            if r.probe_only {
                "探活模式(不解析余额)".to_string()
            } else {
                "自定义渠道".to_string()
            },
        )],
        error: r.error.clone(),
        limited: false,
        auth_source: "keyring".into(),
        auth_label: "自定义渠道 · 密码管理器".into(),
        day,
        week,
        month,
        day_prev,
        week_prev,
        month_prev,
        plan: None,
        estimated: has_amount,
        stale: !r.valid,
        updated_at,
        claim: None,
        hidden: false,
        peak: None,
        gap_day_from,
        gap_week_from,
        gap_month_from,
        gap_days,
        accounts,
    }
}

/// 自定义渠道的占位(没填密钥 / 已隐藏)。
fn custom_placeholder(
    ch: &custom::CustomChannel,
    hidden: bool,
    error: Option<String>,
) -> ChannelView {
    let (name, short, color) = custom_identity(ch);
    ChannelView {
        id: ch.id.clone(),
        name,
        short,
        color,
        kind: Kind::Amount,
        unstable: false,
        has_key: false,
        valid: false,
        remaining: None,
        used: None,
        total: None,
        unit: ch.normalized_unit(),
        windows: Vec::new(),
        expiring: Vec::new(),
        expiring_soon: None,
        extra: vec![("取数".to_string(), "自定义渠道".to_string())],
        error,
        limited: false,
        auth_source: "keyring".into(),
        auth_label: "自定义渠道 · 密码管理器".into(),
        day: None,
        week: None,
        month: None,
        day_prev: None,
        week_prev: None,
        month_prev: None,
        plan: None,
        estimated: false,
        stale: false,
        updated_at: 0,
        claim: None,
        hidden,
        peak: None,
        gap_day_from: None,
        gap_week_from: None,
        gap_month_from: None,
        gap_days: Vec::new(),
        accounts: 1,
    }
}

/// 一次「测试连接」的结果。
///
/// **只带域名、状态与解析出的数值** —— 报告是要用户复制给作者看的东西,
/// 密钥、Cookie、完整响应体一律不进(与"密钥不进任何文件"同一条纪律)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeReport {
    pub id: String,
    pub name: String,
    pub ok: bool,
    /// 这次用的凭据是从哪读的(人话,如「本机 Trae 登录」/「密码管理器」)
    pub auth_label: String,
    /// 实际打通的入口(主域名还是某个备用域名)
    pub url_used: Option<String>,
    /// 总耗时(含凭据读取)
    pub elapsed_ms: u64,
    pub remaining: Option<f64>,
    pub used: Option<f64>,
    pub total: Option<f64>,
    pub unit: String,
    pub error: Option<String>,
    /// 本地那一半:最新一条快照的时刻与近 7 天的条数
    pub snapshot_last_at: Option<i64>,
    pub snapshot_count_7d: i64,
    /// 这个渠道用过几把密钥(> 1 = 有历史账户)
    pub accounts: i64,
}

/// 单渠道并发取数的产出:要么自带成品视图(占位/隐藏),
/// 要么留 (result, stale, ts, cred) 给同步段组视图;success 记快照与缓存。
struct FetchedOne {
    i: usize,
    def: &'static providers::ProviderDef,
    /// 本轮的密钥指纹(空串 = 这个渠道不按密钥分段:会话型凭据会轮换,分不了);
    /// 快照写入与消耗推算都按它分段
    fp: String,
    view: Option<ChannelView>,
    pending: Option<(FetchResult, bool, i64, Option<appauth::Credential>)>,
    success: Option<(FetchResult, i64)>,
}

/// 取数器。持有 HTTP 客户端和上次成功结果的缓存,失败时用于降级显示。
pub struct Fetcher {
    pub http: reqwest::Client,
    cache: Mutex<HashMap<String, (FetchResult, i64)>>,
    /// 每个渠道上次成功的入口地址。本轮先试它,避免每分钟都先撞挂掉的域名。
    /// 只存内存:重启后退回定义顺序,不往磁盘写状态。
    url_memo: Mutex<HashMap<String, String>>,
    /// 上次成功解析的配置。DB 读不到/解析失败时沿用 —— 隐藏渠道不该被静默放开(P2-11)
    cfg_cache: Mutex<Option<Config>>,
    /// fetch_shared 的门 + 近期结果:手动刷新撞上轮询不再双份请求账务接口(O-4)
    gate: tokio::sync::Mutex<()>,
    last_run: Mutex<Option<(i64, Vec<ChannelView>)>>,
    /// 网页登录型渠道的会话来源。外壳在启动时装一个"去 webview 的 Cookie 罐里现取"
    /// 的回调:会话留在 WebView2 的 profile 里(平台自己管过期与轮换),挂件不存副本 ——
    /// 一个是凭据管理器放不下(实测 blob 上限约 1280 字符,而两个会话 Cookie 近 6 KB),
    /// 另一个是平台一旦轮换 Cookie,存下来的副本会静默失效。
    web_cred_src: Mutex<Option<Arc<dyn Fn(&str) -> Option<String> + Send + Sync>>>,
    /// 已经认领过"无指纹历史快照"的 (渠道 + 指纹)。认领本身是一次性动作,
    /// 这个集合只是让每轮不必再跑那条 UPDATE。
    adopted: Mutex<HashSet<String>>,
    /// 每本账"有几本账"的缓存:COUNT(DISTINCT key_fp) 是整渠道扫描,
    /// 不该每轮都跑;指纹没变就直接复用。
    accounts_cache: Mutex<HashMap<String, i64>>,
    /// 被服务器明确拒过(401/403)的凭据 → 拒绝时刻。
    ///
    /// 2026-10-20 真机:用户很久以前粘贴过一个手动令牌(早已作废),它排在桌面端凭据
    /// **前面**(用户明确填的东西优先),于是每轮都先打一次 401,白等一个往返。
    /// 修好"401 该换下一份"之后已经能取到数了,但这份废令牌仍在每轮重试 ——
    /// 按 `key_fp` 记账,被拒过的排到候选末尾(不是删除、也不是永久跳过:
    /// 用户重新登录/平台恢复后,过了 [`REJECTED_TTL`] 自然回到原位)。
    rejected: Mutex<HashMap<String, i64>>,
}

impl Default for Fetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .user_agent("TokenScope/0.1")
                .build()
                .expect("HTTP 客户端初始化失败"),
            cache: Mutex::new(HashMap::new()),
            url_memo: Mutex::new(HashMap::new()),
            cfg_cache: Mutex::new(None),
            gate: tokio::sync::Mutex::new(()),
            last_run: Mutex::new(None),
            web_cred_src: Mutex::new(None),
            adopted: Mutex::new(HashSet::new()),
            accounts_cache: Mutex::new(HashMap::new()),
            rejected: Mutex::new(HashMap::new()),
        }
    }

    /// 装上网页登录型渠道的会话来源(见字段注释)。**回调是阻塞的**(要同步问 webview 的
    /// Cookie 罐),只能在非主线程上被调用 —— 取数路径本来就跑在 async worker 上,安全。
    pub fn set_web_credential_source(
        &self,
        f: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
    ) {
        *self
            .web_cred_src
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(Arc::from(f));
    }

    /// 作废"5 秒内复用上次结果"的窗口。登录刚完成 / 密钥刚改过时必须调一次,
    /// 否则那一次刷新会拿旧结果回给界面,用户看到的是"已经登录了但还没有数"。
    pub fn invalidate(&self) {
        *self.last_run.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// 用一份**还没生效**的凭据试打一次:网页登录抓到会话之后,先证明这份会话真能取数,
    /// 再决定要不要关窗、报"已登录"。不验证就报成功的话,一份被平台拒掉的会话会变成
    /// "状态显示已登录、却一直取不到数"—— 用户只能靠猜。
    pub async fn probe(&self, def: &providers::ProviderDef, credential: &str) -> bool {
        let (result, _, _) = fetch_channel(&self.http, def, credential, None).await;
        result.valid
    }

    /// 「测试连接」:真打一次接口,把过程如实报出来(设置里那个逐渠道测试按钮)。
    ///
    /// 与轮询**共用同一条取数链路**,所以"测试通过"和"平时取得到"是同一件事;
    /// 但**不写快照** —— 手动测试要是落了快照,会把当天的观测缺口悄悄填上
    /// (测试一下就把"今天 10:23 才开机"的记录改掉了),那是篡改自己的数据。
    pub async fn probe_report(
        &self,
        def: &'static providers::ProviderDef,
        store: &Arc<Mutex<Store>>,
    ) -> ProbeReport {
        let started = std::time::Instant::now();
        let web_src = self
            .web_cred_src
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let (creds, secret_err) =
            tokio::task::spawn_blocking(move || credentials(def, web_src.as_deref()))
                .await
                .unwrap_or_else(|_| (Vec::new(), Some("凭据读取异常".to_string())));
        // 本地这一半:快照管道有没有在动(照小鲸鱼的"两半区自检"—— 网络通了不等于
        // 记录在写,两半都要各证一半)。
        let (last_at, count_7d, accounts) = {
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            let fp = if matches!(def.auth, AuthKind::Keyring) {
                creds.first().map(|c| key_fp(&c.token)).unwrap_or_default()
            } else {
                String::new()
            };
            let fpo = if fp.is_empty() { None } else { Some(fp.as_str()) };
            let now = now_ts();
            (
                s.last_ts_since(def.id, fpo, 0).ok().flatten(),
                s.window_coverage(def.id, fpo, now - 7 * 86_400, now)
                    .map(|c| c.count)
                    .unwrap_or(0),
                s.fingerprint_count(def.id).unwrap_or(1).max(1),
            )
        };
        let mut rep = ProbeReport {
            id: def.id.to_string(),
            name: def.name.to_string(),
            ok: false,
            auth_label: auth_label(def, creds.first()),
            url_used: None,
            elapsed_ms: 0,
            remaining: None,
            used: None,
            total: None,
            unit: unit_for(def.default_kind).to_string(),
            error: None,
            snapshot_last_at: last_at,
            snapshot_count_7d: count_7d,
            accounts,
        };
        let Some(cred) = creds.first() else {
            rep.error = Some(
                secret_err.unwrap_or_else(|| format!("没检测到 {} 的登录凭据", def.name)),
            );
            rep.elapsed_ms = started.elapsed().as_millis() as u64;
            return rep;
        };
        // 轮询记住的入口优先:测试要回答的是"平时这条路通不通,用的哪个域名"
        let memo = self
            .url_memo
            .lock()
            .ok()
            .and_then(|m| m.get(def.id).cloned());
        let (r, url, _kind) = fetch_channel(&self.http, def, &cred.token, memo.as_deref()).await;
        rep.ok = r.valid;
        rep.url_used = url;
        rep.remaining = r.remaining;
        rep.used = r.used;
        rep.total = r.total;
        rep.unit = r.unit.clone();
        rep.error = r.error;
        // 耗时含凭据读取(既有 network 也有 IO),如实叫"总耗时"
        rep.elapsed_ms = started.elapsed().as_millis() as u64;
        rep
    }


    /// get_channels / 托盘「立即刷新」共用的入口:5 秒内刚跑完一轮直接复用结果,
    /// 并发调用在同一把门锁上排队 —— 账务接口有速率顾虑,双份轮询不礼貌(O-4)。
    pub async fn fetch_shared(&self, store: &Arc<Mutex<Store>>) -> Vec<ChannelView> {
        let _g = self.gate.lock().await;
        {
            let last = self.last_run.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((ts, views)) = last.as_ref() {
                // 必须同时要求 *ts <= now_ts():用墙钟做减法,系统时间被往前校正后
                // 差值为负、恒小于 5,界面就"永久不再更新"了(报告 P2-23)
                if *ts <= now_ts() && now_ts() - *ts < 5 {
                    return views.clone();
                }
            }
        }
        let views = self.fetch_all(store).await;
        *self.last_run.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((now_ts(), views.clone()));
        views
    }

    /// 取全部渠道。
    ///
    /// 结构上刻意分成两段:**await 段**只做网络请求(全渠道**并发**),
    /// **同步段**才锁库读写。因为 `std::sync::MutexGuard` 不是 Send,一旦跨越
    /// await 就会让整个 future 失去 Send,而 Tauri 的命令要求 future 是 Send。
    /// 并行化原因:单入口超时预算 8s、每渠道最多 4 入口 × 多凭据,串行时一个
    /// 挂掉的域名会把整轮拖到分钟级;join_all 后整轮最坏 ≈ 单渠道最坏。
    pub async fn fetch_all(&self, store: &Arc<Mutex<Store>>) -> Vec<ChannelView> {
        let n = providers::PROVIDERS.len();
        let mut out: Vec<Option<ChannelView>> = (0..n).map(|_| None).collect();

        // ── await 段:纯网络,不碰数据库 ──
        // 每轮开头读一次配置(隐藏清单与申请制规则都从它来;之前每轮 load 两遍,
        // 报告 O-12)。解析失败绝不回落 default —— 那会把隐藏渠道静默"解除隐藏"
        // 恢复取数(扫描报告 P2-11):沿用上次成功的配置,首轮无缓存才用默认并记日志。
        let cfg: Arc<Config> = Arc::new({
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            let mut cache = self.cfg_cache.lock().unwrap_or_else(|p| p.into_inner());
            match Config::try_load(&s) {
                Ok(c) => {
                    *cache = Some(c.clone());
                    c
                }
                Err(e) => {
                    eprintln!("{e},本轮沿用上次的配置");
                    cache.clone().unwrap_or_default()
                }
            }
        });
        // 隐藏 = 不发请求、不读本机登录凭据;仍返回占位视图,管理页要靠它列「已隐藏」卡片。
        // Arc:并发时每渠道克隆一次指针,而不是把 Vec 移进每个 future。
        let hidden: Arc<Vec<String>> = Arc::new(cfg.hidden_channels.clone());
        // 订阅套餐要用的两份缓存:美元牌价(每月一次)与周期记忆(跨期才更新)。
        // 单独一次短锁读出来 —— 下面的 refresh 会 await,不能带着 MutexGuard 跨过去。
        let (fx_cached, cycle_cached): (Option<FxRate>, Vec<Option<CycleMemo>>) = {
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            (fx::load(&s), load_cycles(&s))
        };
        // 当月已有牌价时这个 await 是纯内存返回,不打网络;只有跨月后的第一轮才会
        // 真去试两个公开接口(每个 6 秒上限)。
        let fx_now = fx::refresh(&self.http, fx_cached.clone(), now_ts()).await;
        // 网页登录型的会话来源(外壳注入)。每轮取一次,每渠道克隆一个指针。
        let web_src: Option<Arc<dyn Fn(&str) -> Option<String> + Send + Sync>> =
            self.web_cred_src
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();

        let fetched = futures::future::join_all(
            providers::PROVIDERS
                .iter()
                .enumerate()
                .map(|(i, def)| {
                    let hidden = hidden.clone();
                    let web_src = web_src.clone();
                    async move {
                    // 隐藏 = 不发请求、**也不读本机登录凭据**(裁决原文)。之前的顺序
                    // 反了:credentials() 先跑,隐藏的 App 型渠道每轮仍读本地文件 + 跑 AES
                    // + 查凭据管理器(扫描报告 P1-1)。
                    if hidden.iter().any(|h| h == def.id) {
                        let mut v = placeholder(def);
                        v.has_key = if matches!(def.auth, AuthKind::Keyring | AuthKind::WebSession) {
                            let id = def.id.to_string();
                            tokio::task::spawn_blocking(move || {
                                secrets::channel_key(&id).ok().flatten().is_some()
                            })
                            .await
                            .unwrap_or(false)
                        } else {
                            false // App 型:隐藏时连本地文件都不该碰
                        };
                        v.hidden = true;
                        return FetchedOne {
                            i,
                            def,
                            fp: String::new(),
                            view: Some(v),
                            pending: None,
                            success: None,
                        };
                    }
                    // 文件 IO + AES + keyring RPC 都是阻塞调用:下沉到 spawn_blocking,
                    // 凭据服务卡住不再拖垮 tokio worker 上的整轮取数(扫描报告 O-5)。
                    // 网页登录的会话来源也是阻塞的(要同步问 webview 的 Cookie 罐),
                    // 正好一起待在这个阻塞任务里。
                    let web_src = web_src.clone();
                    let (creds, secret_err) =
                        tokio::task::spawn_blocking(move || credentials(def, web_src.as_deref()))
                            .await
                            .unwrap_or_else(|_| (Vec::new(), Some("凭据读取异常".to_string())));
                    // 被服务器拒过(401/403)的凭据沉到末尾:一份早已作废的手动令牌排在
                    // 桌面端凭据前面会让每轮都先白打一个往返(2026-10-20 真机日志)。
                    // 只降权不丢弃 —— 用户重新登录/平台恢复后它还会被再试一次。
                    let creds = {
                        let now = now_ts();
                        let rej = self
                            .rejected
                            .lock()
                            .map(|m| m.clone())
                            .unwrap_or_default();
                        demote_rejected(creds, &rej, now, |t| key_fp(t))
                    };
                    // 密钥指纹只对"稳定的 API Key"有意义:网页会话 / 本机登录态的凭据
                    // 每轮都可能轮换,拿它分段等于每轮开一本新账 —— 那两类一律空串,
                    // 退回"不分段"的老口径(与后端 store::record_fp 的约定一致)。
                    // 用**库里现在存的那把钥匙**算,不用"这次试成功的那把":
                    // 取数失败时也要按当前 key 分段,否则失败期间的快照会落错账。
                    let fp: String = if matches!(def.auth, AuthKind::Keyring) {
                        creds
                            .first()
                            .map(|c| key_fp(&c.token))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    if creds.is_empty() {
                        // 「读不到凭据」经常是瞬时的:Codex 正在写 auth.json(文件被占)、
                        // 凭据管理器 RPC 卡一下、问 webview 要 Cookie 时 3 秒超时。
                        // 这种时候不该把已有的数字抹成「——」—— 沿用上次成功快照并标注时间,
                        // 与"取数失败"走同一条口径(报告 P1-4;本文件头第 3-4 行的原则)。
                        let reason = secret_err.unwrap_or_else(|| {
                            // 措辞按凭据类型分开:密钥型渠道说"登录凭据"会让人以为要去登录,
                            // 实际只是没填密钥(2026-10-20 用户日志里 4 个密钥型渠道都这么写,
                            // 看着像登录出了问题)。
                            match def.auth {
                                AuthKind::Keyring => format!("还没填 {} 的密钥", def.name),
                                _ => format!("没检测到 {} 的登录凭据(可能只是瞬时读不到)", def.name),
                            }
                        });
                        crate::diaglog::write_keyed(
                            def.id,
                            "no-cred",
                            &format!(
                                "没有可用凭据({}) · 凭据类型={}",
                                reason,
                                match def.auth {
                                    AuthKind::Keyring => "keyring",
                                    AuthKind::WebSession => "web(桌面端/网页会话/手动密钥)",
                                    AuthKind::App(_) => "本机登录态",
                                }
                            ),
                        );
                        let cached = self
                            .cache
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .get(def.id)
                            .cloned();
                        if let Some((mut prev, prev_ts)) = cached {
                            prev.valid = false;
                            prev.limited = false;
                            prev.error = Some(reason);
                            return FetchedOne {
                                i,
                                def,
                                fp,
                                view: None,
                                pending: Some((prev, true, prev_ts, None)),
                                success: None,
                            };
                        }
                        let mut v = placeholder(def);
                        v.error = Some(reason);
                        return FetchedOne {
                            i,
                            def,
                            fp: String::new(),
                            view: Some(v),
                            pending: None,
                            success: None,
                        };
                    }

                    let memo = self
                        .url_memo
                        .lock()
                        .ok()
                        .and_then(|m| m.get(def.id).cloned());

                    // 有多个凭据候选时依次试:只有"服务器明确说这份凭据不行"(401/403)
                    // 才换下一份 —— 网络不通时换凭据是白费功夫。
                    let mut chosen: Option<appauth::Credential> = None;
                    // 最后一次实际尝试过的凭据:失败时也要拿它标"来源",否则界面会说
                    // "未检测到登录信息"—— 明明检测到了,只是 token 过期(见 bug 报告 #2)
                    let mut last_cred: Option<appauth::Credential> = None;
                    let mut result = FetchResult::default();
                    let mut used_url = None;
                    // 每个候选凭据试一轮;诊断日志要知道"试了几份、最后卡在哪"。
                    let mut tried: Vec<String> = Vec::new();
                    for (ci, cred) in creds.iter().enumerate() {
                        last_cred = Some(cred.clone());
                        let desc = cred_desc(def, cred);
                        let (r, url, kind) =
                            fetch_channel(&self.http, def, &cred.token, memo.as_deref()).await;
                        result = r;
                        used_url = url;
                        tried.push(format!("{desc}→{}", if result.valid { "通" } else { "不通" }));
                        // 服务器明确说这份凭据不行:记下来,下一轮把它排到末尾。
                        // 成功了就把这条撤销(同一份凭据在别的入口下可能就是通的,
                        // 比如主域抖动时备用域 401、主域恢复后又能用)。
                        let cfp = key_fp(&cred.token);
                        if let Ok(mut m) = self.rejected.lock() {
                            if result.valid {
                                m.remove(&cfp);
                            } else if another_credential_may_help(kind) {
                                m.insert(cfp, now_ts());
                            }
                        }
                        if result.valid {
                            chosen = Some(cred.clone());
                            break;
                        }
                        if another_credential_may_help(kind) {
                            // L1:服务器明确拒了这份 → 换下一份。记下"拒了哪份、用哪份顶替"。
                            // 这一行平时不出现(凭据没被拒时不会走到),出现就说明换过了。
                            let next = creds.get(ci + 1).map(|c| cred_desc(def, c));
                            crate::diaglog::write_keyed(
                                def.id,
                                "cred-switch",
                                &format!(
                                    "换下一份凭据:{desc} 被拒 → {}",
                                    next.unwrap_or_else(|| "没有更多候选了".into())
                                ),
                            );
                            continue;
                        }
                        break;
                    }
                    // 渠道级一行:失败必记(节流),成功只在"从失败变成功"时有信息量,
                    // 由 diaglog 的签名去重自然处理 —— 同一成功状态不会反复落。
                    crate::diaglog::write_keyed(
                        def.id,
                        "channel",
                        &if result.valid {
                            format!(
                                "取数成功 · 凭据 [{}] · 入口 {}",
                                tried.join("，"),
                                used_url.as_deref().unwrap_or("?")
                            )
                        } else {
                            format!(
                                "取数失败 · 凭据 [{}] · 原因:{}",
                                if tried.is_empty() { "无可用凭据".into() } else { tried.join("，") },
                                result.error.as_deref().unwrap_or("未知")
                            )
                        },
                    );
                    let ts = now_ts();

                    if should_clear_memo(result.valid, used_url.is_some()) {
                        if let Ok(mut m) = self.url_memo.lock() {
                            m.remove(def.id);
                        }
                    }
                    if let (true, Some(url)) = (result.valid, used_url.as_ref()) {
                        if let Ok(mut m) = self.url_memo.lock() {
                            m.insert(def.id.to_string(), url.clone());
                        }
                    }

                    // 失败也要给界面一个"凭据是从哪读的",否则 App 型渠道的说明会退化成
                    // "未检测到登录信息",与真正的失败原因(token 过期)自相矛盾
                    let shown_cred = chosen.or_else(|| last_cred.clone());
                    if result.valid {
                        FetchedOne {
                            i,
                            def,
                            fp,
                            view: None,
                            pending: Some((result.clone(), false, ts, shown_cred)),
                            success: Some((result, ts)),
                        }
                    } else {
                        // 失败降级:沿用上次成功值,标注时间,绝不显示成 0
                        let cached = self
                            .cache
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .get(def.id)
                            .cloned();
                        let p = match cached {
                            Some((mut prev, prev_ts)) => {
                                prev.error = result.error;
                                prev.valid = false;
                                // 失联后"已限流"已成无法再确认的旧事实,别再挂着报警
                                // (扫描报告 P2-10);数值沿用快照并标注时间即可。
                                prev.limited = false;
                                (prev, true, prev_ts, shown_cred)
                            }
                            None => (result, false, ts, shown_cred),
                        };
                        FetchedOne {
                            i,
                            def,
                            fp,
                            view: None,
                            pending: Some(p),
                            success: None,
                        }
                    }
                    }
                }),
        )
        .await;

        let cache_updates: Vec<(String, FetchResult, i64)> = fetched
            .iter()
            .filter_map(|f| {
                f.success
                    .as_ref()
                    .map(|(r, ts)| (f.def.id.to_string(), r.clone(), *ts))
            })
            .collect();

        // ── 自定义渠道(裁决 6A):没有静态定义,单独取 ──
        // 数量少(个位数),串行即可;凭据走同一个凭据管理器(账户名 = 渠道 id),
        // 所以"自定义"在密钥纪律上和内置渠道没有任何区别。
        let mut custom_results: Vec<(custom::CustomChannel, Option<custom::CustomResult>, String)> =
            Vec::new();
        for ch in cfg.custom_channels.iter() {
            if hidden.iter().any(|h| h == &ch.id) {
                continue; // 隐藏 = 连凭据都不读(与内置渠道同一条裁决)
            }
            let cid = ch.id.clone();
            let key = tokio::task::spawn_blocking(move || secrets::channel_key(&cid).ok().flatten())
                .await
                .unwrap_or(None);
            match key {
                None => custom_results.push((ch.clone(), None, String::new())),
                Some(k) => {
                    let fp = key_fp(&k);
                    let r = custom::fetch_custom(&self.http, ch, &k).await;
                    custom_results.push((ch.clone(), Some(r), fp));
                }
            }
        }
        let mut custom_views: Vec<ChannelView> = Vec::new();

        // ── 同步段:一次性锁库完成快照写入与消耗推算,期间不跨 await ──
        {
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            for f in &fetched {
                // 升级上来的库里,历史行还没有指纹:首次拿到稳定指纹时认领一次,
                // 让"还没换过 key"的用户的账目连续(见 store::adopt_orphan_snapshots)。
                if !f.fp.is_empty() {
                    let first = {
                        let mut done = self.adopted.lock().unwrap_or_else(|p| p.into_inner());
                        done.insert(format!("{}:{}", f.def.id, f.fp))
                    };
                    if first {
                        if let Err(e) = s.adopt_orphan_snapshots(f.def.id, &f.fp) {
                            eprintln!("认领无指纹历史快照失败 {}: {e}", f.def.id);
                        }
                    }
                }
                if let Some((res, ts)) = f.success.as_ref() {
                    let kind = match res.kind {
                        Kind::Amount => "amount",
                        Kind::Percent => "percent",
                        Kind::Points => "points",
                    };
                    // 注意顺序:先写快照再组视图 —— 申请制额度的跳升检测要用最新快照
                    if let Err(e) =
                        s.record_fp(f.def.id, &f.fp, *ts, res.remaining, res.used, res.total, kind)
                    {
                        // 写库失败以前是 let _ = 静默吞掉:消耗会悄悄偏小且无从排查(O-7)
                        eprintln!("写入快照失败 {}: {e}", f.def.id);
                    }
                }
            }
            // 配置用本轮开头读好的那份(以前这里还会再 load 一遍,报告 O-12)
            for f in fetched {
                if let Some(v) = f.view {
                    out[f.i] = Some(v);
                } else if let Some((result, stale, ts, cred)) = f.pending {
                    // "这个渠道用过几把密钥"是整渠道扫描,按指纹缓存,别每轮都跑
                    let accounts = if f.fp.is_empty() {
                        1
                    } else {
                        let mut cache = self
                            .accounts_cache
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
                        match cache.get(&f.fp) {
                            Some(n) => *n,
                            None => {
                                let n = s.fingerprint_count(f.def.id).unwrap_or(1).max(1);
                                cache.insert(f.fp.clone(), n);
                                n
                            }
                        }
                    };
                    out[f.i] = Some(build_view(
                        f.def,
                        result,
                        stale,
                        ts,
                        &s,
                        &cfg,
                        cred.as_ref(),
                        &f.fp,
                        accounts,
                    ));
                }
            }
            // 牌价只在"跨月取到了新数"那一轮落库,不是每轮都写
            if let Some(f) = fx_now.as_ref() {
                if fx_cached.as_ref() != Some(f) {
                    if let Err(e) = fx::save(&s, f) {
                        eprintln!("写入汇率缓存失败: {e}");
                    }
                }
            }
            // 自定义渠道的视图在这里组装(快照写入也在这一段的锁里)
            for ch in cfg.custom_channels.iter() {
                if cfg.hidden_channels.iter().any(|h| h == &ch.id) {
                    custom_views.push(custom_placeholder(ch, true, None));
                    continue;
                }
                let Some((_, res, fp)) = custom_results.iter().find(|(c, _, _)| c.id == ch.id) else {
                    custom_views.push(custom_placeholder(ch, false, None));
                    continue;
                };
                let Some(r) = res else {
                    custom_views.push(custom_placeholder(ch, false, None));
                    continue;
                };
                // 历史行认领(与内置渠道同一套);探活模式不写快照 ——
                // 它没有数值,"记一笔"只会把当天的观测缺口悄悄填上
                if !fp.is_empty() {
                    let first = {
                        let mut done = self.adopted.lock().unwrap_or_else(|p| p.into_inner());
                        done.insert(format!("{}:{}", ch.id, fp))
                    };
                    if first {
                        if let Err(e) = s.adopt_orphan_snapshots(&ch.id, fp) {
                            eprintln!("认领无指纹历史快照失败 {}: {e}", ch.id);
                        }
                    }
                    if r.valid && !r.probe_only {
                        if let Some(v) = r.remaining {
                            if let Err(e) =
                                s.record_fp(&ch.id, fp, now_ts(), Some(v), None, None, "amount")
                            {
                                eprintln!("写入快照失败 {}: {e}", ch.id);
                            }
                        }
                    }
                }
                let accounts = if fp.is_empty() {
                    1
                } else {
                    let mut cache = self
                        .accounts_cache
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    match cache.get(fp) {
                        Some(n) => *n,
                        None => {
                            let n = s.fingerprint_count(&ch.id).unwrap_or(1).max(1);
                            cache.insert(fp.clone(), n);
                            n
                        }
                    }
                };
                custom_views.push(build_custom_view(ch, r, &s, fp, accounts));
            }
            attach_plan_costs(&mut out, &s, &cycle_cached, fx_now.as_ref());
        }

        for (id, result, ts) in cache_updates {
            if let Ok(mut c) = self.cache.lock() {
                c.insert(id, (result, ts));
            }
        }

        // 内置渠道在前、自定义渠道在后(顺序由前端的排序/自定义排序再排一次)
        let mut all: Vec<ChannelView> = out.into_iter().flatten().collect();
        all.extend(custom_views);
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    /// 2026-10-21:端到端验一次"取数失败 → 日志里有一行可用的现场"。
    ///
    /// 这条用例守的是整套日志的**存在意义**:以前 9 个内置渠道里只有 WorkBuddy
    /// 那一路有日志,其余的失败时日志里只有"取数失败"四个字。
    /// 现在 `fetch_once` 埋一行 L2 —— URL、状态码、耗时、错误码全在里面。
    #[test]
    fn a_failed_request_leaves_a_usable_line_in_the_log() {
        let _lock = crate::diaglog::test_lock();
        let dir = std::env::temp_dir().join(format!("ts-fetch-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _arm = crate::diaglog::arm_test_dir(dir.clone());

        // 127.0.0.1:1 —— 连都不连得上,必然失败,而且不依赖外网
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        let def = providers::find("4sapi").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_res, _kind) = rt.block_on(fetch_once(
            &client,
            def,
            "http://127.0.0.1:1/v1/usage",
            "sk-test-key",
        ));

        let text = crate::diaglog::read_log();
        assert!(!text.is_empty(), "失败必须留下日志");
        assert!(text.contains("E_"), "要有错误码:{text}");
        assert!(text.contains("127.0.0.1:1"), "要有 URL:{text}");
        assert!(text.contains("ms="), "要有耗时字段:{text}");
        assert!(text.contains("host=127.0.0.1"), "要有主机名:{text}");
        // 连接都没连上 → **没有 HTTP 状态码**。这时刻意不写 status=
        // (写一个空状态码会让人以为"服务器回了个空状态")。
        assert!(!text.contains("status="), "连不上时不该有 status=:{text}");
        // 密钥绝不能出现
        assert!(!text.contains("sk-test-key"), "密钥不该出现在日志里:{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2026-10-21:状态码 → 类别 的**两套映射不能打架**。
    ///
    /// 取数层把 401/403/407 判成 `Auth`(该换下一份凭据),
    /// 错误码层必须给同一类结论(要去登录)—— 否则日志里的码会把人引向错误的动作。
    /// 两套映射以前各写一份,407 那边就对不上(取数层当"凭据被拒",
    /// 码层说"请求被拒绝,等一等")。
    #[test]
    fn status_code_mappings_agree() {
        for code in 400..=599u16 {
            let kind = kind_for_status(code);
            let ec = crate::errcodes::for_status(code);
            assert!(ec.starts_with("E_"), "{code} 没有对应错误码:{ec}");
            if kind == FailKind::Auth {
                assert_eq!(
                    crate::errcodes::action(ec),
                    crate::errcodes::Action::Login,
                    "{code} 在取数层算凭据被拒,码层也要说'去登录'"
                );
            }
            // 5xx / 429 在取数层是可重试的,码层也不该说"要用户动手"
            if kind == FailKind::Server {
                assert_eq!(
                    crate::errcodes::action(ec),
                    crate::errcodes::Action::Wait,
                    "{code} ���服务端/限流问题,不该要用户动手"
                );
            }
        }
    }

    /// 2026-10-21:失败落盘要带上文 —— 单看"HTTP 401"没用,
    /// 看到"之前换过域名、换过凭据"才知道是谁的问题。
    #[test]
    fn a_failure_carries_the_events_that_came_before_it() {
        let _lock = crate::diaglog::test_lock();
        let dir = std::env::temp_dir().join(format!("ts-fetch-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _arm = crate::diaglog::arm_test_dir(dir.clone());

        // 先来几条"之前发生过的事"(成功的事件默认不落盘,但进内存环形缓冲)
        crate::diaglog::result(
            "fetch",
            Some("4sapi"),
            "cred",
            &[("cred_source", "手动粘贴")],
        );
        crate::diaglog::result(
            "fetch",
            Some("4sapi"),
            "attempt",
            &[("host", "api.4sapi.com")],
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        let def = providers::find("4sapi").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_res, _kind) = rt.block_on(fetch_once(
            &client,
            def,
            "http://127.0.0.1:1/v1/usage",
            "sk-test-key",
        ));

        let text = crate::diaglog::read_log();
        assert!(text.contains("↳ 上文"), "失败要带上文:{text}");
        assert!(text.contains("cred_source=手动粘贴"), "上文里有之前的事件:{text}");
        assert!(text.contains("api.4sapi.com"), "上文里有之前的决策:{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2026-10-21:URL 里的查询参数如果带密钥,也不能落进日志。
    #[test]
    fn a_secret_in_the_query_string_never_reaches_the_log() {
        let _lock = crate::diaglog::test_lock();
        let dir = std::env::temp_dir().join(format!("ts-fetch-log2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _arm = crate::diaglog::arm_test_dir(dir.clone());

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        let def = providers::find("4sapi").unwrap();
        let secret = "SUPERSECRETVALUE1234567890ABCDEFGH";
        let url = format!("http://127.0.0.1:1/v1/usage?api_key={secret}");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_res, _kind) = rt.block_on(fetch_once(&client, def, &url, "sk-test-key"));

        let text = crate::diaglog::read_log();
        assert!(!text.contains(secret), "查询参数里的密钥不该落盘:{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 单测里把"读桌面端凭据"关掉:它要起 WorkBuddy 进程、还依赖本机装没装,
    /// 两条都不该出现在单元测试里。测这条路径的用例自己 set_desktop_reader。
    fn _init_desktop_off() {
        disable_desktop_reader();
    }

    fn test_rate() -> FxRate {
        FxRate {
            rate: 7.0,
            month: "2026-10".into(),
            src: "test".into(),
        }
    }

    /// `attach_plan_costs` 按 PROVIDERS 下标读写,所以测试也得造**同长同序**的向量:
    /// 以前测试里递的是长度 1 的 `[memo]`,恰好替生产代码把 P1 那个下标错位遮住了。
    fn pidx(id: &str) -> usize {
        providers::PROVIDERS
            .iter()
            .position(|d| d.id == id)
            .expect("渠道在表里")
    }

    fn aligned_views(v: ChannelView) -> Vec<Option<ChannelView>> {
        let i = pidx(&v.id);
        let mut out: Vec<Option<ChannelView>> =
            (0..providers::PROVIDERS.len()).map(|_| None).collect();
        out[i] = Some(v);
        out
    }

    fn aligned_cycles(pairs: &[(&str, Option<CycleMemo>)]) -> Vec<Option<CycleMemo>> {
        providers::PROVIDERS
            .iter()
            .map(|d| {
                pairs
                    .iter()
                    .find(|(id, _)| *id == d.id)
                    .and_then(|(_, m)| *m)
            })
            .collect()
    }

    fn plan_at(views: &[Option<ChannelView>], id: &str) -> Option<PlanCost> {
        views
            .iter()
            .flatten()
            .find(|v| v.id == id)
            .and_then(|v| v.plan.clone())
    }

    /// $10 / 10 天周期 / 汇率 7 → 每天 7 元。它只跟"过了多久"有关,与用了多少 token 无关。
    #[test]
    fn plan_cost_accrues_with_elapsed_time_only() {
        let d = 86_400.0;
        let today0 = 1_000_000;
        let c = plan_cost(
            10.0,
            10.0,
            &test_rate(),
            false,
            today0 + (0.5 * d) as i64, // 今天过了半天
            today0,
            today0 - 3 * d as i64, // 本周已过 3.5 天
            today0 - 5 * d as i64, // 本月已过 5.5 天
            today0 - 35 * d as i64,
        );
        assert!((c.per_day - 7.0).abs() < 1e-9);
        assert!((c.day - 3.5).abs() < 1e-6, "半天 → 3.5 元");
        assert!((c.week - 24.5).abs() < 1e-3);
        assert!((c.month - 38.5).abs() < 1e-3);
        // 上期"同时段"长度与本期一样,所以订阅这一块的环比天然接近 ±0%
        assert!((c.day_prev - c.day).abs() < 1e-6);
        assert!((c.week_prev - c.week).abs() < 1e-3);
        assert!(!c.learned);
        assert_eq!(c.rate_month, "2026-10");
    }

    /// 上月只有 28 天、本月已过 29 天:上月同期必须钳在 28 天之内,
    /// 否则多出来的一天会把基线抬高,"本月环比"凭空少一截(报告 P2-4 同款坑)。
    #[test]
    fn plan_cost_prev_month_never_leaks_into_this_month() {
        let d = 86_400.0;
        let month0 = 40 * d as i64;
        let prev0 = month0 - 28 * d as i64;
        let now = month0 + 29 * d as i64;
        let c = plan_cost(
            10.0,
            10.0,
            &test_rate(),
            true,
            now,
            now - 3_600,
            month0 - 7 * d as i64,
            month0,
            prev0,
        );
        assert!(
            c.month_prev < c.per_day * 28.0 + 1e-6,
            "上月同期不该超过上月本身的长度: {:?}",
            c.month_prev
        );
        assert!(c.month_prev > c.per_day * 27.0);
    }

    #[test]
    fn cycle_length_is_learned_only_from_a_real_reset() {
        let d = 86_400i64;
        let a = learn_cycle(None, 100);
        assert_eq!(a.days, None, "只看到一次重置,还学不到长度");
        assert_eq!(learn_cycle(Some(a), 100), a, "同一周期内重复观察不该改动任何东西");
        assert_eq!(
            learn_cycle(Some(a), 100 + 2 * d),
            a,
            "服务端调表 2 天不足以断定跨了一期,否则会学出个「2 天周期」"
        );
        let b = learn_cycle(Some(a), 100 + 30 * d);
        assert_eq!(b.days, Some(30.0));
        assert_eq!(
            learn_cycle(Some(b), 100),
            b,
            "时钟被往前校正时重置时刻会倒退,这时必须原样返回"
        );
        let c = learn_cycle(Some(b), 100 + 130 * d);
        assert_eq!(c.days, Some(30.0), "100 天的间隔像改了套餐,保留旧周期只挪锚点");
        assert_eq!(c.last_reset, 100 + 130 * d);
    }

    /// 没跨过真实重置之前,用"重置日往前推一个整月"当本期长度 —— 第一天就准。
    #[test]
    fn cycle_length_is_inferred_from_the_reset_date() {
        assert!(
            (inferred_cycle_days(1_793_664_000).unwrap() - 31.0).abs() < 1e-9,
            "11-03 重置 → 10-03 起算,31 天"
        );
        assert!(
            (inferred_cycle_days(1_804_032_000).unwrap() - 28.0).abs() < 1e-9,
            "3-03 重置 → 2-03 起算,平年 28 天"
        );
        assert!(
            (inferred_cycle_days(1_709_596_800).unwrap() - 29.0).abs() < 1e-9,
            "2024 闰年 2 月是 29 天"
        );
        assert!(inferred_cycle_days(i64::MIN).is_none(), "荒谬时间戳不该 panic 也不该出数");
    }

    /// 周期长度三级口径:学到的 > 从重置日推的 > 30 天兜底。
    #[test]
    fn attach_prefers_observed_cycle_over_inference() {
        let s = Store::open_memory().unwrap();
        let def = providers::find("opencode-go").unwrap();
        let mut v = placeholder(def);
        v.windows = vec![providers::Window {
            label: "周期".into(),
            percent: 10.0,
            remain_percent: 90.0,
            status: "ok".into(),
            resets_at: Some("2026-11-03T00:00:00+00:00".into()),
            main: true,
        }];

        let mut observed = aligned_views(v.clone());
        attach_plan_costs(
            &mut observed,
            &s,
            &aligned_cycles(&[(
                "opencode-go",
                Some(CycleMemo {
                    last_reset: 1,
                    days: Some(10.0),
                }),
            )]),
            Some(&test_rate()),
        );
        let p = plan_at(&observed, "opencode-go").unwrap();
        assert!((p.days - 10.0).abs() < 1e-9, "已经学到的优先用");
        assert!(p.learned);

        let mut inferred = aligned_views(v);
        attach_plan_costs(&mut inferred, &s, &aligned_cycles(&[]), Some(&test_rate()));
        let p = plan_at(&inferred, "opencode-go").unwrap();
        assert!(
            (p.days - 31.0).abs() < 1e-9,
            "没学到时按重置日往前推一个整月,而不是 30 天"
        );
        assert!(!p.learned, "推出来的必须标明不是实测");
        assert!(inferred[pidx("opencode-go")]
            .as_ref()
            .unwrap()
            .extra
            .iter()
            .any(|(k, _)| k == "套餐摊销(按上一期长度)"));
    }

    /// F1:周期记忆是**按 PROVIDERS 下标**读的。opencode-go 在 PROVIDERS[1],以前
    /// `cycles` 只装了长度 1 的"有套餐的那几条" → `cycles.get(1)` 恒为 None,
    /// 学到的周期永远读不到、`save_cycle` 每轮白写一次库(P1)。
    /// 这条测试走的是生产的构造路径(load_cycles)与真实下标,不是手搓向量。
    #[test]
    fn plan_cost_reads_the_memo_at_the_providers_index() {
        let s = Store::open_memory().unwrap();
        let def = providers::find("opencode-go").expect("opencode-go 渠道存在");
        assert_eq!(pidx("opencode-go"), 1, "这条测试的前提:它不在下标 0 上");
        save_cycle(
            &s,
            "opencode-go",
            &CycleMemo {
                last_reset: 1,
                days: Some(30.0),
            },
        )
        .unwrap();

        let mut v = placeholder(def);
        v.windows = vec![providers::Window {
            label: "周期".into(),
            percent: 10.0,
            remain_percent: 90.0,
            status: "ok".into(),
            resets_at: Some("2026-12-03T00:00:00+00:00".into()),
            main: true,
        }];
        let mut out = aligned_views(v);
        let cycles = load_cycles(&s);
        assert_eq!(
            cycles.len(),
            providers::PROVIDERS.len(),
            "周期记忆必须与 views 同长"
        );
        attach_plan_costs(&mut out, &s, &cycles, Some(&test_rate()));
        let p = plan_at(&out, "opencode-go").expect("有汇率就该挂上摊销");
        assert!(p.learned, "库里那份记忆要在它的真实下标上被读到");
        assert_eq!(p.days, 30.0);
    }

    #[test]
    fn plan_cost_needs_a_rate_and_skips_hidden_and_usage_based_channels() {
        let s = Store::open_memory().unwrap();
        let def = providers::find("opencode-go").expect("opencode-go 渠道存在");

        let mut none = aligned_views(placeholder(def));
        attach_plan_costs(&mut none, &s, &aligned_cycles(&[]), None);
        assert!(
            plan_at(&none, "opencode-go").is_none(),
            "一次汇率都没取到时不许拿猜的数往合计里加钱"
        );

        let mut got = aligned_views(placeholder(def));
        attach_plan_costs(&mut got, &s, &aligned_cycles(&[]), Some(&test_rate()));
        let p = plan_at(&got, "opencode-go").expect("有汇率就该挂上");
        assert!((p.usd - 10.0).abs() < 1e-9);
        assert!((p.days - ASSUMED_CYCLE_DAYS).abs() < 1e-9);
        assert!(!p.learned, "没跨过真实重置前必须标明周期是按 30 天估的");
        assert!(p.day > 0.0);

        let mut hidden_view = placeholder(def);
        hidden_view.hidden = true;
        let mut hid = aligned_views(hidden_view);
        attach_plan_costs(&mut hid, &s, &aligned_cycles(&[]), Some(&test_rate()));
        assert!(
            plan_at(&hid, "opencode-go").is_none(),
            "隐藏的渠道不进合计,挂上等于偷偷把用户关掉的东西算回钱"
        );

        let mut amount = aligned_views(placeholder(providers::find("4sapi").unwrap()));
        attach_plan_costs(&mut amount, &s, &aligned_cycles(&[]), Some(&test_rate()));
        assert!(plan_at(&amount, "4sapi").is_none());
    }

    /// 观察到一次重置先只记锚点;下一次跨期才学出周期,并且落库(重启后还记得)。
    #[test]
    fn learned_cycle_survives_across_rounds() {
        let s = Store::open_memory().unwrap();
        let def = providers::find("opencode-go").unwrap();
        let mk = |reset: &str| {
            let mut v = placeholder(def);
            v.windows = vec![providers::Window {
                label: "周期".into(),
                percent: 50.0,
                remain_percent: 50.0,
                status: "ok".into(),
                resets_at: Some(reset.into()),
                main: true,
            }];
            v
        };
        let mut first = aligned_views(mk("2026-11-03T00:00:00+00:00"));
        attach_plan_costs(&mut first, &s, &aligned_cycles(&[]), Some(&test_rate()));
        let m1 = load_cycle(&s, "opencode-go").expect("第一次观察就该落锚点");
        assert_eq!(m1.days, None);

        let mut second = aligned_views(mk("2026-12-03T00:00:00+00:00"));
        attach_plan_costs(
            &mut second,
            &s,
            &aligned_cycles(&[("opencode-go", Some(m1))]),
            Some(&test_rate()),
        );
        let m2 = load_cycle(&s, "opencode-go").unwrap();
        let days = m2.days.expect("跨了一次真实重置,该学到周期长度");
        assert!((days - 30.0).abs() < 0.2, "11-03 → 12-03 是 30 天,实测学到 {days}");
        let p = plan_at(&second, "opencode-go").unwrap();
        assert!(p.learned);
        assert!((p.days - days).abs() < 1e-9);
    }

    /// WorkBuddy 改走网页登录(裁决 1A/3B)之后的界面口径:没登录时说"未登录 WorkBuddy
    /// 账号",auth_source 用 "web" 让前端出登录按钮。凭据的两种形态分开报 ——
    /// 会话是从宿主浏览器 profile 里现取的,cookie 前缀是它的标志(2026-10-08)。
    #[test]
    fn web_session_channel_reports_login_state_not_local_files() {
        let def = providers::find("workbuddy").expect("workbuddy 渠道存在");
        assert_eq!(def.auth, AuthKind::WebSession);
        assert_eq!(placeholder(def).auth_source, "web");
        assert_eq!(auth_label(def, None), "未登录 WorkBuddy 账号");
        let session = appauth::Credential {
            token: "cookie:session=A; session_2=B".into(),
            source: String::new(),
            device_id: None,
        };
        assert_eq!(auth_label(def, Some(&session)), "浏览器网页会话");
        let pasted = appauth::Credential {
            token: "eyJhbGciOi.abc".into(),
            source: String::new(),
            device_id: None,
        };
        assert_eq!(auth_label(def, Some(&pasted)), "手动粘贴的密钥");
        // 过期指引必须指对方向:WorkBuddy 现在首选桌面端(它会自动续期),
        // 网页会话那条仍说清"约 7 天属正常",免得每次过期都被当成平台又改了接口
        let msg = expired_credential_msg(def).expect("WorkBuddy 要有过期指引");
        assert!(msg.contains("桌面端"), "要先指向会自续期的桌面端:{msg}");
        assert!(msg.contains("重新登录"), "网页会话那条也要留着:{msg}");
    }

    /// 另外两类渠道不能被这次改造带偏:本机登录型仍说"打开一次 X",密钥型不瞎指路。
    #[test]
    fn other_auth_kinds_keep_their_own_wording() {
        let trae = providers::find("trae").expect("trae 渠道存在");
        assert_eq!(placeholder(trae).auth_source, "app");
        assert_eq!(auth_label(trae, None), "未检测到 Trae 登录信息");
        assert_eq!(
            expired_credential_msg(trae).as_deref(),
            Some("登录状态已失效 · 打开一次 Trae 即可")
        );

        let codex = providers::find("codex").expect("codex 渠道存在");
        assert_eq!(placeholder(codex).auth_source, "app");

        let ds = providers::find("deepseek").expect("deepseek 渠道存在");
        assert_eq!(placeholder(ds).auth_source, "keyring");
        assert_eq!(auth_label(ds, None), "Windows 凭据管理器");
        assert_eq!(expired_credential_msg(ds), None);
    }

    #[test]
    fn month_start_is_first_day() {
        let ts = local_month_start();
        let d = Local.timestamp_opt(ts, 0).unwrap();
        assert_eq!(d.day(), 1);
        assert_eq!(d.hour(), 0);
        assert_eq!(d.minute(), 0);
    }

    /// 上月起点:1 号零点,且早于本月起点;跨年时(1 月)要落到去年 12 月。
    #[test]
    fn prev_month_start_is_first_day_of_previous_month() {
        let prev = local_prev_month_start();
        let cur = local_month_start();
        let p = Local.timestamp_opt(prev, 0).unwrap();
        let c = Local.timestamp_opt(cur, 0).unwrap();
        assert_eq!(p.day(), 1);
        assert!(prev < cur);
        let (py, pm) = (p.year(), p.month());
        let (cy, cm) = (c.year(), c.month());
        assert!((pm == cm - 1 && py == cy) || (cm == 1 && pm == 12 && py == cy - 1));
    }

    #[test]
    fn week_start_is_monday() {
        let ts = local_week_start();
        let d = Local.timestamp_opt(ts, 0).unwrap();
        assert_eq!(d.weekday(), Weekday::Mon);
        assert_eq!(d.hour(), 0);
    }

    #[test]
    fn today_start_is_before_now_and_same_day() {
        let ts = local_midnight_today();
        let now = Local::now();
        assert!(ts <= now.timestamp());
        let d = Local.timestamp_opt(ts, 0).unwrap();
        assert_eq!(d.day(), now.day());
        assert_eq!(d.hour(), 0);
    }

    /// 申请制:按"上次申请 + 最短间隔"倒计时,到点变成可申请。
    #[test]
    fn claim_counts_down_until_eligible() {
        let cfg = ClaimConfig {
            enabled: true,
            amount: 200.0,
            min_interval_days: 14,
            ..Default::default()
        };
        let now = 1_700_000_000;
        let eight_days_ago = now - 8 * 86_400;

        let s = claim_state(&cfg, Some(eight_days_ago), now, Some(19.0), None).unwrap();
        assert_eq!(s.days_until_eligible, Some(6));
        assert!(!s.eligible);
        assert_eq!(s.source, "auto");
        assert_eq!(s.amount, 200.0);

        // 满 14 天 → 可申请
        let s = claim_state(&cfg, Some(now - 14 * 86_400), now, Some(19.0), None).unwrap();
        assert_eq!(s.days_until_eligible, Some(0));
        assert!(s.eligible);
    }

    /// 手动修正优先;但如果快照检测到比"手动写入时刻"更晚的跳升(又申请了一次),自动值接管。
    #[test]
    fn manual_fix_wins_until_a_newer_jump_appears() {
        let now = 1_700_000_000;
        let manual_day = now - 3 * 86_400;
        let cfg = ClaimConfig {
            enabled: true,
            amount: 200.0,
            min_interval_days: 14,
            manual_last_at: Some(manual_day),
            manual_set_at: Some(now - 2 * 86_400), // 两天前手动改的
        };

        // 自动检测到的是"手动之前"的跳升 → 用手动值
        let older_jump = now - 5 * 86_400;
        let s = claim_state(&cfg, Some(older_jump), now, Some(50.0), None).unwrap();
        assert_eq!(s.last_claim_at, Some(manual_day));
        assert_eq!(s.source, "manual");

        // 手动之后又跳升了一次 → 用自动值
        let newer_jump = now - 86_400;
        let s = claim_state(&cfg, Some(newer_jump), now, Some(190.0), None).unwrap();
        assert_eq!(s.last_claim_at, Some(newer_jump));
        assert_eq!(s.source, "auto");
        assert_eq!(s.days_until_eligible, Some(13));
    }

    /// 撑不到下次可申请:按当前速度余额只够 2 天,但还要等 6 天 → 预警。
    #[test]
    fn shortage_risk_when_balance_runs_out_before_eligible() {
        let cfg = ClaimConfig {
            enabled: true,
            amount: 200.0,
            min_interval_days: 14,
            ..Default::default()
        };
        let now = 1_700_000_000;
        let last = now - 8 * 86_400;

        let s = claim_state(&cfg, Some(last), now, Some(20.0), Some(10.0)).unwrap();
        assert_eq!(s.days_of_balance, Some(2.0));
        assert!(s.shortage_risk);

        // 消耗慢就够用
        let s = claim_state(&cfg, Some(last), now, Some(200.0), Some(10.0)).unwrap();
        assert!(!s.shortage_risk);
    }

    /// 端到端:快照里出现"余额跳升" → 自动识别为一次申请,并算出倒计时。
    #[test]
    fn claim_detects_an_application_from_snapshot_jumps() {
        let s = Store::open_memory().unwrap();
        let now = now_ts();
        let applied_at = now - 12 * 86_400;
        // 申请前余额 19,申请后补到 200,又花到 18.99
        s.record("4sapi", applied_at, Some(19.0), None, None, "amount").unwrap();
        s.record("4sapi", applied_at + 300, Some(200.0), None, None, "amount").unwrap();
        s.record("4sapi", now - 60, Some(18.99), None, None, "amount").unwrap();

        let cfg = Config::default();
        let def = providers::PROVIDERS
            .iter()
            .find(|d| d.id == "4sapi")
            .expect("4sapi 应在渠道表里");

        let st = claim_for(def, &cfg, &s, Some(18.99), "").expect("4sapi 默认开启申请制");
        assert_eq!(st.source, "auto");
        assert_eq!(st.last_claim_at, Some(applied_at + 300));
        // 按整天向下取整:差 5 分钟不满 12 天 → 算 11 天,还剩 3 天(宁可保守,
        // 也不能提前说"可申请"—— 用户去申请会被拒)
        assert_eq!(st.days_until_eligible, Some(3));
        assert!(!st.eligible);

        // 小额抖动不算申请
        s.record("hapi", now - 100, Some(50.0), None, None, "amount").unwrap();
        s.record("hapi", now - 90, Some(55.0), None, None, "amount").unwrap();
        let mut cfg2 = Config::default();
        cfg2.claim_channels.insert("hapi".into(), ClaimConfig { enabled: true, ..Default::default() });
        let def_h = providers::PROVIDERS.iter().find(|d| d.id == "hapi").unwrap();
        let st = claim_for(def_h, &cfg2, &s, Some(55.0), "").unwrap();
        assert_eq!(st.source, "none");
    }

    /// 未启用 / 未记录到申请时间:不硬编倒计时。
    #[test]
    fn claim_without_config_or_history_is_reported_as_such() {
        let off = ClaimConfig { enabled: false, ..Default::default() };
        assert!(claim_state(&off, Some(1), 2, Some(1.0), None).is_none());

        let on = ClaimConfig { enabled: true, ..Default::default() };
        let s = claim_state(&on, None, 1_700_000_000, Some(18.99), None).unwrap();
        assert_eq!(s.last_claim_at, None);
        assert_eq!(s.days_until_eligible, None);
        assert!(!s.eligible);
        assert_eq!(s.source, "none");
    }

    /// 换域名重试的策略:网络/5xx/429 才值得换;凭据被拒、其它 4xx 与业务失败不换。
    #[test]
    fn only_transport_and_server_failures_retry() {
        assert!(FailKind::Transport.retryable());
        assert!(FailKind::Server.retryable());
        assert!(!FailKind::Auth.retryable());     // 401/403:换域名也是同一答案,该换凭据
        assert!(!FailKind::Client.retryable());   // 404/400:路径问题,换域名同一答案
        assert!(!FailKind::Business.retryable()); // code:false:服务器已明确答复
    }

    /// 尝试顺序:上次成功的优先,其次主地址,再依次补备用,且不重复。
    #[test]
    fn attempt_order_puts_last_good_first_and_dedups() {
        let fbs = ["https://b/x", "https://c/x"];
        assert_eq!(attempt_order("https://a/x", &fbs, None), vec!["https://a/x", "https://b/x", "https://c/x"]);
        assert_eq!(attempt_order("https://a/x", &fbs, Some("https://c/x")), vec!["https://c/x", "https://a/x", "https://b/x"]);
        // last_good 就是主地址时不产生重复项
        assert_eq!(attempt_order("https://a/x", &fbs, Some("https://a/x")), vec!["https://a/x", "https://b/x", "https://c/x"]);
    }

    #[test]
    fn week_start_not_after_today_start() {
        assert!(local_week_start() <= local_midnight_today());
    }

    /// 近 30 天到期的合计:只算未来 30 天内的,已过期的不算,更远的不算。
    #[test]
    fn expiring_soon_sums_next_30_days() {
        let now = 1_700_000_000;
        let day = 86_400;
        let list = vec![
            Expiring { at: now - day, amount: 5.0, label: "已过期".into() },
            Expiring { at: now + 3 * day, amount: 10.0, label: "a".into() },
            Expiring { at: now + 20 * day, amount: 2.5, label: "b".into() },
            Expiring { at: now + 40 * day, amount: 100.0, label: "c".into() },
        ];
        let s = expiring_soon(&list, now).expect("30 天内有到期");
        assert_eq!(s.amount, 12.5);
        assert_eq!(s.at, now + 3 * day, "取最近的那天");

        // 只有 30 天外的 → 没有"近期过期"
        assert!(expiring_soon(&list[3..], now).is_none());
        // 空列表 / 已过期 → 也是 None(不能报 0,界面会渲染成"有 0 分要过期")
        assert!(expiring_soon(&[], now).is_none());
        assert!(expiring_soon(&list[..1], now).is_none());
    }

    /// 换凭据的判定:只有服务器明确拒绝这份凭据(401/403/407)才换下一份登录态。
    #[test]
    fn only_auth_failures_try_another_credential() {
        assert!(another_credential_may_help(Some(FailKind::Auth)));
        assert!(!another_credential_may_help(Some(FailKind::Client)));
        assert!(!another_credential_may_help(Some(FailKind::Transport)));
        assert!(!another_credential_may_help(Some(FailKind::Server)));
        assert!(!another_credential_may_help(Some(FailKind::Business)));
        assert!(!another_credential_may_help(None));
    }

    /// 401 必须永远是 `Auth`,**不管响应体是不是 JSON**。
    ///
    /// 2026-10-20 真机(user diag.log):WorkBuddy 的 401 由网关回了一页 HTML,
    /// 旧代码在"响应不是合法 JSON"那支一律给 `Server` → 只换域名、不换凭据 →
    /// 明明已经解出桌面端令牌,循环却停在一份作废的手动令牌上,三天取不到数。
    #[test]
    fn auth_status_wins_over_body_shape() {
        // 状态码口径本身
        assert_eq!(kind_for_status(401), FailKind::Auth);
        assert_eq!(kind_for_status(403), FailKind::Auth);
        assert_eq!(kind_for_status(407), FailKind::Auth);
        assert_eq!(kind_for_status(429), FailKind::Server);
        assert_eq!(kind_for_status(500), FailKind::Server);
        assert_eq!(kind_for_status(404), FailKind::Client);
        assert_eq!(kind_for_status(400), FailKind::Client);

        // 关键那条:Auth 才允许换凭据
        assert!(
            another_credential_may_help(Some(kind_for_status(401))),
            "401 必须触发换下一份凭据(否则桌面端令牌永远轮不到)"
        );
        // 而 5xx 仍然不该换凭据(换域名才有意义)
        assert!(!another_credential_may_help(Some(kind_for_status(503))));
    }

    /// 被 401 拒过的凭据要沉到末尾,**但不能被丢掉**:用户重新登录后它还会被再试一次。
    /// 2026-10-20 真机:一份早已作废的手动令牌排在桌面端凭据前面,每轮先白打一个 401。
    #[test]
    fn rejected_credentials_are_demoted_not_dropped() {
        let mk = |t: &str, src: &str| appauth::Credential {
            token: t.to_string(),
            source: src.to_string(),
            device_id: None,
        };
        let manual = mk("manual-dead-token", "");
        let desktop = mk("desktop:live-token", "WorkBuddy 桌面端");
        let now = 1_800_000_000i64;

        // 被拒的是手动那份 → 它沉底,桌面端排到第一
        let mut rej: HashMap<String, i64> = HashMap::new();
        rej.insert(key_fp("manual-dead-token"), now - 60);
        let got = demote_rejected(
            vec![manual.clone(), desktop.clone()],
            &rej,
            now,
            |t| key_fp(t),
        );
        assert_eq!(got[0].token, "desktop:live-token", "可用那份要排第一");
        assert_eq!(got[1].token, "manual-dead-token", "被拒的沉到末尾");
        assert_eq!(got.len(), 2, "**不许丢** —— 丢了用户就再也看不到那份凭据的状态");

        // 超过 TTL → 回到原位再试一次(可能是瞬时 401,也可能用户已重新登录)
        let got = demote_rejected(
            vec![manual.clone(), desktop.clone()],
            &rej,
            now + REJECTED_TTL,
            |t| key_fp(t),
        );
        assert_eq!(got[0].token, "manual-dead-token", "TTL 过后要恢复原顺序");

        // 没有任何拒绝记录 → 原样返回
        let got = demote_rejected(
            vec![manual.clone(), desktop.clone()],
            &HashMap::new(),
            now,
            |t| key_fp(t),
        );
        assert_eq!(got[0].token, "manual-dead-token");
        assert_eq!(got[1].token, "desktop:live-token");
    }

    /// F2:失败且真的发过请求 → 清掉"上次成功地址";成功不清、没发过请求也不清。
    /// 以前清备忘这一支永远进不去(失败时拿不到 attempted 地址),渠道会被钉在
    /// 一个挂掉的备用镜像上,每轮先白等满超时。
    #[test]
    fn failed_attempt_clears_the_url_memo_but_success_keeps_it() {
        assert!(should_clear_memo(false, true), "失败 + 发过请求 → 清备忘");
        assert!(!should_clear_memo(true, true), "成功时备忘正要写进去,不能清");
        assert!(!should_clear_memo(false, false), "没发过请求就没什么可清的");
        assert!(!should_clear_memo(true, false));
    }

    /// F3:没有 `main: true` 的窗口时宁可不学 —— 不能拿 5 小时窗/周窗的间隔当包月周期。
    #[test]
    fn main_window_reset_needs_a_main_window() {
        let def = providers::find("opencode-go").unwrap();
        let mut v = placeholder(def);
        v.windows = vec![
            providers::Window {
                label: "5 小时".into(),
                percent: 10.0,
                remain_percent: 90.0,
                status: "ok".into(),
                resets_at: Some("2026-11-03T00:00:00+00:00".into()),
                main: false,
            },
            providers::Window {
                label: "周".into(),
                percent: 20.0,
                remain_percent: 80.0,
                status: "ok".into(),
                resets_at: Some("2026-11-10T00:00:00+00:00".into()),
                main: false,
            },
        ];
        assert!(main_window_reset(&v).is_none(), "没有主窗就该返回 None");

        // 有主窗时照旧取它(而不是顺手拿第一个)
        let mut w = placeholder(def);
        w.windows = vec![
            providers::Window {
                label: "5 小时".into(),
                percent: 10.0,
                remain_percent: 90.0,
                status: "ok".into(),
                resets_at: Some("2026-11-03T00:00:00+00:00".into()),
                main: false,
            },
            providers::Window {
                label: "周期".into(),
                percent: 20.0,
                remain_percent: 80.0,
                status: "ok".into(),
                resets_at: Some("2026-12-03T00:00:00+00:00".into()),
                main: true,
            },
        ];
        assert_eq!(
            main_window_reset(&w),
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-12-03T00:00:00+00:00")
                    .unwrap()
                    .timestamp()
            )
        );
    }

    /// F4:本地没有 0 点(DST 造出的不存在的时刻)时,退路必须是**请求的那一天**的
    /// UTC 零点,不能拿"今天"顶替 —— 否则周一/月初/上月初全算错。
    #[test]
    fn midnight_fallback_uses_the_requested_day_not_today() {
        let want = |y, m, d| {
            Utc.with_ymd_and_hms(y, m, d, 0, 0, 0)
                .unwrap()
                .timestamp()
        };
        assert_eq!(
            resolve_midnight(chrono::LocalResult::None, 2026, 3, 3),
            want(2026, 3, 3)
        );
        assert_eq!(
            resolve_midnight(chrono::LocalResult::None, 2020, 1, 2),
            want(2020, 1, 2),
            "请求的是哪一天就用哪一天"
        );
        // 正常/歧义(DST 回拨)时仍按 earliest 取第一段,不动行为
        let t = Local.with_ymd_and_hms(2026, 3, 3, 0, 0, 0).unwrap();
        assert_eq!(
            resolve_midnight(chrono::LocalResult::Single(t), 2026, 3, 3),
            t.timestamp()
        );
        let later = t + Duration::hours(1);
        assert_eq!(
            resolve_midnight(chrono::LocalResult::Ambiguous(t, later), 2026, 3, 3),
            t.timestamp(),
            "0 点被重复时取第一段"
        );
    }

    /// F5:响应体的上限在**边收边判**时就生效,不是等整包读进内存之后再判。
    #[test]
    fn body_cap_is_enforced_while_collecting_chunks() {
        let mut out: Vec<u8> = Vec::new();
        assert!(push_chunk(&mut out, b"12345", 10).is_ok());
        assert_eq!(out, b"12345");
        assert!(push_chunk(&mut out, b"12345", 10).is_ok());
        assert_eq!(out.len(), 10);
        let e = push_chunk(&mut out, b"x", 10).unwrap_err();
        assert!(e.contains("响应过大"), "{e}");
        assert_eq!(out.len(), 10, "超限的那块不许拼进去");
        // 单块就超限也要立刻报错(分块传输时第一块就可能已经超了)
        let mut first: Vec<u8> = Vec::new();
        assert!(push_chunk(&mut first, &vec![0u8; 11], 10).is_err());
        assert!(first.is_empty());
    }

    // ───────────────── 指纹分段 / 缺口标注 / 峰谷进视图 ─────────────────

    /// 指纹:同一把 key 恒定、两侧空白不影响、换一位就变、且不含明文片段。
    #[test]
    fn key_fp_is_stable_and_hides_the_key() {
        let a = key_fp("sk-abc123");
        assert_eq!(a, key_fp("sk-abc123"));
        assert_eq!(a, key_fp("  sk-abc123  "), "裁剪两侧空白后同一把");
        assert_ne!(a, key_fp("sk-abc124"), "换一位就是另一本账");
        assert_eq!(a.len(), 24, "12 字节的十六进制");
        assert!(!a.contains("abc"), "指纹里不许出现密钥片段");
    }

    /// 峰谷状态只挂在标了 `peak_pricing` 的渠道上(DeepSeek),别的渠道一律 None。
    #[test]
    fn peak_state_only_for_peak_priced_channels() {
        let store = Store::open_memory().unwrap();
        let cfg = Config::default();
        let res = FetchResult {
            valid: true,
            kind: Kind::Amount,
            unit: "CNY".into(),
            remaining: Some(42.0),
            ..Default::default()
        };
        let ds = providers::find("deepseek").expect("渠道在表里");
        let v = build_view(ds, res.clone(), false, now_ts(), &store, &cfg, None, "fpA", 1);
        let p = v.peak.expect("DeepSeek 该有峰谷状态");
        assert!(p.next_change_at.is_some(), "总能算出下一次切换");
        assert!(!p.table_stale, "2026 年内表没过期");

        let api = providers::find("4sapi").unwrap();
        let v2 = build_view(api, res, false, now_ts(), &store, &cfg, None, "fpA", 1);
        assert!(v2.peak.is_none(), "中转站没有峰谷时段这回事");
    }

    /// 缺口进视图 + 消耗只在同一本账里推算(换 key 后不跨账户)。
    ///
    /// **时间必须相对"现在"取**:以前写死 `midnight + 2h / +3h / +4h`,凌晨 2 点前跑测试时
    /// 那些快照还在未来,`v.day` 自然算不出来 —— 2026-10-19 02:14 就这么挂过一次
    /// (与业务无关,是夹具写死了钟点)。
    ///
    /// **2026-10-21 00:07 又挂了一次**,这次是另一种:想构造"距零点 20 分钟的缺口",
    /// 但刚过午夜时 `midnight+20min` 还在未来,`lo < hi` 不成立 → 缺口只剩 4 分钟
    /// → 判不出缺口。根本原因还是**依赖真实钟点**。
    ///
    /// 现在把"现在"钉成**今天 03:00**(合成值,直接传给 build_view),与真实钟点彻底解耦:
    /// 凌晨、白天、深夜跑都得到同一组快照时刻。
    #[test]
    fn gap_and_fingerprint_reach_the_view() {
        let store = Store::open_memory().unwrap();
        let cfg = Config::default();
        let def = providers::find("deepseek").unwrap();
        let midnight = local_midnight_today();
        // 合成"现在" = 今天 03:00。build_view 接 now 参数,所以不必是真实时间;
        // 但**必须**与 record_fp 落的时刻同一坐标系(都是"今天零点 + 偏移")。
        let now = midnight + 3 * 3600;
        let res = FetchResult {
            valid: true,
            kind: Kind::Amount,
            unit: "CNY".into(),
            remaining: Some(90.0),
            ..Default::default()
        };
        // A 账:今天 00:20 才开始有观测(距零点 20 分钟 > OBSERVE_GAP_SEC),掉 10 元。
        let a1 = midnight + 20 * 60;
        let a2 = a1 + 60;
        store
            .record_fp(def.id, "fpA", a1, Some(100.0), None, None, "amount")
            .unwrap();
        store
            .record_fp(def.id, "fpA", a2, Some(90.0), None, None, "amount")
            .unwrap();
        // B 账:另一把 key,余额低得多(旧口径会把它算成一大笔"消耗")
        store
            .record_fp(def.id, "fpB", a2 + 30, Some(10.0), None, None, "amount")
            .unwrap();
        let v = build_view(def, res.clone(), false, now, &store, &cfg, None, "fpA", 2);
        assert_eq!(
            v.gap_day_from,
            Some(a1),
            "今天 a1 才有第一条,要如实标出来"
        );
        assert_eq!(v.day, Some(10.0), "只算 A 账自己的掉额");
        assert_eq!(v.accounts, 2, "用过两把密钥");
        assert!(
            v.gap_days.contains(&midnight),
            "今天本身就是缺口日(起点不在零点):{:?}",
            v.gap_days
        );

        // A 账起点贴着零点 → 不算缺口
        let s2 = Store::open_memory().unwrap();
        s2.record_fp(def.id, "fpC", midnight + 60, Some(100.0), None, None, "amount").unwrap();
        s2.record_fp(def.id, "fpC", midnight + 3600, Some(95.0), None, None, "amount").unwrap();
        let v2 = build_view(
            def,
            FetchResult { remaining: Some(95.0), ..res.clone() },
            false,
            // 同一个合成 now:这里以前传 now_ts(),刚过午夜时第二笔(midnight+1h)还在未来
            now,
            &s2,
            &cfg,
            None,
            "fpC",
            1,
        );
        assert_eq!(v2.gap_day_from, None, "正常开机不该天天挂一行缺口提示");
    }
}
