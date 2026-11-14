//! 即时建议按用户期限、受限窗口、库存和资格顺序判定。
use super::{
    parse_reset, AdviceInput, Metrics, Need, Reason, ELIGIBILITY_H, FULL_WINDOW, HOUR,
    RESET_IMMINENT_H, WAIT_GAP_H,
};

fn covers_five_hour(input: &AdviceInput) -> Option<bool> {
    match input.plan_type.as_deref()?.to_ascii_lowercase().as_str() {
        "plus" | "business" | "business_standard" => Some(true),
        "pro" | "pro_100" | "pro_200" | "pro_500" => Some(false),
        _ => None,
    }
}

pub(super) fn recommend(
    input: &AdviceInput,
    need: &Need,
    m: &Metrics,
    now: i64,
    warnings: &mut Vec<String>,
) -> (&'static str, Reason) {
    let active = matches!(need.kind.as_str(), "urgent" | "soon");
    let weekly_short = need.amount() > m.weekly_remain_percent;
    let five_blocked = input
        .five_hour
        .as_ref()
        .is_some_and(|w| w.remain_percent <= 0.0 || w.status == "rate-limited");
    if !active {
        return if need.kind == "idle" {
            ("hold", Reason::NoNeed)
        } else if five_blocked {
            ("wait", Reason::FiveHourWait)
        } else if m.can_wait == Some(false) {
            ("hold", Reason::NoResetBeforeSubEnd)
        } else if m.gap_hours.is_some_and(|gap| gap <= WAIT_GAP_H) {
            ("wait", Reason::GapTooShort)
        } else {
            ("wait", Reason::WaitForReset)
        };
    }
    if !five_blocked && m.weekly_remain_percent >= FULL_WINDOW {
        let code = if m.five_hour_remain_percent.is_some_and(|v| v < FULL_WINDOW) {
            Reason::FullWindowFiveHour
        } else if m.five_hour_remain_percent.is_some() {
            Reason::FullWindowBoth
        } else {
            Reason::FullWindow
        };
        return ("hold", code);
    }
    if !weekly_short && !five_blocked {
        return ("hold", Reason::RemainingCovers);
    }

    let day_end = input
        .today_end_at
        .filter(|t| *t >= now)
        .unwrap_or(now + (WAIT_GAP_H * HOUR) as i64);
    let deadline = input
        .sub_end_at
        .filter(|t| *t > now)
        .map_or(day_end, |end| day_end.min(end));
    let weekly_ready = if weekly_short {
        m.next_reset_at
    } else {
        Some(now)
    };
    let five_ready = if five_blocked {
        input
            .five_hour
            .as_ref()
            .and_then(parse_reset)
            .filter(|t| *t > now)
    } else {
        Some(now)
    };
    if need.kind == "soon"
        && weekly_ready
            .zip(five_ready)
            .is_some_and(|(week, five)| week.max(five) <= deadline)
    {
        return (
            "wait",
            if five_blocked {
                Reason::FiveHourWait
            } else {
                Reason::GapTooShort
            },
        );
    }
    if five_blocked {
        if five_ready.is_none() && need.kind != "urgent" {
            return ("unknown", Reason::FiveHourResetUnknown);
        }
        match covers_five_hour(input) {
            None => return ("unknown", Reason::FiveHourCoverageUnknown),
            Some(false) => {
                // 今天内短窗能恢复时，仍可用卡补不足的周额度。
                if need.kind != "soon" || five_ready.is_none_or(|t| t > deadline) {
                    return ("hold", Reason::FiveHourNotCovered);
                }
            }
            Some(true) => {}
        }
    }
    match m.cards_available {
        None => return ("unknown", Reason::CardsUnknown),
        Some(0) => return ("hold", Reason::NoCards),
        Some(_) => {}
    }
    if m.weekly_remain_percent >= ELIGIBILITY_H {
        match covers_five_hour(input) {
            Some(true)
                if m.five_hour_remain_percent
                    .is_some_and(|v| v < ELIGIBILITY_H) => {}
            Some(false) => return ("hold", Reason::EligibilityUnavailable),
            Some(true) if m.five_hour_remain_percent.is_some() => {
                return ("hold", Reason::EligibilityUnavailable)
            }
            _ if m
                .five_hour_remain_percent
                .is_some_and(|v| v >= ELIGIBILITY_H) =>
            {
                return ("hold", Reason::EligibilityUnavailable)
            }
            _ => return ("unknown", Reason::FiveHourCoverageUnknown),
        }
    }
    if weekly_ready
        .zip(five_ready)
        .is_some_and(|(week, five)| (week.max(five) - now) as f64 <= RESET_IMMINENT_H * HOUR)
    {
        warnings.push("reset_imminent".into());
    }
    warnings.push("verify_eligibility".into());
    let code = if five_blocked && covers_five_hour(input) == Some(true) {
        Reason::FiveHourBlocked
    } else if m
        .earliest_card_expiry
        .zip(m.next_reset_at)
        .is_some_and(|(expiry, reset)| expiry < reset)
    {
        Reason::CardExpiresBeforeReset
    } else if m.can_wait == Some(false) {
        Reason::DeadlineBeforeSubEnd
    } else {
        Reason::DeadlineBeforeReset
    };
    ("now", code)
}
