use super::*;
use serde_json::json;

/// 北京日:跨时区也对(用已知时刻验证)。
#[test]
fn beijing_day_is_utc_plus_8() {
    use chrono::{FixedOffset, TimeZone};
    let tz = FixedOffset::east_opt(8 * 3600).unwrap();
    // 北京 2026-10-13 00:30
    let ts = tz.with_ymd_and_hms(2026, 10, 13, 0, 30, 0).unwrap().timestamp();
    assert_eq!(beijing_day(ts), "2026-10-13");
    // 往前一小时:北京 10-12 23:30 —— 属于前一天(用 UTC 或本机时区会算成 10-12/10-13 的错位)
    assert_eq!(beijing_day(ts - 3600), "2026-10-12");
}

/// 抖动:同一天同一渠道恒定,不同天/不同渠道不同;落在 23–43 分。
#[test]
fn scheduled_minute_is_stable_and_in_window() {
    let a = scheduled_minute("2026-10-13", "trae", 0);
    let b = scheduled_minute("2026-10-13", "trae", 0);
    assert_eq!(a, b, "同一天同一渠道只抖一次(重启不重摇)");
    assert!((23..=43).contains(&a), "落在 00:23–00:43: {a}");
    let c = scheduled_minute("2026-10-14", "trae", 0);
    let d = scheduled_minute("2026-10-13", "workbuddy", 0);
    assert!(a != c || a != d, "不同的日子/渠道不该完全一样");
}

/// Qoder 的跨日时刻是 **10:00**(官方活动描述 + 实测 startAt/endAt),
/// 抖动窗口因此落在 10:00–10:20,而不是 00:23–00:43。
#[test]
fn qoder_schedules_at_ten_am() {
    let a = scheduled_minute("2026-10-20", "qoder", 10);
    let b = scheduled_minute("2026-10-20", "qoder", 10);
    assert_eq!(a, b, "同一天同一渠道只抖一次");
    assert!((600..=620).contains(&a), "落在 10:00–10:20 = 600–620 分: {a}");
    // 国内版是另一个渠道 id,抖动值与它无关(各抖各的)
    let cn = scheduled_minute("2026-10-20", "qoder-cn", 10);
    assert!((600..=620).contains(&cn));
}

/// **签到周期日**:Qoder 的"今天"从 10:00 起算。
///
/// 这条是本次改动的核心 —— 拿自然日当幂等键会让 09:59 与 10:01 落进同一天,
/// 于是 10:00 那一期永远不会被领(「昨天已领」被读成「今天已领」)。
#[test]
fn qoder_cycle_day_rolls_at_ten_am_beijing() {
    use chrono::{FixedOffset, TimeZone};
    let tz = FixedOffset::east_opt(8 * 3600).unwrap();
    let at = |d, h, m| {
        tz.with_ymd_and_hms(2026, 10, d, h, m, 0)
            .unwrap()
            .timestamp()
    };
    // 10-21 09:59(还没跨日)→ 仍属于 10-20 那一期
    assert_eq!(cycle_day(at(21, 9, 59), 10), "2026-10-20");
    // 10-21 10:00(刚跨日)→ 已经是 10-21 那一期
    assert_eq!(cycle_day(at(21, 10, 0), 10), "2026-10-21");
    // 10-21 23:30 → 10-21
    assert_eq!(cycle_day(at(21, 23, 30), 10), "2026-10-21");
    // 00:23 的渠道不受影响:仍是自然日
    assert_eq!(cycle_day(at(21, 9, 59), 0), "2026-10-21");
    assert_eq!(cycle_day(at(21, 0, 30), 0), "2026-10-21");
}

/// 日历带的日结论:有流水看最后一行短码;空槽按"该不该签"分三种,
/// **绝不能把开启之前画成"没签"** —— 那是把没发生的事说成失败了。
#[test]
fn day_status_buckets() {
    // 有流水的天:结论跟最后一行走
    assert_eq!(day_status(Some("claimed:trae"), true, false), "claimed");
    assert_eq!(day_status(Some("already"), true, false), "already");
    assert_eq!(day_status(Some("inactive:qoder"), true, false), "inactive");
    assert_eq!(day_status(Some("manual:活动名"), true, false), "manual");
    // 重试没成 / 凭据失效 / 接口变动 / 确认中:都是"attempted 没签成"
    for o in ["retry:ready", "retry:device", "auth:401", "shape:boom", "no_cred:x", "confirming", "failed:boom"] {
        assert_eq!(day_status(Some(o), true, false), "failed", "{o}");
    }
    // 空槽:今天还没结论 → pending;该签没签 → missed;开启之前 → no_data
    assert_eq!(day_status(None, true, true), "pending");
    assert_eq!(day_status(None, true, false), "missed");
    assert_eq!(day_status(None, false, false), "no_data");
    // 渠道关着:就算历史上记过流水,空槽也不是"没签"
    assert_eq!(day_status(None, false, true), "pending");
    // 空串短码当没流水处理
    assert_eq!(day_status(Some(""), true, false), "missed");
}

/// 触发时刻:抖动点之前不算到点,之后(含当天更晚)都算 —— 关机一整天再开机也能补跑。
#[test]
fn due_now_window_and_catch_up() {
    use chrono::{FixedOffset, TimeZone};
    let tz = FixedOffset::east_opt(8 * 3600).unwrap();
    let at = |h, m| tz.with_ymd_and_hms(2026, 10, 13, h, m, 0).unwrap().timestamp();
    let day = "2026-10-13";
    let sched = scheduled_minute(day, "trae", 0);
    assert!(!due_now(at(0, sched.saturating_sub(1)), day, "trae", 0), "还没到抖动的点");
    assert!(due_now(at(0, 43), day, "trae", 0), "抖动上限 43 分,到这里一定该签");
    assert!(due_now(at(9, 0), day, "trae", 0), "白天开机 → 补跑");
    assert_eq!(beijing_hm(at(9, 5)), (9, 5));
}

/// Qoder 的触发窗口:10:00 之前**不该**试(那时还没跨日,查到的是昨天那期),
/// 10:20 之后一定该试。
#[test]
fn qoder_due_now_is_after_ten_am() {
    use chrono::{FixedOffset, TimeZone};
    let tz = FixedOffset::east_opt(8 * 3600).unwrap();
    let at = |h, m| tz.with_ymd_and_hms(2026, 10, 20, h, m, 0).unwrap().timestamp();
    let day = "2026-10-20";
    // 09:59 还没到(哪怕 00:23 那套早就该跑了)
    assert!(!due_now(at(9, 59), day, "qoder", 10), "10:00 之前不算到点");
    // 10:20 是抖动上限,到这里一定该签
    assert!(due_now(at(10, 20), day, "qoder", 10));
    // 下午开机 → 补跑
    assert!(due_now(at(15, 0), day, "qoder", 10));
    // 而 00:23 的渠道在 09:59 早就该跑了(两套口径互不影响)
    assert!(due_now(at(9, 59), day, "trae", 0));
}

/// 退避序列与封顶;跨日清零。
#[test]
fn backoff_and_day_rollover() {
    assert_eq!(next_delay(1), 120);
    assert_eq!(next_delay(2), 300);
    assert_eq!(next_delay(5), 1200);
    assert_eq!(next_delay(9), 1200, "封顶");
    assert_eq!(next_delay(0), 120);

    let mut c = CheckinCfg::default();
    c.attempts = 3;
    c.next_try_at = Some(123);
    c.last_day = Some("2026-10-12".into());
    c.roll_day("2026-10-13");
    assert_eq!(c.attempts, 0, "跨日清零");
    assert!(c.next_try_at.is_none());
}

/// 当天处理完了就不再自动试;可重试的才继续。
/// 首页徽标要的是"今天没有待办了"这个**正面**结论,与 done_for_day(含 auth/shape
/// —— 那是"别再试了",不是"签完了")不能混用。2026-10-21 首页徽标上线时刻意分开。
#[test]
fn signed_today_is_a_positive_conclusion_not_just_stop_trying() {
    let today = "2026-10-21";
    let mk = |code: &str| {
        let mut c = CheckinCfg::default();
        c.last_day = Some(today.into());
        c.last_code = Some(code.into());
        c
    };

    // 真签完的三种
    for code in ["claimed", "claimed:100", "already", "inactive:本期无活动"] {
        assert!(mk(code).signed_today(today), "{code} 应算今天没有待办");
    }
    // auth / shape:今天不该再试,但**不是签完了** —— 徽标要说"需处理",不能说"已签"
    for code in ["auth", "shape:缺字段", "retry:ready", "retry:device", "failed:boom"] {
        assert!(!mk(code).signed_today(today), "{code:?} 不是'签完了'");
    }
    // 昨天签的不算今天
    let mut y = CheckinCfg::default();
    y.last_day = Some("2026-10-20".into());
    y.last_code = Some("claimed".into());
    assert!(!y.signed_today(today), "跨日了就得重新签");
}

#[test]
fn done_for_day_rules() {
    let today = "2026-10-13";
    let mut c = CheckinCfg::default();
    c.last_day = Some(today.into());
    c.last_code = Some("claimed:100".into());
    assert!(c.done_for_day(today));
    c.last_code = Some("already".into());
    assert!(c.done_for_day(today));
    c.last_code = Some("auth".into());
    assert!(c.done_for_day(today), "凭据失效不再自动重试");
    c.last_code = Some("retry:device".into());
    assert!(!c.done_for_day(today), "限流当天还要再试");
    // 昨天签的不算今天
    c.last_day = Some("2026-10-12".into());
    assert!(!c.done_for_day(today));
}

/// v0.1.47 真机踩到的那件事:`auth` 记在升级**之前**,升级后凭据链路已经修好,
/// 但当天仍被那条旧 `auth` 挡住,界面一直显示"凭据失效"。
/// 判据是「记录 auth 的版本 ≠ 当前版本」 → 陈旧 → 放开当天。
#[test]
fn stale_auth_does_not_block_the_day_after_version_change() {
    let today = "2026-10-20";
    let mut c = CheckinCfg::default();
    c.last_day = Some(today.into());
    c.last_code = Some("auth".into());

    // ① 老数据:字段还不存在(None) → 一定陈旧
    assert!(c.auth_is_stale("0.1.47"));
    assert!(c.done_for_day(today), "done_for_day 本身不变:auth 仍算当天完结");
    assert!(!c.blocks_today(today, "0.1.47"), "但换版本后不该再挡当天");

    // ② 同一版本判出的 auth → 不陈旧,当天照旧完结(不担心无限重试)
    c.last_auth_version = Some("0.1.47".into());
    assert!(!c.auth_is_stale("0.1.47"));
    assert!(c.blocks_today(today, "0.1.47"));
    // 但换到 0.1.48 又该放行
    assert!(c.auth_is_stale("0.1.48"));
    assert!(!c.blocks_today(today, "0.1.48"));

    // ③ claimed / already 是平台给的既成事实,任何版本都不重判
    for code in ["claimed", "already"] {
        let mut d = CheckinCfg::default();
        d.last_day = Some(today.into());
        d.last_code = Some(code.into());
        d.last_auth_version = Some("0.1.0".into());
        assert!(!d.auth_is_stale("0.1.47"), "{code} 不该因版本变化被重判");
        assert!(!d.inactive_is_stale("0.1.47"), "{code} 不该因版本变化被重判");
        assert!(d.blocks_today(today, "0.1.47"), "{code} 仍应挡住当天");
    }

    // ③b `inactive` / `manual` 是**我们的分类器**给的结论(v0.1.57 起):
    // 旧版本判的"本期无活动"要重问 —— Qoder 把活动换成 VIEW_DETAILS + CLAIMABLE 后,
    // 旧结论就是假阴性,不重判的话升级当天仍然显示"软件里没有"。
    for code in ["inactive:本期无活动", "manual:1024 活动"] {
        let mut d = CheckinCfg::default();
        d.last_day = Some(today.into());
        d.last_code = Some(code.into());
        d.last_auth_version = Some("0.1.0".into());
        assert!(!d.auth_is_stale("0.1.47"), "{code} 不是凭据判决");
        assert!(d.inactive_is_stale("0.1.47"), "{code} 换版本必须重判");
        assert!(!d.blocks_today(today, "0.1.47"), "{code} 不该挡住新版本当天");
        // 同一版本内照样当天完结,不会无限重试
        d.last_auth_version = Some("0.1.47".into());
        assert!(d.blocks_today(today, "0.1.47"), "{code} 同版本内当天完结");
    }

    // ④ 不是今天的 auth 不影响今天
    let mut e = CheckinCfg::default();
    e.last_day = Some("2026-10-19".into());
    e.last_code = Some("auth".into());
    assert!(!e.blocks_today(today, "0.1.47"));
}

/// Trae:真机取样的响应(2026-10-13)—— 已签、100 分、在顶层。
#[test]
fn trae_real_sample_is_already_checked_in() {
    let body = json!({"checked_in":true,"code":0,"credits":100,"did_checked_in":true,
                      "enable":true,"extra_credits":100,"message":"success"});
    assert_eq!(parse_trae(&body, false), Outcome::Already(Some(100.0)));
    // 未签的形态(enable 且 checked_in=false)→ 可领
    let body = json!({"checked_in":false,"code":0,"credits":100,"enable":true});
    assert_eq!(parse_trae(&body, false), Outcome::Retry("ready".into()));
    // 领取后仍然没签 → 结构对不上,不谎报
    assert!(matches!(parse_trae(&json!({"checked_in":false,"code":0}), true), Outcome::BadShape(_)));
}

/// Trae 的错误码:凭据失效、设备被拒、已签,都不看 message。
#[test]
fn trae_codes_are_interpreted_by_code_not_message() {
    assert_eq!(parse_trae(&json!({"code":1001,"message":"随便写的"}), false), Outcome::AuthNeeded);
    assert_eq!(parse_trae(&json!({"code":1002}), false), Outcome::AuthNeeded);
    // 9074 = 设备/出口不被接受(文案伪装成"太挤了")
    assert_eq!(parse_trae(&json!({"code":9074,"message":"当前参与用户太多,请稍后再试"}), false), Outcome::Retry("device".into()));
    // 9095 = 本设备今日已签
    assert_eq!(parse_trae(&json!({"code":9095}), false), Outcome::Already(None));
    assert!(matches!(parse_trae(&json!({"code":12345}), false), Outcome::Failed(_)));
    // enable=false = 活动/入口没开
    assert!(matches!(parse_trae(&json!({"code":0,"enable":false}), false), Outcome::Inactive(_)));
}

/// 数额字段被 data 包一层也要认(否则会把已签判成未签,重复领取)。
#[test]
fn trae_credits_may_be_nested() {
    let body = json!({"code":0,"data":{"checked_in":true,"points":150}});
    assert_eq!(parse_trae(&body, false), Outcome::Already(Some(150.0)));
}

/// WorkBuddy:10001 按请求域名区域解释，未知区域不猜。
#[test]
fn workbuddy_10001_branches_by_region() {
    let already = json!({"code":10001,"msg":"今日已签到"});
    assert!(matches!(
        parse_workbuddy_for_region(&already, false, WorkBuddyRegion::Mainland),
        Outcome::Already(_)
    ));
    let inactive = json!({"code":10001,"msg":"签到活动未开启或已过期"});
    assert!(matches!(
        parse_workbuddy_for_region(&inactive, false, WorkBuddyRegion::International),
        Outcome::Inactive(_)
    ));
    assert!(matches!(
        parse_workbuddy_for_region(&already, false, WorkBuddyRegion::Unknown),
        Outcome::BadShape(_)
    ));
}

/// WorkBuddy:活动关了 / 凭据失效 / 可领 / 领到。
#[test]
fn workbuddy_main_states() {
    assert!(matches!(
        parse_workbuddy(&json!({"code":0,"data":{"active":false}}), false),
        Outcome::Inactive(_)
    ));
    assert_eq!(parse_workbuddy(&json!({"code":401,"msg":"unauthorized"}), false), Outcome::AuthNeeded);
    let ready = json!({"code":0,"data":{"active":true,"today_checked_in":false,"daily_credit":100}});
    assert_eq!(parse_workbuddy(&ready, false), Outcome::Retry("ready".into()));
    // 领取成功:数额有就给,没有就 None(设计稿:响应可能没有数额字段)
    assert_eq!(parse_workbuddy(&ready, true), Outcome::Claimed(Some(100.0)));
    let no_amount = json!({"code":0,"data":{}});
    assert_eq!(parse_workbuddy(&no_amount, true), Outcome::Claimed(None));
    // 结构不认识 → BadShape(而不是当"可领"去重试)
    assert!(matches!(parse_workbuddy(&json!({"foo":1}), false), Outcome::Failed(_)));
}

// ───────────── Qoder(2026-10-20 真机样本)─────────────

/// **真机原样的 actionType 是 `CLAIM_BENEFIT`(带 E)** —— 这条回归用例守着
/// 一个真实踩过的坑:早先照混淆 bundle 写成了 `CLAIM_BEFIT`,
/// 结果 `has_campaign` 恒为 false,把"今天可领"静默判成"本期无打卡活动",
/// 用户当天直接丢 100 分(2026-10-20 真机用例抓到的)。
#[test]
fn qoder_action_type_is_claim_benefit_with_e() {
    assert_eq!(QODER_CLAIM_ACTION, "CLAIM_BENEFIT");
    assert!(is_claim_action("CLAIM_BENEFIT"), "真机上的拼写必须被认");
    // 少一个 E 的历史写法也认(别名表),但主名是带 E 的
    assert!(is_claim_action("CLAIM_BEFIT"));
    assert!(!is_claim_action("VIEW_DETAILS"));
    assert!(!is_claim_action(""));

    // 用真机原样的 actionType 走一遍:必须认出"有活动"
    let body = json!({
        "campaigns":[
            {"campaignId":"a","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMED",
             "benefit":{"kind":"CREDITS","amount":100}},
            {"campaignId":"b","actionType":"VIEW_DETAILS","claimStatus":"CLAIMED"}
        ]
    });
    let pick = qoder_pick(&body);
    assert!(pick.has_campaign, "CLAIM_BENEFIT 必须被认成打卡活动");
    assert!(pick.already, "那条 CLAIMED 的应判为已领");
}

/// 真机抓到的**可领**形态(把 claimStatus 换成 CLAIMABLE):
/// 必须回 `retry:ready:{活动id}` —— 调用方据此拼 claim URL。
#[test]
fn qoder_claimable_yields_ready_with_id() {
    let body = json!({
        "uid":"01a068b6-c10f-7658-9045-5084b2acb9f2",
        "showCampaign":true,"claimable":true,
        "campaigns":[{
            "campaignId":"01a15721-d469-7a24-abd5-a81b73dc68f9",
            "campaignKey":"act-20261020-910",
            "actionType":"CLAIM_BENEFIT",
            "startAt":1792461600,"endAt":1792547940,
            "claimStatus":"CLAIMABLE",
            "benefit":{"kind":"CREDITS","amount":100,
                       "validity":{"mode":"RELATIVE_DAYS","days":30}}
        }]
    });
    let out = parse_qoder(&body, false);
    assert_eq!(
        out,
        Outcome::Retry("ready:01a15721-d469-7a24-abd5-a81b73dc68f9".into())
    );
    // 调用方要能从短码里取回活动 id
    assert_eq!(
        ready_claim_id("ready:01a15721-d469-7a24-abd5-a81b73dc68f9"),
        Some("01a15721-d469-7a24-abd5-a81b73dc68f9")
    );
    assert_eq!(ready_claim_id("already"), None);
    assert_eq!(ready_claim_id("ready:"), Some(""));
}

/// **2026-10-22 真机样本**:当天的活动换成了 `VIEW_DETAILS` + `CLAIMABLE`
/// (「1024 | Community Boost」,顶层 `claimable` 已经是 true),
/// 旧口径只认 CLAIM_BENEFIT → 回 `Inactive("本期无打卡活动")`,
/// 而用户在自己的客户端里明明看得到一个可领的活动 —— 这就是"软件里显示没有"。
///
/// 现在的口径:`Manual(标题)` —— 有活动、但只能在客户端点开领,如实说出来。
#[test]
fn qoder_view_details_claimable_is_manual_not_inactive() {
    let body = json!({
        "uid":"01a068b6-c10f-7658-9045-5084b2acb9f2",
        "showCampaign":true,
        "claimable":true,
        "campaigns":[
            {"campaignId":"01a15d6a-c6df-7e4c-ac3d-b46f98d053f7",
             "campaignKey":"act-20261021-273",
             "actionType":"VIEW_DETAILS",
             "startAt":1792634400,"endAt":1793375940,
             "claimStatus":"CLAIMABLE",
             "placements":[{"type":"POPUP","campaignUrl":"https://openapi.qoder.sh/growth-page/activity-iframe",
               "content":{"en":{"title":"1024 | Community Boost"},
                          "zh":{"title":"1024 | 创造力集结"}}}]},
            {"campaignId":"01a159af-8a04-76b8-b0f4-59d8e2b60583",
             "campaignKey":"act-20261020-944",
             "actionType":"VIEW_DETAILS",
             "claimStatus":"CLAIMED"}
        ]
    });
    let pick = qoder_pick(&body);
    assert!(!pick.has_campaign, "VIEW_DETAILS 不是 CLAIM_BENEFIT");
    assert_eq!(pick.manual.as_deref(), Some("1024 | 创造力集结"));
    assert_eq!(parse_qoder(&body, false), Outcome::Manual("1024 | 创造力集结".into()));

    // 没有可领的 VIEW_DETAILS(只有 CLAIMED 的)→ 仍然是"本期无活动",别谎报有活动
    let quiet = json!({"campaigns":[{"campaignId":"x","actionType":"VIEW_DETAILS","claimStatus":"CLAIMED"}]});
    assert_eq!(parse_qoder(&quiet, false), Outcome::Inactive("本期无打卡活动".into()));

    // 空列表同理
    assert_eq!(parse_qoder(&json!({"campaigns":[]}), false), Outcome::Inactive("本期无打卡活动".into()));

    // CLAIM_BENEFIT 可领时仍然优先走"能直接领",不能被 manual 抢走
    let both = json!({"campaigns":[
        {"campaignId":"m","actionType":"VIEW_DETAILS","claimStatus":"CLAIMABLE",
         "placements":[{"content":{"zh":{"title":"活动页"}}}]},
        {"campaignId":"c","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMABLE",
         "benefit":{"kind":"CREDITS","amount":100}}
    ]});
    assert_eq!(parse_qoder(&both, false), Outcome::Retry("ready:c".into()));
}

/// `manual` 与 `inactive` 一样算"今天没有待办":挂件做不了这件事,
/// 天天在聚合行里喊"还有 N 个没签"只会变成关不掉的噪音。
#[test]
fn qoder_manual_blocks_the_day_but_keeps_its_code() {
    let c = CheckinCfg {
        last_day: Some("2026-10-22".into()),
        last_code: Some("manual:1024 | 创造力集结".into()),
        ..Default::default()
    };
    assert!(c.done_for_day("2026-10-22"));
    assert!(c.signed_today("2026-10-22"));
    // 换一天照样要重新查
    assert!(!c.done_for_day("2026-10-23"));
    // 但"同一天完结"要带版本:这条 manual 是旧版本判的(last_auth_version = None)
    // → 不算完结,节拍器会重问一次(v0.1.57 升级后当天就能看到新结论)
    assert!(!c.blocks_today("2026-10-22", "0.1.57"));
    let mut fresh = c.clone();
    fresh.last_auth_version = Some("0.1.57".into());
    assert!(fresh.blocks_today("2026-10-22", "0.1.57"), "同一版本内当天完结");
    assert!(!fresh.blocks_today("2026-10-22", "0.1.58"), "换版本重判");
    assert_eq!(Outcome::Manual("x".into()).verdict(), "manual");
    assert_eq!(Outcome::Manual("x".into()).code(), "manual:x");
    assert_eq!(Outcome::Manual("x".into()).credits(), None);
}

/// 真机原样的**已领**形态(本机 10-20 就是这个):Already + 数额。
#[test]
fn qoder_claimed_is_already_with_amount() {
    let body = json!({
        "uid":"01a068b6","showCampaign":true,"claimable":false,
        "campaigns":[{
            "campaignId":"01a15721","campaignKey":"act-20261020-910",
            "actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMED",
            "benefit":{"kind":"CREDITS","amount":100}
        }]
    });
    assert_eq!(parse_qoder(&body, false), Outcome::Already(Some(100.0)));
}

/// **只有 VIEW_DETAILS 活动 = 本期没有打卡活动**,不能报成"今天已签"。
///
/// 这是设计稿 §3.2 单列的一条:把 Inactive 谎报成 Already,用户会以为领到了;
/// 报成 Failed 又会天天报警。真机日志里就有这么一条 VIEW_DETAILS。
#[test]
fn qoder_view_details_only_is_inactive_not_already() {
    let body = json!({
        "uid":"01a068b6","showCampaign":true,"claimable":false,
        "campaigns":[{
            "campaignId":"01a05bce","campaignKey":"act-20260901-493",
            "actionType":"VIEW_DETAILS","claimStatus":"CLAIMED"
        }]
    });
    let out = parse_qoder(&body, false);
    assert!(matches!(out, Outcome::Inactive(_)), "实际: {out:?}");
    assert!(!matches!(out, Outcome::Already(_)), "绝不能谎报成已领");
    // 空列表同理
    let empty = json!({"showCampaign":false,"campaigns":[]});
    assert!(matches!(parse_qoder(&empty, false), Outcome::Inactive(_)));
}

/// 已领 + 可领同时存在(上一期领过、这一期可领)→ 必须优先报"可领"。
#[test]
fn qoder_claimable_wins_over_older_claimed() {
    let body = json!({
        "claimable":true,
        "campaigns":[
            {"campaignId":"old","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMED",
             "benefit":{"kind":"CREDITS","amount":100}},
            {"campaignId":"new","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMABLE",
             "benefit":{"kind":"CREDITS","amount":200}}
        ]
    });
    assert_eq!(parse_qoder(&body, false), Outcome::Retry("ready:new".into()));
}

/// 领取响应的三种终态 + 幂等回执。真机实测 `replayed:true` 就是"之前已领"。
#[test]
fn qoder_claim_response_states() {
    // 刚领到
    let fresh = json!({"grantId":"g1","status":"CLAIMED","replayed":false,
                       "benefit":{"kind":"CREDITS","amount":100}});
    assert_eq!(parse_qoder(&fresh, true), Outcome::Claimed(Some(100.0)));
    // 幂等回执:之前已领,不重复发币
    let replay = json!({"grantId":"g1","status":"CLAIMED","replayed":true,
                        "benefit":{"kind":"CREDITS","amount":100}});
    assert_eq!(parse_qoder(&replay, true), Outcome::Already(Some(100.0)));
    // 平台明确说已被领过
    assert_eq!(
        parse_qoder(&json!({"status":"ALREADY_CLAIMED"}), true),
        Outcome::Already(None)
    );
    // 还在处理中 → 当天可以再来看一次
    assert!(matches!(parse_qoder(&json!({"status":"PENDING"}), true), Outcome::Retry(_)));
    // 撤销 → 没得领
    assert!(matches!(parse_qoder(&json!({"status":"REVOKED"}), true), Outcome::Inactive(_)));
    // 没有 status = 结构变了,绝不谎报成功
    assert!(matches!(parse_qoder(&json!({"grantId":"g1"}), true), Outcome::BadShape(_)));
}

/// 领取失败的 `failureCode` 要映射到"用户能理解的处置",而不是一律 Failed。
#[test]
fn qoder_failure_codes_map_to_actionable_states() {
    let with = |fc: &str| {
        json!({"status":"FAILED","failureCode":fc})
    };
    assert!(matches!(parse_qoder(&with("REDEMPTION_CODE_OUT_OF_STOCK"), true), Outcome::Inactive(_)));
    assert!(matches!(parse_qoder(&with("ACHIEVEMENT_NOT_COMPLETED"), true), Outcome::Inactive(_)));
    assert!(matches!(parse_qoder(&with("CAMPAIGN_NOT_ACTIVE"), true), Outcome::Inactive(_)));
    // 风控:重试无用 —— 报成 Inactive(要用户自己去看),不是 AuthNeeded(别指错方向去重新登录)
    assert!(matches!(parse_qoder(&with("RISK_BLOCKED"), true), Outcome::Inactive(_)));
    // 认不出来的码 → Failed,但要把 code 原样带出来(排查时它才是能查的东西)
    match parse_qoder(&with("SOMETHING_NEW"), true) {
        Outcome::Failed(m) => assert!(m.contains("SOMETHING_NEW"), "{m}"),
        other => panic!("期望 Failed,实际 {other:?}"),
    }
}

/// 凭据失效的三个字符串码 + HTTP 层已由调用方处理。
#[test]
fn qoder_auth_codes() {
    for code in ["UNAUTHORIZED", "TOKEN_INVALID", "TOKEN_EXPIRE"] {
        assert_eq!(
            parse_qoder(&json!({"code": code, "message":"whatever"}), false),
            Outcome::AuthNeeded,
            "code={code}"
        );
    }
    // 其它字符串 code → Failed(并把 code 带出来)
    match parse_qoder(&json!({"code":"SOMETHING_ELSE"}), false) {
        Outcome::Failed(m) => assert!(m.contains("SOMETHING_ELSE"), "{m}"),
        other => panic!("期望 Failed,实际 {other:?}"),
    }
}

/// 结构不认识(没有 campaigns 数组)→ BadShape,停止自动签到但不谎报。
#[test]
fn qoder_bad_shape_stops_but_does_not_lie() {
    assert!(matches!(parse_qoder(&json!({"foo":1}), false), Outcome::BadShape(_)));
    assert!(matches!(parse_qoder(&json!({"campaigns":"not-an-array"}), false), Outcome::BadShape(_)));
    // 有 CLAIM_BENEFIT 但状态是没见过的值 → 也是结构变化
    let weird = json!({"campaigns":[{"campaignId":"x","actionType":"CLAIM_BENEFIT",
                                     "claimStatus":"FUTURE_STATE"}]});
    assert!(matches!(parse_qoder(&weird, false), Outcome::BadShape(_)));
}

/// 数额缺失时不给数字(不编) —— 与 Trae/WorkBuddy 同口径。
#[test]
fn qoder_missing_amount_gives_none() {
    let body = json!({
        "campaigns":[{"campaignId":"c1","actionType":"CLAIM_BENEFIT",
                      "claimStatus":"CLAIMABLE","benefit":{"kind":"CREDITS"}}]
    });
    assert_eq!(parse_qoder(&body, false), Outcome::Retry("ready:c1".into()));
    // 已领但没数额 → Already(None),界面不显示"+?"
    let no_amt = json!({
        "campaigns":[{"campaignId":"c1","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMED"}]
    });
    assert_eq!(parse_qoder(&no_amt, false), Outcome::Already(None));
    // amount=0 视为"没有数额"(0 会被渲染成"没领到")
    let zero = json!({
        "campaigns":[{"campaignId":"c1","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMED",
                      "benefit":{"kind":"CREDITS","amount":0}}]
    });
    assert_eq!(parse_qoder(&zero, false), Outcome::Already(None));
}

/// **真机:WorkBuddy 签到走桌面端凭据**(2026-10-20 修的那个分叉)。
///
/// 这条用例就是修复的验收标准:以前签到只读网页 Cookie,而桌面端凭据明明可用,
/// 于是"余额天天出数、打卡天天报 auth"。现在把顺序对齐后,
/// 桌面端 token 必须能打通签到接口(实测 200,且不挑 UA / X-Client-Platform)。
///
/// 默认 ignore:`cargo test --lib -- --ignored wb_checkin_desktop_cred`。
#[test]
#[ignore]
fn wb_checkin_desktop_cred_reaches_the_endpoint() {
    let now_ms = chrono::Local::now().timestamp_millis();
    let tokens = crate::workbuddy::desktop_tokens(now_ms);
    assert!(
        !tokens.is_empty(),
        "本机读不到 WorkBuddy 桌面端凭据(没装/没登录),这条用例需要真机环境"
    );
    eprintln!("桌面端候选凭据 {} 份", tokens.len());

    let client = direct_client();
    let def = find("workbuddy").expect("workbuddy 签到定义在表里");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // 逐份试(取数那条路也是 401 换下一份 —— 签到照抄,别只试第一份就放弃)
    let mut last = Outcome::AuthNeeded;
    for (i, tok) in tokens.iter().enumerate() {
        // **不带 `desktop:` 前缀**:那是取数层的标记,带进来会变成
        // `Bearer desktop:xxx`,必然 401
        let cred = Cred::Web(tok.clone());
        let out = rt.block_on(call(&client, def, &cred, false, None));
        eprintln!("第 {} 份凭据 → {:?}", i + 1, out);
        if !matches!(out, Outcome::AuthNeeded) {
            // 打通了:要么可领(ready)、要么已签(already)、要么没活动(inactive)
            eprintln!("✅ 桌面端凭据能打通签到接口: {out:?}");
            return;
        }
        last = out;
    }
    panic!("所有桌面端凭据都被签到接口拒绝(最后一份:{last:?})");
}

// ───────────── 到账核对(ArrivalInfo)─────────────

/// 正增量落 confirmed;数额记下,时刻记下。
#[test]
fn arrival_positive_delta_confirms() {
    let mut a = ArrivalInfo::start(1000.0, 100, "2026-10-24".into());
    a.observe(1100.0, 200, "2026-10-24");
    assert_eq!(a.status, "confirmed");
    assert_eq!(a.delta, Some(100.0));
    assert_eq!(a.observed_at, Some(200));
}

/// 持平 / 消耗抵消 / 浮点噪声都不是"确认失败" —— 维持 pending 等下一份快照。
#[test]
fn arrival_non_positive_stays_pending() {
    let mut a = ArrivalInfo::start(1000.0, 100, "2026-10-24".into());
    a.observe(1000.0, 200, "2026-10-24");
    assert_eq!(a.status, "pending", "持平不是失败:发放可能还没到");
    a.observe(940.0, 300, "2026-10-24");
    assert_eq!(a.status, "pending", "消耗把增量压成负数,更不能判失败");
    a.observe(1000.4, 400, "2026-10-24");
    assert_eq!(a.status, "pending", "低于 EPS 的抖动不算");
    a.observe(1000.5, 500, "2026-10-24");
    assert_eq!(a.status, "confirmed", "正好到阈值算(grants::detect 同口径)");
    assert_eq!(a.delta, Some(0.5));
}

/// 跨过签到周期日还没见到增量 → unseen("未见",不是"没领到")。
/// Qoder 的周期日从 10:00 起算,cycle_day 的口径必须一致。
#[test]
fn arrival_day_roll_without_delta_is_unseen() {
    let mut a = ArrivalInfo::start(1000.0, 100, "2026-10-24".into());
    a.observe(1000.0, 200, "2026-10-24");
    assert_eq!(a.status, "pending");
    a.observe(1000.0, 300, "2026-10-25");
    assert_eq!(a.status, "unseen");
    assert_eq!(a.delta, None, "unseen 不该带增量");
}

/// 落定的单子是事实:confirmed / unseen 之后再来快照不再改写。
#[test]
fn arrival_settled_is_final() {
    let mut a = ArrivalInfo::start(1000.0, 100, "2026-10-24".into());
    a.observe(1100.0, 200, "2026-10-24");
    a.observe(50.0, 300, "2026-10-25");
    assert_eq!(a.status, "confirmed", "confirmed 之后的消耗不改写结论");
    assert_eq!(a.delta, Some(100.0));

    let mut b = ArrivalInfo::start(1000.0, 100, "2026-10-24".into());
    b.observe(1000.0, 300, "2026-10-25");
    assert_eq!(b.status, "unseen");
    b.observe(2000.0, 400, "2026-10-25");
    assert_eq!(b.status, "unseen", "unseen 之后的跳升(充值)也不改写");
}

/// 旧配置 JSON 里没有 arrival 字段:serde default 必须兜住(None),
/// 老库升级上来不炸 —— 与 CheckinCfg 其它字段的向后兼容同一套约定。
#[test]
fn arrival_field_is_backward_compatible() {
    let old: CheckinCfg =
        serde_json::from_str(r#"{"enabled":true,"auto":true,"lastCode":"claimed"}"#).unwrap();
    assert_eq!(old.arrival, None);
    assert_eq!(old.last_code.as_deref(), Some("claimed"));

    let with_arrival: CheckinCfg = serde_json::from_str(
        r#"{"enabled":true,"auto":true,
                "arrival":{"status":"confirmed","baseline":1000,"baselineAt":100,
                           "delta":100,"observedAt":200,"day":"2026-10-24"}}"#,
    )
    .unwrap();
    let a = with_arrival.arrival.expect("arrival 应该读得回来");
    assert_eq!(a.status, "confirmed");
    assert_eq!(a.delta, Some(100.0));
}
