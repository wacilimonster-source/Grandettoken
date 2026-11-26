//! 取数与视图组装。
//!
//! 核心原则:**失败不降级成 0**。取数失败时继续显示上次成功快照并标注时间,
//! 因为余额类工具最危险的 bug 就是把请求失败渲染成"余额归零",用户会以为欠费了。

use crate::appauth::{self};
use crate::config::{ClaimConfig, Config};
use crate::custom;
use crate::fx::{self, FxRate};
use crate::peak;
use crate::qoderweb;
use crate::providers::{self, AuthKind, Expiring, FetchResult, Kind, Method};
use crate::store::{
    consumed_in_window_rows, consumption_since_rows, daily_coverage_rows, first_ts_rows,
    last_jump_rows, window_coverage_rows, HistoryRow, Store,
};
use crate::secrets;
use crate::recovery::{Recovery, RecoveryView, Snapshot};
use crate::weblogin;
use chrono::{Datelike, Duration, Local, TimeZone, Utc, Weekday};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// 判"数据有缺口"的空档线。定义在 store(归档/曲线/文案三处共用同一个数字),
/// 这里转发一次,调用方继续用 `fetch::OBSERVE_GAP_SEC`。
pub use crate::store::OBSERVE_GAP_SEC;

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

/// 积分到账事件的保留期。30 天:界面看的是"最近有没有断",再长就要分页与导出,
/// 收益不成正比。清理挂在写入之后顺手做(到账是低频事件,不值得单开定时器)。
pub const GRANT_KEEP_SEC: i64 = 30 * 86_400;

/// 桌面端请求的 UA(与 dsh-connect-workbuddy 同口径:CLI 形态的 CodeBuddy 身份)。
pub(crate) const DESKTOP_UA: &str = "CLI/2.63.2 CodeBuddy/2.63.2";

/// 传给界面的一行。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChannelView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google_cloud: Option<crate::google_cloud::CreditView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_plan_scope: Option<String>,
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
    pub reset_credits: Option<crate::codex::ResetCredits>,
    /// 逐笔到期(升序)。积分型渠道才有内容。
    pub expiring: Vec<Expiring>,
    /// false 时空的 expiring 列表表示“接口没有完整明细”,不能说成“没有到期”。
    pub expiry_complete: bool,
    pub expiry_stale: bool,
    /// 账户额度有效期(Qoder):账户级字段,与逐笔 `expiring` 严格分开。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_expiry: Option<providers::AccountExpiry>,
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
    pub recovery: Option<RecoveryView>,
    pub updated_at: i64,
    /// 申请制额度状态;None = 该渠道没启用申请制(见 ClaimConfig)
    pub claim: Option<ClaimState>,
    /// 用户隐藏了该渠道:不显示、不取数(见 Config::hidden_channels)。
    /// 视图仍会返回,管理页要靠它列出「已隐藏」的卡片。
    pub hidden: bool,
    /// 这次隐藏是**系统自动**做的(没检测到凭据),不是用户按的眼睛。
    /// 管理页据此把话说清("未检测到登录 · 已自动隐藏"),眼睛的动作也变成"仍然显示"。
    pub auto_hidden: bool,
    /// 峰谷计价渠道(DeepSeek)的时段状态;其它渠道 None。
    pub peak: Option<PeakView>,
    /// 观测缺口:今天/本周/本月**从几点起才有数据**(超过 [`OBSERVE_GAP_SEC`] 才算)。
    /// None = 起点没缺口(或这个窗口压根没数据)。
    pub gap_day_from: Option<i64>,
    /// 今日积分到账。只有 [`providers::tracks_grants`] 的渠道有值:
    /// `None` = 这个渠道不记到账;`Some(全零)` = 记,但今天还没到账(**确定的"没有"**,
    /// 与"读不到"要分开 —— 后者用 `stale` / `valid` 表达)。
    pub grant_today: Option<crate::grants::DayGrants>,
    /// 近 7 天(含今天)的到账汇总。渠道卡上的「近 7 天 +180 · 6 天有」用它。
    pub grant_week: Option<crate::grants::DayGrants>,
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

#[path = "fetch_creds.rs"]
mod fetch_creds;
pub use fetch_creds::{
    auto_hide_candidates, current_key_fp, expired_credential_msg, key_fp, QODER_CHANGED_KEY,
    QODER_SCOPE_KEY,
};
use fetch_creds::*;

#[path = "fetch_once.rs"]
mod fetch_once;
pub use fetch_once::{
    another_credential_may_help, attempt_order, fetch_channel, unit_for, FailKind,
};
pub(crate) use fetch_once::{kind_for_status, push_chunk, retry_after_seconds};
use fetch_once::*;

#[path = "fetch_view.rs"]
mod fetch_view;
pub use fetch_view::{
    claim_state, cycle_key, inferred_cycle_days, learn_cycle, load_cycle, local_datetime_string,
    local_day_of, local_day_start_of, local_day_string, local_midnight_today, local_month_start,
    local_prev_month_start, local_tz_offset, local_week_start, now_ts, plan_cost, save_cycle,
    ASSUMED_CYCLE_DAYS, CYCLE_KEY_PREFIX,
};
pub(crate) use fetch_view::{build_view, local_date_start, placeholder};
use fetch_view::*;

#[path = "fetch_db.rs"]
mod fetch_db;
#[path = "fetch_enrich.rs"]
mod fetch_enrich;

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

/// Qoder 网页会话的验证结果。**三档对应三种完全不同的用户动作**:
/// 去登录 / 不用做什么(等挂件适配) / 检查网络。
#[derive(Debug, Clone, PartialEq)]
pub enum QoderWebProbe {
    /// 会话有效,且拿到逐包数据
    Packed,
    /// 平台明确拒了这份会话(401/403 / TOKEN_*)—— 该重新登录
    Rejected,
    /// 会话有效,但响应里找不到逐包数据 —— 挂件这一侧的问题
    Unrecognized(String),
    /// 网络 / 拿不到 JSON 之类的真失败
    Failed(String),
}

#[derive(Default)]
struct ChannelState {
    cache: HashMap<String, (FetchResult, i64)>,
    scopes: HashMap<String, String>,
    recovery: HashMap<String, Recovery>,
    url_memo: HashMap<String, String>,
    rejected: HashMap<String, i64>,
    adopted: HashSet<String>,
    accounts_cache: HashMap<String, i64>,
}

type WebCredentialSource = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// 取数器。持有 HTTP 客户端和上次成功结果的缓存,失败时用于降级显示。
#[derive(Clone)]
pub struct Fetcher {
    pub http: reqwest::Client,
    state: Arc<Mutex<ChannelState>>,
    qoder_web: Arc<Mutex<qoderweb::QoderWebState>>,
    progress: Arc<Mutex<crate::progress::Progress>>,
    progress_sink: Arc<Mutex<Option<Arc<dyn Fn(Vec<ChannelView>) + Send + Sync>>>>,
    /// 上次成功解析的配置。DB 读不到/解析失败时沿用 —— 隐藏渠道不该被静默放开(P2-11)
    cfg_cache: Arc<Mutex<Option<Config>>>,
    /// fetch_shared 的门 + 近期结果:手动刷新撞上轮询不再双份请求账务接口(O-4)
    gate: Arc<tokio::sync::Mutex<()>>,
    last_run: Arc<Mutex<Option<(i64, Vec<ChannelView>)>>>,
    /// 网页登录型渠道的会话来源。外壳在启动时装一个"去 webview 的 Cookie 罐里现取"
    /// 的回调:会话留在 WebView2 的 profile 里(平台自己管过期与轮换),挂件不存副本 ——
    /// 一个是凭据管理器放不下(实测 blob 上限约 1280 字符,而两个会话 Cookie 近 6 KB),
    /// 另一个是平台一旦轮换 Cookie,存下来的副本会静默失效。
    web_cred_src: Arc<Mutex<Option<WebCredentialSource>>>,

}

impl Default for Fetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher {
    pub fn set_progress_sink(&self, sink: Arc<dyn Fn(Vec<ChannelView>) + Send + Sync>) {
        *crate::lockx::lock(&self.progress_sink) = Some(sink);
    }

    fn publish(&self, v: ChannelView) {
        let views = crate::lockx::lock(&self.progress).update(v);
        self.emit_progress(views);
    }

    fn emit_progress(&self, views: Option<Vec<ChannelView>>) {
        let sink = crate::lockx::lock(&self.progress_sink).clone();
        if let (Some(sink), Some(views)) = (sink, views) { sink(views); }
    }

    fn failed(&self, id: &str, kind: Option<FailKind>) -> RecoveryView {
        let mut channel = self.channel_state();
        let states = &mut channel.recovery;
        let mut next = Recovery::failed(states.get(id), now_ts(), kind);
        // Stable one-second spread between channels, without random dependencies.
        next.next += id.bytes().map(u64::from).sum::<u64>() as i64 % 2;
        let view = next.view(now_ts());
        states.insert(id.into(), next);
        view
    }

    fn channel_state(&self) -> std::sync::MutexGuard<'_, ChannelState> {
        crate::lockx::lock(&self.state)
    }

    fn cached(&self, store: &Arc<Mutex<Store>>, id: &str, scope: &str) -> Option<(FetchResult, i64)> {
        {
            let mut state = self.channel_state();
            if scope.is_empty() { return state.cache.get(id).cloned(); }
            if state.scopes.get(id).is_some_and(|old| old == scope) {
                if let Some(hit) = state.cache.get(id) { return Some(hit.clone()); }
            } else {
                state.cache.remove(id);
                state.recovery.remove(id);
                state.scopes.insert(id.into(), scope.into());
            }
        }
        let snapshot = Snapshot::load(&crate::lockx::lock(store), id, scope)?;
        let hit = (snapshot.result, snapshot.ts);
        let mut state = self.channel_state();
        if state.scopes.get(id).is_some_and(|active| active == scope) {
            state.cache.insert(id.into(), hit.clone());
            Some(hit)
        } else {
            None
        }
    }

    async fn quota_attempt(&self, def: &providers::ProviderDef, cred: &appauth::Credential, memo: Option<&str>) -> (FetchResult, Option<String>, Option<FailKind>) {
        let attempt = self.channel_state().recovery.get(def.id).map_or(0, |r| r.attempts);
        let order = attempt_order(def.url, def.fallback_urls, memo);
        let url = &order[attempt as usize % order.len()];
        let (r, kind) = fetch_enriched_attempt(&self.http, def, url, &cred.token, cred.account_id.as_deref()).await;
        (r, Some(url.clone()), kind)
    }

    /// 下一次失败重试的到期时刻(墙钟秒)。没有待重试渠道时返回 `None`。
    ///
    /// 给外壳的轮询用。它原本在 60/300/900 秒的等待窗口里**每秒醒一次**,只为了看
    /// 有没有渠道到期该重试 —— 一轮白醒 60~900 次(约 86,400 次/天),每次都要锁一遍
    /// state、克隆 Arc、遍历这张表。有了这个值,它可以一次睡到下一个到期点。
    ///
    /// 只读、不改任何状态:触发条件仍是 `r.next <= now_ts()`,精度仍按秒
    /// (调用方最小睡 1 秒),所以重试时机与原来等价。
    pub fn next_retry_at(&self) -> Option<i64> {
        self.channel_state().recovery.values().map(|r| r.next).min()
    }

    /// Called by the shell ticker. Only failed, due channels are queried.
    pub async fn fetch_retries(&self, store: &Arc<Mutex<Store>>) -> Option<Vec<ChannelView>> {
        self.fetch_retries_using(store, store).await
    }

    pub async fn fetch_retries_using(&self, store: &Arc<Mutex<Store>>, reader: &Arc<Mutex<Store>>) -> Option<Vec<ChannelView>> {
        if !self.channel_state().recovery.values().any(|r| r.next <= now_ts()) { return None; }
        let _guard = self.gate.lock().await;
        let due: HashSet<String> = self.channel_state().recovery.iter()
            .filter(|(_, r)| r.next <= now_ts()).map(|(id, _)| id.clone()).collect();
        if due.is_empty() { return None; }
        let views = self.fetch_selected(store, reader, Some(&due)).await;
        *crate::lockx::lock(&self.last_run) = Some((now_ts(), views.clone()));
        Some(views)
    }

    pub fn request_refresh(&self) {
        if let Some((at, _)) = crate::lockx::lock(&self.last_run).as_mut() {
            *at = i64::MIN;
        }
        self.reset_retry_schedule();
    }

    pub fn forget_channel(&self, store: &Store, id: &str) {
        {
            let mut active = self.channel_state();
            active.cache.remove(id);
            active.scopes.remove(id);
            active.recovery.remove(id);
        }
        crate::lockx::lock(&self.progress).rows.retain(|v| v.id != id);
        crate::lockx::lock(&self.qoder_web).forget(id);
        let _ = store.delete_setting(&format!("quota_snapshot_v1:{id}"));
        let _ = store.delete_setting(&format!("{QODER_SCOPE_KEY}:{id}"));
        let _ = store.delete_setting(&format!("{QODER_CHANGED_KEY}:{id}"));
        self.invalidate();
    }

    /// 网页会话被用户主动换掉(退出网页登录 / 切换账号)时清掉逐包缓存。
    ///
    /// 只清内存,不动余额快照与会话时刻:会话时刻由外壳在登录收尾时写,
    /// 这里要是顺手删了,"换号后是否陈旧"的判据就断了。
    pub fn forget_qoder_web(&self, id: &str) {
        crate::lockx::lock(&self.qoder_web).forget_packs(id);
        self.invalidate();
    }

    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .user_agent("TokenScope/0.1")
                .build()
                .expect("HTTP 客户端初始化失败"), // 常量超时与 ASCII User-Agent,仅进程级 TLS 栈损坏时失败
            state: Arc::default(),
            qoder_web: Arc::default(),
            progress: Arc::default(),
            progress_sink: Arc::default(),
            cfg_cache: Arc::default(),
            gate: Arc::default(),
            last_run: Arc::default(),
            web_cred_src: Arc::default(),
        }
    }

    /// 装上网页登录型渠道的会话来源(见字段注释)。**回调是阻塞的**(要同步问 webview 的
    /// Cookie 罐),只能在非主线程上被调用 —— 取数路径本来就跑在 async worker 上,安全。
    pub fn set_web_credential_source(
        &self,
        f: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
    ) {
        *crate::lockx::lock(&self.web_cred_src) = Some(Arc::from(f));
    }

    /// 作废"5 秒内复用上次结果"的窗口。登录刚完成 / 密钥刚改过时必须调一次,
    /// 否则那一次刷新会拿旧结果回给界面,用户看到的是"已经登录了但还没有数"。
    pub fn invalidate(&self) {
        *crate::lockx::lock(&self.last_run) = None;
        self.reset_retry_schedule();
    }

    fn reset_retry_schedule(&self) {
        self.channel_state().recovery.retain(|_, r| matches!(r.kind, Some(FailKind::RateLimited(_))) && r.next > now_ts());
    }

    /// 用一份**还没生效**的凭据试打一次:网页登录抓到会话之后,先证明这份会话真能取数,
    /// 再决定要不要关窗、报"已登录"。不验证就报成功的话,一份被平台拒掉的会话会变成
    /// "状态显示已登录、却一直取不到数"—— 用户只能靠猜。
    pub async fn probe(&self, def: &providers::ProviderDef, credential: &str) -> bool {
        let (result, _, _) = fetch_channel(&self.http, def, credential, None).await;
        result.valid
    }

    /// 取某区自己的端点 memo。返回 Arc 后立刻释放锁 —— `MutexGuard` 不能带过 await。
    fn qoder_memo_for(&self, id: &str) -> Arc<qoderweb::PacksMemo> {
        crate::lockx::lock(&self.qoder_web).memo(id)
    }

    /// Qoder 网页会话验证:候选端点里有一条给出"包形状"的响应(哪怕为空)就算有效。
    /// 不验证就报成功的话,一份被平台拒掉的会话会变成"状态显示已登录、却一直取不到数"
    /// —— 与 WorkBuddy 的 probe 同一条纪律。会话有效但没有包 ≠ 失败。
    ///
    /// 三档必须分开(2026-11-03):**会话被拒**要让用户去重登;**结构没认出来**是挂件
    /// 这一侧的事,重登一百次也没用;其余才是网络问题。以前只回 bool,后两种被并成
    /// "登录失败 · 被平台拒绝",实测把用户引向了无用的重登。
    pub async fn probe_qoder_web(&self, id: &str, session: &str) -> QoderWebProbe {
        let Some(site) = crate::weblogin::qoder_site(id) else {
            return QoderWebProbe::Failed("未知的 Qoder 区域".into());
        };
        let memo = self.qoder_memo_for(id);
        match qoderweb::fetch_packs(&self.http, session, &memo, site).await {
            Ok(_) => QoderWebProbe::Packed,
            Err(qoderweb::PacksErr::Unauthorized) => QoderWebProbe::Rejected,
            Err(qoderweb::PacksErr::Unrecognized(detail)) => QoderWebProbe::Unrecognized(detail),
            Err(qoderweb::PacksErr::Other(detail)) => QoderWebProbe::Failed(detail),
        }
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
        if def.id == "antigravity" {
            if let Ok(c) = Config::try_load(&store.lock().unwrap_or_else(|p| p.into_inner())) {
                crate::antigravity::set_cli_path(c.antigravity_cli_path);
            }
        }
        let web_src = self.web_cred_src.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let (creds, secret_err) =
            tokio::task::spawn_blocking(move || credentials(def, web_src.as_deref(), None))
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
        let Some(_) = creds.first() else {
            rep.error = Some(
                secret_err.unwrap_or_else(|| format!("没检测到 {} 的登录凭据", def.name)),
            );
            rep.elapsed_ms = started.elapsed().as_millis() as u64;
            return rep;
        };
        // 与轮询走同一条路:被 401/403 拒过的凭据先沉底,再依次试。
        //
        // 2026-10-23 真机:Trae 有两份本机登录态(旧的 TRAE SOLO CN、正在用的
        // Trae CN)。轮询会"401 → 换下一份"取到数,但「测试连接 / 体检」以前
        // **只拿 creds.first() 打一次**,于是永远报"登录状态已失效 · 打开一次
        // Trae 即可"。用户按提示开了客户端也没用 —— 问题在那份旧凭据,不在登录。
        let creds = {
            let now = now_ts();
            let rej = self.channel_state().rejected.clone();
            demote_rejected(creds, &rej, now, |t| key_fp(t))
        };
        // 轮询记住的入口优先:测试要回答的是"平时这条路通不通,用的哪个域名"
        let memo = self.channel_state().url_memo.get(def.id).cloned();
        let mut chosen: Option<(FetchResult, Option<String>, appauth::Credential)> = None;
        let mut last: Option<(FetchResult, Option<String>, appauth::Credential)> = None;
        for cred in &creds {
            let (r, url, kind) = fetch_channel_scoped(&self.http, def, &cred.token, memo.as_deref(), cred.account_id.as_deref()).await;
            // 与轮询同一套降权记账:这次探到的 401 也帮下一轮少打一次。
            {
            let mut state = self.channel_state();
            let m = &mut state.rejected;
                let fp = key_fp(&cred.token);
                if r.valid {
                    m.remove(&fp);
                } else if another_credential_may_help(kind) {
                    m.insert(fp, now_ts());
                }
            }
            let retry_next = another_credential_may_help(kind);
            let ok = r.valid;
            last = Some((r, url, cred.clone()));
            if ok {
                chosen = last.clone();
                break;
            }
            if !retry_next {
                break;
            }
        }
        let Some((r, url, cred)) = chosen.or(last) else {
            rep.error = Some("没有可用凭据".into());
            rep.elapsed_ms = started.elapsed().as_millis() as u64;
            return rep;
        };
        rep.auth_label = auth_label(def, Some(&cred));
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
        self.fetch_shared_using(store, store).await
    }

    /// 首屏使用最近一轮内存数据，网络刷新仍由调用方单独调度。
    pub fn last_views(&self) -> Option<Vec<ChannelView>> {
        crate::lockx::lock(&self.last_run).as_ref().map(|(_, views)| views.clone())
    }

    pub async fn fetch_shared_using(&self, store: &Arc<Mutex<Store>>, reader: &Arc<Mutex<Store>>) -> Vec<ChannelView> {
        let _g = self.gate.lock().await;
        {
            let last = self.last_run.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((ts, views)) = last.as_ref() {
                // 必须同时要求 *ts <= now_ts():用墙钟做减法,系统时间被往前校正后
                // 差值为负、恒小于 5,界面就"永久不再更新"了(报告 P2-23)
                let now = now_ts();
                if (0..5).contains(&now.saturating_sub(*ts)) {
                    return views.clone();
                }
            }
        }
        let views = self.fetch_selected(store, reader, None).await;
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
        self.fetch_selected(store, store, None).await
    }

    async fn fetch_selected(&self, store: &Arc<Mutex<Store>>, reader: &Arc<Mutex<Store>>, selected: Option<&HashSet<String>>) -> Vec<ChannelView> {
        let previous = crate::lockx::lock(&self.progress).rows.clone();
        crate::lockx::lock(&self.progress).begin();
        let should_fetch = |id: &str| selected.map_or_else(
            || self.channel_state().recovery.get(id).map_or(true, |r| r.next <= now_ts()),
            |ids| ids.contains(id));
        let n = providers::PROVIDERS.len();
        let out: Vec<Option<ChannelView>> = (0..n).map(|_| None).collect();

        // ── await 段:纯网络,不碰数据库 ──
        // 每轮开头读一次配置;解析失败沿用上次成功的配置(报告 P2-11),避免隐藏渠道恢复取数。
        let cfg: Arc<Config> = Arc::new({
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            let mut cache = self.cfg_cache.lock().unwrap_or_else(|p| p.into_inner());
            match Config::try_load(&s) {
                Ok(c) => { *cache = Some(c.clone()); c }
                Err(e) => { eprintln!("{e},本轮沿用上次的配置"); cache.clone().unwrap_or_default() }
            }
        });
        crate::antigravity::set_cli_path(cfg.antigravity_cli_path.clone());
        let hidden: Arc<Vec<String>> = Arc::new(cfg.hidden_channels.clone());
        let auto_hide: Arc<Vec<String>> = Arc::new(auto_hide_candidates(&cfg));
        let (fx_cached, cycle_cached) = { let s = store.lock().unwrap_or_else(|p| p.into_inner()); (fx::load(&s), load_cycles(&s)) };
        let fx_now = fx::refresh(&self.http, fx_cached.clone(), now_ts()).await;
        let web_src: Option<Arc<dyn Fn(&str) -> Option<String> + Send + Sync>> = self.web_cred_src.lock().unwrap_or_else(|p| p.into_inner()).clone();

        let wb_cycle = Arc::new(crate::workbuddy_cycle::Credentials::default());
        let builtin = futures::future::join_all(
            providers::PROVIDERS
                .iter()
                .enumerate()
                .map(|(i, def)| {
                    let wb_cycle = wb_cycle.clone();
                    let hidden = hidden.clone();
                    let auto_hide = auto_hide.clone();
                    let web_src = web_src.clone();
                    let cfg = &cfg;
                    let should_fetch = &should_fetch;
                    let previous = &previous;
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
                    if !should_fetch(def.id) {
                        if let Some(v) = crate::antigravity_accounts::previous_view(previous, def.id) {
                            return FetchedOne { i, def, fp: String::new(), view: Some(v), pending: None, success: None };
                        }
                    }
                    if def.id == "antigravity" {
                        let view = crate::antigravity_accounts::collect(store, cfg).await;
                        return FetchedOne { i, def, fp: String::new(), view: Some(view), pending: None, success: None };
                    }
                    // 文件 IO + AES + keyring RPC 都是阻塞调用:下沉到 spawn_blocking,
                    // 凭据服务卡住不再拖垮 tokio worker 上的整轮取数(扫描报告 O-5)。
                    // 网页登录的会话来源也是阻塞的(要同步问 webview 的 Cookie 罐),
                    // 正好一起待在这个阻塞任务里。
                    let web_src = web_src.clone();
                    // 逐包到期需要独立的网页会话来源，避免被凭据闭包移走。
                    let qoder_web_src = web_src.clone();
                    let wb_reader = wb_cycle.clone();
                    let (creds, secret_err) =
                        tokio::task::spawn_blocking(move || credentials(def, web_src.as_deref(), Some(&wb_reader)))
                            .await
                            .unwrap_or_else(|_| (Vec::new(), Some("凭据读取异常".to_string())));
                    // 被服务器拒过(401/403)的凭据沉到末尾:一份早已作废的手动令牌排在
                    // 桌面端凭据前面会让每轮都先白打一个往返(2026-10-20 真机日志)。
                    // 只降权不丢弃 —— 用户重新登录/平台恢复后它还会被再试一次。
                    let creds = {
                        let now = now_ts();
                        let rej = self.channel_state().rejected.clone();
                        demote_rejected(creds, &rej, now, |t| key_fp(t))
                    };
                    // API Key 按当前库中密钥分段，失败期间也保持账目归属。
                    // 轮换的网页/应用令牌使用空指纹，账号隔离由各渠道独立处理。
                    let fp: String = if matches!(def.auth, AuthKind::Keyring) {
                        creds
                            .first()
                            .map(|c| key_fp(&c.token))
                            .unwrap_or_default()
                    } else if wb_desktop_only(def.id) && !creds.is_empty() {
                        // 与令牌共享本轮账户快照，换账号后的余额不能认作旧账户到账。
                        accounts_fp(&wb_cycle.accounts(true))
                    } else {
                        String::new()
                    };
                    let scope = creds.first().map(|c| {
                        credential_scope(c)
                    }).unwrap_or_default();
                    // 取数开始不再发「更新中」过场(2026-11-09 用户反馈:请求数据时
                    // 托盘绿底变灰、数字变白,胶囊光环闪失败态,取完又回绿)。
                    // 以前这里把缓存快照临时置为 invalid + retrying 推给界面 —— 把
                    // "正在请求"当成了状态变化。现在请求期间四种形态都保持上一次
                    // 成功后的画面,数字与颜色只在这一轮真正取到数后更新;真正的
                    // 失败仍走下面 no-cred / pending 的发布口径,不受影响。
                    let cached = self.cached(reader, def.id, &scope);
                    if creds.is_empty() {
                        if cached.is_some() || secret_err.is_some() { self.failed(def.id, None); }
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
                        let cached = cached.clone();
                        if let Some((mut prev, prev_ts)) = cached {
                            prev.valid = false;
                            prev.limited = false;
                            prev.error = Some(reason);
                            let fetched = FetchedOne {
                                i,
                                def,
                                fp,
                                view: None,
                                pending: Some((prev, true, prev_ts, None)),
                                success: None,
                            };
                            self.publish_pending(reader, cfg, &fetched).await;
                            return fetched;
                        }
                        let mut v = placeholder(def);
                        v.error = Some(reason);
                        // 从没取到过数(缓存里没有它)+ 在自动隐藏名单里 + 用户没表过态
                        // → 当作"没装",连这一行都不占。
                        //
                        // **只在没有缓存视图时藏**:有缓存说明它曾经正常取过数,
                        // 这次读不到多半是瞬时的(客户端正在写文件),藏起来会让人
                        // 以为配置丢了 —— 那种情况继续走"沿用旧快照 + 标注时间"。
                        if auto_hide.iter().any(|h| h == def.id) {
                            v.hidden = true;
                            v.auto_hidden = true;
                        }
                        return FetchedOne {
                            i,
                            def,
                            fp: String::new(),
                            view: Some(v),
                            pending: None,
                            success: None,
                        };
                    }

                    let memo = self.channel_state().url_memo.get(def.id).cloned();

                    // 有多个凭据候选时依次试:只有"服务器明确说这份凭据不行"(401/403)
                    // 才换下一份 —— 网络不通时换凭据是白费功夫。
                    let mut chosen: Option<appauth::Credential> = None;
                    // 最后一次实际尝试过的凭据:失败时也要拿它标"来源",否则界面会说
                    // "未检测到登录信息"—— 明明检测到了,只是 token 过期(见 bug 报告 #2)
                    let mut last_cred: Option<appauth::Credential> = None;
                    let mut result = FetchResult::default();
                    let mut used_url = None;
                    let mut failure_kind = None;
                    // 每个候选凭据试一轮;诊断日志要知道"试了几份、最后卡在哪"。
                    let mut tried: Vec<String> = Vec::new();
                    for (ci, cred) in creds.iter().enumerate() {
                        last_cred = Some(cred.clone());
                        let desc = cred_desc(def, cred);
                        let (r, url, kind) = self.quota_attempt(def, cred, memo.as_deref()).await;
                        result = r;
                        failure_kind = kind;
                        used_url = url;
                        tried.push(format!("{desc}→{}", if result.valid { "通" } else { "不通" }));
                        // 服务器明确说这份凭据不行:记下来,下一轮把它排到末尾。
                        // 成功了就把这条撤销(同一份凭据在别的入口下可能就是通的,
                        // 比如主域抖动时备用域 401、主域恢复后又能用)。
                        let cfp = key_fp(&cred.token);
                        {
            let mut state = self.channel_state();
            let m = &mut state.rejected;
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
                    if result.valid {
                        self.channel_state().recovery.remove(def.id);
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

                    self.enrich_qoder_result(def, chosen.as_ref(), store, qoder_web_src, &mut result).await;
                    let ts = now_ts();

                    if should_clear_memo(result.valid, used_url.is_some()) {
                        {
                            let mut state = self.channel_state();
                            let m = &mut state.url_memo;
                            m.remove(def.id);
                        }
                    }
                    if let (true, Some(url)) = (result.valid, used_url.as_ref()) {
                        {
                            let mut state = self.channel_state();
                            let m = &mut state.url_memo;
                            m.insert(def.id.to_string(), url.clone());
                        }
                    }

                    // 失败也要给界面一个"凭据是从哪读的",否则 App 型渠道的说明会退化成
                    // "未检测到登录信息",与真正的失败原因(token 过期)自相矛盾
                    let shown_cred = chosen.or_else(|| last_cred.clone());
                    let fetched = if result.valid {
                        let cred = shown_cred.as_ref().unwrap(); // creds 非空且 result.valid 为真时 chosen/last_cred 必为 Some
                        let actual_scope = credential_scope(cred);
                        self.channel_state().scopes.insert(def.id.into(), actual_scope.clone());
                        fetch_db::save_recovery(store.clone(), def.id, Snapshot::new(actual_scope, ts, result.clone())).await;
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
                        let cached = match shown_cred.as_ref() {
                            Some(cred) => self.cached(reader, def.id, &credential_scope(cred)),
                            None => cached.clone(),
                        };
                        self.failed(def.id, failure_kind);
                        let p = match cached {
                            Some((mut prev, prev_ts)) => {
                                prev.error = result.error;
                                prev.valid = false;
                                // 失联后"已限流"已成无法再确认的旧事实,别再挂着报警
                                // (扫描报告 P2-10);数值沿用快照并标注时间即可。
                                prev.limited = false;
                                (prev, true, prev_ts, shown_cred)
                            }
                            None => (result, false, 0, shown_cred),
                        };
                        FetchedOne {
                            i,
                            def,
                            fp,
                            view: None,
                            pending: Some(p),
                            success: None,
                        }
                    };
                    self.publish_pending(reader, cfg, &fetched).await;
                    fetched
                    }
                }),
        );

        // ── 自定义渠道(裁决 6A):与内置渠道同时取 ──
        // 天生独立(各自读凭据、各自请求),以前却等内置全部结束后才串行跑,
        // 每个 8 秒上限 → N 个慢渠道把整轮拖成 N×8 秒。现在两组同时驱动,
        // 最坏等待取两者较大值;自定义内部再限 4 并发,避免几十个渠道瞬间打满。
        let custom_channels: Vec<_> = cfg.custom_channels.iter().filter(|c| should_fetch(&c.id)).cloned().collect();
        let custom = self.fetch_custom_with_progress(reader, &custom_channels, hidden.as_slice());
        let (fetched, custom_results) = crate::progress::drive(async { futures::join!(builtin, custom) }, || {
            let views = crate::lockx::lock(&self.progress).flush();
            self.emit_progress(views);
        }).await;

        let cache_updates: Vec<(String, FetchResult, i64)> = fetched
            .iter()
            .filter_map(|f| {
                f.success
                    .as_ref()
                    .map(|(r, ts)| (f.def.id.to_string(), r.clone(), *ts))
            })
            .collect();

        let worker = self.clone();
        let writer = store.clone();
        let reader = reader.clone();
        let fallback = previous.clone();
        let batch = fetch_db::DbBatch { cfg, out, fetched, previous, custom_results, fx_cached, fx_now, cycle_cached };
        let mut all = match tokio::task::spawn_blocking(move || worker.finish_db(&writer, &reader, batch)).await {
            Ok(views) => views,
            Err(error) => {
                crate::diaglog::write_keyed("fetch", "view", &error.to_string());
                fallback
            }
        };

        for (id, result, ts) in cache_updates {
            self.channel_state().cache.insert(id, (result, ts));
        }

        for v in &mut all {
            v.recovery = self.channel_state().recovery.get(&v.id).map(|r| r.view(now_ts()));
        }
        crate::lockx::lock(&self.progress).finish(all.clone());
        all
    }
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "fetch_tests_view.rs"]
mod tests_view;
