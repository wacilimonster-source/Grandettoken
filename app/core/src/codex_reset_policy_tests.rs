use super::tests::{credits, input, three_cards, win, NOW, RESET};
use super::*;

#[test]
fn full_weekly_does_not_hide_a_blocked_five_hour_window() {
    let mut i = input(100.0, 0.0, "urgent", Some(three_cards()));
    i.five_hour = Some(win(0.0, 100.0, Some(RESET), "5h"));
    for (plan, verdict, reason) in [
        (Some("plus"), "now", "five_hour_blocked"),
        (Some("pro"), "hold", "five_hour_not_covered"),
        (None, "unknown", "five_hour_coverage_unknown"),
    ] {
        i.plan_type = plan.map(str::to_string);
        let a = advise(&i, NOW);
        assert_eq!(a.verdict, verdict);
        assert_eq!(a.reason_code, reason);
    }
}

#[test]
fn soon_waits_for_all_blocked_windows_when_deadline_allows_it() {
    let mut i = input(0.0, 100.0, "soon", Some(credits(1, &[NOW + 600], None)));
    i.plan_type = Some("plus".into());
    i.today_end_at = Some(RESET + 3_600);
    i.five_hour = Some(win(0.0, 100.0, Some(RESET + 1_800), "5h"));
    let a = advise(&i, NOW);
    assert_eq!(a.verdict, "wait");
    assert_eq!(a.reason_code, "five_hour_wait");
    assert!(a.warnings.iter().any(|w| w == "card_expiry_loss"));
    i.today_end_at = Some(NOW + 3_600);
    assert_eq!(advise(&i, NOW).verdict, "now");
}

#[test]
fn plan_scope_and_eligibility_are_checked_before_recommending_a_card() {
    let mut i = input(85.0, 15.0, "urgent", Some(three_cards()));
    i.five_hour = Some(win(40.0, 60.0, Some(RESET), "5h"));
    i.plan_type = Some("plus".into());
    assert_eq!(advise(&i, NOW).verdict, "now");
    i.plan_type = Some("pro".into());
    assert_eq!(advise(&i, NOW).reason_code, "eligibility_unavailable");
    i.plan_type = None;
    assert_eq!(advise(&i, NOW).reason_code, "five_hour_coverage_unknown");
}

#[test]
fn malformed_short_window_and_old_snapshots_do_not_produce_now() {
    let mut i = input(0.0, 100.0, "urgent", Some(three_cards()));
    i.five_hour = Some(win(50.0, 100.0, Some(RESET), "5h"));
    assert_eq!(advise(&i, NOW).reason_code, "data_inconsistent");
    i.five_hour = None;
    i.data_updated_at = Some(NOW - STALE_MIN - 1);
    assert_eq!(advise(&i, NOW).reason_code, "stale_data");
}

#[test]
fn already_redeemed_short_window_is_protected_even_when_weekly_is_full() {
    let mut i = input(100.0, 0.0, "urgent", Some(three_cards()));
    i.plan_type = Some("plus".into());
    i.five_hour = Some(win(0.0, 100.0, Some(RESET), "5h"));
    i.reset_credits.as_mut().unwrap().last_redeemed_at = Some(NOW + 1);
    assert_eq!(advise(&i, NOW).reason_code, "just_redeemed");
}

#[test]
fn last_days_today_need_does_not_carry_into_a_new_day() {
    let midnight = local_today_end(NOW).unwrap() + 1;
    let need = Need {
        kind: "soon".into(),
        amount_percent: Some(50.0),
        at: Some(midnight - 600),
    };
    assert_eq!(need.normalized(midnight).0.kind, "patient");
}

#[test]
fn counterfactual_gain_accounts_for_moving_the_free_reset_anchor() {
    let gain = plan::now_gain(NOW, NOW + 6 * 86_400, NOW + 3_600, 20.0, 50.0);
    assert!(gain < 0.0, "{gain}");
}

#[test]
fn almost_full_weekly_is_not_treated_as_full_for_a_real_shortfall() {
    let mut i = input(99.0, 1.0, "urgent", Some(three_cards()));
    i.plan_type = Some("plus".into());
    i.five_hour = Some(win(40.0, 60.0, Some(RESET), "5h"));
    assert_eq!(advise(&i, NOW).verdict, "now");
}

#[test]
fn pro_can_restore_weekly_and_wait_for_short_window_within_todays_deadline() {
    let mut i = input(0.0, 100.0, "soon", Some(three_cards()));
    i.plan_type = Some("pro".into());
    i.today_end_at = Some(NOW + 3_600);
    i.five_hour = Some(win(0.0, 100.0, Some(NOW + 600), "5h"));
    let a = advise(&i, NOW);
    assert_eq!(a.verdict, "now");
    assert_eq!(a.reason_code, "deadline_before_reset");
}

#[test]
fn short_window_imminent_recovery_is_visible_when_weekly_is_full() {
    let mut i = input(100.0, 0.0, "urgent", Some(three_cards()));
    i.plan_type = Some("plus".into());
    i.five_hour = Some(win(0.0, 100.0, Some(NOW + 600), "5h"));
    let a = advise(&i, NOW);
    assert_eq!(a.verdict, "now");
    assert!(a.warnings.iter().any(|w| w == "reset_imminent"));
    assert!(a.warnings.iter().any(|w| w == "verify_eligibility"));
}
