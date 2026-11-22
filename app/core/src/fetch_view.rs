use super::*;

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
pub(super) fn load_cycles(store: &Store) -> Vec<Option<CycleMemo>> {
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
pub(super) fn main_window_reset(v: &ChannelView) -> Option<i64> {
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
pub(super) fn attach_plan_costs(
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

/// 申请事件的最小跳升幅度(元)。低于这个数当四舍五入抖动,不记为一次申请。
pub(super) const CLAIM_JUMP_MIN_DELTA: f64 = 20.0;
/// 回溯窗口:与快照保留期(90 天)对齐
pub(super) const CLAIM_LOOKBACK_SEC: i64 = 90 * 86_400;

/// 合成"上次申请时间":手动修正优先,除非快照检测到了比手动写入时刻更晚的一次跳升
/// —— 那说明用户又申请了一次,自动值接管。
pub(super) fn effective_last_claim(cfg: &ClaimConfig, auto_at: Option<i64>) -> Option<(i64, &'static str)> {
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
pub(super) fn seven_day_burn(store: &Store, id: &str, key_fp: &str, now: i64) -> Option<f64> {
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

/// 近 7 天日均的批量版本(同一份历史行):覆盖不足 1 天仍然返回 None。
pub(super) fn seven_day_burn_rows(rows: &[HistoryRow], now: i64) -> Option<f64> {
    let since = now - 7 * 86_400;
    let first = first_ts_rows(rows, since)?;
    let covered = now - first;
    if covered < 86_400 {
        return None;
    }
    let used = consumed_in_window_rows(rows, since, now);
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
pub(super) fn local_date_start(y: i32, m: u32, d: u32) -> i64 {
    resolve_midnight(Local.with_ymd_and_hms(y, m, d, 0, 0, 0), y, m, d)
}

/// [`local_date_start`] 的决策体(抽出来单测:DST 的坑没法在测试里造时区)。
pub(super) fn resolve_midnight(
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
    local_day_of(now_ts())
}

/// 某一时刻落在哪个本地自然日。到账事件按它分天 —— 与 [`local_day_string`] 同一个口径,
/// 只是允许传时刻(测试要钉住"哪一天",不能用真实时钟)。
pub fn local_day_of(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d").to_string())
        // 时间戳超出可表示范围(损坏的库 / 时钟乱跳):给一个明确的哨兵,不 panic
        .unwrap_or_else(|| "1970-01-01".into())
}

/// 某一时刻所在本地日的零点(秒)。查"今天的事件"时当起点用。
pub fn local_day_start_of(ts: i64) -> i64 {
    match Local.timestamp_opt(ts, 0).single() {
        Some(d) => local_date_start(d.year(), d.month(), d.day()),
        None => ts - ts.rem_euclid(86_400),
    }
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

/// 汇总"近 30 天会过期多少"。
pub(super) fn expiring_soon(expiring: &[Expiring], now: i64) -> Option<Expiring> {
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
pub(super) fn claim_for(
    def: &providers::ProviderDef,
    cfg: &Config,
    store: &Store,
    remaining: Option<f64>,
    key_fp: &str,
    now: i64,
) -> Option<ClaimState> {
    let ccfg = cfg.claim_channels.get(def.id)?;
    if !ccfg.enabled {
        return None;
    }
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

/// 申请制判断的批量版本:输入换成已经读出的历史行,不再逐窗口查库。
/// `now` 由调用方传入 —— 与窗口/缺口共用同一个时刻,避免跨秒后边界测试不稳。
pub(super) fn claim_for_rows(
    def: &providers::ProviderDef,
    cfg: &Config,
    rows: &[HistoryRow],
    remaining: Option<f64>,
    now: i64,
) -> Option<ClaimState> {
    let ccfg = cfg.claim_channels.get(def.id)?;
    if !ccfg.enabled {
        return None;
    }
    let auto_at = last_jump_rows(
        rows,
        now - CLAIM_LOOKBACK_SEC,
        CLAIM_JUMP_MIN_DELTA,
        ccfg.amount,
        2 * 3600,
    );
    let burn = seven_day_burn_rows(rows, now);
    claim_state(ccfg, auto_at, now, remaining, burn)
}

/// 日/周/月消耗与三个环比(金额型通用)。内置渠道与自定义渠道**共用这一份**,
/// 口径漂移的机会只留一个。
#[allow(clippy::type_complexity)]
pub(super) fn amount_windows(
    store: &Store,
    id: &str,
    key_fp: &str,
    a: AmountAnchors,
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
    // 环比口径 = 上一周期的「同时段」:昨日 0 点到昨天此刻、上周同时段、
    // 上月同时段 —— 拿整段上周期比会系统性偏大(它天然比本期多一段尾巴)
    (
        cons(a.today0, a.now),
        cons(a.week0, a.now),
        cons(a.month0, a.now),
        cons(a.today0 - 86_400, a.now - 86_400),
        cons(a.week0 - 7 * 86_400, a.now - 7 * 86_400),
        // 终点必须钳在上月之内:以前把"本月已过秒数"直接加到上月 1 号上,
        // 于是 3 月 31 日时窗口落到 3 月 3 日,本月的消耗被算进"上月同期"基线
        // (短月尤其明显)。同时只取一次 prev_month_start,免得跨零点两次不等。
        cons(a.prev0, a.month_prev_to()),
    )
}

/// 一轮批量历史读取的时间锚点。
///
/// 六个消耗窗口与三个缺口必须在**同一个时刻**取"今天/本周/本月" ——
/// 以前两类各算一次,跨零点时可能一边用旧的一天、一边用新的一天(P2-4 同类)。
#[derive(Clone, Copy)]
pub(super) struct AmountAnchors {
    pub(super) today0: i64,
    pub(super) week0: i64,
    pub(super) month0: i64,
    pub(super) prev0: i64,
    pub(super) now: i64,
}

impl AmountAnchors {
    pub(super) fn at(now: i64) -> Self {
        Self {
            today0: local_midnight_today(),
            week0: local_week_start(),
            month0: local_month_start(),
            prev0: local_prev_month_start(),
            now,
        }
    }

    /// 批量读取的起点:同时覆盖上月同期基线、92 天逐日覆盖与 90 天申请跳升。
    pub(super) fn history_from(&self) -> i64 {
        (self.prev0 - 86_400)
            .min(self.now - 92 * 86_400)
            .min(self.now - CLAIM_LOOKBACK_SEC)
    }

    /// 上月同期的终点:必须钳在上月之内(与 `amount_windows` 的原公式一致)。
    pub(super) fn month_prev_to(&self) -> i64 {
        (self.prev0 + (self.now - self.month0)).min(self.month0 - 1)
    }
}

/// 金额型渠道的全部窗口统计。批量路径与旧查询路径产出同一个结构,
/// 调用方不需要知道这一轮走的是哪条路。
#[derive(Default)]
pub(super) struct AmountStats {
    pub(super) day: Option<f64>,
    pub(super) week: Option<f64>,
    pub(super) month: Option<f64>,
    pub(super) day_prev: Option<f64>,
    pub(super) week_prev: Option<f64>,
    pub(super) month_prev: Option<f64>,
    pub(super) gap_day_from: Option<i64>,
    pub(super) gap_week_from: Option<i64>,
    pub(super) gap_month_from: Option<i64>,
    pub(super) gap_days: Vec<i64>,
}

impl AmountStats {
    /// 批量路径:全部从同一次 `history_between` 的结果里切出来。
    pub(super) fn from_rows(rows: &[HistoryRow], a: AmountAnchors) -> Self {
        Self {
            day: consumption_since_rows(rows, a.today0, a.now),
            week: consumption_since_rows(rows, a.week0, a.now),
            month: consumption_since_rows(rows, a.month0, a.now),
            day_prev: consumption_since_rows(rows, a.today0 - 86_400, a.now - 86_400),
            week_prev: consumption_since_rows(rows, a.week0 - 7 * 86_400, a.now - 7 * 86_400),
            month_prev: consumption_since_rows(rows, a.prev0, a.month_prev_to()),
            gap_day_from: gap_from_rows(rows, a.today0, a.now),
            gap_week_from: gap_from_rows(rows, a.week0, a.now),
            gap_month_from: gap_from_rows(rows, a.month0, a.now),
            gap_days: daily_coverage_rows(rows, a.now - 92 * 86_400, a.now, local_tz_offset())
                .into_iter()
                .filter(|d| gap_of_day(d, a.now))
                .map(|d| d.day_start)
                .collect(),
        }
    }

    /// 回退路径:与旧版本逐条相同的 SQL 查询(批量读取失败时整块退回)。
    pub(super) fn legacy(store: &Store, id: &str, key_fp: &str, a: AmountAnchors) -> Self {
        let (day, week, month, day_prev, week_prev, month_prev) =
            amount_windows(store, id, key_fp, a);
        let fp_opt = if key_fp.is_empty() { None } else { Some(key_fp) };
        let tz = local_tz_offset();
        Self {
            day,
            week,
            month,
            day_prev,
            week_prev,
            month_prev,
            gap_day_from: gap_from(store, id, fp_opt, a.today0, a.now),
            gap_week_from: gap_from(store, id, fp_opt, a.week0, a.now),
            gap_month_from: gap_from(store, id, fp_opt, a.month0, a.now),
            gap_days: store
                .daily_coverage(id, fp_opt, a.now - 92 * 86_400, a.now, tz)
                .unwrap_or_default()
                .into_iter()
                .filter(|d| gap_of_day(d, a.now))
                .map(|d| d.day_start)
                .collect(),
        }
    }
}

/// 加载一段历史:成功则把行交给 `from_rows`,失败(含超过 50 万行)整块退回
/// `legacy`。**不做部分新旧混算** —— 一半窗口用批量、一半用旧查询,数字口径会漂。
pub(super) fn with_amount_history<T>(
    store: &Store,
    id: &str,
    key_fp: &str,
    a: AmountAnchors,
    from_rows: impl FnOnce(&[HistoryRow]) -> T,
    legacy: impl FnOnce() -> T,
) -> T {
    let fp_opt = if key_fp.is_empty() { None } else { Some(key_fp) };
    match store.history_between(id, fp_opt, a.history_from(), a.now) {
        Ok(rows) => from_rows(&rows),
        Err(e) => {
            eprintln!("批量读取历史失败 {id}: {e};本轮整块回退到逐窗口查询");
            legacy()
        }
    }
}

pub(crate) fn build_view(
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
    // 今日 / 近 7 天积分到账(只有记到账的渠道才算):查一次、切两段。
    // 「今天」由后端按 `updated_at` 算 —— 前端拿 new Date() 再算一遍,跨天/时区/DST
    // 三处都会对不上(与 done_today 同一条纪律)。查询失败就**不显示**这一行,
    // 而不是显示"今日未获取":后者是在替平台下结论,我们没有那个证据。
    let (grant_today, grant_week) = if providers::tracks_grants(def.id) {
        let day0 = local_day_start_of(updated_at);
        // 起点再归一次零点:固定减 6*86400 在夏令时切换日会落进隔壁一天的边缘一小时,
        // 窗口就不再是完整的 7 个自然日(无 DST 时区不受影响,归一化本身是幂等的)。
        let week_from = local_day_start_of(day0 - 6 * 86_400);
        match store.grants_since(def.id, week_from) {
            Ok(rows) => {
                let today = local_day_of(updated_at);
                let todays: Vec<crate::grants::Grant> =
                    rows.iter().filter(|g| g.day == today).cloned().collect();
                (
                    Some(crate::grants::summarize(&todays)),
                    Some(crate::grants::summarize(&rows)),
                )
            }
            Err(e) => {
                eprintln!("读取积分到账事件失败 {}: {e}", def.id);
                (None, None)
            }
        }
    } else {
        (None, None)
    };
    // 「现在」一律用调用方传进来的 `updated_at`,**不要**在这里再取一次 now_ts():
    // 生产调用方传的就是 now_ts()(见本文件 `let ts = now_ts();`),两者等价;
    // 而测试可以传一个合成时刻,把"今天几点"钉住 —— 之前这里偷偷用真实时钟,
    // 导致缺口类测试在刚过午夜时必挂(2026-10-21 00:07 真机撞到)。
    let now2 = updated_at;
    // P1:每个金额型渠道每轮只读一次历史,六个消耗窗口、三个缺口、逐日覆盖、
    // 申请制跳升与近 7 天日均都从同一份行里切出来。读失败(或行数超限)时
    // **整块**退回旧查询 —— 不允许一半窗口用新路径、一半用旧路径。
    let anchors = AmountAnchors::at(now2);
    let (stats, claim) = if has_amount {
        with_amount_history(
            store,
            def.id,
            key_fp,
            anchors,
            |rows| {
                (
                    AmountStats::from_rows(rows, anchors),
                    claim_for_rows(def, cfg, rows, result.remaining, now2),
                )
            },
            || {
                (
                    AmountStats::legacy(store, def.id, key_fp, anchors),
                    claim_for(def, cfg, store, result.remaining, key_fp, now2),
                )
            },
        )
    } else {
        // 非金额型没有消耗窗口;申请制状态仍按旧路径查(与旧版本一致)。
        (
            AmountStats::default(),
            claim_for(def, cfg, store, result.remaining, key_fp, now2),
        )
    };
    let AmountStats {
        day,
        week,
        month,
        day_prev,
        week_prev,
        month_prev,
        gap_day_from,
        gap_week_from,
        gap_month_from,
        gap_days,
    } = stats;

    let soon = if result.expiry_stale { None } else { expiring_soon(&result.expiring, now2) };

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
        google_cloud: None,
        quota_plan_scope: cred.filter(|_| matches!(def.id, "opencode-go" | "codex")).map(credential_scope),
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
        reset_credits: result.reset_credits,
        expiring: result.expiring,
        expiry_complete: result.expiry_complete,
        expiry_stale: result.expiry_stale,
        account_expiry: result.account_expiry,
        expiring_soon: soon,
        extra: result.extra,
        error: result.error,
        limited: result.limited,
        auth_source: match def.auth {
            AuthKind::Keyring => "keyring".into(),
            // 国际版借的是 WebSession 的凭据链路,但界面必须按 app 型渲染:
            // 没有「重新登录」按钮,也没有可粘贴的输入框(见 wb_desktop_only)。
            AuthKind::WebSession if wb_desktop_only(def.id) => "app".into(),
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
        recovery: None,
        updated_at,
        claim,
        hidden: false,
        auto_hidden: false,
        peak,
        gap_day_from,
        gap_week_from,
        gap_month_from,
        gap_days,
        accounts,
        grant_today,
        grant_week,
    }
}

/// 窗口里"从几点起才有数据":首次观测离窗口起点超过空档线才算缺口。
/// None = 起点没缺口,或这个窗口压根没有观测 —— 后者是"没数据",不是"有缺口"。
pub(super) fn gap_from(store: &Store, id: &str, key_fp: Option<&str>, from_ts: i64, now: i64) -> Option<i64> {
    let cov = store.window_coverage(id, key_fp, from_ts, now).ok()?;
    let first = cov.first_ts?;
    if first - from_ts > OBSERVE_GAP_SEC {
        Some(first)
    } else {
        None
    }
}

/// [`gap_from`] 的批量版本:覆盖统计直接从已读出的行里算(含无金额的行)。
pub(super) fn gap_from_rows(rows: &[HistoryRow], from_ts: i64, now: i64) -> Option<i64> {
    let cov = window_coverage_rows(rows, from_ts, now);
    let first = cov.first_ts?;
    if first - from_ts > OBSERVE_GAP_SEC {
        Some(first)
    } else {
        None
    }
}

/// 一天算不算"部分数据":起点晚于本地零点、或(已经过去的那天)末尾提前收工,
/// 超过空档线。判据与 peak.rs / store.rs 里的注释同一套数字。
pub(super) fn gap_of_day(d: &crate::store::DayCoverage, now: i64) -> bool {
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
pub(crate) fn placeholder(def: &providers::ProviderDef) -> ChannelView {
    ChannelView {
        google_cloud: None,
        quota_plan_scope: None,
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
        reset_credits: None,
        expiring: vec![],
        expiry_complete: false,
        expiry_stale: false,
        account_expiry: None,
        expiring_soon: None,
        extra: vec![],
        error: None,
        limited: false,
        auth_source: match def.auth {
            AuthKind::Keyring => "keyring".into(),
            AuthKind::WebSession if wb_desktop_only(def.id) => "app".into(),
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
        recovery: None,
        updated_at: 0,
        claim: None,
        hidden: false,
        auto_hidden: false,
        // 占位(没配密钥/已隐藏)不显示时段与缺口:没有数据可解释,少即是多
        peak: None,
        gap_day_from: None,
        gap_week_from: None,
        gap_month_from: None,
        gap_days: Vec::new(),
        accounts: 1,
        // 没有凭据 / 自定义渠道:不记到账,也不显示这一行(宁可不显示,不替平台下结论)
        grant_today: None,
        grant_week: None,
    }
}

/// 自定义渠道的展示名/简称/颜色(没填的给一个体面的默认,不显示空字符串)。
pub(super) fn custom_identity(ch: &custom::CustomChannel) -> (String, String, String) {
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
pub(super) fn build_custom_view(
    ch: &custom::CustomChannel,
    r: &custom::CustomResult,
    store: &Store,
    key_fp: &str,
    accounts: i64,
) -> ChannelView {
    let now = now_ts();
    // 探活模式没有数值:不装成"消耗"、不画日周月(它只回答"活着吗")
    let has_amount = r.valid && !r.probe_only && r.remaining.is_some();
    // 与内置渠道同一条批量路径:一次读取、内存切窗口;失败整块回退。
    let anchors = AmountAnchors::at(now);
    let stats = if has_amount {
        with_amount_history(
            store,
            &ch.id,
            key_fp,
            anchors,
            |rows| AmountStats::from_rows(rows, anchors),
            || AmountStats::legacy(store, &ch.id, key_fp, anchors),
        )
    } else {
        AmountStats::default()
    };
    let AmountStats {
        day,
        week,
        month,
        day_prev,
        week_prev,
        month_prev,
        gap_day_from,
        gap_week_from,
        gap_month_from,
        gap_days,
    } = stats;
    let fp_opt = if key_fp.is_empty() { None } else { Some(key_fp) };
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
        google_cloud: None,
        quota_plan_scope: None,
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
        reset_credits: None,
        expiring: Vec::new(),
        expiry_complete: false,
        expiry_stale: false,
        account_expiry: None,
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
        stale: !r.valid && r.remaining.is_some(),
        recovery: None,
        updated_at,
        claim: None,
        hidden: false,
        auto_hidden: false,
        peak: None,
        gap_day_from,
        gap_week_from,
        gap_month_from,
        gap_days,
        accounts,
        // 自定义渠道不记到账
        grant_today: None,
        grant_week: None,
    }
}

/// 自定义渠道的占位(没填密钥 / 已隐藏)。
pub(super) fn custom_placeholder(
    ch: &custom::CustomChannel,
    hidden: bool,
    error: Option<String>,
) -> ChannelView {
    let (name, short, color) = custom_identity(ch);
    ChannelView {
        google_cloud: None,
        quota_plan_scope: None,
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
        reset_credits: None,
        expiring: Vec::new(),
        expiry_complete: false,
        expiry_stale: false,
        account_expiry: None,
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
        recovery: None,
        updated_at: 0,
        claim: None,
        hidden,
        auto_hidden: false,
        peak: None,
        gap_day_from: None,
        gap_week_from: None,
        gap_month_from: None,
        gap_days: Vec::new(),
        accounts: 1,
        // 没有凭据 / 自定义渠道:不记到账,也不显示这一行(宁可不显示,不替平台下结论)
        grant_today: None,
        grant_week: None,
    }
}

