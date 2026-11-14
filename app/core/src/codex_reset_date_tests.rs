//! 可选订阅到期日的判定与序列化回归，复用主矩阵的固定时钟和库存。
use super::tests::{credits, input, three_cards, NOW, RESET, SUB};
use super::*;

#[test]
fn optional_date_preserves_immediate_advice_across_need_and_inventory_states() {
    for need in ["urgent", "soon", "patient", "idle"] {
        for remain in [0.0, 60.0] {
            for cards in [
                None,
                Some(ResetCredits {
                    available_count: Some(0),
                    credits: Some(vec![]),
                    last_redeemed_at: None,
                    updated_at: NOW,
                    error: None,
                }),
                Some(three_cards()),
            ] {
                let mut i = input(remain, 100.0 - remain, need, cards);
                i.need.amount_percent = Some(50.0);
                i.today_end_at = Some(RESET + 3_600);
                let dated = advise(&i, NOW);
                i.sub_end_at = None;
                let undated = advise(&i, NOW);
                assert_eq!(undated.verdict, dated.verdict, "{need} {remain}");
                assert_eq!(undated.reason_code, dated.reason_code, "{need} {remain}");
                assert_eq!(undated.metrics.sub_end_at, None);
                assert_eq!(undated.metrics.can_wait, None);
                assert_eq!(undated.metrics.days_to_sub_end, None);
                let has_known_cards = undated.metrics.cards_available.is_some_and(|n| n > 0);
                assert_eq!(undated.metrics.plan_available, has_known_cards);
                assert_eq!(undated.metrics.shortfall_percent.is_some(), has_known_cards);
                if has_known_cards {
                    assert_eq!(undated.metrics.plan_end_source.as_deref(), Some("cards"));
                    assert!(undated.warnings.iter().any(|w| w == "subscription_unknown"));
                } else {
                    assert!(undated.warnings.iter().any(|w| w == "plan_unavailable"));
                }
                assert!(dated.metrics.plan_available);
            }
        }
    }
}

#[test]
fn subscription_deadline_is_applied_only_when_provided() {
    let mut i = input(0.0, 100.0, "patient", Some(three_cards()));
    i.sub_end_at = Some(RESET - 3_600);
    let dated = advise(&i, NOW);
    assert_eq!(dated.reason_code, "no_reset_before_sub_end");
    assert_eq!(dated.metrics.can_wait, Some(false));
    i.sub_end_at = None;
    assert_eq!(advise(&i, NOW).reason_code, "gap_too_short");

    i.sub_end_at = Some(NOW - 1);
    assert_eq!(advise(&i, NOW).reason_code, "sub_expired");
    i.sub_end_at = None;
    assert_eq!(advise(&i, NOW).verdict, "wait");

    i.sub_end_at = Some(SUB);
    let restored = advise(&i, NOW);
    assert_eq!(restored.metrics.can_wait, Some(true));
    assert!(restored.metrics.plan_available);
    assert!(restored.metrics.shortfall_percent.is_some());
}

#[test]
fn empty_date_still_handles_need_before_todays_reset_deadline() {
    let mut i = input(0.0, 100.0, "soon", Some(three_cards()));
    i.sub_end_at = None;
    i.today_end_at = Some(NOW + 3_600);
    i.plan_burn_percent_per_day = Some(30.0);
    let a = advise(&i, NOW);
    assert_eq!(a.verdict, "now");
    assert_eq!(a.reason_code, "deadline_before_reset");
    assert!(a.metrics.plan_available);
    assert!(a.metrics.shortfall_percent.is_some());
    assert_eq!(a.metrics.plan_end_source.as_deref(), Some("cards"));
}

#[test]
fn missing_and_null_dates_deserialize_as_unknown() {
    for raw in ["{}", r#"{"subEndAt":null}"#] {
        let input: AdviceInput = serde_json::from_str(raw).unwrap();
        assert_eq!(input.sub_end_at, None);
        let profile: Profile = serde_json::from_str(raw).unwrap();
        assert_eq!(profile.sub_end_at, None);
    }
    let patch: ProfilePatch = serde_json::from_str(r#"{"clearSubEnd":true}"#).unwrap();
    assert!(patch.clear_sub_end);
    assert_eq!(patch.sub_end_at, None);
    let dated: Profile = serde_json::from_str(&format!(r#"{{"subEndAt":{SUB}}}"#)).unwrap();
    let restored: Profile = serde_json::from_str(&serde_json::to_string(&dated).unwrap()).unwrap();
    assert_eq!(restored.sub_end_at, Some(SUB));
}

#[test]
fn card_plan_uses_latest_valid_expiry_and_keeps_each_cards_own_deadline() {
    let last = NOW + 10 * 86_400;
    let cards = credits(3, &[NOW - 1, NOW + 3_600, last], None);
    let mut i = input(0.0, 100.0, "patient", Some(cards));
    i.sub_end_at = None;
    let a = advise(&i, NOW);
    assert_eq!(a.metrics.plan_end_at, Some(last));
    assert_eq!(a.metrics.earliest_card_expiry, Some(NOW + 3_600));
    assert_eq!(a.metrics.sub_end_at, None);
    assert_eq!(a.metrics.can_wait, None);
    assert!(a.metrics.plan_available);
    assert!(a.plan_lines[0].contains("最后一张有效卡到期"));
    let (planned, _, shortfall, cover) = plan_horizon(
        NOW,
        last,
        RESET,
        a.metrics.burn_percent_per_day.unwrap(),
        0.0,
        2,
        &[NOW + 3_600, last],
        0,
    );
    assert_eq!(a.metrics.planned_cards, planned);
    assert_eq!(a.metrics.shortfall_percent, Some(shortfall));
    assert_eq!(a.metrics.cover_cards, cover);

    i.sub_end_at = Some(SUB);
    let dated = advise(&i, NOW);
    assert_eq!(dated.metrics.plan_end_at, Some(SUB));
    assert_eq!(
        dated.metrics.plan_end_source.as_deref(),
        Some("subscription")
    );
}

#[test]
fn unknown_expired_and_failed_inventory_cannot_define_a_plan_deadline() {
    let mut failed = three_cards();
    failed.error = Some("read failed".into());
    for cards in [credits(1, &[], None), credits(1, &[NOW - 1], None), failed] {
        let mut i = input(0.0, 100.0, "urgent", Some(cards));
        i.sub_end_at = None;
        let a = advise(&i, NOW);
        assert_eq!(a.metrics.plan_end_at, None);
        assert!(!a.metrics.plan_available);
        assert_eq!(a.metrics.shortfall_percent, None);
        assert!(a.warnings.iter().any(|w| w == "plan_unavailable"));
    }
}

#[test]
fn clearing_subscription_date_keeps_expiry_in_immediate_advice() {
    let mut i = input(
        0.0,
        100.0,
        "soon",
        Some(credits(2, &[NOW + 3_600, SUB], None)),
    );
    i.sub_end_at = None;
    i.today_end_at = Some(RESET + 3_600);
    assert_eq!(advise(&i, NOW).reason_code, "gap_too_short");
    i.need.kind = "urgent".into();
    assert_eq!(advise(&i, NOW).reason_code, "card_expires_before_reset");
    i.need.kind = "patient".into();
    let patient = advise(&i, NOW);
    assert_eq!(patient.verdict, "wait");
    assert!(patient.warnings.iter().any(|w| w == "card_expiry_loss"));
    i.need.kind = "urgent".into();
    i.need.amount_percent = Some(50.0);
    i.weekly.as_mut().unwrap().remain_percent = 60.0;
    i.weekly.as_mut().unwrap().percent = 40.0;
    assert_eq!(advise(&i, NOW).reason_code, "remaining_covers");
}

#[test]
fn card_deadline_and_advice_expire_as_clock_advances() {
    let mut i = input(0.0, 100.0, "urgent", Some(credits(1, &[NOW + 3_600], None)));
    i.sub_end_at = None;
    assert!(advise(&i, NOW).metrics.plan_available);
    i.data_updated_at = Some(NOW + 3_600);
    let later = advise(&i, NOW + 3_600);
    assert_eq!(later.metrics.plan_end_at, None);
    assert!(!later.metrics.plan_available);
    assert_eq!(later.reason_code, "no_cards");
}

#[test]
fn fresh_cycle_has_a_deadline_but_requires_more_usage_samples() {
    let mut i = input(96.0, 4.0, "patient", Some(three_cards()));
    i.sub_end_at = None;
    let a = advise(&i, NOW);
    assert!(a.metrics.plan_end_at.is_some());
    assert!(!a.metrics.plan_available);
    assert!(a.plan_lines[0].contains("样本不足"));
}

#[test]
fn plan_excludes_a_natural_reset_exactly_at_its_deadline() {
    let mut i = input(
        0.0,
        100.0,
        "patient",
        Some(credits(1, &[RESET + CYCLE_SEC], None)),
    );
    i.sub_end_at = None;
    let a = advise(&i, NOW);
    assert!(
        a.plan_lines[0].contains("自然重置 1 次"),
        "{:?}",
        a.plan_lines
    );
}

#[test]
fn expired_first_card_does_not_hide_a_later_expiring_usable_card() {
    let (planned, _, _, _) = plan_horizon(
        NOW,
        NOW + 8 * 86_400,
        RESET,
        15.5,
        0.0,
        2,
        &[NOW + 86_400, NOW + 7 * 86_400],
        0,
    );
    assert_eq!(planned, 1);
}
