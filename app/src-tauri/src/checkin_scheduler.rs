use super::*;

/// 拿不到凭据时也要**留一条流水**。
///
/// 以前这条路径只 `eprintln` 一句就返回,于是"今天一次都没试"在 `checkin_log` 里
/// 是一片空白 —— 与"还没到点"完全无法区分。2026-10-20 排查 WorkBuddy 时就撞上了:
/// 10-16 / 10-17 两天没有任何记录,只能靠推断才知道那两天是凭据读不到。
///
/// 结果码用 `no_cred` 前缀(不是 `auth`):**"凭据读不到"与"平台说凭据失效"是两件事** ——
/// 前者是本机没装/没登录/解不开,后者是服务端拒绝了这份凭据。混成一个会让用户
/// 照着错误的提示去操作。
///
/// 这条**不算当天完结**(`done_for_day` 不认 `no_cred`),所以白天补装/登录后
/// 节拍器还会再试 —— 与 `retry` 同口径。
pub(crate) async fn record_checkin_skip(app: &tauri::AppHandle, id: &str, day: &str, now: i64, why: &str) {
    let state = app.state::<AppState>();
    // 退避计数从 live 配置起算;以前是整份克隆后自算、落库后再把陈旧克隆**整份写回内存**
    // —— 用户在落库窗口里改的任何设置都会被倒回旧值(报告 B4,P3-35 的残余形态)。
    let attempts = {
        let c = lock_rw(&state.config);
        let prev = c
            .checkin
            .get(id)
            .map(|x| if x.last_day.as_deref() == Some(day) { x.attempts } else { 0 })
            .unwrap_or(0);
        prev + 1
    };
    {
        // 块级作用域:guard 在 await 前彻底出借结束(显式 drop 在生成器分析里仍可能
        // 被视作"跨 await 存活",块边界不会)
        let store = lock_rw(&state.store);
        if let Err(e) =
            store.record_checkin(id, now, day, &format!("no_cred:{why}"), None, attempts, why)
        {
            eprintln!("{e}");
        }
    }
    // 配置走串行闸,只合并本渠道的 entry;配置损坏时不动配置(P1-16),流水已留下
    if refuse_if_config_broken().is_ok() {
        if let Err(e) = save_config_off_main(&state, |c| {
            let entry = c.checkin.entry(id.to_string()).or_default();
            let same_day = entry.last_day.as_deref() == Some(day);
            entry.attempts = if same_day { entry.attempts + 1 } else { 1 };
            entry.last_day = Some(day.to_string());
            entry.last_code = Some(format!("no_cred:{why}"));
            entry.last_try_at = Some(now);
            entry.last_message = Some(why.to_string());
            // 排一次退避:凭据问题不会在 2 分钟内自愈,别每 5 分钟硬撞一次
            entry.next_try_at = Some(now + checkin::next_delay(entry.attempts));
        })
        .await
        {
            eprintln!("签到配置落库失败: {e}");
        }
    }
    let _ = app.emit("checkin-updated", serde_json::json!({ "id": id }));
}

/// 每日节拍器:每 5 分钟看一次"今天该不该签"。
/// **不塞进 poll_loop 的 60 秒里** —— 签到是写操作,节奏要跟取数分开(设计稿第 8 节)。
///
/// **先跑一轮再睡**(2026-10-20 修):以前 `sleep(300)` 在循环最前面,启动后要干等
/// 整整 5 分钟才判第一次。而界面在这段时间挂的是**上次运行留下的结论** ——
/// 真机现场:应用 23:38 启动,23:43 才重试成功;中间那 5 分钟卡片一直显示
/// 早已过期的"凭据失效",用户被「去登录」引到死胡同(网页登录还被网关拒)。
pub(crate) fn spawn_checkin_ticker(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            checkin_tick_once(&app).await;
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        }
    });
}

/// 走一轮:对每个"今天还没处理完"的渠道尝试签到。
pub(crate) async fn checkin_tick_once(app: &tauri::AppHandle) {
    if UPDATING.load(std::sync::atomic::Ordering::Relaxed) {
        return; // 更新期间不签到(报告 P1-15)
    }
    for def in checkin::CHECKINS {
        if def.auth == checkin::CheckinAuth::QoderApp {
            monitor_qoder_activities(app, def.id).await;
        } else {
            monitor_standard_activity(app, def.id).await;
        }
    }
    let (cfg, now) = {
        let state = app.state::<AppState>();
        let cfg = lock_rw(&state.config).clone();
        (cfg, now_ts())
    };
    for def in checkin::CHECKINS {
        let Some(c) = cfg.checkin.get(def.id) else { continue };
        if !c.enabled {
            continue; // 总开关关闭 = 不领取也不监测
        }
        if cfg.hidden_channels.iter().any(|h| h == def.id) {
            continue; // 隐藏 = 不读本机登录凭据、也不对外发请求(与取数同口径)
        }
        if !c.auto {
            continue; // 自动领取关闭;各渠道的只读活动监测已在上方独立执行
        }
        // **逐渠道算周期日**:Qoder 的"今天"从 10:00 起算,不是自然日。
        // 用自然日会让 09:59 与 10:01 混成一天,当天那一期永远领不到。
        let today = checkin::cycle_day(now, def.day_roll_hour);
        let activity_ready = activity_ready_today(c, &today);
        if checkin_blocks_today(c, &today) && !activity_ready {
            continue;
        }
        if c.next_try_at.map(|t| t > now).unwrap_or(false) {
            continue; // 退避中
        }
        // 跨日时刻 + 该渠道抖动之后才允许(当天更晚也算 → 开机补跑)
        if !checkin::due_now(now, &today, def.id, def.day_roll_hour) {
            continue;
        }
        if let Err(e) = run_checkin(app, def.id, false).await {
            eprintln!("签到 {} 跳过: {e}", def.name);
            // 与手动入口(checkin_now)同一条过滤:互斥忙 / 开关类「本来就不该跑」的
            // 拒绝不记流水也不推进退避 —— 否则手动签到进行中时节拍器会拿到
            // 「正在签到」,把 attempts 双计、流水里留一行假"没签成"(报告 L10)。
            let gate_like = ["已隐藏", "开关没开", "正在签到", "未知签到渠道"]
                .iter()
                .any(|k| e.contains(k));
            if !gate_like {
                // 拿不到凭据:记一句就走,别每 5 分钟刷屏。
                // **但也要落一条流水** —— 以前这里只 eprintln,绿色版从托盘启动时
                // 那行没人看得到,于是"没试"与"试了失败"在数据里无法区分
                // (2026-10-20 排查:workbuddy 10-16/10-17 两天在 checkin_log 里是空白)。
                record_checkin_skip(app, def.id, &today, now, &e).await;
            }
        }
        // 两渠道之间串行 + 间隔(设计稿第 8 节)
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    }
}
