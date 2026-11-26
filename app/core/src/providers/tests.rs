use super::*;
use serde_json::json;

#[test]
fn four_s_api_converts_points() {
    let r = extract_4sapi(&json!({
        "code": true,
        "data": {
            "total_granted": "250000000",
            "total_available": "93500000",
            "total_used": "156500000"
        }
    }));
    assert!(r.valid);
    assert_eq!(r.remaining, Some(187.0));
    assert_eq!(r.used, Some(313.0));
    assert_eq!(r.total, Some(500.0));
}

#[test]
fn four_s_api_falls_back_when_fields_missing() {
    // 缺字段时必须报错,不能默默返回 0 —— 那会被渲染成"已耗尽"
    let r = extract_4sapi(&json!({ "code": true, "data": {} }));
    assert!(!r.valid);
    assert!(r.remaining.is_none());
    assert!(r.error.is_some());
}

/// 业务失败(HTTP 200 但 code:false)必须判为无效 —— 之前 `||` 写法会把它吞掉。
#[test]
fn four_s_api_business_failure_is_invalid() {
    let r = extract_4sapi(&json!({
        "code": false,
        "data": { "total_granted": 1000, "total_available": 200, "total_used": 800 }
    }));
    assert!(!r.valid, "code:false 必须判为无效");
    // is_active:false 同理(老的部署用它表示停用)
    let r = extract_4sapi(&json!({
        "is_active": false,
        "data": { "total_granted": 1000, "total_available": 200, "total_used": 800 }
    }));
    assert!(!r.valid, "is_active:false 必须判为无效");
    // 正常响应(两个字段都没有)仍然是有效的
    let r = extract_4sapi(&json!({
        "data": { "total_granted": 1000, "total_available": 200, "total_used": 800 }
    }));
    assert!(r.valid);
    assert_eq!(r.remaining, Some(0.0)); // 200/500000 四舍五入到分
}

#[test]
fn opencode_picks_tightest_window() {
    let r = extract_opencode_go(&json!({
        "usage": {
            "rolling": {"status": "ok", "percent": 12.4, "resetsAt": "a"},
            "weekly":  {"status": "ok", "percent": 33.1, "resetsAt": "b"},
            "monthly": {"status": "rate-limited", "percent": 98.7, "resetsAt": "c"}
        }
    }));
    assert!(r.valid);
    assert_eq!(r.remaining, Some(1.3)); // 最紧张的月度窗口
    assert!(r.limited);
    assert_eq!(r.windows.len(), 3);
}

#[test]
fn opencode_reports_entitlement_error() {
    let r = extract_opencode_go(&json!({
        "type": "error",
        "error": {"type": "EntitlementError", "message": "OpenCode Go subscription required."}
    }));
    assert!(!r.valid);
    assert_eq!(r.error.as_deref(), Some("未订阅 Go 套餐"));
}

#[test]
fn deepseek_splits_balance() {
    let r = extract_deepseek(&json!({
        "is_available": true,
        "balance_infos": [{
            "currency": "CNY",
            "total_balance": "256.00",
            "granted_balance": "0.00",
            "topped_up_balance": "256.00"
        }]
    }));
    assert_eq!(r.remaining, Some(256.0));
    assert!(r.total.is_none()); // 充值型不画进度条
    assert_eq!(r.unit, "CNY");
    assert!(r.extra.iter().all(|(_, v)| v.starts_with('¥')));
    assert!(!r.extra.iter().any(|(k, _)| k == "币种"));
}

/// F6:美元账户不能被标成 CNY、副行也不能带 ¥ —— 单位必须是接口给的那个币种,
/// 并且明说它没折进人民币合计(汇总条只加 CNY)。
#[test]
fn deepseek_uses_the_currency_the_api_reports() {
    let r = extract_deepseek(&json!({
        "is_available": true,
        "balance_infos": [{
            "currency": "usd",
            "total_balance": "12.34",
            "granted_balance": "0.00",
            "topped_up_balance": "12.34"
        }]
    }));
    assert_eq!(r.unit, "USD", "币种大写、且照接口写,不再一律 CNY");
    assert_eq!(r.remaining, Some(12.34), "数值照旧给界面显示");
    assert!(
        r.extra.iter().all(|(_, v)| !v.starts_with('¥')),
        "非人民币的副行不能写 ¥: {:?}",
        r.extra
    );
    assert!(r.extra.iter().any(|(_, v)| v.contains("USD 12.34")));
    let note = r
        .extra
        .iter()
        .find(|(k, _)| k == "币种")
        .map(|(_, v)| v.as_str())
        .expect("要有一条说明币种的行");
    assert!(note.contains("未折进") && note.contains("CNY"), "{note}");
}

#[test]
fn hapi_divides_points_by_20() {
    let r = extract_hapi(&json!({ "data": { "balance": 1974.5, "isValid": true } }));
    assert_eq!(r.remaining, Some(98.73));
}

#[test]
fn hapi_error_is_surfaced() {
    let r = extract_hapi(&json!({ "error": "invalid key" }));
    assert!(!r.valid);
    assert!(r.error.is_some());
}

/// Trae:逐笔到期。每笔的数量/已用/到期都要落到 expiring 里,按到期升序;
/// 没有 credits_limit 的免费订阅包要跳过(不能按 0 记一笔);**已过期的包整笔排除**,
/// 否则卡片余额会比官方口径大(扫描报告 P2-4)。
/// 到期时间一律相对「现在」构造 —— 写成绝对时间戳的话,这个测试会随着日历过期。
#[test]
fn trae_fills_per_pack_expiry() {
    let now = chrono::Utc::now().timestamp();
    let day = 86_400;
    let body = json!({
        "usage_summary": { "total_amount": 850, "consumed_amount": 344.724 },
        "user_entitlement_pack_list": [
            {   // 免费订阅包:没有 credits_limit,必须跳过
                "expire_time": now + 30 * day,
                "entitlement_base_info": { "quota": { "no_bonus_quota": true } }
            },
            {   // 已用完的签到包:剩余 0,不进到期表,但计入已用
                "expire_time": now + 3 * day,
                "usage": { "credits_amount": 150 },
                "entitlement_base_info": {
                    "quota": { "credits_limit": 150 },
                    "product_extra": { "package_extra": { "package_name": "签到奖励" } }
                }
            },
            {   // 还有 5.276 的签到包,3 天后到期
                "expire_time": now + 3 * day,
                "usage": { "credits_amount": 194.724 },
                "entitlement_base_info": {
                    "quota": { "credits_limit": 200 },
                    "product_extra": { "package_extra": { "package_name": "签到奖励" } }
                }
            },
            {   // 500 分的每月赠送,12 天后到期
                "expire_time": now + 12 * day,
                "entitlement_base_info": {
                    "quota": { "credits_limit": 500 },
                    "product_extra": { "package_extra": { "package_name": "每月登录赠送" } }
                }
            },
            {   // 昨天就过期了:一分都不该算进余额,也不占额度笔数
                "expire_time": now - day,
                "entitlement_base_info": {
                    "quota": { "credits_limit": 40 },
                    "product_extra": { "package_extra": { "package_name": "过期赠送" } }
                }
            }
        ]
    });
    let r = extract_trae(&body);
    assert!(r.valid, "{:?}", r.error);
    assert_eq!(r.kind, Kind::Points);
    // 150 + 200 + 500 = 850 发放;已用 150 + 194.724 → 剩 505.276(过期那笔 40 不算)
    assert_eq!(r.remaining, Some(505.28));
    assert_eq!(r.used, Some(344.72));
    assert!(r.total.is_none(), "积分型不设总额上限");
    assert_eq!(r.expiring.len(), 2, "用完与已过期的那两笔都不进到期表");
    assert_eq!(r.expiring[0].at, now + 3 * day); // 升序:近的在前
    assert_eq!(r.expiring[0].amount, 5.28);
    assert_eq!(r.expiring[0].label, "签到奖励");
    assert_eq!(r.expiring[1].at, now + 12 * day);
    assert_eq!(r.expiring[1].amount, 500.0);
    assert_eq!(r.extra[2].1, "3 笔", "过期包不占额度包数");
}

/// 结构不对必须报无效,而不是给一个"剩余 0"的假结果。
#[test]
fn trae_reports_bad_shape() {
    let r = extract_trae(&json!({ "code": 401, "msg": "unauthorized" }));
    assert!(!r.valid);
    assert!(r.error.is_some());
    // 只有免费包(没有 credits_limit)= 没有可用额度包
    let r = extract_trae(&json!({
        "user_entitlement_pack_list": [{ "expire_time": 1, "entitlement_base_info": {} }]
    }));
    assert!(!r.valid);
}

/// WorkBuddy:数量/剩余是字符串小数,到期优先取 DeductionEndTime(毫秒),
/// 退回 CycleEndTime(北京时间字符串);退款/已过期的包不计入。
#[test]
fn workbuddy_reads_packs_and_expiry() {
    let body = json!({
        "code": 0, "msg": "OK",
        "data": { "Response": { "Data": { "Accounts": [
            {
                "PackageCode": "TCACA_code_007_nzdH5h4Nl0", "Status": 0,
                "CapacityType": 1,
                "CycleCapacitySizePrecise": "100",
                "CycleCapacityRemainPrecise": "9.4000001",
                "CycleCapacityUsedPrecise": "90.5999999",
                "CycleEndTime": "2026-09-22 10:59:25",
                "DeductionEndTime": "1790045965000"
            },
            {
                "PackageCode": "TCACA_code_008_cfWoLwvjU4", "Status": 0,
                "CycleCapacitySizePrecise": "500", "CycleCapacityRemainPrecise": "500",
                "CycleEndTime": "2026-09-30 23:59:59"
            },
            {   // 已过期:不计入
                "PackageCode": "TCACA_code_007_nzdH5h4Nl0", "Status": 2,
                "CycleCapacitySizePrecise": "100", "CycleCapacityRemainPrecise": "100",
                "CycleEndTime": "2026-09-18 15:33:13"
            },
            {   // 已用完:剩余 0,只计入发放/已用
                "PackageCode": "TCACA_code_007_nzdH5h4Nl0", "Status": 3,
                "CycleCapacitySizePrecise": "100", "CycleCapacityRemainPrecise": "0",
                "CycleEndTime": "2026-09-18 15:33:13"
            }
        ]}}}
    });
    let r = extract_workbuddy(&body);
    assert!(r.valid, "{:?}", r.error);
    assert_eq!(r.kind, Kind::Points);
    // 9.4000001 + 500 + 0 = 509.4(过期的 100 不算)
    assert_eq!(r.remaining, Some(509.4));
    assert_eq!(r.expiring.len(), 2);
    assert_eq!(r.expiring[0].amount, 9.4);
    assert_eq!(r.expiring[0].label, "活动赠送");
    // DeductionEndTime 是毫秒,要按秒算
    assert_eq!(r.expiring[0].at, 1790045965);
    // 没有 DeductionEndTime 时退回 CycleEndTime(北京时间 → unix)
    assert_eq!(r.expiring[1].label, "免费月度");
    assert_eq!(
        r.expiring[1].at,
        parse_beijing_time("2026-09-30 23:59:59").unwrap()
    );
}

#[test]
fn workbuddy_reports_business_error() {
    let r = extract_workbuddy(&json!({ "code": 10085, "msg": "请求不合法,如有疑问请联系客服" }));
    assert!(!r.valid);
    assert_eq!(
        r.error.as_deref(),
        // 带上 code:平台的文案会说谎(9074 那类),code 才是能查的东西
        Some("请求不合法,如有疑问请联系客服(code 10085)")
    );
}

/// code!=0 且 Accounts 是空数组:要说"业务错误",不能说"无可用的积分包"。
/// 后者听起来像账户状态正常,于是用户去积分商城转一圈,而不是重新登录(P2-19)。
#[test]
fn workbuddy_empty_accounts_with_error_code_is_not_no_packs() {
    let r = extract_workbuddy(&json!({
        "code": 401,
        "msg": "登录已过期",
        "data": { "Response": { "Data": { "Accounts": [] } } }
    }));
    assert!(!r.valid);
    assert!(
        r.error.as_deref().unwrap_or("").contains("登录已过期"),
        "该说业务原因,实际:{:?}",
        r.error
    );
    // 而 code=0 且真的没有包,才说"无可用的积分包"
    let ok = extract_workbuddy(&json!({
        "code": 0,
        "data": { "Response": { "Data": { "Accounts": [] } } }
    }));
    assert_eq!(ok.error.as_deref(), Some("无可用的积分包"));
}

#[test]
fn beijing_time_roundtrip() {
    // 2026-09-18 15:34:49(UTC+8)= 1789716889
    assert_eq!(parse_beijing_time("2026-09-18 15:34:49"), Some(1789716889));
    assert_eq!(parse_beijing_time("  2026-09-18 15:34:49 "), Some(1789716889));
    assert_eq!(parse_beijing_time("2026/09/18"), None);
}

/// 本机实抓的 free 号响应:单个 30 天窗、0% 已用。大数字取 primary。
#[test]
fn codex_free_single_month_window() {
    let r = extract_codex(&json!({
        "plan_type": "free",
        "rate_limit": {
            "allowed": true, "limit_reached": false,
            "primary_window": {
                "used_percent": 0, "limit_window_seconds": 2592000,
                "reset_after_seconds": 2592000, "reset_at": 1792378201
            },
            "secondary_window": null
        }
    }));
    assert!(r.valid);
    assert_eq!(r.kind, Kind::Percent);
    assert_eq!(r.windows.len(), 1);
    assert_eq!(r.windows[0].label, "本月");
    assert_eq!(r.windows[0].remain_percent, 100.0);
    assert_eq!(r.remaining, Some(100.0));
    assert!(!r.limited);
    assert_eq!(r.extra[0], ("套餐".into(), "free".into()));
    assert!(r.windows[0].resets_at.as_deref().unwrap().ends_with("+00:00"));
}

/// 付费号:周窗是展示主窗,不受上游 primary/secondary 位置影响。
#[test]
fn codex_paid_two_windows_headline_is_weekly() {
    let r = extract_codex(&json!({
        "plan_type": "plus",
        "rate_limit": {
            "allowed": true, "limit_reached": false,
            "primary_window": { "used_percent": 76.5, "limit_window_seconds": 18000, "reset_at": 1792378201 },
            "secondary_window": { "used_percent": 39.0, "limit_window_seconds": 604800, "reset_at": 1792378201 }
        }
    }));
    assert!(r.valid);
    assert_eq!(r.windows.len(), 2);
    assert_eq!(r.windows[0].label, "本周");
    assert_eq!(r.windows[1].label, "5 小时");
    assert!(r.windows[0].main);
    assert!(!r.windows[1].main);
    assert_eq!(r.remaining, Some(61.0));
    assert_eq!(r.used, Some(39.0));
}

#[test]
fn codex_rate_limited_marks_limited() {
    let r = extract_codex(&json!({
        "plan_type": "plus",
        "rate_limit": {
            "allowed": false, "limit_reached": true,
            "primary_window": { "used_percent": 100, "limit_window_seconds": 18000, "reset_at": 1792378201 },
            "secondary_window": null
        }
    }));
    assert!(r.valid);
    assert!(r.limited);
    assert_eq!(r.windows[0].status, "rate-limited");
    assert_eq!(r.windows[0].remain_percent, 0.0);
}

#[test]
fn codex_weekly_is_primary_even_when_short_window_is_exhausted_or_slots_swap() {
    for swapped in [false, true] {
        let week = json!({"used_percent":2,"limit_window_seconds":604800});
        let short = json!({"used_percent":100,"limit_window_seconds":18000});
        let (primary, secondary) = if swapped { (week, short) } else { (short, week) };
        let r = extract_codex(&json!({"plan_type":"plus", "rate_limit":{
            "primary_window":primary,"secondary_window":secondary
        }}));
        assert!(r.valid && r.limited);
        assert_eq!(r.remaining, Some(98.0));
        assert_eq!(r.windows[0].label, "本周");
        assert!(r.windows[0].main);
        assert_eq!(r.windows[1].status, "rate-limited");
        assert!(!r.windows[1].main);
    }
}

/// 只给 `reset_after_seconds` 的响应也要能算出"什么时候重置":
/// 换算成绝对时刻下发,并且取数时刻是**现在**(不是 1970,也不是 5 万年后)。
#[test]
fn codex_reset_after_seconds_becomes_absolute_time() {
    let before = chrono::Utc::now().timestamp();
    let r = extract_codex(&json!({"plan_type":"plus", "rate_limit":{
        "primary_window":{"used_percent":10,"limit_window_seconds":604800,
                          "reset_after_seconds":3600}
    }}));
    let at = r.windows[0].resets_at.as_deref().expect("reset_after_seconds 该被换算");
    let ts = chrono::DateTime::parse_from_rfc3339(at).unwrap().timestamp();
    assert!((ts - (before + 3600)).abs() <= 5, "换算结果应≈现在+3600,实际 {ts}");

    // 两个都在时以 reset_at 为准(绝对时刻不需要猜)
    let r = extract_codex(&json!({"rate_limit":{"primary_window":{
        "used_percent":1,"limit_window_seconds":604800,
        "reset_at":1793951075,"reset_after_seconds":3600
    }}}));
    assert_eq!(
        r.windows[0].resets_at.as_deref(),
        Some("2026-11-06T07:44:35+00:00")
    );
}

#[test]
fn codex_missing_rate_limit_is_invalid() {
    let r = extract_codex(&json!({ "detail": "Not authenticated" }));
    assert!(!r.valid);
    assert!(r.error.is_some());
}

#[test]
fn codex_week_only_after_plan_upgrade_is_a_valid_main_window() {
    let r = extract_codex(&json!({
        "plan_type":"pro",
        "rate_limit": {"primary_window":null, "secondary_window": {
            "used_percent":12, "limit_window_seconds":604800, "reset_at":1793951075
        }},
        "rate_limit_reset_credits":{"available_count":2}
    }));
    assert!(r.valid);
    assert_eq!(r.windows.len(), 1);
    assert!(r.windows[0].main);
    assert_eq!(r.windows[0].label, "本周");
    assert_eq!(r.remaining, Some(88.0));
    assert_eq!(r.extra[0].1, "pro");
    assert_eq!(r.reset_credits.unwrap().available_count, Some(2));
}

/// Qoder:**本机真机样本**(2026-10-20,personal_standard 档)。
/// 量全在 addOnQuota,userQuota 全 0 —— 只看 userQuota 会显示成 0(这是最大的坑)。
#[test]
fn qoder_real_sample_reads_addon_quota() {
    let r = extract_qoder(&json!({
        "userId": "01a068b6-c10f-7658-9045-5084b2acb9f2",
        "userType": "personal_standard",
        "usageType": "credits",
        "totalUsagePercentage": 0.15,
        "isQuotaExceeded": false,
        "expiresAt": 253402214400000_i64,
        "userQuota": { "total": 0.0, "used": 0.0, "remaining": 0.0, "percentage": 0.0, "unit": "credits" },
        "addOnQuota": { "total": 100.0, "used": 14.0, "remaining": 86.0, "percentage": 0.15, "unit": "credits" }
    }));
    assert!(r.valid, "{:?}", r.error);
    assert_eq!(r.kind, Kind::Points);
    assert_eq!(r.remaining, Some(86.0), "必须读到 addOnQuota,不能只看 userQuota");
    assert_eq!(r.used, Some(14.0));
    assert!(r.total.is_none(), "积分型不画进度条");
    assert_eq!(r.unit, "credits");
    // 套餐与账户级到期都要如实透出;9999 年那个天文数字要折成"不过期"
    assert!(r.extra.iter().any(|(k, v)| k == "套餐" && v == "personal_standard"));
    assert!(r.extra.iter().any(|(k, v)| k == "账户额度到期" && v == "不过期"));
    // 结构化字段:哨兵日期 = 不过期,at 必须是 null(不能把 9999 年当倒计时)
    let acc = r.account_expiry.expect("account expiry");
    assert_eq!(acc.state, AccountExpiryState::Never);
    assert_eq!(acc.at, None);
}

/// 账户有效期:日期可精确到分钟;RFC3339 字符串也认;非法值归未知,不猜。
#[test]
fn qoder_account_expiry_states_are_structured() {
    let r = extract_qoder(&json!({
        "userQuota": { "remaining": 10.0 },
        "expiresAt": 1793951075000_i64
    }));
    // 前端契约:字段名 accountExpiry,state 小写,at 为 Unix 秒
    let j = serde_json::to_value(&r).unwrap();
    assert_eq!(j["accountExpiry"]["state"], "dated");
    assert_eq!(j["accountExpiry"]["at"], 1_793_951_075);
    let acc = r.account_expiry.expect("dated");
    assert_eq!(acc.state, AccountExpiryState::Dated);
    assert_eq!(acc.at, Some(1_793_951_075));

    let want = chrono::DateTime::parse_from_rfc3339("2026-11-30T23:59:00+08:00")
        .unwrap()
        .timestamp();
    let r = extract_qoder(&json!({
        "userQuota": { "remaining": 10.0 },
        "expiresAt": "2026-11-30T23:59:00+08:00"
    }));
    assert_eq!(r.account_expiry.unwrap().at, Some(want));

    let r = extract_qoder(&json!({
        "userQuota": { "remaining": 10.0 },
        "expiresAt": "not a date"
    }));
    assert_eq!(r.account_expiry.unwrap().state, AccountExpiryState::Unknown);

    let r = extract_qoder(&json!({ "userQuota": { "remaining": 10.0 } }));
    assert!(r.account_expiry.is_none(), "缺失字段保持 None,不编造未知/不过期");
}

/// 两个块都有量时要**相加**(套餐额度 + 加量包),不是取其中一个。
#[test]
fn qoder_sums_both_quota_blocks() {
    let r = extract_qoder(&json!({
        "userQuota": { "total": 500.0, "used": 100.0, "remaining": 400.0 },
        "addOnQuota": { "total": 100.0, "used": 14.0, "remaining": 86.0 }
    }));
    assert!(r.valid, "{:?}", r.error);
    assert_eq!(r.remaining, Some(486.0), "400 + 86");
    assert_eq!(r.used, Some(114.0));
}

/// snake_case 字段名也要认(社区记录上游两个版本都用过)。
#[test]
fn qoder_accepts_snake_case_quota_fields() {
    let r = extract_qoder(&json!({
        "user_quota": { "total": 0.0, "remaining": 0.0 },
        "add_on_quota": { "total": 100.0, "remaining": 86.0 }
    }));
    assert!(r.valid, "{:?}", r.error);
    assert_eq!(r.remaining, Some(86.0));
}

/// 缺 remaining 必须判**取数失败**,绝不用 0 顶替 —— 0 会被渲染成"已耗尽"。
#[test]
fn qoder_missing_remaining_is_invalid_not_zero() {
    let r = extract_qoder(&json!({
        "addOnQuota": { "total": 100.0, "used": 14.0 }
    }));
    assert!(!r.valid, "没有 remaining 不能算成功");
    assert!(r.remaining.is_none(), "绝不能返回 0");
    assert!(r.error.is_some());
    // 两个配额块都没有 → 也是失败
    let r2 = extract_qoder(&json!({ "userId": "x" }));
    assert!(!r2.valid);
    assert!(r2.remaining.is_none());
}

/// 凭据失效的错误码要指对方向("打开一次 Qoder"),而不是泛泛的"接口返回错误"。
#[test]
fn qoder_auth_errors_point_at_the_client() {
    for code in ["UNAUTHORIZED", "TOKEN_INVALID", "TOKEN_EXPIRE"] {
        let r = extract_qoder(&json!({ "code": code, "message": "whatever" }));
        assert!(!r.valid);
        assert_eq!(
            r.error.as_deref(),
            Some("登录状态已失效 · 打开一次 Qoder 即可"),
            "code={code}"
        );
    }
    // 认不出来的 code 要带出来(排查时它才是能查的东西)
    let r = extract_qoder(&json!({ "code": "SOMETHING_NEW" }));
    assert!(!r.valid);
    assert!(r.error.as_deref().unwrap_or("").contains("SOMETHING_NEW"));
}

/// 已用尽的账号要显式标出来(而不是只显示一个 0)。
#[test]
fn qoder_exceeded_quota_is_flagged() {
    let r = extract_qoder(&json!({
        "isQuotaExceeded": true,
        "addOnQuota": { "total": 100.0, "used": 100.0, "remaining": 0.0 }
    }));
    assert!(r.valid);
    assert_eq!(r.remaining, Some(0.0));
    assert!(r.extra.iter().any(|(k, v)| k == "配额" && v == "已用尽"));
}

#[test]
fn four_s_api_hosts_come_from_the_channel_definition() {
    let hosts = four_s_api_hosts();
    assert_eq!(hosts[0], "4sapi.com");
    assert!(hosts.contains(&"4sapi.cn".to_string()));
    assert!(hosts.contains(&"4sapi.org".to_string()));
    assert_eq!(hosts.len(), 4, "主地址 + 三个备用,域名只在这一处维护");
    assert_eq!(url_host("https://a.example:8443/x/y"), Some("a.example".to_string()));
    assert_eq!(url_host("not a url"), Some("not a url".to_string()));
}

#[test]
fn codex_window_labels_scale_with_seconds() {
    assert_eq!(codex_window_label(900.0), "15 分钟");
    assert_eq!(codex_window_label(3600.0), "1 小时");
    assert_eq!(codex_window_label(18000.0), "5 小时");
    assert_eq!(codex_window_label(86_400.0), "当日");
    assert_eq!(codex_window_label(604_800.0), "本周");
    assert_eq!(codex_window_label(2_592_000.0), "本月");
    assert_eq!(codex_window_label(9_000_000.0), "周期");
    // 字段缺失(limit_window_seconds 不在/为 0)必须落兜底名,不能猜「5 小时」
    assert_eq!(codex_window_label(0.0), "周期");
}
