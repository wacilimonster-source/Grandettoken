use super::*;

fn mem() -> Store {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT, provider_id TEXT NOT NULL,
                ts INTEGER NOT NULL, remaining REAL, used REAL, total REAL, kind TEXT NOT NULL,
                key_fp TEXT NOT NULL DEFAULT '');
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .unwrap();
    Store { conn }
}

/// 走完整建表 + 迁移的库(验新表/迁移用)。`mem()` 只搭了最小的两张表。
fn mem_migrated() -> Store {
    let conn = Connection::open_in_memory().unwrap();
    let s = Store { conn };
    s.init_schema().unwrap();
    s.migrate().unwrap();
    s
}

#[test]
fn grant_rows_roundtrip_and_dedupe_by_second() {
    let s = mem_migrated();
    let g = |ts: i64, amount: f64| crate::grants::Grant {
        channel: "workbuddy-ai".into(),
        day: "2026-10-23".into(),
        ts,
        amount,
        source: "delta".into(),
        from_bal: Some(1000.0),
        to_bal: Some(1000.0 + amount),
    };
    assert!(s.insert_grant(&g(200, 30.0)).unwrap());
    assert!(!s.insert_grant(&g(200, 30.0)).unwrap(), "同一秒重放不该记第二条");
    assert!(s.insert_grant(&g(100, 500.0)).unwrap());

    let rows = s.grants_since("workbuddy-ai", 0).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].ts, 100, "按时间升序,界面按天分组要顺着看");
    assert_eq!(rows[1].amount, 30.0);
    // 别的渠道不串
    assert!(s.grants_since("workbuddy", 0).unwrap().is_empty());
}

#[test]
fn prev_remaining_is_strictly_earlier() {
    let s = mem_migrated();
    s.record_fp("workbuddy-ai", "", 100, Some(1000.0), None, None, "points").unwrap();
    s.record_fp("workbuddy-ai", "", 200, Some(1030.0), None, None, "points").unwrap();
    assert_eq!(s.prev_remaining("workbuddy-ai", "", 200).unwrap(), Some(1000.0));
    assert_eq!(s.prev_remaining("workbuddy-ai", "", 300).unwrap(), Some(1030.0));
    assert_eq!(s.prev_remaining("workbuddy-ai", "", 100).unwrap(), None, "首条没有基线");
    assert_eq!(s.prev_remaining("workbuddy", "", 300).unwrap(), None, "别的渠道不串");
}

#[test]
fn prune_grants_keeps_the_window() {
    let s = mem_migrated();
    for ts in [100, 200, 300] {
        let g = crate::grants::Grant {
            channel: "workbuddy-ai".into(),
            day: "2026-10-23".into(),
            ts,
            amount: 30.0,
            source: "delta".into(),
            from_bal: None,
            to_bal: None,
        };
        s.insert_grant(&g).unwrap();
    }
    assert_eq!(s.prune_grants(250).unwrap(), 2);
    assert_eq!(s.grants_since("workbuddy-ai", 0).unwrap().len(), 1);
}

/// 迁移前的现场:只有 vault_items(且里面已经有重复行),`user_version` 还是 0。
/// 用 (id, platform, label, created_at)。
fn mem_with_vault(rows: &[(&str, &str, &str, i64)]) -> Store {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE vault_items (
                id         TEXT PRIMARY KEY,
                platform   TEXT    NOT NULL,
                label      TEXT    NOT NULL,
                hint       TEXT    NOT NULL DEFAULT '',
                note       TEXT    NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );",
    )
    .unwrap();
    for (id, platform, label, created_at) in rows {
        conn.execute(
            "INSERT INTO vault_items (id, platform, label, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, platform, label, created_at],
        )
        .unwrap();
    }
    Store { conn }
}

    /// 索引是**派生数据**:整表重抄之后,它必须完全等于"这一次扫描到的真相"。
///
/// 这条测试盯的是最容易漏的一种走偏:逐条 upsert 只能"加和改",**清不掉**
/// "索引里有、指针里没有"的行 —— 而那正是取消引用时索引没删干净的形态。
#[test]
fn vault_ref_rebuild_replaces_the_whole_index() {
    // 用 open_memory()(会跑 init_schema),不能用 mem():后者只有裸连接,
    // vault_refs 表根本没建出来 —— 那样测的是表不存在,不是对账逻辑。
    let s = Store::open_memory().unwrap();

    // 第一次对账:两个渠道各引用一条
    s.vault_ref_rebuild(&[
        ("4sapi".to_string(), "uuid-a".to_string()),
        ("hapi".to_string(), "uuid-b".to_string()),
    ])
    .unwrap();
    let pairs = s.vault_ref_pairs().unwrap();
    assert!(pairs.contains(&("4sapi".to_string(), "uuid-a".to_string())));
    assert!(pairs.contains(&("hapi".to_string(), "uuid-b".to_string())));

    // 第二次对账:4sapi 改引用 uuid-c,hapi 取消引用 —— 旧行必须消失
    s.vault_ref_rebuild(&[
        ("4sapi".to_string(), "uuid-c".to_string()),
        ("hapi".to_string(), "uuid-b".to_string()),
    ])
    .unwrap();
    let pairs = s.vault_ref_pairs().unwrap();
    assert!(
        !pairs.iter().any(|(_, v)| v == "uuid-a"),
        "旧的 uuid-a 行必须被清掉,否则列表会说谎"
    );
    assert!(pairs.contains(&("4sapi".to_string(), "uuid-c".to_string())));
    assert!(pairs.contains(&("hapi".to_string(), "uuid-b".to_string())));

    // 第三次对账:什么都不剩(全部取消引用)
    s.vault_ref_rebuild(&[]).unwrap();
    assert!(s.vault_ref_pairs().unwrap().is_empty(), "空真相 -> 空索引");

    // 一个条目可以被多个渠道引用(裁决 6)
    s.vault_ref_rebuild(&[
        ("4sapi".to_string(), "uuid-shared".to_string()),
        ("hapi".to_string(), "uuid-shared".to_string()),
    ])
    .unwrap();
    let mut got: Vec<String> = s
        .vault_ref_pairs()
        .unwrap()
        .iter()
        .filter(|(_, v)| v == "uuid-shared")
        .map(|(c, _)| c.clone())
        .collect();
    got.sort();
    assert_eq!(got, vec!["4sapi".to_string(), "hapi".to_string()]);
}

#[test]
fn consumption_sums_decreases_and_ignores_topups() {
    let s = mem();
    // 余额 100 -> 90 -> 85 -> 充值回 200 -> 195
    for (ts, rem) in [(1000, 100.0), (2000, 90.0), (3000, 85.0), (4000, 200.0), (5000, 195.0)] {
        s.record("p", ts, Some(rem), None, None, "amount").unwrap();
    }
    // 消耗 = 10 + 5 + 0(充值不算) + 5 = 20
    assert_eq!(s.consumption_since("p", 1000, 5000).unwrap(), Some(20.0));
}

#[test]
fn consumption_is_none_without_baseline() {
    let s = mem();
    s.record("p", 5000, Some(10.0), None, None, "amount").unwrap();
    // 窗口起点 4000 之前没有基线,窗口内也只有一条 → 真的无从推算
    assert_eq!(s.consumption_since("p", 4000, 6000).unwrap(), None);
}

/// F8:有基线但**窗口内一条都没有** → None,不是 Some(0.0)。
/// 报 0 等于说"这一期消耗 0",环比百分比会凭空被抬高(doc 早写明要两条快照)。
#[test]
fn consumption_is_none_when_the_window_itself_has_no_row() {
    let s = mem();
    // 只有窗口起点之前的那一条快照(基线),窗口里什么都没有
    s.record("p", 1000, Some(100.0), None, None, "amount").unwrap();
    assert_eq!(
        s.consumption_since("p", 2000, 3000).unwrap(),
        None,
        "窗口内没有快照时不能编一个 0 出来"
    );
    // 窗口内一旦有行,原有的"跨起点按时间折算"行为一字不变:
    // 基线 100(ts=1000)→ 窗口首行 80(ts=2500),起点 2000 处按线性折算余额
    // = 100 - 20×(1000/1500) = 86.67,差额 6.67
    s.record("p", 2500, Some(80.0), None, None, "amount").unwrap();
    assert_eq!(s.consumption_since("p", 2000, 3000).unwrap(), Some(6.67));
}

#[test]
fn migration_preserves_multiple_accounts_in_the_same_group() {
    let s = mem_with_vault(&[("a", "4SAPI", "工作", 100), ("b", "4sapi", "工作", 200)]);
    s.conn.execute_batch("PRAGMA user_version = 5;").unwrap();
    s.migrate().unwrap();
    let list = s.vault_list().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|v| v.api_endpoint.is_empty() && v.website.is_empty()));
    s.migrate().unwrap();
    assert_eq!(s.vault_list().unwrap().len(), 2);
}

#[test]
fn migration_removes_legacy_unique_index_without_losing_data() {
    let s = mem_with_vault(&[("a", "Hapi", "工作", 100)]);
    s.conn.execute_batch("CREATE UNIQUE INDEX idx_vault_uniq ON vault_items (platform COLLATE NOCASE, label COLLATE NOCASE); PRAGMA user_version = 5;").unwrap();
    s.migrate().unwrap();
    let mut second = s.vault_get("a").unwrap().unwrap();
    second.id = "b".into(); second.note = "另一个账号".into();
    s.vault_upsert(&second).unwrap();
    assert_eq!(s.vault_list().unwrap().len(), 2);
}

#[test]
fn vault_group_allows_multiple_accounts_and_preserves_addresses() {
    let s = Store::open_memory().unwrap();
    let mk = |id: &str, account: &str| VaultItem {
        id: id.into(), platform: "4SAPI".into(), label: "工作".into(),
        hint: String::new(), note: account.into(),
        api_endpoint: "https://example.com/v1".into(), website: "https://example.com".into(),
        created_at: 1, updated_at: 1,
    };
    s.vault_upsert(&mk("a", "alice")).unwrap();
    s.vault_upsert(&mk("b", "bob")).unwrap();
    let list = s.vault_list().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(s.vault_get("a").unwrap().unwrap().api_endpoint, "https://example.com/v1");
    assert_eq!(s.vault_get("b").unwrap().unwrap().website, "https://example.com");
}

/// 无窗口前基线(装软件当周/当月):从窗口内首快照起算下限,而不是整格「——」。
#[test]
fn missing_baseline_falls_back_to_window_floor() {
    let s = mem();
    // 快照从 3000 开始,窗口从 1000 起:100→90→充值 200→195
    for (ts, rem) in [(3000, 100.0), (4000, 90.0), (5000, 200.0), (6000, 195.0)] {
        s.record("p", ts, Some(rem), None, None, "amount").unwrap();
    }
    // 下限 = 窗口内相邻下降之和 = 10 + 0 + 5(3000 之前那段看不到,不算)
    assert_eq!(s.consumption_since("p", 1000, 6000).unwrap(), Some(15.0));
}

/// 跨窗口起点的基线对:窗口外那段时间的下降不能记进窗口
/// (修复"今日消耗把昨晚花销背进今天"的越界计数)。
#[test]
fn baseline_pair_is_prorated_to_window_start() {
    let s = mem();
    // 0 时点余额 100,100 时点余额 80:线性掉 20。窗口从 50 起 → 只算后一半 10。
    s.record("p", 0, Some(100.0), None, None, "amount").unwrap();
    s.record("p", 100, Some(80.0), None, None, "amount").unwrap();
    assert_eq!(s.consumption_since("p", 50, 200).unwrap(), Some(10.0));
    // 窗口起点恰好等于基线时刻:退化为整段计入
    assert_eq!(s.consumption_since("p", 0, 200).unwrap(), Some(20.0));
    // 起点贴着窗口首行:只剩 0 段
    assert_eq!(s.consumption_since("p", 99, 200).unwrap(), Some(0.2));
}

#[test]
fn settings_roundtrip() {
    let s = mem();
    s.set_setting("interval", "300").unwrap();
    assert_eq!(s.get_setting("interval").unwrap().as_deref(), Some("300"));
    s.set_setting("interval", "60").unwrap();
    assert_eq!(s.get_setting("interval").unwrap().as_deref(), Some("60"));
}

#[test]
fn last_jump_finds_most_recent_application() {
    let s = mem();
    // 18.99 -> 申请补到 200 -> 慢慢花 -> 再申请一次 -> 又花一点
    for (ts, rem) in [
        (1000, 18.99),
        (2000, 200.0), // 第一次申请
        (3000, 180.0),
        (4000, 150.0),
        (5000, 195.0), // 第二次申请(补到 200 后花掉 5)
        (6000, 190.0),
    ] {
        s.record("p", ts, Some(rem), None, None, "amount").unwrap();
    }
    // 只认最近的跳升
    assert_eq!(s.last_jump_since("p", 0, 20.0, 200.0, 0).unwrap(), Some(5000));
    // 阈值高于实际跳升(45)-> 只剩第一次(181.01)
    assert_eq!(s.last_jump_since("p", 0, 182.0, 200.0, 0).unwrap(), None);
    // 窗口把第一次跳升切掉
    assert_eq!(s.last_jump_since("p", 2500, 20.0, 200.0, 0).unwrap(), Some(5000));
}

#[test]
fn small_wobbles_are_not_applications() {
    let s = mem();
    for (ts, rem) in [(1000, 100.0), (2000, 100.4), (3000, 99.8), (4000, 105.0)] {
        s.record("p", ts, Some(rem), None, None, "amount").unwrap();
    }
    // 5 元的抖动在 20 元阈值下不算申请
    assert_eq!(s.last_jump_since("p", 0, 20.0, 0.0, 0).unwrap(), None);
    assert_eq!(s.last_jump_since("p", 0, 4.0, 0.0, 0).unwrap(), Some(4000));
}

/// 平台对失败请求退还预扣,一次能涨几十元 —— 只看涨幅会把它记成"用户申请过",
/// 于是申请倒计时被顶回去、用户白等(报告 P2-1)。
#[test]
fn refunds_far_below_the_quota_line_are_not_applications() {
    let s = mem();
    for (ts, rem) in [
        (1000, 12.0),
        (2000, 60.0), // 涨 48 元,但离 200 的额度线很远 -> 是退还,不是申请
        (3000, 40.0),
        (4000, 198.0), // 这才是一次申请
    ] {
        s.record("p", ts, Some(rem), None, None, "amount").unwrap();
    }
    assert_eq!(s.last_jump_since("p", 0, 20.0, 200.0, 0).unwrap(), Some(4000));
    // 隔太久的两条(休眠后第一跳)也不算跳升
    assert_eq!(s.last_jump_since("p", 0, 20.0, 200.0, 500).unwrap(), None);
}

#[test]
fn consumed_in_window_ignores_topups_and_needs_no_baseline() {
    let s = mem();
    for (ts, rem) in [(3000, 100.0), (4000, 90.0), (5000, 200.0), (6000, 195.0)] {
        s.record("p", ts, Some(rem), None, None, "amount").unwrap();
    }
    // 10 + 0(申请补到 200,不算消耗) + 5 = 15
    assert_eq!(s.consumed_in_window("p", 1000, 6000).unwrap(), 15.0);
    // 窗口从历史中间开始:只用窗口内的相邻行,得到 0 + 5
    assert_eq!(s.consumed_in_window("p", 3500, 6000).unwrap(), 5.0);
    assert_eq!(s.first_ts_since("p", 3500).unwrap(), Some(4000));

    // 对照:consumption_since 无窗口前基线时退化为窗口内下限估算(同为 15)
    assert_eq!(s.consumption_since("p", 1000, 6000).unwrap(), Some(15.0));
}

// ───────────────── 密钥指纹分段(换 key 账目不互相推算) ─────────────────

/// 老库(没有 key_fp 列)升级后,列要真的补上,历史行落在空串里。
#[test]
fn migration_adds_key_fp_to_old_snapshots_table() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT, provider_id TEXT NOT NULL,
                ts INTEGER NOT NULL, remaining REAL, used REAL, total REAL, kind TEXT NOT NULL);
             INSERT INTO snapshots (provider_id, ts, remaining, kind) VALUES ('p', 1000, 80.0, 'amount');",
    )
    .unwrap();
    let s = Store { conn };
    s.migrate().unwrap();
    // 补出来的列能写能读:给历史行认领指纹
    assert_eq!(s.adopt_orphan_snapshots("p", "fpA").unwrap(), 1);
    assert_eq!(s.consumption_since_fp("p", "fpA", 500, 2000).unwrap(), None, "只有一条,推不出消耗");
    s.record_fp("p", "fpA", 2000, Some(70.0), None, None, "amount").unwrap();
    assert_eq!(s.consumption_since_fp("p", "fpA", 500, 3000).unwrap(), Some(10.0));
    // 幂等:再跑一次迁移不动数据
    s.migrate().unwrap();
    assert_eq!(s.fingerprint_count("p").unwrap(), 1);
}

/// 两本账互不推算;不带指纹的旧口径会把中间那笔跨账户跳变算成消耗 —— 这正是
/// 分段要挡住的东西。
#[test]
fn fingerprint_scoping_keeps_accounts_apart() {
    let s = Store::open_memory().unwrap();
    s.record_fp("p", "A", 1000, Some(100.0), None, None, "amount").unwrap();
    s.record_fp("p", "A", 2000, Some(90.0), None, None, "amount").unwrap();
    s.record_fp("p", "B", 3000, Some(500.0), None, None, "amount").unwrap();
    s.record_fp("p", "B", 4000, Some(400.0), None, None, "amount").unwrap();
    assert_eq!(s.consumption_since_fp("p", "A", 500, 5000).unwrap(), Some(10.0));
    assert_eq!(s.consumption_since_fp("p", "B", 500, 5000).unwrap(), Some(100.0));
    // 旧口径(不分段):A→B 的 410 元跳升被 clamp 掉,但两本账的下降被加在一起
    assert_eq!(
        s.consumption_since("p", 500, 5000).unwrap(),
        Some(110.0),
        "不分段 = 跨账户求和,这就是升级后要避免的旧行为"
    );
    assert_eq!(s.fingerprint_count("p").unwrap(), 2, "两本账");
}

/// 认领只发生一次:换 key 写进来的新指纹不会被再认领走。
#[test]
fn adopt_orphans_claims_legacy_rows_only_once() {
    let s = Store::open_memory().unwrap();
    s.record("p", 1000, Some(50.0), None, None, "amount").unwrap(); // 升级前的行(空指纹)
    assert_eq!(s.adopt_orphan_snapshots("p", "fpA").unwrap(), 1);
    assert_eq!(s.adopt_orphan_snapshots("p", "fpA").unwrap(), 0, "再认领没有新行");
    s.record_fp("p", "fpB", 2000, Some(20.0), None, None, "amount").unwrap();
    assert_eq!(s.adopt_orphan_snapshots("p", "fpB").unwrap(), 0, "别把 A 的账抢过来");
    assert_eq!(s.fingerprint_count("p").unwrap(), 2);
    // 空指纹不认领(调用方没拿到稳定凭据时不该动数据)
    assert_eq!(s.adopt_orphan_snapshots("p", "").unwrap(), 0);
}

// ───────────────── 缺口覆盖 / 变动台账 / 长期趋势 ─────────────────

#[test]
fn window_and_daily_coverage_report_gaps() {
    let s = Store::open_memory().unwrap();
    let tz = 8 * 3600;
    // 北京 10-16:09:00 开机,23:00 关机;10-17 只观测了 14:00 之后两条
    let d16 = 16 * 86_400 - tz; // 10-16 00:00(北京)
    let d17 = 17 * 86_400 - tz;
    s.record("p", d16 + 9 * 3600, Some(100.0), None, None, "amount").unwrap();
    s.record("p", d16 + 23 * 3600, Some(80.0), None, None, "amount").unwrap();
    s.record("p", d17 + 14 * 3600, Some(70.0), None, None, "amount").unwrap();
    s.record("p", d17 + 15 * 3600, Some(66.0), None, None, "amount").unwrap();

    let cov = s.window_coverage("p", None, d16, d17 + 86_400).unwrap();
    assert_eq!(cov.count, 4);
    assert_eq!(cov.first_ts, Some(d16 + 9 * 3600));
    assert_eq!(cov.last_ts, Some(d17 + 15 * 3600));

    let days = s.daily_coverage("p", None, d16, d17 + 86_400, tz).unwrap();
    assert_eq!(days.len(), 2);
    assert_eq!(days[0].day_start, d16);
    assert_eq!(days[0].first_ts, d16 + 9 * 3600, "16 日 09:00 才开始有观测");
    assert_eq!(days[0].count, 2);
    assert_eq!(days[1].day_start, d17);
    assert_eq!(days[1].first_ts, d17 + 14 * 3600);
    assert_eq!(days[1].count, 2);
}

/// 台账:同方向的连续轮询并成一条,方向一变立刻另起;空档秒数如实带上。
#[test]
fn change_ledger_merges_consecutive_drops_and_splits_on_topup() {
    let s = Store::open_memory().unwrap();
    s.record("p", 9 * 3600, Some(100.0), None, None, "amount").unwrap();
    s.record("p", 9 * 3600 + 60, Some(99.5), None, None, "amount").unwrap();
    s.record("p", 9 * 3600 + 120, Some(99.0), None, None, "amount").unwrap();
    // 一小时后充值 +50
    s.record("p", 10 * 3600, Some(149.0), None, None, "amount").unwrap();
    // 紧接着又花掉 1 元
    s.record("p", 10 * 3600 + 60, Some(148.0), None, None, "amount").unwrap();

    let ch = s.recent_changes("p", None, 0, 20).unwrap();
    assert_eq!(ch.len(), 3, "掉→充→掉,三次变动:{ch:?}");
    // 最新在前
    assert!((ch[0].delta + 1.0).abs() < 1e-9 && ch[0].samples == 1);
    assert_eq!(ch[0].gap_secs, 60, "与上一笔间隔 1 分钟");
    assert!((ch[1].delta - 50.0).abs() < 1e-9, "充值那条是 +50");
    assert_eq!(ch[1].gap_secs, 10 * 3600 - (9 * 3600 + 120), "含一小时的空白");
    assert!((ch[2].delta + 1.0).abs() < 1e-9, "两笔 0.5 合并成 -1.0");
    assert_eq!(ch[2].samples, 2, "两次轮询参与了聚合");
    assert_eq!(ch[2].start_ts, 9 * 3600);
    assert_eq!(ch[2].end_ts, 9 * 3600 + 120);

    // limit 只留最近的几条(一条变动记的是 [起点, 终点点] 这一段)
    let one = s.recent_changes("p", None, 0, 1).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].start_ts, 10 * 3600);
    assert_eq!(one[0].end_ts, 10 * 3600 + 60);
}

/// 台账按指纹分段:另一本账的变动不该出现在这本账里。
#[test]
fn change_ledger_respects_fingerprint() {
    let s = Store::open_memory().unwrap();
    s.record_fp("p", "A", 1000, Some(100.0), None, None, "amount").unwrap();
    s.record_fp("p", "A", 1060, Some(95.0), None, None, "amount").unwrap();
    s.record_fp("p", "B", 1200, Some(500.0), None, None, "amount").unwrap();
    s.record_fp("p", "B", 1260, Some(400.0), None, None, "amount").unwrap();
    let a = s.recent_changes("p", Some("A"), 0, 20).unwrap();
    assert_eq!(a.len(), 1);
    assert!((a[0].delta + 5.0).abs() < 1e-9, "A 账只掉了 5");
}

/// 明细归档成日级摘要:删之前先压,摘要保留;再跑一次不重复累加。
#[test]
fn prune_archives_details_into_daily_summaries() {
    let s = Store::open_memory().unwrap();
    let tz = 8 * 3600;
    let now = 100 * 86_400;
    let day = |d: i64| d * 86_400 - tz; // 那天的本地零点
    // 三天明细,每天掉 10 元(相邻下降)
    for d in [10_i64, 11, 12] {
        s.record("p", day(d) + 10 * 3600, Some(100.0), None, None, "amount").unwrap();
        s.record("p", day(d) + 11 * 3600, Some(90.0), None, None, "amount").unwrap();
    }
    // 保留窗口:now - 20 天,只有 10~12 号那三天被清
    let cutoff = now - 20 * 86_400;
    let removed = s.prune(cutoff, tz).unwrap();
    assert_eq!(removed, 6, "6 条明细被清掉");

    let sums = s.daily_summaries("p", None, 0).unwrap();
    assert_eq!(sums.len(), 3, "三天都有摘要");
    assert_eq!(sums[0].day_start, day(10));
    assert!((sums[0].consumed - 10.0).abs() < 1e-9, "每天 10 元: {sums:?}");
    assert!((sums[2].consumed - 10.0).abs() < 1e-9, "每天 10 元: {sums:?}");
    assert_eq!(sums[0].first_ts, day(10) + 10 * 3600);
    assert_eq!(sums[0].last_ts, day(10) + 11 * 3600);
    assert_eq!(sums[0].count, 2);

    // 幂等:再清一次,摘要不会翻倍
    let again = s.prune(cutoff, tz).unwrap();
    assert_eq!(again, 0);
    let sums2 = s.daily_summaries("p", None, 0).unwrap();
    assert!((sums2[0].consumed - 10.0).abs() < 1e-9, "重跑不重复累加");
    assert_eq!(sums2[0].count, 2);
}

/// 摘要的保留期是"明细之外再一年":太老的日直接清掉,一年内的留着。
#[test]
fn daily_summary_retention_drops_ancient_days() {
    let s = Store::open_memory().unwrap();
    let tz = 8 * 3600;
    let now = 500 * 86_400;
    let old = (now - 400 * 86_400) / 86_400; // 400 天前
    let mid = (now - 100 * 86_400) / 86_400; // 100 天前
    for d in [old, mid] {
        s.record("p", d * 86_400 - tz + 10 * 3600, Some(100.0), None, None, "amount").unwrap();
        s.record("p", d * 86_400 - tz + 11 * 3600, Some(95.0), None, None, "amount").unwrap();
    }
    s.prune(now - 90 * 86_400, tz).unwrap();
    let sums = s.daily_summaries("p", None, 0).unwrap();
    assert_eq!(sums.len(), 1, "只留 100 天前那条(400 天前超出一年保留期): {sums:?}");
    assert_eq!(sums[0].day_start, mid * 86_400 - tz);
    assert!((sums[0].consumed - 5.0).abs() < 1e-9, "{sums:?}");
}

/// 百分比渠道不进归档:daily_summary 只收 kind='amount' 的消耗。
#[test]
fn archive_ignores_non_amount_kinds() {
    let s = Store::open_memory().unwrap();
    let tz = 0;
    let d = 10 * 86_400;
    s.record("p", d + 3600, Some(50.0), None, None, "percent").unwrap();
    s.record("p", d + 7200, Some(40.0), None, None, "percent").unwrap();
    s.prune(100 * 86_400, tz).unwrap();
    let sums = s.daily_summaries("p", None, 0).unwrap();
    assert_eq!(sums.len(), 1, "覆盖仍记(它证明应用在跑)");
    assert_eq!(sums[0].count, 2);
    assert!((sums[0].consumed - 0.0).abs() < 1e-9, "百分比渠道没有余额消耗");
}

/// 跨归档边界的那一笔不能丢(第三轮体检 P2-④)。
/// 构造:10 号两条明细(100→90,掉 10),11 号两条(80→70),清理线压在 11 号零点 ——
/// 于是"90→80"这一跳的**前一行在窗口内、后一行在窗口外**,修复前它两头都不沾:
/// 这一轮读不到后一行,下一轮前一行已被删除 → 永久少记一笔。
#[test]
fn archive_keeps_the_drop_that_straddles_the_cutoff() {
    let s = Store::open_memory().unwrap();
    let tz = 0i64;
    let day = |d: i64| d * 86_400;
    s.record("p", day(10) + 10 * 3600, Some(100.0), None, None, "amount").unwrap();
    s.record("p", day(10) + 11 * 3600, Some(90.0), None, None, "amount").unwrap();
    s.record("p", day(11) + 3600, Some(80.0), None, None, "amount").unwrap();
    s.record("p", day(11) + 2 * 3600, Some(70.0), None, None, "amount").unwrap();

    s.prune(day(11), tz).unwrap();
    let sums = s.daily_summaries("p", None, 0).unwrap();
    assert_eq!(sums.len(), 1, "只有 10 号那天进了摘要: {sums:?}");
    assert!(
        (sums[0].consumed - 20.0).abs() < 1e-9,
        "10 号 = 当天 10 + 跨边界的 10: {sums:?}"
    );

    // 再清一轮:11 号那两条归档。同一对相邻行不能被算第二次(前一行已不在表里)。
    s.prune(day(12), tz).unwrap();
    let sums2 = s.daily_summaries("p", None, 0).unwrap();
    let d10 = sums2.iter().find(|x| x.day_start == day(10)).unwrap();
    let d11 = sums2.iter().find(|x| x.day_start == day(11)).unwrap();
    assert!((d10.consumed - 20.0).abs() < 1e-9, "10 号不该被再算一次: {sums2:?}");
    assert!((d11.consumed - 10.0).abs() < 1e-9, "11 号只有 80→70: {sums2:?}");
}

/// 长期趋势曲线:明细与日级摘要拼在一条线上;有缺口的日子标出来。
#[test]
fn trend_series_merges_archive_and_details_and_marks_gaps() {
    let s = Store::open_memory().unwrap();
    let tz = 0;
    let now = 100 * 86_400;
    let from = now - 10 * 86_400; // 10 天窗口
    // 8 天前:一天的明细(掉 10 元)——会被归档
    let old = now - 8 * 86_400;
    s.record("p", old + 3600, Some(100.0), None, None, "amount").unwrap();
    s.record("p", old + 7200, Some(90.0), None, None, "amount").unwrap();
    // 今天:明细(掉 3 元)
    s.record("p", now - 3600, Some(50.0), None, None, "amount").unwrap();
    s.record("p", now - 1800, Some(47.0), None, None, "amount").unwrap();
    // 归档(把 8 天前那天压进摘要并清掉明细;90 天清理线设在 5 天前)
    s.prune(now - 5 * 86_400, tz).unwrap();
    assert_eq!(s.daily_summaries("p", None, from).unwrap().len(), 1, "归档那天在摘要里");

    let (vals, gaps) = s.trend_series("p", None, from, now, 10, tz).unwrap();
    assert_eq!(vals.len(), 10);
    let total: f64 = vals.iter().sum();
    assert!((total - 13.0).abs() < 1e-9, "归档的 10 元 + 今天的 3 元 = 13:{vals:?}");
    // 8 天前那天:明细起点 01:00(离零点 1 小时 > 10 分钟)且末尾 02:00 收工 → 有缺口
    assert!(!gaps.is_empty(), "那天只观测了两小时,必须标成缺口");
    assert_eq!(vals[2], 10.0, "归档那天落在第 3 个桶");
}

/// 窗口里一天都没数据的那种"空",不算缺口(那是没数据,不是有缺口)。
#[test]
fn trend_series_ignores_days_without_any_observation() {
    let s = Store::open_memory().unwrap();
    let now = 100 * 86_400;
    let from = now - 10 * 86_400;
    s.record("p", now - 3600, Some(10.0), None, None, "amount").unwrap();
    s.record("p", now - 1800, Some(9.0), None, None, "amount").unwrap();
    let (vals, gaps) = s.trend_series("p", None, from, now, 10, 0).unwrap();
    let total: f64 = vals.iter().sum();
    assert!((total - 1.0).abs() < 1e-9);
    // 只有"今天"这一天有观测,其余 9 天是"没数据" —— 只有今天可能算缺口
    assert!(gaps.len() <= 1, "没数据的日子不该被标成缺口:{gaps:?}");
}

// ───────────── P1:批量历史 vs 旧查询的差分测试 ─────────────

/// 把一批 (ts, remaining, fp) 落进库里。`remaining = None` 模拟"取到数但没有金额"。
fn seed(store: &Store, provider: &str, rows: &[(i64, Option<f64>, &str)]) {
    for (ts, remaining, fp) in rows {
        store
            .record_fp(provider, fp, *ts, *remaining, None, None, "amount")
            .unwrap();
    }
}

/// 同一份数据、同一个窗口:旧 SQL 查询与「批量读取 + 纯函数」必须逐值相等。
fn assert_rows_match_legacy(
    store: &Store,
    provider: &str,
    fp: &str,
    from_ts: i64,
    to_ts: i64,
) {
    // 消费/缺口/跳升各自需要不同的读取范围,但都来自同一次查询能覆盖的范围。
    let history_from = (from_ts - 86_400).min(to_ts - 92 * 86_400);
    let rows = store
        .history_between(provider, Some(fp), history_from, to_ts)
        .unwrap();

    assert_eq!(
        consumption_since_rows(&rows, from_ts, to_ts),
        store.consumption_since_fp(provider, fp, from_ts, to_ts).unwrap(),
        "consumption_since 不一致 provider={provider} fp={fp} from={from_ts} to={to_ts}"
    );
    assert_eq!(
        consumed_in_window_rows(&rows, from_ts, to_ts),
        store.consumed_in_window_fp(provider, fp, from_ts, to_ts).unwrap(),
        "consumed_in_window 不一致 provider={provider}"
    );
    assert_eq!(
        window_coverage_rows(&rows, from_ts, to_ts),
        store.window_coverage(provider, Some(fp), from_ts, to_ts).unwrap(),
        "window_coverage 不一致 provider={provider}"
    );
    assert_eq!(
        daily_coverage_rows(&rows, from_ts, to_ts, 8 * 3600),
        store
            .daily_coverage(provider, Some(fp), from_ts, to_ts, 8 * 3600)
            .unwrap(),
        "daily_coverage 不一致 provider={provider}"
    );
    assert_eq!(
        last_jump_rows(&rows, from_ts, 20.0, 100.0, 2 * 3600),
        store
            .last_jump_since_fp(provider, fp, from_ts, 20.0, 100.0, 2 * 3600)
            .unwrap(),
        "last_jump 不一致 provider={provider}"
    );
    assert_eq!(
        first_ts_rows(&rows, from_ts),
        store.first_ts_since_fp(provider, fp, from_ts).unwrap(),
        "first_ts 不一致 provider={provider}"
    );

    // 旧口径(不限指纹)也必须一致。
    let any = store
        .history_between(provider, None, history_from, to_ts)
        .unwrap();
    assert_eq!(
        consumption_since_rows(&any, from_ts, to_ts),
        store.consumption_since(provider, from_ts, to_ts).unwrap(),
        "不限指纹的 consumption_since 不一致 provider={provider}"
    );
    assert_eq!(
        window_coverage_rows(&any, from_ts, to_ts),
        store.window_coverage(provider, None, from_ts, to_ts).unwrap()
    );
}

/// 固定边界夹具:空、单行、只有基线、跨月、2 月 28/29、3 月 31、闰年、
/// 00:00:00、23:59:59、充值(负差值)、重复时间戳。
#[test]
fn history_rows_match_legacy_on_boundary_fixtures() {
    let s = mem_migrated();
    let cases: &[(&str, &[(i64, Option<f64>, &str)], i64, i64)] = &[
        // 空库
        ("empty", &[], 1_700_000_000, 1_700_100_000),
        // 窗口内只有一条(不足两条 → None)
        ("single", &[(1_700_000_100, Some(50.0), "fp")], 1_700_000_000, 1_700_100_000),
        // 只有窗口起点之前的基线 → None
        (
            "baseline-only",
            &[(1_699_900_000, Some(80.0), "fp")],
            1_700_000_000,
            1_700_100_000,
        ),
        // 跨窗口起点:基线在起点前 1 小时,窗口首条在起点后 10 分钟
        (
            "crossing",
            &[
                (1_699_996_400, Some(100.0), "fp"),
                (1_700_000_600, Some(70.0), "fp"),
                (1_700_001_200, Some(65.0), "fp"),
            ],
            1_700_000_000,
            1_700_100_000,
        ),
        // 充值(余额上升)按 0 计,不上负消耗
        (
            "recharge",
            &[
                (1_700_000_000, Some(100.0), "fp"),
                (1_700_000_900, Some(90.0), "fp"),
                (1_700_001_800, Some(500.0), "fp"),
                (1_700_002_700, Some(480.0), "fp"),
            ],
            1_700_000_000,
            1_700_100_000,
        ),
        // 2 月 28/29(2024 闰年):本地日桶按 +8 时区切
        (
            "leap",
            &[
                (1_709_107_200, Some(200.0), "fp"), // 2024-02-28 00:00 UTC
                (1_709_193_600, Some(190.0), "fp"), // 2024-02-29 00:00 UTC
                (1_709_280_000, Some(180.0), "fp"), // 2024-03-01 00:00 UTC
            ],
            1_709_100_000,
            1_709_300_000,
        ),
        // 3 月 31 日 + 23:59:59
        (
            "month-end",
            &[
                (1_711_843_199, Some(90.0), "fp"), // 2024-03-31 23:59:59 UTC
                (1_711_843_200, Some(80.0), "fp"), // 2024-04-01 00:00:00 UTC
            ],
            1_711_800_000,
            1_711_900_000,
        ),
        // 重复时间戳:排序必须稳定(ts ASC, id ASC)
        (
            "ties",
            &[
                (1_700_000_100, Some(100.0), "fp"),
                (1_700_000_100, Some(90.0), "fp"),
                (1_700_000_100, Some(80.0), "fp"),
                (1_700_000_200, Some(70.0), "fp"),
            ],
            1_700_000_000,
            1_700_100_000,
        ),
        // 无金额的观测:覆盖/缺口要算,消耗不能算
        (
            "null-remaining",
            &[
                (1_700_000_000, None, "fp"),
                (1_700_000_300, Some(50.0), "fp"),
                (1_700_000_600, None, "fp"),
                (1_700_000_900, Some(40.0), "fp"),
            ],
            1_700_000_000,
            1_700_100_000,
        ),
        // 两个指纹交错:分段查询不能串账
        (
            "two-accounts",
            &[
                (1_700_000_000, Some(100.0), "fp"),
                (1_700_000_100, Some(10.0), "other"),
                (1_700_000_200, Some(95.0), "fp"),
                (1_700_000_300, Some(9.0), "other"),
            ],
            1_700_000_000,
            1_700_100_000,
        ),
    ];

    for (provider, rows, from, to) in cases {
        seed(&s, provider, rows);
        assert_rows_match_legacy(&s, provider, "fp", *from, *to);
    }
}

/// 随机夹具(固定种子):空洞、充值、重复时间戳、两本账交错。
/// 200 组落在同一个内存库上,一次跑完;不引入 rand 依赖。
#[test]
fn history_rows_match_legacy_on_random_fixtures() {
    let s = mem_migrated();
    // 简单的 LCG:固定种子 → 每次运行同一组数据,失败可复现。
    let mut state: u64 = 0x5EED_2026_1027;
    let mut next = || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        state >> 33
    };

    let base = 1_700_000_000i64;
    for g in 0..200 {
        let provider = format!("rand-{g}");
        let mut ts = base + (next() % 86_400) as i64;
        let mut rows: Vec<(i64, Option<f64>, &str)> = Vec::new();
        let mut bal = 1_000.0f64;
        for _ in 0..40 {
            // 0 步 = 重复时间戳;最多 6 小时 = 稀疏观测
            ts += (next() % (6 * 3600)) as i64 * if next() % 4 == 0 { 1 } else { 0 };
            let fp = if next() % 2 == 0 { "A" } else { "B" };
            // 15% 的空金额行;10% 是充值(余额跳升)
            let remaining = if next() % 100 < 15 {
                None
            } else {
                if next() % 10 == 0 {
                    bal += 300.0;
                } else {
                    bal = (bal - (next() % 100) as f64).max(0.0);
                }
                Some((bal * 100.0).round() / 100.0)
            };
            rows.push((ts, remaining, fp));
        }
        seed(&s, &provider, &rows);

        let from = base + (next() % 3_600) as i64;
        let to = ts + 1;
        assert_rows_match_legacy(&s, &provider, "A", from, to);
        assert_rows_match_legacy(&s, &provider, "B", from, to);
    }
}

/// 批量读取的超限保护:多读一行用来判断"是否超过 50 万行"。
#[test]
fn history_query_reads_one_extra_row_to_detect_overflow() {
    assert_eq!(HISTORY_QUERY_LIMIT, MAX_HISTORY_ROWS + 1);
}

// ───────────── P1b:覆盖索引 ─────────────

fn explain(s: &Store) -> Vec<String> {
    let mut stmt = s
        .conn
        .prepare(
            "EXPLAIN QUERY PLAN
                 SELECT ts, remaining FROM snapshots
                  WHERE provider_id = ?1 AND key_fp = ?2
                    AND ts >= ?3 AND ts <= ?4
                  ORDER BY ts ASC, remaining ASC, id ASC
                  LIMIT ?5",
        )
        .unwrap();
    stmt.query_map(params!["p", "fp", 0i64, 10i64, 100i64], |r| r.get(3))
        .unwrap()
        .filter_map(Result::ok)
        .collect()
}

#[test]
fn history_query_uses_the_covering_index() {
    let s = mem_migrated();
    let plan = explain(&s);
    assert!(
        plan.iter()
            .any(|p| p.contains("COVERING INDEX") && p.contains("idx_snap_fp_cover")),
        "批量历史查询必须走覆盖索引:{plan:?}"
    );
}

/// 老库(只有 snapshots,没有 key_fp 列)升级:先补列、再建索引,顺序不能反。
#[test]
fn covering_index_is_built_after_key_fp_column_on_old_db() {
    let s = mem();
    s.migrate().unwrap();
    let n: i64 = s
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
                  WHERE type='index' AND name='idx_snap_fp_cover'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "升级后覆盖索引必须存在");
    let v: i64 = s
        .conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 7, "迁移版本号要推进,否则每次启动都重跑补列");
}

/// 手动性能基准(不进常规测试):
///
/// ```text
/// set TOKENSCOPE_BENCH_DB=<线上库的拷贝>
/// cargo test --release bench_history_batch -- --ignored --nocapture
/// ```
///
/// 对比「旧路径 13 次重叠查询」与「1 次批量读取 + 内存切窗口」。窗口用近似的
/// 相对时间,只比量级,不参与正确性判定(正确性由差分测试守)。
#[test]
#[ignore = "manual benchmark: set TOKENSCOPE_BENCH_DB to a COPY of tokenscope.db"]
fn bench_history_batch() {
    let Ok(path) = std::env::var("TOKENSCOPE_BENCH_DB") else {
        eprintln!("跳过:没有设置 TOKENSCOPE_BENCH_DB");
        return;
    };
    let s = Store::open(Path::new(&path)).expect("打开基准库失败(请用拷贝)");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // 取行数最多的几个真实渠道/指纹组合
    let mut stmt = s
        .conn
        .prepare(
            "SELECT provider_id, key_fp, COUNT(*) AS n FROM snapshots
                  WHERE remaining IS NOT NULL AND ts >= ?1
                  GROUP BY provider_id, key_fp ORDER BY n DESC LIMIT 4",
        )
        .unwrap();
    let targets: Vec<(String, String, i64)> = stmt
        .query_map(params![now - 92 * 86_400], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect();

    let rounds = 10;
    for (provider, fp, rows) in targets {
        let history_from = now - 92 * 86_400;

        let t0 = std::time::Instant::now();
        for _ in 0..rounds {
            let h = s
                .history_between(&provider, Some(&fp), history_from, now)
                .unwrap();
            let _ = consumption_since_rows(&h, now - 86_400, now);
            let _ = consumption_since_rows(&h, now - 7 * 86_400, now);
            let _ = consumption_since_rows(&h, now - 30 * 86_400, now);
            let _ = consumption_since_rows(&h, now - 2 * 86_400, now - 86_400);
            let _ = consumption_since_rows(&h, now - 8 * 86_400, now - 7 * 86_400);
            let _ = consumption_since_rows(&h, history_from, now);
            let _ = window_coverage_rows(&h, now - 86_400, now);
            let _ = window_coverage_rows(&h, now - 7 * 86_400, now);
            let _ = window_coverage_rows(&h, now - 30 * 86_400, now);
            let _ = daily_coverage_rows(&h, history_from, now, 8 * 3600);
            let _ = last_jump_rows(&h, now - 90 * 86_400, 20.0, 200.0, 7200);
            let _ = first_ts_rows(&h, now - 7 * 86_400);
        }
        let batch = t0.elapsed().as_secs_f64() * 1000.0 / rounds as f64;

        let t1 = std::time::Instant::now();
        for _ in 0..rounds {
            for from in [
                now - 86_400,
                now - 7 * 86_400,
                now - 30 * 86_400,
                now - 2 * 86_400,
                now - 8 * 86_400,
                history_from,
            ] {
                let _ = s.consumption_since_fp(&provider, &fp, from, now);
            }
            let _ = s.window_coverage(&provider, Some(&fp), now - 86_400, now);
            let _ = s.window_coverage(&provider, Some(&fp), now - 7 * 86_400, now);
            let _ = s.window_coverage(&provider, Some(&fp), now - 30 * 86_400, now);
            let _ = s.daily_coverage(&provider, Some(&fp), history_from, now, 8 * 3600);
            let _ = s.last_jump_since_fp(&provider, &fp, now - 90 * 86_400, 20.0, 200.0, 7200);
            let _ = s.first_ts_since_fp(&provider, &fp, now - 7 * 86_400);
        }
        let legacy = t1.elapsed().as_secs_f64() * 1000.0 / rounds as f64;
        println!(
            "{provider}/{fp}: rows≈{rows}  批量 {batch:.1} ms  旧路径 {legacy:.1} ms  加速 {:.1}x",
            legacy / batch.max(0.001)
        );
    }
}

/// 按天聚合:一天多条(节拍器重试)时,结论取当天**最后一行**,条数如实报。
/// 签到中心的日历带靠它;与 `checkin_of_day` 判"今天处理过了吗"必须同一口径。
#[test]
fn checkin_day_rows_take_last_row_of_each_day() {
    let s = mem_migrated();
    let rec = |ts: i64, day: &str, outcome: &str, credits: Option<f64>| {
        s.record_checkin("trae", ts, day, outcome, credits, 1, "").unwrap();
    };
    // 10-27 三条:两次重试失败,最后一次成功
    rec(1_794_796_800, "2026-10-27", "retry:ready", None);
    rec(1_794_800_400, "2026-10-27", "retry:device", None);
    rec(1_794_804_000, "2026-10-27", "claimed:trae", Some(100.0));
    // 10-28 一条:平台侧已签
    rec(1_794_890_400, "2026-10-28", "already", None);
    // 10-26 的不进窗口
    rec(1_794_710_400, "2026-10-26", "failed:boom", None);

    let rows = s.checkin_day_rows("trae", "2026-10-27").unwrap();
    assert_eq!(rows.len(), 2, "窗口外(10-26)不进,10-27/28 各一条: {rows:?}");
    assert_eq!(rows[0].day, "2026-10-28", "新的在前");
    assert_eq!(rows[0].outcome, "already");
    assert_eq!(rows[0].count, 1);
    assert_eq!(rows[1].day, "2026-10-27");
    assert_eq!(rows[1].outcome, "claimed:trae", "当天结论以最后一行为准");
    assert_eq!(rows[1].credits, Some(100.0));
    assert_eq!(rows[1].count, 3, "当天尝试次数如实报");
    assert_eq!(rows[1].ts, 1_794_804_000);

    // 渠道隔离:别的渠道一条都不给
    assert!(s.checkin_day_rows("qoder", "2026-10-26").unwrap().is_empty());
    // first_checkin_ts:最早一条,给"该签了"的起点兜底
    assert_eq!(s.first_checkin_ts("trae").unwrap(), Some(1_794_710_400));
    assert_eq!(s.first_checkin_ts("none").unwrap(), None);
}

/// 明细查询的 limit 是硬闸:签到中心要 90 天 × 多次重试,400 撑不住,
/// 但也不能一次捞几万条把界面拖死 —— limit 参数由调用方定。
#[test]
fn recent_checkins_respects_limit() {
    let s = mem_migrated();
    for i in 0..10 {
        s.record_checkin(
            "trae",
            1_794_796_800 + i,
            "2026-10-27",
            "retry:ready",
            None,
            i,
            "",
        )
        .unwrap();
    }
    assert_eq!(s.recent_checkins("trae", 0, 4).unwrap().len(), 4);
    assert_eq!(s.recent_checkins("trae", 0, 100).unwrap().len(), 10);
    // 新的在前
    assert_eq!(s.recent_checkins("trae", 0, 1).unwrap()[0].attempts, 9);
}

#[test]
fn wal_checkpoint_truncates_wal_and_passes_integrity_check() {
    let dir = std::env::temp_dir().join(format!("gt-b6a-wal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("wal_test.db");
    let wal_path = dir.join("wal_test.db-wal");

    let s = Store::open(&db_path).unwrap();
    for i in 0..100i64 {
        s.record_fp("codex", "fp1", 1_795_000_000 + i * 60, Some(100.0 - i as f64 * 0.5), None, Some(100.0), "balance")
            .unwrap();
        s.set_setting(&format!("k_{i}"), "val_0123456789_abcdef").unwrap();
    }
    let wal_before = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    assert!(wal_before > 0, "写入 200 条记录后 -wal 应非空: {wal_before}");

    let (busy, log, checkpointed) = s.wal_checkpoint().unwrap();
    assert_eq!(busy, 0, "独占连接 checkpoint 不应被占用阻塞");
    assert_eq!(log, checkpointed, "应将 WAL 中的全部帧刷回主库");

    let wal_after = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    assert_eq!(wal_after, 0, "TRUNCATE checkpoint 后 -wal 应截断为 0 字节");
    assert_eq!(s.integrity_check().unwrap(), "ok");
    assert_eq!(s.get_setting("k_99").unwrap().as_deref(), Some("val_0123456789_abcdef"));

    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}
