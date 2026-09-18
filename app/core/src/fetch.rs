//! 取数与视图组装。
//!
//! 核心原则:**失败不降级成 0**。取数失败时继续显示上次成功快照并标注时间,
//! 因为余额类工具最危险的 bug 就是把请求失败渲染成"余额归零",用户会以为欠费了。

use crate::appauth::{self};
use crate::config::{ClaimConfig, Config};
use crate::providers::{self, AuthKind, Expiring, FetchResult, Kind, Method};
use crate::store::Store;
use crate::secrets;
use chrono::{Datelike, Duration, Local, TimeZone, Weekday};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 距到期多久算"近期"。界面上的"近 30 天将过期"用的就是它。
pub const EXPIRING_SOON_DAYS: i64 = 30;

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
    /// 凭据来源:"keyring"(用户填的密钥) / "app"(复用本机应用的登录态)
    pub auth_source: String,
    /// 凭据来源的说明文字,直接显示在界面上
    pub auth_label: String,
    /// 今日 / 本周 / 本月消耗。None 表示数据窗口有空洞,不能报 0
    pub day: Option<f64>,
    pub week: Option<f64>,
    pub month: Option<f64>,
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

/// 一次尝试的失败类型 —— 它决定"要不要换个域名再试"。
/// 分错会让排障变难:密钥错(4xx)换域名只是白等,网络故障不换就白白瞎掉一个渠道。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    /// 连不上 / DNS / 超时:换域名
    Transport,
    /// 5xx、429、响应不是 JSON:换域名(可能是入口自身的问题或限流)
    Server,
    /// 4xx(除 429):密钥/权限问题,换域名也是同一个答案
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
/// 几份登录态(Trae 装了多个版本、WorkBuddy 有几份 .info 备份),其中一份可能已作废。
/// 网络不通、5xx、业务失败都跟身份无关,换凭据只是白等。
pub fn another_credential_may_help(kind: Option<FailKind>) -> bool {
    kind == Some(FailKind::Client)
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
fn seven_day_burn(store: &Store, id: &str, now: i64) -> Option<f64> {
    let since = now - 7 * 86_400;
    let first = store.first_ts_since(id, since).ok().flatten()?;
    let covered = now - first;
    if covered < 86_400 {
        return None;
    }
    let used = store.consumed_in_window(id, since, now).ok()?;
    Some(used / (covered as f64 / 86_400.0))
}

pub fn now_ts() -> i64 {
    Local::now().timestamp()
}

/// 本地零点。按用户本地时区切,不按 UTC —— 否则跨时区时"今日消耗"会指错日子。
fn local_date_start(y: i32, m: u32, d: u32) -> i64 {
    Local
        .with_ymd_and_hms(y, m, d, 0, 0, 0)
        .single()
        .map(|x| x.timestamp())
        .unwrap_or_else(now_ts)
}

pub fn local_midnight_today() -> i64 {
    let n = Local::now();
    local_date_start(n.year(), n.month(), n.day())
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

/// 单次尝试:一个地址、一次请求。返回结果与"这次失败值不值得换域名"。
async fn fetch_once(
    client: &reqwest::Client,
    def: &providers::ProviderDef,
    url: &str,
    key: &str,
) -> (FetchResult, Option<FailKind>) {
    let mut req = match def.method {
        Method::Get => client.get(url),
        Method::PostJson(body) => client
            .post(url)
            .header("Content-Type", "application/json")
            .body(body()),
    }
    .header(
        "Authorization",
        format!("{}{}", def.auth_prefix, key.trim()),
    )
    .header("Accept", "application/json")
    // 单次尝试的预算:4 个入口最坏 32 秒,还收在 60 秒轮询周期内。
    // 用客户端的 20 秒默认值会让"某个域名挂着"直接拖垮整轮取数。
    .timeout(std::time::Duration::from_secs(8));

    for (k, v) in def.extra_headers {
        req = req.header(*k, *v);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = if e.is_timeout() {
                "请求超时".to_string()
            } else if e.is_connect() {
                "无法连接(检查网络或代理)".to_string()
            } else {
                format!("请求失败: {e}")
            };
            return (
                FetchResult {
                    valid: false,
                    error: Some(msg),
                    ..Default::default()
                },
                Some(FailKind::Transport),
            );
        }
    };

    let status = resp.status();
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => {
            return (
                FetchResult {
                    valid: false,
                    error: Some(format!("响应不是合法 JSON (HTTP {})", status.as_u16())),
                    ..Default::default()
                },
                // 可能是入口返回的错误页 —— 换个域名值得一试
                Some(FailKind::Server),
            )
        }
    };

    let mut result = (def.extract)(&body);

    // 非 2xx 却解析成功,说明接口用 HTTP 状态码表达业务错误 —— 以状态码为准。
    // OpenCode 的 401/403 带 type:error 体,已被 extractor 处理,不覆盖它的文案。
    if !status.is_success() && result.valid {
        result.valid = false;
        result.error = Some(format!("HTTP {}", status.as_u16()));
    }
    // 复用本机登录态的渠道:401/403 不是"配置错了",而是那份登录态过期了 ——
    // 用户能做的只有去客户端里重新登录一次,直接把话说清楚。
    if !status.is_success() && (status.as_u16() == 401 || status.as_u16() == 403) {
        if let AuthKind::App(app) = def.auth {
            result.error = Some(format!("登录状态已失效 · 打开一次 {} 即可", app.label()));
        }
    }

    let kind = if result.valid {
        None
    } else if !status.is_success() {
        let code = status.as_u16();
        Some(if code == 429 || code >= 500 {
            FailKind::Server
        } else {
            FailKind::Client // 401/403 等:密钥或权限问题,换域名也是同一答案
        })
    } else {
        Some(FailKind::Business) // HTTP 200 但业务失败
    };

    (result, kind)
}

/// 返回 (结果, 成功时用的地址, 最后一次失败的类别)。调用方据此记住"哪个入口是通的"。
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
            let used = if result.valid { Some(url.clone()) } else { None };
            return (result, used, kind);
        }
        last_kind = kind;
        last = Some(result);
    }
    // order 至少含主地址,这里到不了;给个兜底避免 unwrap
    (
        last.unwrap_or(FetchResult {
            valid: false,
            error: Some("没有可用的接口地址".into()),
            ..Default::default()
        }),
        None,
        last_kind,
    )
}

/// 取一个渠道的凭据候选(按可信度排序)。空的返回值 = 没有可用凭据。
fn credentials(def: &providers::ProviderDef) -> Vec<appauth::Credential> {
    match def.auth {
        AuthKind::Keyring => secrets::get(def.id)
            .map(|k| {
                vec![appauth::Credential {
                    token: k,
                    source: String::new(),
                }]
            })
            .unwrap_or_default(),
        // 本机应用的登录态:可能有几份(多个安装 / 旧备份),按可信度依次试
        AuthKind::App(app) => appauth::candidates(app),
    }
}

/// 凭据来源的说明文字,直接显示在界面上。
fn auth_label(def: &providers::ProviderDef, cred: Option<&appauth::Credential>) -> String {
    match def.auth {
        AuthKind::Keyring => "Windows 凭据管理器".into(),
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
fn claim_for(def: &providers::ProviderDef, cfg: &Config, store: &Store, remaining: Option<f64>) -> Option<ClaimState> {
    let ccfg = cfg.claim_channels.get(def.id)?;
    if !ccfg.enabled {
        return None;
    }
    let now = now_ts();
    let auto_at = store
        .last_jump_since(def.id, now - CLAIM_LOOKBACK_SEC, CLAIM_JUMP_MIN_DELTA)
        .ok()
        .flatten();
    let burn = seven_day_burn(store, def.id, now);
    claim_state(ccfg, auto_at, now, remaining, burn)
}

fn build_view(
    def: &providers::ProviderDef,
    result: FetchResult,
    stale: bool,
    updated_at: i64,
    store: &Store,
    cfg: &Config,
    cred: Option<&appauth::Credential>,
) -> ChannelView {
    // 只有金额型才有"消耗"这个概念可算;配额型的用量由接口直接给出。
    // 注意:**不要**把 `result.valid` 串进来 —— 消耗是本地快照推算的,和这次网络请求
    // 成不成功无关。之前取数一失败就把三个窗口一起置 None,汇总里"今日/本周/本月"
    // 会整片显示"数据不足",明明本地有快照。
    let has_amount = result.kind == Kind::Amount && result.remaining.is_some();
    let (day, week, month) = if has_amount {
        let now = now_ts();
        (
            store
                .consumption_since(def.id, local_midnight_today(), now)
                .ok()
                .flatten(),
            store
                .consumption_since(def.id, local_week_start(), now)
                .ok()
                .flatten(),
            store
                .consumption_since(def.id, local_month_start(), now)
                .ok()
                .flatten(),
        )
    } else {
        (None, None, None)
    };

    let soon = expiring_soon(&result.expiring, now_ts());

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
            AuthKind::App(_) => "app".into(),
        },
        auth_label: auth_label(def, cred),
        day,
        week,
        month,
        estimated: result.kind == Kind::Amount,
        stale,
        updated_at,
        claim: claim_for(def, cfg, store, result.remaining),
        hidden: false,
    }
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
            AuthKind::App(_) => "app".into(),
        },
        auth_label: auth_label(def, None),
        day: None,
        week: None,
        month: None,
        estimated: false,
        stale: false,
        updated_at: 0,
        claim: None,
        hidden: false,
    }
}

/// 单渠道并发取数的产出:要么自带成品视图(占位/隐藏),
/// 要么留 (result, stale, ts, cred) 给同步段组视图;success 记快照与缓存。
struct FetchedOne {
    i: usize,
    def: &'static providers::ProviderDef,
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
        }
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
        // 隐藏渠道清单先读出来:隐藏 = 不发请求、不读本机登录凭据。
        // 仍返回一个占位视图(has_key 照实),管理页要靠它列出「已隐藏」卡片。
        // Arc:并发时每渠道克隆一次指针,而不是把 Vec 移进每个 future。
        let hidden: Arc<Vec<String>> = Arc::new({
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            Config::load(&s).hidden_channels
        });

        let fetched = futures::future::join_all(
            providers::PROVIDERS
                .iter()
                .enumerate()
                .map(|(i, def)| {
                    let hidden = hidden.clone();
                    async move {
                    let creds = credentials(def);
                    if hidden.iter().any(|h| h == def.id) {
                        let mut v = placeholder(def);
                        v.has_key = !creds.is_empty();
                        v.hidden = true;
                        return FetchedOne {
                            i,
                            def,
                            view: Some(v),
                            pending: None,
                            success: None,
                        };
                    }
                    if creds.is_empty() {
                        return FetchedOne {
                            i,
                            def,
                            view: Some(placeholder(def)),
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
                    for cred in creds.iter() {
                        last_cred = Some(cred.clone());
                        let (r, url, kind) =
                            fetch_channel(&self.http, def, &cred.token, memo.as_deref()).await;
                        result = r;
                        used_url = url;
                        if result.valid {
                            chosen = Some(cred.clone());
                            break;
                        }
                        if another_credential_may_help(kind) {
                            continue;
                        }
                        break;
                    }
                    let ts = now_ts();

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
                                (prev, true, prev_ts, shown_cred)
                            }
                            None => (result, false, ts, shown_cred),
                        };
                        FetchedOne {
                            i,
                            def,
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

        // ── 同步段:一次性锁库完成快照写入与消耗推算,期间不跨 await ──
        {
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            for f in &fetched {
                if let Some((res, ts)) = f.success.as_ref() {
                    let kind = match res.kind {
                        Kind::Amount => "amount",
                        Kind::Percent => "percent",
                        Kind::Points => "points",
                    };
                    // 注意顺序:先写快照再组视图 —— 申请制额度的跳升检测要用最新快照
                    let _ = s.record(f.def.id, *ts, res.remaining, res.used, res.total, kind);
                }
            }
            // 配置从库里读:前端改完设置立刻生效,不必等下一次轮询换内存态
            let cfg = Config::load(&s);
            for f in fetched {
                if let Some(v) = f.view {
                    out[f.i] = Some(v);
                } else if let Some((result, stale, ts, cred)) = f.pending {
                    out[f.i] = Some(build_view(f.def, result, stale, ts, &s, &cfg, cred.as_ref()));
                }
            }
        }

        for (id, result, ts) in cache_updates {
            if let Ok(mut c) = self.cache.lock() {
                c.insert(id, (result, ts));
            }
        }

        out.into_iter().flatten().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn month_start_is_first_day() {
        let ts = local_month_start();
        let d = Local.timestamp_opt(ts, 0).unwrap();
        assert_eq!(d.day(), 1);
        assert_eq!(d.hour(), 0);
        assert_eq!(d.minute(), 0);
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

        let st = claim_for(def, &cfg, &s, Some(18.99)).expect("4sapi 默认开启申请制");
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
        let st = claim_for(def_h, &cfg2, &s, Some(55.0)).unwrap();
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

    /// 换域名重试的策略:网络/5xx/429 才值得换;密钥错与业务失败不换。
    #[test]
    fn only_transport_and_server_failures_retry() {
        assert!(FailKind::Transport.retryable());
        assert!(FailKind::Server.retryable());
        assert!(!FailKind::Client.retryable());   // 401/403:换域名也是同一答案
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

    /// 换凭据的判定:只有 401/403 值得换下一份登录态。
    #[test]
    fn only_client_failures_try_another_credential() {
        assert!(another_credential_may_help(Some(FailKind::Client)));
        assert!(!another_credential_may_help(Some(FailKind::Transport)));
        assert!(!another_credential_may_help(Some(FailKind::Server)));
        assert!(!another_credential_may_help(Some(FailKind::Business)));
        assert!(!another_credential_may_help(None));
    }
}
