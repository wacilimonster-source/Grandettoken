use super::*;

// ───────────────── 指纹分段 / 缺口标注 / 峰谷进视图 ─────────────────

/// 指纹:同一把 key 恒定、两侧空白不影响、换一位就变、且不含明文片段。
#[test]
fn key_fp_is_stable_and_hides_the_key() {
    let a = key_fp("sk-abc123");
    assert_eq!(a, key_fp("sk-abc123"));
    assert_eq!(a, key_fp("  sk-abc123  "), "裁剪两侧空白后同一把");
    assert_ne!(a, key_fp("sk-abc124"), "换一位就是另一本账");
    assert_eq!(a.len(), 24, "12 字节的十六进制");
    assert!(!a.contains("abc"), "指纹里不许出现密钥片段");
}

/// 今日到账只挂在 `tracks_grants` 的渠道上,且"没有"与"不记"要分开:
/// `Some(全零)` = 记了,今天确实没有;`None` = 这个渠道压根不记(国内版有签到)。
#[test]
fn grant_today_only_for_tracked_channels() {
    let store = Store::open_memory().unwrap();
    let cfg = Config::default();
    // 钉在"今天正午",与真实钟点解耦(这个文件里已经栽过两次,见 gap 那个用例)
    let now = local_day_start_of(now_ts()) + 12 * 3600;
    store
        .insert_grant(&crate::grants::Grant {
            channel: "workbuddy-ai".into(),
            day: local_day_of(now),
            ts: now - 60,
            amount: 30.0,
            source: "delta".into(),
            from_bal: Some(100.0),
            to_bal: Some(130.0),
        })
        .unwrap();

    let res = FetchResult {
        valid: true,
        kind: Kind::Points,
        unit: "credits".into(),
        remaining: Some(130.0),
        ..Default::default()
    };
    let wba = providers::find("workbuddy-ai").expect("国际版渠道在表里");
    let v = build_view(wba, res.clone(), false, now, &store, &cfg, None, "", 1);
    let gt = v.grant_today.expect("国际版要记到账");
    assert!(gt.has_activity, "今天有 +30,算活跃获取");
    assert_eq!(gt.activity_total, 30.0);
    assert_eq!(gt.total, 30.0);

    // 国内版:不记 —— 它有签到,别拿余额再猜一遍
    let wb = providers::find("workbuddy").unwrap();
    let v2 = build_view(wb, res, false, now, &store, &cfg, None, "", 1);
    assert!(v2.grant_today.is_none());
}

/// 没有到账记录时是 `Some(全零)`,不是 `None` —— "今天没有"是一个确定的结论。
#[test]
fn no_grant_today_is_a_known_zero() {
    let store = Store::open_memory().unwrap();
    let cfg = Config::default();
    let now = local_day_start_of(now_ts()) + 12 * 3600;
    let res = FetchResult {
        valid: true,
        kind: Kind::Points,
        unit: "credits".into(),
        remaining: Some(100.0),
        ..Default::default()
    };
    let wba = providers::find("workbuddy-ai").unwrap();
    let v = build_view(wba, res, false, now, &store, &cfg, None, "", 1);
    let gt = v.grant_today.expect("记到账的渠道必须有这一格");
    assert!(!gt.has_activity);
    assert_eq!(gt.total, 0.0);
    assert_eq!(gt.count, 0);
}

/// 自动隐藏只对"没装就等于不存在"的渠道生效,且用户动过眼睛之后彻底让位。
#[test]
fn auto_hide_candidates_only_for_untouched_wbai() {
    let mut cfg = Config::default();
    assert_eq!(
        auto_hide_candidates(&cfg),
        vec!["workbuddy-ai".to_string()],
        "默认只有国际版在名单里"
    );
    cfg.user_touched_channels.push("workbuddy-ai".into());
    assert!(
        auto_hide_candidates(&cfg).is_empty(),
        "用户点过眼睛就是表过态,不许再自动藏"
    );
    // 国内版有网页登录入口,不在名单里 —— 藏了会让人找不到登录按钮
    assert!(!providers::auto_hide_when_no_credential("workbuddy"));
    assert!(providers::auto_hide_when_no_credential("workbuddy-ai"));
}

/// 账户分段指纹(全量测评 P3-5 的判据):与候选顺序无关、换账户必换段、
/// 空集合退回空串(= 不分段的老口径,老客户端没写 uin 时的降级路径)。
#[test]
fn accounts_fp_is_order_insensitive_and_switches_on_account_change() {
    let both = vec!["10001".to_string(), "10002".to_string()];
    let mut shuffled = both.clone();
    shuffled.reverse();
    assert_eq!(accounts_fp(&both), accounts_fp(&shuffled), "重排候选不许换段");
    assert_ne!(
        accounts_fp(&both),
        accounts_fp(&["10001".to_string()]),
        "登着的账户集合变了就必须换段"
    );
    assert_eq!(accounts_fp(&[]), "", "空集合 = 不分段的老口径");
}

/// 峰谷状态只挂在标了 `peak_pricing` 的渠道上(DeepSeek),别的渠道一律 None。
#[test]
fn peak_state_only_for_peak_priced_channels() {
    let store = Store::open_memory().unwrap();
    let cfg = Config::default();
    let res = FetchResult {
        valid: true,
        kind: Kind::Amount,
        unit: "CNY".into(),
        remaining: Some(42.0),
        ..Default::default()
    };
    let ds = providers::find("deepseek").expect("渠道在表里");
    // 用固定日期,不跟真实时钟走:节假日表覆盖 2026 年,按设计从 2026-11 起
    // 会如实报 table_stale(等待次年安排)。这里验的是"表有效时 peak 状态正常",
    // 固定到 2026-06,避免每年 11 月都红一次。
    let now = Utc.with_ymd_and_hms(2026, 6, 15, 4, 0, 0).unwrap().timestamp();
    let v = build_view(ds, res.clone(), false, now, &store, &cfg, None, "fpA", 1);
    let p = v.peak.expect("DeepSeek 该有峰谷状态");
    assert!(p.next_change_at.is_some(), "总能算出下一次切换");
    assert!(!p.table_stale, "2026-06 时表还有效");

    let api = providers::find("4sapi").unwrap();
    let v2 = build_view(api, res, false, now, &store, &cfg, None, "fpA", 1);
    assert!(v2.peak.is_none(), "中转站没有峰谷时段这回事");
}

/// 缺口进视图 + 消耗只在同一本账里推算(换 key 后不跨账户)。
///
/// **时间必须相对"现在"取**:以前写死 `midnight + 2h / +3h / +4h`,凌晨 2 点前跑测试时
/// 那些快照还在未来,`v.day` 自然算不出来 —— 2026-10-19 02:14 就这么挂过一次
/// (与业务无关,是夹具写死了钟点)。
///
/// **2026-10-21 00:07 又挂了一次**,这次是另一种:想构造"距零点 20 分钟的缺口",
/// 但刚过午夜时 `midnight+20min` 还在未来,`lo < hi` 不成立 → 缺口只剩 4 分钟
/// → 判不出缺口。根本原因还是**依赖真实钟点**。
///
/// 现在把"现在"钉成**今天 03:00**(合成值,直接传给 build_view),与真实钟点彻底解耦:
/// 凌晨、白天、深夜跑都得到同一组快照时刻。
#[test]
fn gap_and_fingerprint_reach_the_view() {
    let store = Store::open_memory().unwrap();
    let cfg = Config::default();
    let def = providers::find("deepseek").unwrap();
    let midnight = local_midnight_today();
    // 合成"现在" = 今天 03:00。build_view 接 now 参数,所以不必是真实时间;
    // 但**必须**与 record_fp 落的时刻同一坐标系(都是"今天零点 + 偏移")。
    let now = midnight + 3 * 3600;
    let res = FetchResult {
        valid: true,
        kind: Kind::Amount,
        unit: "CNY".into(),
        remaining: Some(90.0),
        ..Default::default()
    };
    // A 账:今天 00:20 才开始有观测(距零点 20 分钟 > OBSERVE_GAP_SEC),掉 10 元。
    let a1 = midnight + 20 * 60;
    let a2 = a1 + 60;
    store
        .record_fp(def.id, "fpA", a1, Some(100.0), None, None, "amount")
        .unwrap();
    store
        .record_fp(def.id, "fpA", a2, Some(90.0), None, None, "amount")
        .unwrap();
    // B 账:另一把 key,余额低得多(旧口径会把它算成一大笔"消耗")
    store
        .record_fp(def.id, "fpB", a2 + 30, Some(10.0), None, None, "amount")
        .unwrap();
    let v = build_view(def, res.clone(), false, now, &store, &cfg, None, "fpA", 2);
    assert_eq!(
        v.gap_day_from,
        Some(a1),
        "今天 a1 才有第一条,要如实标出来"
    );
    assert_eq!(v.day, Some(10.0), "只算 A 账自己的掉额");
    assert_eq!(v.accounts, 2, "用过两把密钥");
    assert!(
        v.gap_days.contains(&midnight),
        "今天本身就是缺口日(起点不在零点):{:?}",
        v.gap_days
    );

    // A 账起点贴着零点 → 不算缺口
    let s2 = Store::open_memory().unwrap();
    s2.record_fp(def.id, "fpC", midnight + 60, Some(100.0), None, None, "amount").unwrap();
    s2.record_fp(def.id, "fpC", midnight + 3600, Some(95.0), None, None, "amount").unwrap();
    let v2 = build_view(
        def,
        FetchResult { remaining: Some(95.0), ..res.clone() },
        false,
        // 同一个合成 now:这里以前传 now_ts(),刚过午夜时第二笔(midnight+1h)还在未来
        now,
        &s2,
        &cfg,
        None,
        "fpC",
        1,
    );
    assert_eq!(v2.gap_day_from, None, "正常开机不该天天挂一行缺口提示");
}

/// P1:金额型渠道一轮 `build_view` 只读一次历史,不再为每个窗口重复查库。
#[test]
fn build_view_reads_amount_history_once() {
    let store = Store::open_memory().unwrap();
    let cfg = Config::default();
    let def = providers::find("deepseek").unwrap();
    let now = local_midnight_today() + 12 * 3600;
    // 同一本账:今天两笔,足够算出当日消耗
    store
        .record_fp(def.id, "fpA", now - 2 * 3600, Some(100.0), None, None, "amount")
        .unwrap();
    store
        .record_fp(def.id, "fpA", now - 3600, Some(90.0), None, None, "amount")
        .unwrap();
    let res = FetchResult {
        valid: true,
        kind: Kind::Amount,
        unit: "CNY".into(),
        remaining: Some(90.0),
        ..Default::default()
    };
    crate::store::reset_history_reads();
    let v = build_view(def, res, false, now, &store, &cfg, None, "fpA", 1);
    assert_eq!(
        crate::store::history_read_count(),
        1,
        "每轮每个金额渠道只能读一次历史"
    );
    assert_eq!(v.day, Some(10.0), "批量路径仍要算出正确窗口");
}

/// 批量读取失败时必须**整块**回退:所有窗口/缺口与正常路径逐值相同。
#[test]
fn build_view_falls_back_whole_when_history_read_fails() {
    let store = Store::open_memory().unwrap();
    let cfg = Config::default();
    let def = providers::find("deepseek").unwrap();
    let now = local_midnight_today() + 12 * 3600;
    let mut rem = 500.0;
    for i in 0..6 {
        store
            .record_fp(
                def.id,
                "fpA",
                now - (40 - i * 3) * 3600,
                Some(rem),
                None,
                None,
                "amount",
            )
            .unwrap();
        rem -= 7.0;
    }
    let res = FetchResult {
        valid: true,
        kind: Kind::Amount,
        unit: "CNY".into(),
        remaining: Some(rem),
        ..Default::default()
    };
    let batched = build_view(def, res.clone(), false, now, &store, &cfg, None, "fpA", 1);

    crate::store::force_history_failure(true);
    let fallback = build_view(def, res, false, now, &store, &cfg, None, "fpA", 1);
    crate::store::force_history_failure(false);

    assert_eq!(fallback.day, batched.day);
    assert_eq!(fallback.week, batched.week);
    assert_eq!(fallback.month, batched.month);
    assert_eq!(fallback.day_prev, batched.day_prev);
    assert_eq!(fallback.week_prev, batched.week_prev);
    assert_eq!(fallback.month_prev, batched.month_prev);
    assert_eq!(fallback.gap_day_from, batched.gap_day_from);
    assert_eq!(fallback.gap_week_from, batched.gap_week_from);
    assert_eq!(fallback.gap_month_from, batched.gap_month_from);
    assert_eq!(fallback.gap_days, batched.gap_days);
    assert_eq!(fallback.claim.is_some(), batched.claim.is_some());
}

/// 批量统计与旧查询在所有字段上逐值一致(差额测试的 fetch 侧入口)。
#[test]
fn batched_amount_stats_match_legacy_queries() {
    let store = Store::open_memory().unwrap();
    let def = providers::find("deepseek").unwrap();
    let now = local_midnight_today() + 15 * 3600;
    for (i, remain) in [500.0, 480.0, 470.0, 460.0, 430.0].iter().enumerate() {
        store
            .record_fp(
                def.id,
                "fpA",
                now - (5 - i as i64) * 3600 - 86_400,
                Some(*remain),
                None,
                None,
                "amount",
            )
            .unwrap();
    }
    store
        .record_fp(def.id, "fpA", now - 1800, Some(420.0), None, None, "amount")
        .unwrap();

    let anchors = AmountAnchors::at(now);
    let rows = store
        .history_between(def.id, Some("fpA"), anchors.history_from(), now)
        .unwrap();
    let batched = AmountStats::from_rows(&rows, anchors, observed_gap_days(&store, def.id, Some("fpA"), now, local_tz_offset()).unwrap());
    let legacy = AmountStats::legacy(&store, def.id, "fpA", anchors);

    assert_eq!(batched.day, legacy.day);
    assert_eq!(batched.week, legacy.week);
    assert_eq!(batched.month, legacy.month);
    assert_eq!(batched.day_prev, legacy.day_prev);
    assert_eq!(batched.week_prev, legacy.week_prev);
    assert_eq!(batched.month_prev, legacy.month_prev);
    assert_eq!(batched.gap_day_from, legacy.gap_day_from);
    assert_eq!(batched.gap_week_from, legacy.gap_week_from);
    assert_eq!(batched.gap_month_from, legacy.gap_month_from);
    assert_eq!(batched.gap_days, legacy.gap_days);
}

/// 申请制:批量路径(历史行)与旧查询路径给出同一个状态。
#[test]
fn claim_from_rows_matches_claim_from_store() {
    let s = Store::open_memory().unwrap();
    let now = now_ts();
    let applied_at = now - 12 * 86_400;
    s.record("4sapi", applied_at, Some(19.0), None, None, "amount").unwrap();
    s.record("4sapi", applied_at + 300, Some(200.0), None, None, "amount").unwrap();
    s.record("4sapi", now - 60, Some(18.99), None, None, "amount").unwrap();
    let cfg = Config::default();
    let def = providers::PROVIDERS.iter().find(|d| d.id == "4sapi").unwrap();

    let rows = s.history_between(def.id, None, now - CLAIM_LOOKBACK_SEC, now).unwrap();
    let batched = claim_for_rows(def, &cfg, &rows, Some(18.99), now);
    let legacy = claim_for(def, &cfg, &s, Some(18.99), "", now);
    let (a, b) = (batched.expect("两条路都该有申请状态"), legacy.expect("旧路径"));
    assert_eq!(a.amount, b.amount);
    assert_eq!(a.last_claim_at, b.last_claim_at);
    assert_eq!(a.source, b.source);
    assert_eq!(a.days_until_eligible, b.days_until_eligible);
    assert_eq!(a.eligible, b.eligible);
    assert_eq!(a.daily_burn, b.daily_burn);
    assert_eq!(a.days_of_balance, b.days_of_balance);
    assert_eq!(a.shortage_risk, b.shortage_risk);
}

/// P2:自定义渠道组内部并发(上限 4)、按配置顺序返回、隐藏渠道不读凭据。
#[test]
fn custom_channels_fetch_in_parallel_keep_order_and_skip_hidden() {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn serve(delay_ms: u64, body: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // 读到请求头就够(请求体为空)
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.shutdown(Shutdown::Both);
        });
        (format!("http://{addr}/"), h)
    }

    fn channel(id: &str, url: String) -> custom::CustomChannel {
        custom::CustomChannel {
            id: id.into(),
            name: id.into(),
            url,
            ..Default::default()
        }
    }

    let (u1, h1) = serve(250, "11");
    let (u2, h2) = serve(250, "22");
    let channels = vec![
        channel("c1", u1),
        channel("c-hidden", "http://127.0.0.1:1/".into()),
        channel("c2", u2),
    ];
    let hidden = vec!["c-hidden".to_string()];
    let reads = Arc::new(AtomicUsize::new(0));
    let reads_for_key = reads.clone();
    let key_for = move |id: &str| {
        reads_for_key.fetch_add(1, Ordering::SeqCst);
        Some(format!("test-key-{id}"))
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let started = std::time::Instant::now();
    let out = rt.block_on(fetch_custom_all(&client, &channels, &hidden, key_for));
    let elapsed = started.elapsed();
    h1.join().unwrap();
    h2.join().unwrap();

    assert_eq!(out.len(), 2, "隐藏渠道不进结果");
    assert_eq!(
        out.iter().map(|(c, _, _)| c.id.as_str()).collect::<Vec<_>>(),
        vec!["c1", "c2"],
        "并发完成顺序不能改变配置顺序"
    );
    assert_eq!(out[0].1.as_ref().unwrap().remaining, Some(11.0));
    assert_eq!(out[1].1.as_ref().unwrap().remaining, Some(22.0));
    assert_eq!(reads.load(Ordering::SeqCst), 2, "隐藏渠道连凭据都不读");
    assert!(
        elapsed < std::time::Duration::from_millis(450),
        "两个各 250ms 的端点应并行完成(串行 ≥500ms),实际 {elapsed:?}"
    );
}
