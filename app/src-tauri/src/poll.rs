use super::*;

/// 后台轮询。渠道都是账务接口,查询余额不消耗 token,所以间隔可以压得比较短;
/// 唯一约束是对方的限流礼貌性,所以失败时退避。
pub(crate) async fn poll_loop(app: tauri::AppHandle) {    let mut failures = 0u32;
    // 90 天清理原来只在启动时做一次:这台机器常年不重启的话快照表就一直长(报告 O-2)
    let mut last_prune = now_ts();

    loop {
        if UPDATING.load(std::sync::atomic::Ordering::Relaxed) {
            // 更新期间不取数:进程马上可能被 install 结束,拿回来的数也没意义
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }
        let (active_sec, idle_sec, backoff_sec) = {
            let state = app.state::<AppState>();
            let c = lock_rw(&state.config);
            (
                c.active_interval_sec,
                c.idle_interval_sec,
                c.backoff_interval_sec,
            )
        };

        let visible = app
            .get_webview_window("main")
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false);

        let base = if visible { active_sec } else { idle_sec };
        let wait = if failures >= 3 { backoff_sec } else { base };

        // 抖动,避免多渠道同秒并发撞上对方限流
        let jitter = (now_ts() % 5) as u64;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait + jitter);
        // 睡到「本轮结束」与「下一次失败重试到期」里更早的那个。
        //
        // 原来是一秒一醒地自旋等 deadline:60/300/900 秒的间隔就白醒 60/300/900 次
        // (约 86,400 次/天),每次都要锁 state、克隆 Arc、遍历 recovery 表。
        // 这里只改**唤醒次数**,不改任何触发条件:该不该重试仍由 fetch_retries 里的
        // `r.next <= now_ts()` 判定;已经到期时最小睡 1 秒,与原来 1 秒的检查粒度一致。
        loop {
            if tokio::time::Instant::now() >= deadline { break; }
            if UPDATING.load(std::sync::atomic::Ordering::Relaxed) { break; }
            let (store, fetcher) = {
                let state = app.state::<AppState>();
                (state.store.clone(), state.fetcher.clone())
            };
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            // .max(1):next 在过去时也至少睡 1 秒,避免"到期 → 重算 → 又到期"的空转
            let until_retry = fetcher
                .next_retry_at()
                .map(|at| std::time::Duration::from_secs((at - now_ts()).max(1) as u64));
            let sleep_for = match until_retry {
                Some(d) if d < remaining => d,
                _ => remaining,
            };
            if sleep_for.is_zero() { break; }
            tokio::select! {
                _ = tokio::time::sleep(sleep_for) => {}
                // 开始更新:立刻停下,不等这一觉睡醒
                _ = UPDATING_NOTIFY.notified() => {}
            }
            if UPDATING.load(std::sync::atomic::Ordering::Relaxed) { break; }
            if let Some(views) = fetcher.fetch_retries(&store).await {
                emit_channels(&app, &views);
                maybe_notify_crit(&app, &views);
                settle_arrivals(&app).await;
            }
        }
        if UPDATING.load(std::sync::atomic::Ordering::Relaxed) { continue; }

        // 先把 Arc 取出来,避免 State 借用跨越 await
        let (store, fetcher) = {
            let state = app.state::<AppState>();
            (state.store.clone(), state.fetcher.clone())
        };

        if now_ts() - last_prune >= 86_400 {
            last_prune = now_ts();
            let s = lock_rw(&store);
            if let Err(e) = s.prune(now_ts() - 90 * 86_400, tokenscope_core::fetch::local_tz_offset()) {
                eprintln!("定期清理快照失败: {e}");
            }
            if let Err(e) = s.wal_checkpoint() {
                eprintln!("定期 WAL checkpoint 失败: {e}");
            }
        }

        // 走 fetch_shared:用户点「立即刷新」撞上轮询时排队等结果,
        // 而不是对同一批账务接口发出双份请求(报告 O-4)
        let views = fetcher.fetch_shared(&store).await;

        if views.iter().any(|v| v.valid) {
            failures = 0;
        } else if views.iter().any(|v| v.has_key) {
            failures += 1;
        }
        // 一个渠道都没配凭据时不算失败:那是全新安装,退避到 5 分钟只会让
        // 用户「填好 key 却半天看不到数字」

        LAST_REFRESH.store(now_ts(), std::sync::atomic::Ordering::Relaxed);
        emit_channels(&app, &views);
        // 跌破红线的系统通知(裁决 3A):只在跌破那一刻、一天一次、按渠道记账
        maybe_notify_crit(&app, &views);
        // 快照已落库:顺手核一轮待落定的到账核对单(零网络请求,读库比增量)
        settle_arrivals(&app).await;
        // 同步发布:到点才后台跑,不阻塞取数循环
        sync_cmds::maybe_publish(&app, &views);
    }
}

/// 上次取数的时刻(秒)。托盘/热键把窗口唤回来时用它判断要不要补一轮:
/// 轮询循环只在**进入下一轮时**读一次窗口可见性,然后睡满整个间隔(折叠态最长 30 分钟),
/// 所以"藏了两小时再打开"看到的仍是两小时前的数字(报告 P3-37)。
pub(crate) static LAST_REFRESH: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// 窗口重新显示时调用:数据比 max_age_sec 旧就补一轮。新鲜结果不重复打接口
/// (fetch_shared 自己也会与在途的轮询合流,不会发出双份请求)。
pub(crate) fn refresh_if_stale(app: &tauri::AppHandle, max_age_sec: i64) {
    let last = LAST_REFRESH.load(std::sync::atomic::Ordering::Relaxed);
    if last != 0 && now_ts() - last < max_age_sec {
        return;
    }
    let h = app.clone();
    tauri::async_runtime::spawn(async move {
        refresh_now(&h).await;
    });
}

/// 退出:正在写客户端配置时先等它写完(最多 10 秒)再退。
/// 立即退出会停在"文件已改、台账还没落"的那一瞬间,重启后没有任何记录能回滚它(P1-15)。
/// 交给后台任务等,按钮立刻有反应,不会让人以为点了没动。
fn checkpoint_wal(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        let s = lock_rw(&state.store);
        if let Err(e) = s.wal_checkpoint() {
            eprintln!("退出前 WAL checkpoint 失败: {e}");
        }
    }
}

pub(crate) fn quit_when_idle(app: &tauri::AppHandle) {
    if INJECTING.load(std::sync::atomic::Ordering::Relaxed) {
        let h = app.clone();
        tauri::async_runtime::spawn(async move {
            for _ in 0..50 {
                if !INJECTING.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            checkpoint_wal(&h);
            h.exit(0);
        });
    } else {
        checkpoint_wal(app);
        app.exit(0);
    }
}

pub(crate) async fn refresh_now(app: &tauri::AppHandle) {
    let (store, fetcher) = {
        let state = app.state::<AppState>();
        (state.store.clone(), state.fetcher.clone())
    };
    fetcher.request_refresh();
    let views = fetcher.fetch_shared(&store).await;
    LAST_REFRESH.store(now_ts(), std::sync::atomic::Ordering::Relaxed);
    emit_channels(app, &views);
    // 快照已落库:顺手核一轮待落定的到账核对单(零网络请求,读库比增量)
    settle_arrivals(app).await;
}

