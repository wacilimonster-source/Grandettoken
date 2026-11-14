//! Codex 重置卡顾问：只看周额度，回答「现在用 / 等 natural reset / 先留着 / 不知道」。
//!
//! 这里刻意是纯函数：不读数据库、不发网络请求、不兑换卡。Tauri 命令层只负责
//! 把当前 `ChannelView` 与用户填的订阅到期日拼成 `AdviceInput`，调用本模块。
//!
//! 几条不能破坏的口径：
//! - 明确的使用期限优先于省卡偏好，两个窗口分别检查。
//! - 「今天要用」以本地当天结束（23:59:59）为界，不按 24 小时近似。
//! - 5 小时窗参与即时瓶颈判断；长期计划按周额度模拟并标明范围。
//! - `available_count` 缺失不能当成 0；有明细时可以推导，但必须带 `cards_derived`。
//! - 卡到期日必须进 horizon；到期日未知的卡单独计数，不能假设永久有效。
//! - 不做「每 7 天自然重置 + 每张卡再 +1 窗」的简单加法：卡会改写周窗锚点。
//! - reason code 用穷举 `Reason` enum，文案表漏分支会在编译期报错。

use crate::codex::{ResetCredit, ResetCredits};
use crate::providers::Window;
use serde::{Deserialize, Serialize};

#[path = "codex_reset_plan.rs"]
mod plan;
#[path = "codex_reset_decision.rs"]
mod decision;
use plan::{plan_horizon, plan_lines};

pub const FULL_WINDOW: f64 = 100.0;
pub const WAIT_GAP_H: f64 = 24.0;
/// 临近恢复的省卡提示范围；明确的即时需求仍按不能等待处理。
pub const RESET_IMMINENT_H: f64 = 1.0;
/// 官方兑换资格口径:至少一项额度剩余不足 80%。两项都 ≥80% 时兑换大概率被拒。
pub const ELIGIBILITY_H: f64 = 80.0;
pub const MIN_GAIN: f64 = 25.0;
pub const SUNK_MIN_GAIN: f64 = 5.0;
pub const SUB_WARN_DAYS: f64 = 3.0;
pub const NEED_TTL_H: f64 = 12.0;
pub const STALE_MIN: i64 = 15 * 60;
pub const CYCLE_SEC: i64 = 7 * 86_400;

const HOUR: f64 = 3_600.0;
const DAY: f64 = 86_400.0;

/// 用户这次到底要不要用。`patient` 是缺省：能等就等，不为「快过期」烧卡。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Need {
    /// urgent | soon | patient | idle
    pub kind: String,
    /// 这次要用的周额度百分比；urgent/soon 缺省按 100 算，并标注 amount_defaulted。
    pub amount_percent: Option<f64>,
    /// 用户选择这个需求的时间（unix 秒）。超过 NEED_TTL_H 视为过期。
    pub at: Option<i64>,
}

impl Need {
    pub fn normalized(&self, now: i64) -> (Need, Vec<String>) {
        let mut warnings = Vec::new();
        let mut kind = self.kind.trim().to_ascii_lowercase();
        if !matches!(kind.as_str(), "urgent" | "soon" | "patient" | "idle") {
            kind = "patient".into();
            warnings.push("need_defaulted".into());
        }
        if let Some(at) = self.at {
            if now.saturating_sub(at) > (NEED_TTL_H * HOUR) as i64
                || (kind == "soon" && local_today_end(at).is_some_and(|end| now > end)) {
                kind = "patient".into();
                warnings.push("need_defaulted".into());
            }
        }
        let mut amount = self.amount_percent;
        if matches!(kind.as_str(), "urgent" | "soon") {
            match amount {
                Some(v) if v.is_finite() && v > 0.0 => {
                    amount = Some(v.min(100.0));
                }
                _ => {
                    amount = Some(100.0);
                    warnings.push("amount_defaulted".into());
                }
            }
        } else {
            amount = None;
        }
        (
            Need {
                kind,
                amount_percent: amount,
                at: self.at,
            },
            warnings,
        )
    }

    pub fn amount(&self) -> f64 {
        self.amount_percent.unwrap_or(100.0)
    }
}

/// 前端从当前 `ChannelView` 构造的输入。`subEndAt` 为空时使用有效卡的到期日规划。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AdviceInput {
    pub scope: Option<String>,
    pub plan_type: Option<String>,
    pub data_stale: bool,
    pub weekly: Option<Window>,
    pub five_hour: Option<Window>,
    pub reset_credits: Option<ResetCredits>,
    pub data_updated_at: Option<i64>,
    pub sub_end_at: Option<i64>,
    pub need: Need,
    /// 仅用于计划页的速度试算，不改变 verdict / burnPercentPerDay。
    pub plan_burn_percent_per_day: Option<f64>,
    /// 本地「今天」的结束时刻（23:59:59）。命令层会用机器时区补齐；缺省时
    /// 「今天要用」退回 24 小时口径，保证老调用方行为不变。
    pub today_end_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    pub scope: Option<String>,
    pub sub_end_at: Option<i64>,
    pub need: Need,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProfilePatch {
    pub scope: Option<String>,
    pub sub_end_at: Option<i64>,
    /// 前端清空日期时用；`subEndAt: null` 在 serde 里和「字段没传」无法区分。
    pub clear_sub_end: bool,
    pub need: Option<Need>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Metrics {
    pub weekly_remain_percent: f64,
    pub weekly_used_percent: f64,
    pub next_reset_at: Option<i64>,
    pub gap_hours: Option<f64>,
    pub cards_available: Option<u64>,
    pub cards_unknown: bool,
    pub cards_source: String,
    pub can_wait: Option<bool>,
    pub five_hour_remain_percent: Option<f64>,
    pub five_hour_reset_at: Option<i64>,
    pub earliest_card_expiry: Option<i64>,
    pub cards_expiring_before_reset: u64,
    pub all_cards_expire_before_reset: bool,
    pub burn_percent_per_day: Option<f64>,
    pub burn_estimated: bool,
    pub card_gain_percent: Option<f64>,
    pub sub_end_at: Option<i64>,
    pub days_to_sub_end: Option<f64>,
    pub plan_end_at: Option<i64>,
    /// subscription | cards，区分手填订阅截止与卡有效期规划范围。
    pub plan_end_source: Option<String>,
    pub days_to_plan_end: Option<f64>,
    pub data_updated_at: Option<i64>,
    pub plan_available: bool,
    pub planned_cards: u64,
    pub cover_cards: Option<u64>,
    pub shortfall_percent: Option<f64>,
    /// 这次声明的用量；只对 urgent/soon 有值，patient/idle 为 None。
    pub need_amount_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Advice {
    /// now | wait | hold | unknown
    pub verdict: String,
    pub reason_code: String,
    pub headline: String,
    pub detail: String,
    pub metrics: Metrics,
    pub plan_lines: Vec<String>,
    /// 稳定 warning code，前端负责中文；未知 code 必须有兜底文案。
    pub warnings: Vec<String>,
    pub computed_at: i64,
}

/// 即时结论的原因码。用穷举 enum 而不是裸字符串：`reason_copy` 的 match 不留
/// 兜底分支，新增原因码而忘记写文案会直接编译失败 —— 2026-11-13 实机暴露过
/// 「判定矩阵加了 `deadline_before_reset`、文案表没有」导致前端永远显示
/// 「暂时无法判断」的问题，这道编译门就是它的解。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    StaleData,
    SubExpired,
    DataInconsistent,
    NoWeekly,
    FullWindow,
    FullWindowBoth,
    FullWindowFiveHour,
    ResetUnknown,
    StalePastReset,
    JustRedeemed,
    NoResetBeforeSubEnd,
    DeadlineBeforeSubEnd,
    CardExpiresBeforeReset,
    DeadlineBeforeReset,
    GapTooShort,
    WaitForReset,
    RemainingCovers,
    NoNeed,
    NoCards,
    CardsUnknown,
    EligibilityUnavailable,
    FiveHourBlocked,
    FiveHourWait,
    FiveHourCoverageUnknown,
    FiveHourNotCovered,
    FiveHourResetUnknown,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::StaleData => "stale_data",
            Reason::SubExpired => "sub_expired",
            Reason::DataInconsistent => "data_inconsistent",
            Reason::NoWeekly => "no_weekly",
            Reason::FullWindow => "full_window",
            Reason::FullWindowBoth => "full_window_both",
            Reason::FullWindowFiveHour => "full_window_five_hour",
            Reason::ResetUnknown => "reset_unknown",
            Reason::StalePastReset => "stale_past_reset",
            Reason::JustRedeemed => "just_redeemed",
            Reason::NoResetBeforeSubEnd => "no_reset_before_sub_end",
            Reason::DeadlineBeforeSubEnd => "deadline_before_sub_end",
            Reason::CardExpiresBeforeReset => "card_expires_before_reset",
            Reason::DeadlineBeforeReset => "deadline_before_reset",
            Reason::GapTooShort => "gap_too_short",
            Reason::WaitForReset => "wait_for_reset",
            Reason::RemainingCovers => "remaining_covers",
            Reason::NoNeed => "no_need",
            Reason::NoCards => "no_cards",
            Reason::CardsUnknown => "cards_unknown",
            Reason::EligibilityUnavailable => "eligibility_unavailable",
            Reason::FiveHourBlocked => "five_hour_blocked",
            Reason::FiveHourWait => "five_hour_wait",
            Reason::FiveHourCoverageUnknown => "five_hour_coverage_unknown",
            Reason::FiveHourNotCovered => "five_hour_not_covered",
            Reason::FiveHourResetUnknown => "five_hour_reset_unknown",
        }
    }
}

fn clamp_pct(v: f64) -> f64 {
    if v.is_finite() {
        v.max(0.0).min(100.0)
    } else {
        0.0
    }
}

fn parse_reset(w: &Window) -> Option<i64> {
    let s = w.resets_at.as_deref()?;
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp())
        .filter(|t| *t > 0)
}

fn fmt_day(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%m-%d").to_string())
        .unwrap_or_else(|| "—".into())
}

/// 本地「今天」的 23:59:59。命令层用它填 `AdviceInput::today_end_at`，
/// 让「今天要用」按用户所在时区的一天结束为界，而不是按 24 小时近似。
/// 时钟/时区异常（DST 边界等）返回 None，调用方退回 24 小时口径。
pub fn local_today_end(now: i64) -> Option<i64> {
    use chrono::{Datelike, TimeZone};
    let dt = chrono::Local.timestamp_opt(now, 0).single()?;
    let d = dt.date_naive();
    let end = chrono::Local
        .with_ymd_and_hms(d.year(), d.month(), d.day(), 23, 59, 59)
        .single()?;
    let ts = end.timestamp();
    (ts >= now).then_some(ts)
}

fn card_count(r: Option<&ResetCredits>, now: i64) -> (Option<u64>, String, bool) {
    match r {
        // 查询失败会保留旧库存用于展示，建议需要本轮确认的可用卡数。
        Some(r) if r.error.is_some() => (None, "unknown".into(), true),
        Some(r) => {
            if let Some(n) = r.available_count {
                let expired = r.credits.as_ref().map_or(0, |list| list.iter()
                    .filter(|c| c.expires_at.is_some_and(|t| t <= now)).count() as u64);
                (Some(n.saturating_sub(expired)), "api".into(), false)
            } else if let Some(cards) = r.credits.as_ref() {
                (Some(cards.iter().filter(|c| c.expires_at.is_none_or(|t| t > now)).count() as u64), "derived".into(), false)
            } else {
                (None, "unknown".into(), true)
            }
        }
        None => (None, "unknown".into(), true),
    }
}

fn known_expiries(cards: Option<&Vec<ResetCredit>>, now: i64) -> Vec<i64> {
    let mut v: Vec<i64> = cards
        .map(|list| {
            list.iter()
                .filter_map(|c| c.expires_at)
                .filter(|t| *t > now)
                .collect()
        })
        .unwrap_or_default();
    v.sort_unstable();
    v
}

fn unknown_expiry_count(cards: Option<&Vec<ResetCredit>>, total: Option<u64>, now: i64) -> u64 {
    total.unwrap_or(0).saturating_sub(known_expiries(cards, now).len() as u64)
}

fn base_metrics(input: &AdviceInput, now: i64) -> (Metrics, Vec<String>) {
    let mut warnings = Vec::new();
    let weekly = input.weekly.as_ref();
    let remain = weekly.map_or(0.0, |w| clamp_pct(w.remain_percent));
    let used = weekly.map_or(0.0, |w| clamp_pct(w.percent));
    let reset = weekly.and_then(parse_reset);
    let gap_hours = reset.map(|r| (r - now) as f64 / HOUR);
    let (cards, source, cards_unknown) = card_count(input.reset_credits.as_ref(), now);
    let (plan_end_at, plan_end_source) = plan::deadline(input, cards, now);
    let exp = known_expiries(
        input.reset_credits.as_ref().and_then(|r| r.credits.as_ref()),
        now,
    );
    let unknown_exp = unknown_expiry_count(
        input.reset_credits.as_ref().and_then(|r| r.credits.as_ref()),
        cards,
        now,
    );
    let earliest = exp.first().copied();
    let cards_before_reset = reset.map_or(0, |r| exp.iter().filter(|t| **t < r).count() as u64);
    let all_before = cards.map_or(false, |n| {
        n > 0 && unknown_exp == 0 && exp.len() as u64 == n && reset.map_or(false, |r| exp.iter().all(|t| *t < r))
    });
    if source == "derived" {
        warnings.push("cards_derived".into());
    }
    if let (Some(n), Some(list), Some(r)) = (
        input.reset_credits.as_ref().and_then(|r| r.available_count),
        input
            .reset_credits
            .as_ref()
            .and_then(|r| r.credits.as_ref()),
        input.reset_credits.as_ref(),
    ) {
        if list.len() as u64 != n && r.error.is_none() {
            warnings.push("count_mismatch".into());
        }
    }
    if cards.unwrap_or(0) > 0 && unknown_exp > 0 {
        warnings.push("card_expiry_unknown".into());
    }
    if input.data_updated_at.is_none() {
        warnings.push("data_time_unknown".into());
    }
    let seven = input
        .five_hour
        .as_ref()
        .map(|w| clamp_pct(w.remain_percent));
    let m = Metrics {
        weekly_remain_percent: remain,
        weekly_used_percent: used,
        next_reset_at: reset,
        gap_hours,
        cards_available: cards,
        cards_unknown,
        cards_source: source,
        can_wait: None,
        five_hour_remain_percent: seven,
        five_hour_reset_at: input.five_hour.as_ref().and_then(parse_reset),
        earliest_card_expiry: earliest,
        cards_expiring_before_reset: cards_before_reset,
        all_cards_expire_before_reset: all_before,
        burn_percent_per_day: None,
        burn_estimated: false,
        card_gain_percent: None,
        sub_end_at: input.sub_end_at,
        days_to_sub_end: input
            .sub_end_at
            .map(|s| (s - now) as f64 / DAY),
        plan_end_at,
        plan_end_source,
        days_to_plan_end: plan_end_at.map(|s| (s - now) as f64 / DAY),
        data_updated_at: input.data_updated_at,
        plan_available: false,
        planned_cards: 0,
        cover_cards: None,
        shortfall_percent: None,
        need_amount_percent: None,
    };
    (m, warnings)
}

/// 时长口播口径:不足一小时按分钟,不足一天保留 0.1 小时精度,
/// ≥24 小时折成「N 天 M 小时」(小时段四舍五入,满 24 进位成整天)。
/// 前端 codex-reset.js 的 fmtGap 是同一口径的镜像,改这里必须同步改那边。
fn format_gap_hours(h: f64) -> String {
    if h < 1.0 {
        format!("约 {:.0} 分钟", h * 60.0)
    } else if h < 24.0 {
        format!("约 {:.1} 小时", h)
    } else {
        let raw_days = (h / 24.0).floor();
        let mut days = raw_days as u64;
        let rem = (h - raw_days * 24.0).round() as u64;
        if rem >= 24 {
            days += 1;
            return format!("约 {days} 天");
        }
        if rem == 0 {
            format!("约 {days} 天")
        } else {
            format!("约 {days} 天 {rem} 小时")
        }
    }
}

fn reason_copy(code: Reason, m: &Metrics) -> (String, String) {
    let gap = m
        .gap_hours
        .map(format_gap_hours)
        .unwrap_or_else(|| "重置时间未知".into());
    let gain = m
        .card_gain_percent
        .map(|v| format!("{:.1}%", v))
        .unwrap_or_else(|| "未知".into());
    let remain = format!("{:.1}%", m.weekly_remain_percent);
    // urgent/soon 的用量一定被 normalized 成正值；这里兜底成「100%」只为文案通顺。
    let amount = m
        .need_amount_percent
        .map(|v| format!("{:.0}%", v))
        .unwrap_or_else(|| "100%".into());
    match code {
        Reason::SubExpired => (
            "订阅已到期，先别动卡".into(),
            "这张卡在订阅结束后已经用不上；确认订阅状态与到期日再决定。".into(),
        ),
        Reason::NoWeekly => (
            "看不到周额度，无法判断".into(),
            "当前 Codex 视图没有 weekly 窗口；先刷新数据，别凭 5 小时窗猜。".into(),
        ),
        Reason::DataInconsistent => (
            "额度窗口数据对不上".into(),
            "已用与剩余加起来不是 100%；先刷新，别用一张卡去赌。".into(),
        ),
        Reason::FullWindow | Reason::FullWindowBoth => (
            "周额度还满，别用卡".into(),
            "周额度接近满额，建议保留重置卡。已满 100% 的窗口会保留原恢复时间。".into(),
        ),
        Reason::FullWindowFiveHour => (
            "周额度还满，先保留卡".into(),
            "周额度接近满额。若急需恢复 5 小时窗，请在 Codex 核对重置涵盖范围后自行决定。".into(),
        ),
        Reason::ResetUnknown => (
            "下次重置时间不可用".into(),
            "接口没给可解析的周窗重置时间；先刷新，再做决定。".into(),
        ),
        Reason::StalePastReset => (
            "已过重置点，先等刷新".into(),
            "周窗重置点已经过去；这可能是旧快照，刷新后再判断。".into(),
        ),
        Reason::JustRedeemed => (
            "刚用过卡，先等数据回写".into(),
            "最近一次兑换晚于当前数据；等下一轮刷新，避免连续烧卡。".into(),
        ),
        Reason::StaleData => (
            "当前数据较旧，先刷新".into(),
            "Codex 取数失败、快照超过 15 分钟或缺少更新时间，刷新成功后再计算建议。".into(),
        ),
        Reason::NoResetBeforeSubEnd => (
            "订阅结束前等不到下次自然重置".into(),
            "等不到免费重置，但当前需求也不急；先留着卡，等真的要用再说。".into(),
        ),
        Reason::DeadlineBeforeSubEnd => (
            "订阅结束前就需要额度".into(),
            "能等的自然重置在订阅结束后；现在要用，只能用卡。".into(),
        ),
        Reason::CardExpiresBeforeReset => (
            "卡等不到自然重置就会过期".into(),
            "最早到期的卡在下次自然重置前失效；如果这次确实要用，先用最早到期的那张。".into(),
        ),
        Reason::DeadlineBeforeReset => (
            "自然重置赶不上这次，用卡".into(),
            format!(
                "下次自然重置在 {gap} 后；按这次 {amount} 的用量，当前剩余 {remain} 不够，要用只能先兑换一张卡补满。用卡会改写每周重置日。"
            ),
        ),
        Reason::GapTooShort => (
            format!("等 {gap} 的自然重置更合算"),
            format!(
                "同一计划范围内，现在用卡的周额度净收益估算为 {gain}；按所选需求可以等待自然恢复并保留卡。"
            ),
        ),
        Reason::WaitForReset => (
            format!("等 {gap} 的自然重置"),
            "当前选择能等，可以在自然恢复后使用并保留重置卡。".into(),
        ),
        Reason::RemainingCovers => (
            "周额度够这次用量，先留着卡".into(),
            match m.need_amount_percent {
                Some(a) => format!(
                    "这次预计使用 {:.0}% 周额度，当前周窗剩余 {remain} 足够；五小时额度还需按实际工作量核对。",
                    a
                ),
                None => "这次要用掉的量不超过当前周窗剩余；不用重置也能完成。".into(),
            },
        ),
        Reason::NoNeed => (
            "没有明确需求，先留着".into(),
            "没有说这次一定要用；卡是缓冲，不用为了「快过期」强行兑换。".into(),
        ),
        Reason::NoCards => (
            "没有可用重置卡".into(),
            "当前接口显示 0 张可用；先确认卡数，别按「应该还有」操作。".into(),
        ),
        Reason::CardsUnknown => (
            "卡数未知，不能建议用卡".into(),
            "接口没给出可用数量，也没有明细可推导；先刷新重置卡信息。".into(),
        ),
        Reason::EligibilityUnavailable => (
            "当前额度尚不符合兑换门槛".into(),
            "已确认的重置涵盖额度均剩余至少 80%；先使用现有额度，满足资格后再核对本次需求。".into(),
        ),
        Reason::FiveHourBlocked => (
            "五小时额度已耗尽，需要恢复这个窗口".into(),
            "本次需要立即使用；当前套餐的完整重置涵盖五小时窗口，可在 Codex 核对并兑换。周额度已满时，其恢复时间会保留。".into(),
        ),
        Reason::FiveHourWait => (
            "等受限窗口自然恢复，能够赶上本次期限".into(),
            "五小时额度当前耗尽，请按受限窗口显示的恢复时间安排使用；计划页另按周额度估算供给。".into(),
        ),
        Reason::FiveHourCoverageUnknown => (
            "先核对重置是否涵盖五小时额度".into(),
            "现有数据缺少可确认的套餐涵盖范围，请在 Codex 查看这张卡涵盖的额度后再兑换。".into(),
        ),
        Reason::FiveHourNotCovered => (
            "这张完整重置无法解除五小时限制".into(),
            "当前套餐的完整重置只恢复周额度；五小时窗口仍需自然恢复，请核对其恢复时间。".into(),
        ),
        Reason::FiveHourResetUnknown => (
            "五小时恢复时间需要刷新确认".into(),
            "五小时额度已耗尽，恢复时间缺失或已过去；刷新后再判断能否赶上本次期限。".into(),
        ),
    }
}

/// 主判定。`now` 由命令层传入，测试可以固定时间。
pub fn advise(input: &AdviceInput, now: i64) -> Advice {
    let (mut m, mut warnings) = base_metrics(input, now);
    let mut plan_text: Vec<String> = Vec::new();
    let (need, need_warnings) = input.need.normalized(now);
    warnings.extend(need_warnings);
    let urgent_like = matches!(need.kind.as_str(), "urgent" | "soon");
    m.need_amount_percent = if urgent_like {
        Some(need.amount())
    } else {
        None
    };
    if input.data_stale || input.data_updated_at.is_some_and(|t| now.saturating_sub(t) > STALE_MIN) {
        return finish("unknown", Reason::StaleData, m, warnings, input, now, plan_text);
    }
    if input.sub_end_at.is_some_and(|s| s > 0 && s <= now) {
        return finish("hold", Reason::SubExpired, m, warnings, input, now, plan_text);
    }
    if input.weekly.as_ref().is_some_and(|w| !w.percent.is_finite()
        || !w.remain_percent.is_finite() || !(-1.0..=101.0).contains(&w.percent)
        || !(-1.0..=101.0).contains(&w.remain_percent)) {
        return finish("unknown", Reason::DataInconsistent, m, warnings, input, now, plan_text);
    }

    // 0. 最硬的几道门：没有周窗 / 数据自相矛盾 / 周窗满额 / 重置点不可用。
    if input.weekly.is_none() {
        return finish("unknown", Reason::NoWeekly, m, warnings, input, now, plan_text);
    }
    if (m.weekly_remain_percent + m.weekly_used_percent - 100.0).abs() > 1.0 {
        return finish("unknown", Reason::DataInconsistent, m, warnings, input, now, plan_text);
    }
    if input.five_hour.as_ref().is_some_and(|w| !w.percent.is_finite()
        || !w.remain_percent.is_finite() || !(0.0..=100.0).contains(&w.percent)
        || !(0.0..=100.0).contains(&w.remain_percent)
        || (w.percent + w.remain_percent - 100.0).abs() > 1.0) {
        return finish("unknown", Reason::DataInconsistent, m, warnings, input, now, plan_text);
    }
    if let Some(redeemed) = input.reset_credits.as_ref().and_then(|r| r.last_redeemed_at) {
        if input.data_updated_at.is_none_or(|updated| redeemed > updated) {
            return finish("unknown", Reason::JustRedeemed, m, warnings, input, now, plan_text);
        }
    }
    if input.data_updated_at.is_none() && urgent_like {
        return finish("unknown", Reason::StaleData, m, warnings, input, now, plan_text);
    }
    if m.weekly_remain_percent >= FULL_WINDOW {
        let (verdict, code) = decision::recommend(input, &need, &m, now, &mut warnings);
        return finish(verdict, code, m, warnings, input, now, plan_text);
    }
    let Some(reset) = m.next_reset_at else {
        return finish("unknown", Reason::ResetUnknown, m, warnings, input, now, plan_text);
    };
    let gap_h = (reset - now) as f64 / HOUR;
    if gap_h <= 0.0 {
        let code = if now.saturating_sub(reset) <= STALE_MIN {
            Reason::StalePastReset
        } else {
            Reason::ResetUnknown
        };
        return finish("unknown", code, m, warnings, input, now, plan_text);
    }
    if gap_h > 168.0 {
        return finish("unknown", Reason::ResetUnknown, m, warnings, input, now, plan_text);
    }

    // 1. 等得到吗：订阅到期日 vs 下次自然重置。
    let sub_end = input.sub_end_at.filter(|s| *s > 0);
    if let Some(s) = sub_end {
        if s <= now {
            return finish("hold", Reason::SubExpired, m, warnings, input, now, plan_text);
        }
    }
    let can_wait = sub_end.map(|s| reset <= s);
    m.can_wait = can_wait;
    if let Some(s) = sub_end {
        let days = (s - now) as f64 / DAY;
        m.days_to_sub_end = Some(days);
        if days > 0.0 && days < 1.0 && !warnings.iter().any(|w| w == "sub_ends_soon") {
            warnings.push("sub_ends_soon".into());
        } else if (1.0..SUB_WARN_DAYS).contains(&days) && !warnings.iter().any(|w| w == "sub_short") {
            warnings.push("sub_short".into());
        }
    }

    // 2. 周期消耗 / 现在用卡能换回多少。
    let elapsed_days = (7.0 - gap_h / 24.0).max(0.25).min(7.0);
    let burn = if elapsed_days > 0.0 {
        m.weekly_used_percent / elapsed_days
    } else {
        0.0
    };
    m.burn_percent_per_day = Some(burn);
    m.burn_estimated = elapsed_days >= 0.5 && m.weekly_used_percent >= 5.0 && burn > 0.0;
    m.card_gain_percent = if m.burn_estimated {
        Some(plan::now_gain(now, m.plan_end_at.unwrap_or(reset), reset, burn, m.weekly_remain_percent))
    } else {
        None
    };

    let (verdict, code) = decision::recommend(input, &need, &m, now, &mut warnings);
    let card_before_reset = m.earliest_card_expiry.is_some_and(|t| t < reset);

    // 让用户看见同一计划范围内的收益与临期卡损失。
    if verdict == "now" {
        if let Some(gain) = m.card_gain_percent {
            if gain < MIN_GAIN {
                warnings.push("low_value_now".into());
            }
        } else {
            warnings.push("burn_unknown".into());
        }
    }
    if !m.burn_estimated {
        warnings.push("burn_unknown".into());
    }
    if input.five_hour.is_some() {
        warnings.push("five_hour_demand_unknown".into());
        warnings.push("weekly_plan_only".into());
    }
    // 等着不用的任何结论（wait/hold/unknown）都该知道：卡会在下次重置前作废。
    // 选了 NOW 时这条不成立——卡正要被用掉。
    if card_before_reset && verdict != "now" {
        warnings.push("card_expiry_loss".into());
    }
    m.plan_available = m.plan_end_at.is_some() && m.burn_estimated;
    if m.plan_end_source.as_deref() == Some("cards") {
        warnings.push("subscription_unknown".into());
    }
    if !m.plan_available {
        warnings.push("plan_unavailable".into());
    }
    if m.plan_available {
        let plan_burn = input
            .plan_burn_percent_per_day
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(burn);
        let (planned, _used, shortfall, cover) = plan_horizon(
            now,
            m.plan_end_at.unwrap(),
            reset,
            plan_burn,
            m.weekly_remain_percent,
            m.cards_available.unwrap_or(0),
            &known_expiries(
                input.reset_credits.as_ref().and_then(|r| r.credits.as_ref()),
                now,
            ),
            unknown_expiry_count(
                input.reset_credits.as_ref().and_then(|r| r.credits.as_ref()),
                m.cards_available,
                now,
            ),
        );
        m.planned_cards = planned;
        m.cover_cards = cover;
        m.shortfall_percent = Some(shortfall);
        plan_text = plan_lines(&m, need.kind.as_str(), plan_burn);
    } else {
        plan_text = if m.plan_end_at.is_some() {
            vec!["本周期消耗样本不足，暂时无法估算计划；刷新后再看".into()]
        } else {
            vec!["缺少有效重置卡到期时间；刷新卡明细或填写订阅到期日后计算计划".into()]
        };
    }

    finish(verdict, code, m, warnings, input, now, plan_text)
}

fn finish(
    verdict: &str,
    code: Reason,
    m: Metrics,
    mut warnings: Vec<String>,
    input: &AdviceInput,
    now: i64,
    plan_text: Vec<String>,
) -> Advice {
    warnings.sort();
    warnings.dedup();
    let (headline, detail) = reason_copy(code, &m);
    let plan_lines = if plan_text.is_empty() {
        plan_lines_not_ready(code)
    } else {
        plan_text
    };
    let _ = input;
    Advice {
        verdict: verdict.into(),
        reason_code: code.as_str().into(),
        headline,
        detail,
        metrics: m,
        plan_lines,
        warnings,
        computed_at: now,
    }
}

fn plan_lines_not_ready(code: Reason) -> Vec<String> {
    match code {
        Reason::NoWeekly | Reason::ResetUnknown | Reason::StalePastReset => {
            vec!["先刷新 Codex 周窗数据".into()]
        }
        Reason::FullWindow | Reason::FullWindowBoth | Reason::FullWindowFiveHour => {
            vec!["周窗还满，计划区不需要用卡".into()]
        }
        _ => vec!["计划区需要完整的周窗与重置时间".into()],
    }
}


#[cfg(test)]
#[path = "codex_reset_date_tests.rs"]
mod date_tests;

#[cfg(test)]
#[path = "codex_reset_policy_tests.rs"]
mod policy_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_copy_splits_days_and_hours() {
        assert_eq!(format_gap_hours(0.5), "约 30 分钟");
        assert_eq!(format_gap_hours(5.3), "约 5.3 小时");
        // 不足一天的边界:23.96h 仍按小时口径,不折天
        assert_eq!(format_gap_hours(23.96), "约 24.0 小时");
        assert_eq!(format_gap_hours(24.0), "约 1 天");
        // 47.7h:小时段四舍五入到 24,进位成整天
        assert_eq!(format_gap_hours(47.7), "约 2 天");
        assert_eq!(format_gap_hours(144.0), "约 6 天");
        assert_eq!(format_gap_hours(146.4), "约 6 天 2 小时");
    }

    pub(super) const NOW: i64 = 1_794_544_860; // 2026-11-13 12:41 CST
    pub(super) const RESET: i64 = 1_794_563_812; // 2026-11-13 17:56:52 CST
    pub(super) const SUB: i64 = 1_795_967_940; // 2026-11-29 23:59 CST

    pub(super) fn win(remain: f64, used: f64, reset: Option<i64>, key: &str) -> Window {
        Window {
            key: key.into(),
            label: if key == "weekly" { "周额度" } else { "5 小时窗" }.into(),
            percent: used,
            remain_percent: remain,
            status: if used >= 100.0 { "rate-limited" } else { "ok" }.into(),
            resets_at: reset.map(|t| {
                chrono::DateTime::from_timestamp(t, 0)
                    .unwrap()
                    .to_rfc3339()
            }),
            main: key == "weekly",
        }
    }

    pub(super) fn credits(n: u64, expires: &[i64], last_redeemed: Option<i64>) -> ResetCredits {
        ResetCredits {
            available_count: Some(n),
            credits: Some(
                expires
                    .iter()
                    .enumerate()
                    .map(|(i, t)| ResetCredit {
                        title: format!("Full reset #{}", i + 1),
                        expires_at: Some(*t),
                    })
                    .collect(),
            ),
            last_redeemed_at: last_redeemed,
            updated_at: NOW,
            error: None,
        }
    }

    pub(super) fn input(remain: f64, used: f64, need: &str, cards: Option<ResetCredits>) -> AdviceInput {
        AdviceInput {
            scope: None,
            plan_type: None,
            data_stale: false,
            weekly: Some(win(remain, used, Some(RESET), "weekly")),
            five_hour: Some(win(100.0, 0.0, Some(RESET - 180), "5h")),
            reset_credits: cards,
            data_updated_at: Some(NOW),
            sub_end_at: Some(SUB),
            need: Need {
                kind: need.into(),
                amount_percent: Some(100.0),
                at: Some(NOW),
            },
            plan_burn_percent_per_day: None,
            today_end_at: None,
        }
    }

    pub(super) fn three_cards() -> ResetCredits {
        credits(3, &[SUB + 50_000, SUB + 120_000, SUB + 900_000], None)
    }

    #[test]
    fn patient_user_with_six_hours_left_waits_for_natural_reset() {
        let a = advise(&input(0.0, 100.0, "patient", Some(three_cards())), NOW);
        assert_eq!(a.verdict, "wait");
        assert_eq!(a.reason_code, "gap_too_short");
        assert!(a.metrics.card_gain_percent.unwrap() < 5.0);
        assert_eq!(a.metrics.cards_available, Some(3));
    }

    #[test]
    fn invalid_and_expired_inputs_do_not_recommend_using_cards() {
        for (remain, used) in [(150.0, -50.0), (-50.0, 150.0), (f64::NAN, 100.0)] {
            let a = advise(&input(remain, used, "urgent", Some(three_cards())), NOW);
            assert_eq!(a.reason_code, "data_inconsistent");
        }
        let mut i = input(0.0, 100.0, "urgent", Some(credits(1, &[NOW - 1], None)));
        assert_eq!(advise(&i, NOW).reason_code, "no_cards");
        i.sub_end_at = Some(NOW - 1);
        i.weekly = None;
        assert_eq!(advise(&i, NOW).reason_code, "sub_expired");
        i = input(0.0, 100.0, "urgent", Some(three_cards()));
        i.data_updated_at = None;
        i.reset_credits.as_mut().unwrap().last_redeemed_at = Some(NOW - 60);
        assert_eq!(advise(&i, NOW).reason_code, "just_redeemed");
    }

    #[test]
    fn decision_matrix_preserves_wait_and_card_guards() {
        for need in ["urgent", "soon", "patient", "idle"] {
            for cards in [0, 1, 3] {
                for remain in [0.0, 50.0, 99.0, 100.0] {
                    let a = advise(&input(remain, 100.0 - remain, need,
                        Some(credits(cards, &[], None))), NOW);
                    if remain >= FULL_WINDOW || cards == 0 || need == "patient" || need == "idle" {
                        assert_ne!(a.verdict, "now", "{need} {cards} {remain}");
                    }
                }
            }
        }
    }

    #[test]
    fn urgent_user_gets_now_when_cards_exist() {
        let a = advise(&input(0.0, 100.0, "urgent", Some(three_cards())), NOW);
        assert_eq!(a.verdict, "now");
        assert_eq!(a.reason_code, "deadline_before_reset");
        assert_eq!(a.metrics.cards_available, Some(3));
    }

    #[test]
    fn full_weekly_window_never_recommends_now() {
        let a = advise(&input(100.0, 0.0, "urgent", Some(three_cards())), NOW);
        assert_eq!(a.verdict, "hold");
        assert_eq!(a.reason_code, "full_window_both");
    }

    #[test]
    fn full_weekly_with_low_five_hour_is_still_hold() {
        let mut i = input(100.0, 0.0, "urgent", Some(three_cards()));
        i.five_hour = Some(win(40.0, 60.0, Some(RESET), "5h"));
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "hold");
        assert_eq!(a.reason_code, "full_window_five_hour");
    }

    #[test]
    fn unknown_cards_degrade_now_to_unknown() {
        let a = advise(&input(0.0, 100.0, "urgent", None), NOW);
        assert_eq!(a.verdict, "unknown");
        assert_eq!(a.reason_code, "cards_unknown");
        assert!(a.metrics.cards_unknown);
    }

    #[test]
    fn zero_cards_holds_instead_of_now() {
        let a = advise(&input(0.0, 100.0, "urgent", Some(credits(0, &[], None))), NOW);
        assert_eq!(a.verdict, "hold");
        assert_eq!(a.reason_code, "no_cards");
    }

    #[test]
    fn derived_count_is_not_treated_as_unknown() {
        let mut c = credits(3, &[SUB + 50_000, SUB + 120_000, SUB + 900_000], None);
        c.available_count = None;
        let a = advise(&input(0.0, 100.0, "urgent", Some(c)), NOW);
        assert_eq!(a.metrics.cards_available, Some(3));
        assert_eq!(a.metrics.cards_source, "derived");
        assert!(a.warnings.iter().any(|w| w == "cards_derived"));
        assert_eq!(a.verdict, "now");
    }

    #[test]
    fn cannot_wait_patient_holds_instead_of_wait() {
        let mut i = input(0.0, 100.0, "patient", Some(three_cards()));
        i.sub_end_at = Some(RESET - 3_600);
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "hold");
        assert_eq!(a.reason_code, "no_reset_before_sub_end");
        assert_eq!(a.metrics.can_wait, Some(false));
    }

    #[test]
    fn cannot_wait_urgent_with_enough_remaining_holds() {
        let mut i = input(60.0, 40.0, "urgent", Some(three_cards()));
        i.sub_end_at = Some(RESET - 3_600);
        i.need.amount_percent = Some(50.0);
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "hold");
        assert_eq!(a.reason_code, "remaining_covers");
    }

    #[test]
    fn inconsistent_percentages_are_unknown() {
        let a = advise(&input(50.0, 60.0, "patient", Some(three_cards())), NOW);
        assert_eq!(a.verdict, "unknown");
        assert_eq!(a.reason_code, "data_inconsistent");
    }

    #[test]
    fn missing_weekly_is_unknown() {
        let mut i = input(0.0, 100.0, "patient", Some(three_cards()));
        i.weekly = None;
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "unknown");
        assert_eq!(a.reason_code, "no_weekly");
    }

    #[test]
    fn expired_subscription_holds() {
        let mut i = input(0.0, 100.0, "urgent", Some(three_cards()));
        i.sub_end_at = Some(NOW - 1);
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "hold");
        assert_eq!(a.reason_code, "sub_expired");
    }

    #[test]
    fn just_redeemed_is_unknown() {
        let c = credits(3, &[SUB + 50_000], Some(NOW + 120));
        let a = advise(&input(0.0, 100.0, "urgent", Some(c)), NOW);
        assert_eq!(a.verdict, "unknown");
        assert_eq!(a.reason_code, "just_redeemed");
    }

    #[test]
    fn card_expiring_before_reset_turns_now() {
        let c = credits(
            3,
            &[RESET - 3_600, SUB + 50_000, SUB + 120_000],
            None,
        );
        let a = advise(&input(0.0, 100.0, "urgent", Some(c)), NOW);
        assert_eq!(a.verdict, "now");
        assert_eq!(a.reason_code, "card_expires_before_reset");
        assert_eq!(a.metrics.cards_expiring_before_reset, 1);
    }

    #[test]
    fn stale_need_defaults_to_patient() {
        let mut i = input(0.0, 100.0, "urgent", Some(three_cards()));
        i.need.at = Some(NOW - (NEED_TTL_H as i64 + 1) * 3_600);
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "wait");
        assert!(a.warnings.iter().any(|w| w == "need_defaulted"));
    }

    #[test]
    fn horizon_matches_user_ledger() {
        let a = advise(&input(0.0, 100.0, "patient", Some(three_cards())), NOW);
        assert!(a.metrics.plan_available);
        assert_eq!(a.metrics.planned_cards, 0);
        // 补满所有空窗需要 3 张；这与「省卡 0 张」不是一回事。每张卡只填几小时，
        // 所以不建议为了补满去烧卡。
        assert_eq!(a.metrics.cover_cards, Some(3));
        let s = a.metrics.shortfall_percent.unwrap();
        assert!((s - 10.9).abs() < 1.2, "shortfall={s}");
    }

    #[test]
    fn horizon_uses_plan_speed_override_without_changing_verdict() {
        let mut i = input(0.0, 100.0, "patient", Some(three_cards()));
        i.plan_burn_percent_per_day = Some(20.0);
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "wait");
        assert_eq!(a.metrics.planned_cards, 3);
        let s = a.metrics.shortfall_percent.unwrap();
        assert!((s - 5.0).abs() < 2.0, "shortfall={s}");
    }

    #[test]
    fn cover_cards_do_not_use_expired_cards() {
        let only_short = credits(1, &[RESET - 3_600], None);
        let a = advise(&input(0.0, 100.0, "patient", Some(only_short)), NOW);
        // 到期日早于当前时刻的卡已经被 codex.rs 过滤；这里模拟一张仍在未来但早于
        // 重置点的卡，计划不能假设它在 11-20 之后还能用。
        let c = credits(1, &[RESET - 3_600], None);
        let (planned, _, shortfall, cover) = plan_horizon(
            NOW,
            SUB,
            RESET,
            14.8,
            0.0,
            1,
            &known_expiries(c.credits.as_ref(), NOW),
            0,
        );
        assert_eq!(planned, 0);
        assert!(shortfall > 0.0);
        assert_eq!(cover, None);
        let _ = a;
    }

    /// 2026-11-13 实机 bug 的回归门：判定矩阵里任何一条路径都不允许掉进
    /// 「暂时无法判断」兜底文案。现在由 Reason 穷举 enum 在编译期强制，
    /// 这条测试保留为运行期证据。
    #[test]
    fn no_matrix_path_falls_back_to_unjudgeable_copy() {
        for need in ["urgent", "soon", "patient", "idle"] {
            for remain in [0.0, 50.0, 99.0, 100.0] {
                for cards in [None, Some(0), Some(3)] {
                    let i = input(remain, 100.0 - remain, need, cards.map(|n| credits(n, &[], None)));
                    let a = advise(&i, NOW);
                    assert_ne!(a.headline, "暂时无法判断", "{need} {remain} {cards:?}");
                    assert_ne!(a.headline, "", "{need} {remain} {cards:?}");
                    assert_ne!(a.detail, "", "{need} {remain} {cards:?}");
                }
            }
        }
    }

    #[test]
    fn urgent_within_an_hour_respects_deadline_and_warns_about_free_reset() {
        let mut i = input(0.0, 100.0, "urgent", Some(three_cards()));
        i.weekly = Some(win(0.0, 100.0, Some(NOW + 1_800), "weekly"));
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "now");
        assert_eq!(a.reason_code, "deadline_before_reset");
        assert!(a.warnings.iter().any(|w| w == "reset_imminent"));
    }

    #[test]
    fn urgent_far_from_reset_still_now() {
        // 默认重置点距 NOW 约 5.25 小时，大于 RESET_IMMINENT_H。
        let a = advise(&input(0.0, 100.0, "urgent", Some(three_cards())), NOW);
        assert_eq!(a.verdict, "now");
        assert_eq!(a.reason_code, "deadline_before_reset");
        assert!(a.headline.contains("用卡"));
        assert!(a.detail.contains("100%"));
    }

    #[test]
    fn soon_after_local_today_uses_card_when_short() {
        let mut i = input(0.0, 100.0, "soon", Some(three_cards()));
        // 今天还剩 1 小时，重置在 30 小时后（明天之后）→ 今天要用只能用卡。
        i.today_end_at = Some(NOW + 3_600);
        i.weekly = Some(win(0.0, 100.0, Some(NOW + 30 * 3_600), "weekly"));
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "now");
        assert_eq!(a.reason_code, "deadline_before_reset");

        // 剩余够这次用量时不用卡：这是「满足实际使用下最大化自然重置」的核心。
        let mut j = i;
        j.weekly = Some(win(60.0, 40.0, Some(NOW + 30 * 3_600), "weekly"));
        j.need.amount_percent = Some(50.0);
        let b = advise(&j, NOW);
        assert_eq!(b.verdict, "hold");
        assert_eq!(b.reason_code, "remaining_covers");
        assert!(b.detail.contains("50%"), "detail={}", b.detail);
    }

    #[test]
    fn soon_reset_landing_today_still_waits() {
        let mut i = input(0.0, 100.0, "soon", Some(three_cards()));
        // 今天还剩 10 小时，重置 5.25 小时后落在今天内 → 等。
        i.today_end_at = Some(NOW + 10 * 3_600);
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "wait");
        assert_eq!(a.reason_code, "gap_too_short");
    }

    #[test]
    fn soon_does_not_wait_past_midnight_even_when_reset_is_imminent() {
        let mut i = input(0.0, 100.0, "soon", Some(three_cards()));
        i.today_end_at = Some(NOW + 600);
        i.weekly = Some(win(0.0, 100.0, Some(NOW + 1_800), "weekly"));
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "now");
        assert_eq!(a.reason_code, "deadline_before_reset");
    }

    #[test]
    fn soon_on_last_second_of_today_does_not_fall_back_to_tomorrow() {
        let mut i = input(0.0, 100.0, "soon", Some(three_cards()));
        i.today_end_at = Some(NOW);
        i.weekly = Some(win(0.0, 100.0, Some(NOW + 7_200), "weekly"));
        assert_eq!(advise(&i, NOW).verdict, "now");
    }

    #[test]
    fn failed_card_refresh_cannot_recommend_using_previous_inventory() {
        let mut cards = three_cards();
        cards.error = Some("refresh failed".into());
        let a = advise(&input(0.0, 100.0, "urgent", Some(cards)), NOW);
        assert_eq!(a.verdict, "unknown");
        assert_eq!(a.reason_code, "cards_unknown");
        assert_eq!(a.metrics.cards_available, None);
    }

    #[test]
    fn missing_today_end_falls_back_to_24h_semantics() {
        let mut i = input(0.0, 100.0, "soon", Some(three_cards()));
        i.weekly = Some(win(0.0, 100.0, Some(NOW + 30 * 3_600), "weekly"));
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "now");
        assert_eq!(a.reason_code, "deadline_before_reset");
    }

    #[test]
    fn local_today_end_is_later_today_or_none() {
        if let Some(t) = local_today_end(NOW) {
            assert!(t > NOW);
            assert!(t - NOW <= 2 * 86_400);
        }
    }

    #[test]
    fn imminent_reset_with_expiring_card_still_respects_urgent_need() {
        let mut i = input(0.0, 100.0, "urgent", Some(three_cards()));
        i.weekly = Some(win(0.0, 100.0, Some(NOW + 1_800), "weekly"));
        i.reset_credits = Some(credits(
            3,
            &[NOW + 900, SUB + 50_000, SUB + 120_000],
            None,
        ));
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "now");
        assert_eq!(a.reason_code, "card_expires_before_reset");
        assert!(a.warnings.iter().any(|w| w == "reset_imminent"));
    }

    #[test]
    fn both_windows_above_eighty_blocks_ineligible_recommendation() {
        let mut i = input(85.0, 15.0, "urgent", Some(three_cards()));
        i.five_hour = Some(win(90.0, 10.0, Some(RESET - 180), "5h"));
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, "hold");
        assert_eq!(a.reason_code, "eligibility_unavailable");
        assert!(!a.warnings.iter().any(|w| w == "verify_eligibility"));
    }
}
