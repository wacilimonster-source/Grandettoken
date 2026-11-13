//! Codex 重置卡顾问：只看周额度，回答「现在用 / 等 natural reset / 先留着 / 不知道」。
//!
//! 这里刻意是纯函数：不读数据库、不发网络请求、不兑换卡。Tauri 命令层只负责
//! 把当前 `ChannelView` 与用户填的订阅到期日拼成 `AdviceInput`，调用本模块。
//!
//! 几条不能破坏的口径：
//! - 周窗满额（remain >= 99%）绝不返回 NOW；用卡只会把满额重置掉。
//! - 5 小时窗只影响文案，不参与 `can_wait` 与 horizon 的供给计算。
//! - `available_count` 缺失不能当成 0；有明细时可以推导，但必须带 `cards_derived`。
//! - 卡到期日必须进 horizon；到期日未知的卡单独计数，不能假设永久有效。
//! - 不做「每 7 天自然重置 + 每张卡再 +1 窗」的简单加法：卡会改写周窗锚点。

use crate::codex::{ResetCredit, ResetCredits};
use crate::providers::Window;
use serde::{Deserialize, Serialize};

pub const FULL_WINDOW: f64 = 99.0;
pub const WAIT_GAP_H: f64 = 24.0;
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
            if now.saturating_sub(at) > (NEED_TTL_H * HOUR) as i64 {
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

/// 前端从当前 `ChannelView` 构造的输入。`subEndAt` 允许为空：此时仍给即时建议，
/// 但计划区会标 `plan_unavailable`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AdviceInput {
    pub scope: Option<String>,
    pub data_stale: bool,
    pub weekly: Option<Window>,
    pub five_hour: Option<Window>,
    pub reset_credits: Option<ResetCredits>,
    pub data_updated_at: Option<i64>,
    pub sub_end_at: Option<i64>,
    pub need: Need,
    /// 仅用于计划页的速度试算，不改变 verdict / burnPercentPerDay。
    pub plan_burn_percent_per_day: Option<f64>,
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
    pub earliest_card_expiry: Option<i64>,
    pub cards_expiring_before_reset: u64,
    pub all_cards_expire_before_reset: bool,
    pub burn_percent_per_day: Option<f64>,
    pub burn_estimated: bool,
    pub card_gain_percent: Option<f64>,
    pub sub_end_at: Option<i64>,
    pub days_to_sub_end: Option<f64>,
    pub data_updated_at: Option<i64>,
    pub plan_available: bool,
    pub planned_cards: u64,
    pub cover_cards: Option<u64>,
    pub shortfall_percent: Option<f64>,
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

fn card_count(r: Option<&ResetCredits>, now: i64) -> (Option<u64>, String, bool) {
    match r {
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
        data_updated_at: input.data_updated_at,
        plan_available: false,
        planned_cards: 0,
        cover_cards: None,
        shortfall_percent: None,
    };
    (m, warnings)
}

fn reason_copy(code: &str, m: &Metrics) -> (String, String) {
    let gap = m
        .gap_hours
        .map(|h| {
            if h < 1.0 {
                format!("约 {:.0} 分钟", h * 60.0)
            } else {
                format!("约 {:.1} 小时", h)
            }
        })
        .unwrap_or_else(|| "重置时间未知".into());
    let gain = m
        .card_gain_percent
        .map(|v| format!("{:.1}%", v))
        .unwrap_or_else(|| "未知".into());
    match code {
        "sub_expired" => (
            "订阅已到期，先别动卡".into(),
            "这张卡在订阅结束后已经用不上；确认订阅状态与到期日再决定。".into(),
        ),
        "no_weekly" => (
            "看不到周额度，无法判断".into(),
            "当前 Codex 视图没有 weekly 窗口；先刷新数据，别凭 5 小时窗猜。".into(),
        ),
        "data_inconsistent" => (
            "周额度数据对不上".into(),
            "已用与剩余加起来不是 100%；先刷新，别用一张卡去赌。".into(),
        ),
        "full_window_both" | "full_window" => (
            "周额度还满，别用卡".into(),
            "周额度接近满额，建议保留重置卡。已满 100% 的窗口会保留原恢复时间。".into(),
        ),
        "full_window_five_hour" => (
            "周额度还满，先保留卡".into(),
            "周额度接近满额。若急需恢复 5 小时窗，请在 Codex 核对重置涵盖范围后自行决定。".into(),
        ),
        "reset_unknown" => (
            "下次重置时间不可用".into(),
            "接口没给可解析的周窗重置时间；先刷新，再做决定。".into(),
        ),
        "stale_past_reset" => (
            "已过重置点，先等刷新".into(),
            "周窗重置点已经过去；这可能是旧快照，刷新后再判断。".into(),
        ),
        "just_redeemed" => (
            "刚用过卡，先等数据回写".into(),
            "最近一次兑换晚于当前数据；等下一轮刷新，避免连续烧卡。".into(),
        ),
        "stale_data" => (
            "当前数据较旧，先刷新".into(),
            "Codex 上次取数失败或快照已标记为旧数据，刷新成功后再计算建议。".into(),
        ),
        "no_reset_before_sub_end" => (
            "订阅结束前等不到下次自然重置".into(),
            "等不到免费重置，但当前需求也不急；先留着卡，等真的要用再说。".into(),
        ),
        "deadline_before_sub_end" => (
            "订阅结束前就需要额度".into(),
            "能等的自然重置在订阅结束后；现在要用，只能用卡。".into(),
        ),
        "card_expires_before_reset" => (
            "卡等不到自然重置就会过期".into(),
            "最早到期的卡在下次自然重置前失效；如果这次确实要用，先用最早到期的那张。".into(),
        ),
        "gap_too_short" => (
            format!("等 {gap} 的自然重置更合算"),
            format!(
                "现在用卡只能换回 {gain} 个周窗；这次重置马上就到，免费给满额。"
            ),
        ),
        "wait_for_reset" => (
            format!("等 {gap} 的自然重置"),
            "当前需求能等到免费重置；现在用卡换回的额度不值得烧掉一张卡。".into(),
        ),
        "remaining_covers" => (
            "剩余额度够这次需求，先留着卡".into(),
            "这次要用掉的量不超过当前周窗剩余；不用重置也能完成。".into(),
        ),
        "no_need" => (
            "没有明确需求，先留着".into(),
            "没有说这次一定要用；卡是缓冲，不用为了「快过期」强行兑换。".into(),
        ),
        "no_cards" => (
            "没有可用重置卡".into(),
            "当前接口显示 0 张可用；先确认卡数，别按「应该还有」操作。".into(),
        ),
        "cards_unknown" => (
            "卡数未知，不能建议用卡".into(),
            "接口没给出可用数量，也没有明细可推导；先刷新重置卡信息。".into(),
        ),
        "plan_unavailable" => (
            "计划暂不可用".into(),
            "缺订阅到期日或本周期样本太短；即时建议仍可参考，计划先不算。".into(),
        ),
        _ => (
            "暂时无法判断".into(),
            "数据不足或接口异常；先刷新，再决定是否用卡。".into(),
        ),
    }
}

/// 主判定。`now` 由命令层传入，测试可以固定时间。
pub fn advise(input: &AdviceInput, now: i64) -> Advice {
    let (mut m, mut warnings) = base_metrics(input, now);
    let mut plan_text: Vec<String> = Vec::new();
    let mut verdict = "unknown";
    let mut code;
    let (need, need_warnings) = input.need.normalized(now);
    warnings.extend(need_warnings);
    if input.data_stale {
        return finish("unknown", "stale_data", m, warnings, input, now, plan_text);
    }
    if input.sub_end_at.is_some_and(|s| s > 0 && s <= now) {
        return finish("hold", "sub_expired", m, warnings, input, now, plan_text);
    }
    if input.weekly.as_ref().is_some_and(|w| !w.percent.is_finite()
        || !w.remain_percent.is_finite() || !(-1.0..=101.0).contains(&w.percent)
        || !(-1.0..=101.0).contains(&w.remain_percent)) {
        return finish("unknown", "data_inconsistent", m, warnings, input, now, plan_text);
    }

    // 0. 最硬的几道门：没有周窗 / 数据自相矛盾 / 周窗满额 / 重置点不可用。
    if input.weekly.is_none() {
        code = "no_weekly";
        return finish(verdict, code, m, warnings, input, now, plan_text);
    }
    if (m.weekly_remain_percent + m.weekly_used_percent - 100.0).abs() > 1.0 {
        code = "data_inconsistent";
        return finish(verdict, code, m, warnings, input, now, plan_text);
    }
    if m.weekly_remain_percent >= FULL_WINDOW {
        code = if m.five_hour_remain_percent.map_or(false, |v| v < FULL_WINDOW) {
            "full_window_five_hour"
        } else if m.five_hour_remain_percent.is_some() {
            "full_window_both"
        } else {
            "full_window"
        };
        verdict = "hold";
        return finish(verdict, code, m, warnings, input, now, plan_text);
    }
    let Some(reset) = m.next_reset_at else {
        code = "reset_unknown";
        return finish(verdict, code, m, warnings, input, now, plan_text);
    };
    let gap_h = (reset - now) as f64 / HOUR;
    if gap_h <= 0.0 {
        code = if now.saturating_sub(reset) <= STALE_MIN {
            "stale_past_reset"
        } else {
            "reset_unknown"
        };
        return finish(verdict, code, m, warnings, input, now, plan_text);
    }
    if gap_h > 168.0 {
        code = "reset_unknown";
        return finish(verdict, code, m, warnings, input, now, plan_text);
    }
    if let Some(r) = input.reset_credits.as_ref().and_then(|r| r.last_redeemed_at) {
        if input.data_updated_at.is_none_or(|updated| r > updated) {
            code = "just_redeemed";
            return finish(verdict, code, m, warnings, input, now, plan_text);
        }
    }

    // 1. 等得到吗：订阅到期日 vs 下次自然重置。
    let sub_end = input.sub_end_at.filter(|s| *s > 0);
    if let Some(s) = sub_end {
        if s <= now {
            verdict = "hold";
            code = "sub_expired";
            return finish(verdict, code, m, warnings, input, now, plan_text);
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
    let runs_out_h = if burn > 0.0 {
        m.weekly_remain_percent / burn * 24.0
    } else {
        f64::INFINITY
    };
    let idle_h = (gap_h - runs_out_h).max(0.0);
    m.card_gain_percent = if m.burn_estimated {
        Some((burn * idle_h / 24.0).min(100.0 - m.weekly_remain_percent).max(0.0))
    } else {
        None
    };

    // 3. 需求 vs 订阅截止 / 卡到期。
    let urgent_like = matches!(need.kind.as_str(), "urgent" | "soon");
    let need_amount = need.amount();
    let card_before_reset = m.earliest_card_expiry.map_or(false, |t| t < reset);

    if can_wait == Some(false) {
        if urgent_like && need_amount > m.weekly_remain_percent {
            verdict = "now";
            code = "deadline_before_sub_end";
        } else if urgent_like {
            verdict = "hold";
            code = "remaining_covers";
        } else {
            verdict = "hold";
            code = if need.kind == "idle" { "no_need" } else { "no_reset_before_sub_end" };
        }
    } else {
        if urgent_like && need_amount > m.weekly_remain_percent && card_before_reset {
            verdict = "now";
            code = "card_expires_before_reset";
        } else if need.kind == "urgent" {
            if need_amount > m.weekly_remain_percent {
                verdict = "now";
                code = "deadline_before_reset";
            } else {
                verdict = "hold";
                code = "remaining_covers";
            }
        } else if need.kind == "soon" {
            if gap_h <= WAIT_GAP_H {
                verdict = "wait";
                code = "gap_too_short";
            } else if need_amount > m.weekly_remain_percent {
                verdict = "now";
                code = "deadline_before_reset";
            } else {
                verdict = "hold";
                code = "remaining_covers";
            }
        } else if need.kind == "patient" {
            if gap_h <= WAIT_GAP_H {
                verdict = "wait";
                code = "gap_too_short";
            } else {
                verdict = "wait";
                code = "wait_for_reset";
            }
        } else {
            if gap_h <= WAIT_GAP_H {
                verdict = "wait";
                code = "gap_too_short";
            } else {
                verdict = "hold";
                code = "no_need";
            }
        }
    }

    // 4. 只有 NOW 才需要卡数。卡数 unknown / 0 都要降级，不能劝用户用不存在的卡。
    if verdict == "now" {
        warnings.push("verify_eligibility".into());
        match m.cards_available {
            None => {
                verdict = "unknown";
                code = "cards_unknown";
            }
            Some(0) => {
                verdict = "hold";
                code = "no_cards";
            }
            Some(_) => {}
        }
    }

    // 5. 让用户看见「这次用卡值不值」和卡过期损失。
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
    if card_before_reset && !urgent_like {
        warnings.push("card_expiry_loss".into());
    }
    m.plan_available = sub_end.is_some() && m.burn_estimated;
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
            sub_end.unwrap(),
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
        plan_text = vec![
            "先填订阅到期日，才能算到期的缺口".into(),
            "即时建议不受影响；计划区需要订阅截止日".into(),
            "订阅到期日只在本地保存".into(),
        ];
    }

    finish(verdict, code, m, warnings, input, now, plan_text)
}

fn finish(
    verdict: &str,
    code: &str,
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
        reason_code: code.into(),
        headline,
        detail,
        metrics: m,
        plan_lines,
        warnings,
        computed_at: now,
    }
}

fn plan_lines_not_ready(code: &str) -> Vec<String> {
    match code {
        "no_weekly" | "reset_unknown" => vec!["先刷新 Codex 周窗数据".into()],
        "full_window_both" | "full_window" | "full_window_five_hour" => {
            vec!["周窗还满，计划区不需要用卡".into()]
        }
        _ => vec!["计划区需要完整的周窗与重置时间".into()],
    }
}

fn plan_lines(m: &Metrics, need_kind: &str, burn: f64) -> Vec<String> {
    let mut lines = Vec::new();
    let sub_end = m.sub_end_at;
    let natural = natural_resets(m.next_reset_at, sub_end);
    if let Some(s) = sub_end {
        lines.push(format!(
            "自然重置 {} 次 · 到 {} 还剩 {} 天",
            natural,
            fmt_day(s),
            m.days_to_sub_end.unwrap_or(0.0).max(0.0).round() as i64
        ));
    } else {
        lines.push("先填订阅到期日，才能算到期的缺口".into());
    }
    let short = m.shortfall_percent.unwrap_or(0.0);
    if m.cards_available.is_none() {
        lines.push(format!(
            "按 {:.1}%/天：卡数未知，省卡策略先按 0 张算；订阅到期前会有一段 {:.1}% 缺口",
            burn, short
        ));
        lines.push("先刷新重置卡信息；不要按「应该还有」用卡".into());
        return lines;
    }
    let cards = m.cards_available.unwrap_or(0);
    if m.planned_cards == 0 {
        lines.push(format!(
            "按 {:.1}%/天：省卡 0 张；订阅到期前会有一段 {:.1}% 缺口",
            burn, short
        ));
    } else {
        lines.push(format!(
            "按 {:.1}%/天：省卡 {} 张；用后仍有一段 {:.1}% 缺口",
            burn, m.planned_cards, short
        ));
    }
    match m.cover_cards {
        None => {
            lines.push(format!(
                "想全程不断：现有 {} 张也不够补满，仍差 {:.1}%",
                cards, short
            ));
        }
        Some(cover) if cover == 0 => {
            lines.push(format!(
                "自然重置就够；{} 张卡是缓冲，不用为了「不用浪费」强行用",
                cards
            ));
        }
        Some(cover) if cover == m.planned_cards => {
            lines.push(format!(
                "想全程不断：用 {} 张可补满；按当前速度这是值得的",
                cover
            ));
        }
        Some(cover) => {
            let per = if cover > 0 { short / cover as f64 } else { 0.0 };
            lines.push(format!(
                "想全程不断：补满要 {} 张，但每张只换回 {:.0}%–{:.0}%，补不补都行",
                cover,
                per.max(0.0),
                m.card_gain_percent.unwrap_or(per).max(per)
            ));
        }
    }
    if need_kind == "urgent" || need_kind == "soon" {
        lines.push("这次标了「现在就要」：先确认剩余额度真的不够，再手动去 Codex 兑换".into());
    }
    lines
}

fn natural_resets(reset: Option<i64>, sub_end: Option<i64>) -> u32 {
    let (Some(r), Some(s)) = (reset, sub_end) else {
        return 0;
    };
    if r >= s {
        return 0;
    }
    (1 + (s - r) / CYCLE_SEC) as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardPolicy {
    Planned,
    Cover,
}

#[derive(Debug, Default)]
struct SimResult {
    used_cards: u64,
    shortfall: f64,
}

/// 7 天步进的确定性模拟。`planned` 用省卡策略（阈值 25%，卡快到期且已有空窗时 5%）；
/// `cover` 只要还有卡就补，用来回答「补满要几张」。
#[allow(clippy::too_many_arguments)]
fn simulate(
    start: i64,
    sub_end: i64,
    reset: i64,
    burn: f64,
    initial_remain: f64,
    total_cards: u64,
    known_expiries: &[i64],
    policy: CardPolicy,
    max_cards: u64,
) -> SimResult {
    let mut t = start;
    let mut quota = clamp_pct(initial_remain);
    let mut next = reset;
    let mut known = known_expiries.iter().copied().take(total_cards as usize).collect::<Vec<_>>();
    // 已到期/未知到期日的卡：总数减去已知且未过期的卡，最多按总数封顶。
    let mut unknown_cards = total_cards.saturating_sub(known.len() as u64);
    let mut cards_left = total_cards.min(max_cards);
    let mut consumed = 0.0;
    let mut used_cards = 0u64;
    let mut idle_sec = 0i64;
    let mut guard = 0u32;
    let demand = burn * (sub_end - start) as f64 / DAY;

    while t < sub_end && guard < 1000 {
        guard += 1;
        if burn <= 0.0 {
            break;
        }
        let t_exhaust = t + (quota / burn * DAY) as i64;
        let event = next.min(sub_end);
        if t_exhaust >= event {
            // 先到自然重置 / 订阅到期：消耗能消耗的部分，再重置或结束。
            let need = burn * (event - t) as f64 / DAY;
            let take = need.min(quota);
            consumed += take;
            quota -= take;
            t = event;
            if t >= sub_end {
                break;
            }
            if t >= next {
                quota = 100.0;
                next = next.saturating_add(CYCLE_SEC);
            }
            continue;
        }

        // 额度在两次自然重置之间耗尽。
        consumed += quota;
        t = t_exhaust;
        if t >= sub_end {
            break;
        }
        if t >= next {
            // 恰好落在重置点：自然重置优先，不花卡。
            quota = 100.0;
            next = next.saturating_add(CYCLE_SEC);
            continue;
        }
        let gap = next.min(sub_end) - t;
        let gain = (burn * gap as f64 / DAY).min(100.0);
        let upstream_sunk = known.first().map_or(false, |e| *e > t && *e < next);
        let threshold = if upstream_sunk && idle_sec > 0 {
            SUNK_MIN_GAIN
        } else {
            MIN_GAIN
        };
        let want_card = cards_left > 0
            && match policy {
                CardPolicy::Cover => true,
                CardPolicy::Planned => gain >= threshold,
            };
        if want_card {
            if take_card(&mut known, &mut unknown_cards, t) {
                cards_left = cards_left.saturating_sub(1);
                used_cards += 1;
                quota = 100.0;
                next = t.saturating_add(CYCLE_SEC);
            } else {
                // 有卡但全部到期/日期未知且已用完，不能凭空造窗。
                cards_left = 0;
                idle_sec += gap;
                t = next;
                quota = 100.0;
                next = next.saturating_add(CYCLE_SEC);
            }
        } else {
            idle_sec += gap;
            t = next;
            quota = 100.0;
            next = next.saturating_add(CYCLE_SEC);
        }
    }

    let shortfall = (demand - consumed).max(0.0);
    let _ = idle_sec;
    SimResult {
        used_cards,
        shortfall,
    }
}

fn take_card(known: &mut Vec<i64>, unknown: &mut u64, now: i64) -> bool {
    if let Some(pos) = known.iter().position(|e| *e > now) {
        known.remove(pos);
        true
    } else if *unknown > 0 {
        *unknown -= 1;
        true
    } else {
        false
    }
}

fn plan_horizon(
    now: i64,
    sub_end: i64,
    reset: i64,
    burn: f64,
    initial_remain: f64,
    total_cards: u64,
    known_expiries: &[i64],
    unknown_cards: u64,
) -> (u64, u64, f64, Option<u64>) {
    // 未知到期日只参与库存展示，计划仅承诺已确认有效期的卡。
    let total_cards = total_cards.saturating_sub(unknown_cards);
    let planned = simulate(
        now,
        sub_end,
        reset,
        burn,
        initial_remain,
        total_cards,
        known_expiries,
        CardPolicy::Planned,
        total_cards,
    );
    let demand = burn * (sub_end - now) as f64 / DAY;
    let _ = demand;
    let mut cover = None;
    for n in 0..=total_cards {
        let sim = simulate(
            now,
            sub_end,
            reset,
            burn,
            initial_remain,
            total_cards,
            known_expiries,
            CardPolicy::Cover,
            n,
        );
        if sim.shortfall <= 0.01 {
            cover = Some(n);
            break;
        }
    }
    let shortfall = planned.shortfall.max(0.0);
    (planned.used_cards, planned.used_cards, shortfall, cover)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_794_544_860; // 2026-11-13 12:41 CST
    const RESET: i64 = 1_794_563_812; // 2026-11-13 17:56:52 CST
    const SUB: i64 = 1_795_967_940; // 2026-11-29 23:59 CST

    fn win(remain: f64, used: f64, reset: Option<i64>, key: &str) -> Window {
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

    fn credits(n: u64, expires: &[i64], last_redeemed: Option<i64>) -> ResetCredits {
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

    fn input(remain: f64, used: f64, need: &str, cards: Option<ResetCredits>) -> AdviceInput {
        AdviceInput {
            scope: None,
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
        }
    }

    fn three_cards() -> ResetCredits {
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
}
