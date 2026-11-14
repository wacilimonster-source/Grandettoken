//! 重置建议的计划截止、逐卡供给模拟与计划文案。
use super::{
    clamp_pct, fmt_day, known_expiries, AdviceInput, Metrics, CYCLE_SEC, DAY, MIN_GAIN,
    SUNK_MIN_GAIN,
};

pub(super) fn deadline(
    input: &AdviceInput,
    cards: Option<u64>,
    now: i64,
) -> (Option<i64>, Option<String>) {
    if let Some(end) = input.sub_end_at.filter(|t| *t > 0) {
        return (Some(end), Some("subscription".into()));
    }
    let mut expiries = known_expiries(
        input
            .reset_credits
            .as_ref()
            .and_then(|r| r.credits.as_ref()),
        now,
    );
    expiries.truncate(cards.unwrap_or(0) as usize);
    (
        expiries.last().copied(),
        expiries.last().map(|_| "cards".into()),
    )
}

pub(super) fn plan_lines(m: &Metrics, need_kind: &str, burn: f64) -> Vec<String> {
    let mut lines = Vec::new();
    let end = m.plan_end_at;
    let natural = natural_resets(m.next_reset_at, end);
    let end_label = if m.plan_end_source.as_deref() == Some("cards") {
        "最后一张有效卡到期"
    } else {
        "订阅到期"
    };
    if let Some(s) = end {
        lines.push(format!(
            "计划至{end_label} {} · 自然重置 {} 次 · 还剩 {} 天",
            fmt_day(s),
            natural,
            m.days_to_plan_end.unwrap_or(0.0).max(0.0).round() as i64
        ));
    }
    let short = m.shortfall_percent.unwrap_or(0.0);
    if m.cards_available.is_none() {
        lines.push(format!(
            "按 {:.1}%/天：卡数未知，省卡策略先按 0 张算；计划截止前会有一段 {:.1}% 缺口",
            burn, short
        ));
        lines.push("先刷新重置卡信息；不要按「应该还有」用卡".into());
        return lines;
    }
    let cards = m.cards_available.unwrap_or(0);
    if m.planned_cards == 0 {
        lines.push(format!(
            "按 {:.1}%/天：省卡 0 张；计划截止前会有一段 {:.1}% 缺口",
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
                "仅按周额度试算连续供给：现有 {} 张也不够补满，仍差 {:.1}%",
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
                "仅按周额度试算连续供给：用 {} 张可补满；按当前速度估算",
                cover
            ));
        }
        Some(cover) => {
            lines.push(format!(
                "仅按周额度试算连续供给：补满估计需 {} 张；省卡方案仍有 {:.1}% 缺口，按实际需求决定",
                cover, short
            ));
        }
    }
    if need_kind == "urgent" || need_kind == "soon" {
        lines.push(
            "这次有明确用量需求，先确认剩余额度与自然重置能否满足，再决定是否手动兑换".into(),
        );
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
    (1 + (s - r - 1) / CYCLE_SEC) as u32
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

pub(super) fn now_gain(now: i64, end: i64, reset: i64, burn: f64, remain: f64) -> f64 {
    let waiting = simulate(
        now,
        end,
        reset,
        burn,
        remain,
        0,
        &[],
        CardPolicy::Planned,
        0,
    );
    let redeeming = simulate(
        now,
        end,
        now.saturating_add(CYCLE_SEC),
        burn,
        100.0,
        0,
        &[],
        CardPolicy::Planned,
        0,
    );
    waiting.shortfall - redeeming.shortfall
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
    let mut known = known_expiries
        .iter()
        .copied()
        .take(total_cards as usize)
        .collect::<Vec<_>>();
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
        let upstream_sunk = known.iter().any(|e| *e > t && *e < next);
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

pub(super) fn plan_horizon(
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
