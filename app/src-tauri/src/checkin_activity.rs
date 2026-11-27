use super::*;

/// 记录 Qoder 活动变化,并把活动与签到结果分开持久化。
/// 返回值表示本次新发现了可由挂件领取的活动。
pub(crate) async fn record_qoder_activities(
    app: &tauri::AppHandle,
    id: &str,
    body: &serde_json::Value,
    now: i64,
) -> bool {
    let Some(mut activities) = checkin::qoder_activities(body) else {
        let period = cycle_day_of(id, now);
        record_channel_activity(
            app,
            id,
            checkin::ActivityState {
                key: format!("{}:{}:account", id, period),
                title: "Qoder 活动状态".into(),
                status: "unknown".into(),
                scope: "account".into(),
                region: if id == "qoder-cn" { "CN" } else { "Global" }.into(),
                period,
                reward: None,
                observed_at: Some(now),
                detail: Some("平台返回了无法识别的活动结构".into()),
            },
            false,
        )
        .await;
        return false;
    };
    let def = checkin::find(id);
    let name = def.map(|d| d.name).unwrap_or(id);
    let mut newly_claimable = false;
    let mut notice: Option<checkin::QoderActivity> = None;
    for activity in &mut activities {
        activity.observed_at = Some(now);
    }

    let state = app.state::<AppState>();
    let save_result = save_config_off_main(&state, |live| {
        let entry = live.checkin.entry(id.to_string()).or_default();
        entry.last_activity_check_at = Some(now);

        for activity in &activities {
            let previous = entry
                .activity_history
                .iter()
                .find(|known| known.id == activity.id);
            let is_new = previous.is_none();
            let is_claimable = checkin::qoder_action_claimable(
                &activity.action_type,
                &activity.claim_status,
            );
            let was_claimable = previous
                .map(|known| {
                    checkin::qoder_action_claimable(&known.action_type, &known.claim_status)
                })
                .unwrap_or(false);
            if is_claimable && !was_claimable {
                newly_claimable = true;
            }
            if is_new || (is_claimable && !was_claimable) {
                let prefer_this = notice
                    .as_ref()
                    .map(|known| {
                        checkin::qoder_action_claimable(&activity.action_type, &activity.claim_status)
                            && !checkin::qoder_action_claimable(
                                &known.action_type,
                                &known.claim_status,
                            )
                    })
                    .unwrap_or(true);
                if prefer_this {
                    notice = Some(activity.clone());
                }
            }

            if let Some(known) = entry
                .activity_history
                .iter_mut()
                .find(|known| known.id == activity.id)
            {
                *known = activity.clone();
            } else {
                entry.activity_history.push(activity.clone());
            }
        }
        entry
            .activity_history
            .sort_by_key(|activity| activity.observed_at.unwrap_or_default());
        if entry.activity_history.len() > 100 {
            let excess = entry.activity_history.len() - 100;
            entry.activity_history.drain(0..excess);
        }
        entry.latest_activity = activities
            .iter()
            .find(|activity| {
                checkin::qoder_action_claimable(&activity.action_type, &activity.claim_status)
            })
            .or_else(|| activities.first())
            .cloned();
        let period = cycle_day_of(id, now);
        entry.activity_state = Some(if let Some(activity) = entry.latest_activity.as_ref() {
            let status = if checkin::qoder_action_claimable(
                &activity.action_type,
                &activity.claim_status,
            ) {
                "claimable"
            } else if activity.claim_status == "CLAIMED" {
                "claimed"
            } else if activity.claim_status == "CLAIMABLE" {
                "manual"
            } else if matches!(activity.claim_status.as_str(), "EXPIRED" | "ENDED") {
                "inactive"
            } else {
                "unknown"
            };
            checkin::ActivityState {
                key: format!("{}:{}", id, activity.id),
                title: activity.title.clone(),
                status: status.to_string(),
                scope: "account".into(),
                region: if id == "qoder-cn" { "CN" } else { "Global" }.into(),
                period,
                reward: None,
                observed_at: Some(now),
                detail: Some(format!("{} · {}", activity.action_type, activity.claim_status)),
            }
        } else {
            checkin::ActivityState {
                key: format!("{}:{}:account", id, period),
                title: "本期暂无可参与活动".into(),
                status: "inactive".into(),
                scope: "account".into(),
                region: if id == "qoder-cn" { "CN" } else { "Global" }.into(),
                period,
                reward: None,
                observed_at: Some(now),
                detail: None,
            }
        });
    })
    .await;
    if let Err(e) = save_result {
        eprintln!("Qoder 活动状态落库失败: {e}");
    }
    let _ = app.emit("checkin-updated", serde_json::json!({ "id": id }));

    if let Some(activity) = notice {
        let can_claim = checkin::qoder_action_claimable(&activity.action_type, &activity.claim_status);
        let auto = {
            let state = app.state::<AppState>();
            let cfg = lock_rw(&state.config);
            cfg.checkin.get(id).map(|c| c.enabled && c.auto).unwrap_or(false)
        };
        // 自动模式下，成功回执会作为通知；不再先报“发现可领”再报“已领取”。
        if !can_claim || !auto {
            let _ = app.emit(
                "checkin-activity",
                serde_json::json!({
                    "id": id,
                    "name": name,
                    "title": activity.title,
                    "actionType": activity.action_type,
                    "claimStatus": activity.claim_status,
                    "canClaim": can_claim,
                }),
            );
        }
    }
    newly_claimable
}

/// 记录一次活动监测尝试,凭据暂不可用时也按一小时节奏重试。
pub(crate) async fn mark_qoder_activity_check(app: &tauri::AppHandle, id: &str, now: i64) {
    let state = app.state::<AppState>();
    if let Err(e) = save_config_off_main(&state, |live| {
        live.checkin
            .entry(id.to_string())
            .or_default()
            .last_activity_check_at = Some(now);
    })
    .await
    {
        eprintln!("Qoder 活动监测时间落库失败: {e}");
    }
}

pub(crate) const QODER_ACTIVITY_INTERVAL_SEC: i64 = 60 * 60;

pub(crate) fn activity_state_from_outcome(
    id: &str,
    outcome: &checkin::Outcome,
    body: Option<&serde_json::Value>,
    now: i64,
) -> checkin::ActivityState {
    let period = cycle_day_of(id, now);
    let scope = if id == "trae" { "device" } else { "account" };
    let trae_device_used = id == "trae"
        && body
            .and_then(|v| v.get("code"))
            .and_then(|v| v.as_i64())
            == Some(9095);
    let status = if trae_device_used {
        "device_used"
    } else {
        match outcome {
            checkin::Outcome::Claimed(_) | checkin::Outcome::Already(_) => "claimed",
            checkin::Outcome::Inactive(_) => "inactive",
            checkin::Outcome::Manual(_) => "manual",
            checkin::Outcome::Retry(reason) if reason == "ready" || reason.starts_with("ready:") => {
                "claimable"
            }
            _ => "unknown",
        }
    };
    let region = match id {
        "trae" | "workbuddy" | "qoder-cn" => "CN",
        "qoder" => "Global",
        _ => "Unknown",
    };
    let detail = match status {
        "unknown" => Some(outcome.verdict().to_string()),
        "device_used" => Some("Trae 每台设备每日共用一次名额".into()),
        _ => None,
    };
    checkin::ActivityState {
        key: format!("{}:{}:{}", id, period, scope),
        title: if id == "trae" {
            "每日设备签到".into()
        } else {
            "每日签到活动".into()
        },
        status: status.into(),
        scope: scope.into(),
        region: region.into(),
        period,
        reward: outcome.credits(),
        observed_at: Some(now),
        detail,
    }
}

/// 保存非 Qoder 渠道的统一活动状态，并只在活动首次出现/状态变化时提醒。
pub(crate) async fn record_channel_activity(
    app: &tauri::AppHandle,
    id: &str,
    activity: checkin::ActivityState,
    announce: bool,
) {
    let def = checkin::find(id);
    let mut should_notify = false;
    let notice_key = format!("{}:{}", activity.key, activity.status);
    let state = app.state::<AppState>();
    let save_result = save_config_off_main(&state, |live| {
        let entry = live.checkin.entry(id.to_string()).or_default();
        entry.last_activity_check_at = activity.observed_at;
        if matches!(activity.status.as_str(), "claimable" | "manual")
            && entry.last_activity_notice_key.as_deref() != Some(notice_key.as_str())
        {
            should_notify = true;
            entry.last_activity_notice_key = Some(notice_key.clone());
        }
        entry.activity_state = Some(activity.clone());
    })
    .await;
    if let Err(e) = save_result {
        eprintln!("{} 活动状态落库失败: {e}", def.map(|d| d.name).unwrap_or(id));
    }
    let _ = app.emit("checkin-updated", serde_json::json!({ "id": id }));

    if !announce || !should_notify {
        return;
    }
    let auto = {
        let state = app.state::<AppState>();
        let enabled = lock_rw(&state.config)
            .checkin
            .get(id)
            .map(|c| c.enabled && c.auto)
            .unwrap_or(false);
        enabled
    };
    // 自动领取的结果事件负责通知；避免同一活动先报发现、再报领到。
    if activity.status == "claimable" && auto {
        return;
    }
    let _ = app.emit(
        "checkin-activity",
        serde_json::json!({
            "id": id,
            "name": def.map(|d| d.name).unwrap_or(id),
            "title": activity.title,
            "status": activity.status,
            "scope": activity.scope,
            "region": activity.region,
            "canClaim": activity.status == "claimable",
        }),
    );
}

/// Trae / WorkBuddy 每小时只读查询活动状态；自动领取是独立动作。
pub(crate) async fn monitor_standard_activity(app: &tauri::AppHandle, id: &str) {
    if UPDATING.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let now = now_ts();
    let (enabled, hidden, last_check) = {
        let state = app.state::<AppState>();
        let cfg = lock_rw(&state.config);
        let c = cfg.checkin.get(id).cloned().unwrap_or_default();
        (
            c.enabled,
            cfg.hidden_channels.iter().any(|h| h == id),
            c.last_activity_check_at,
        )
    };
    if !enabled
        || hidden
        || last_check
            .map(|t| now.saturating_sub(t) < QODER_ACTIVITY_INTERVAL_SEC)
            .unwrap_or(false)
    {
        return;
    }
    let Ok(busy) = CheckinBusy::acquire(id) else {
        return;
    };
    let Some(def) = checkin::find(id) else { return };
    let h = app.clone();
    let credential = tokio::task::spawn_blocking(move || checkin_credential(&h, def)).await;
    if matches!(credential, Ok(Err(CheckinError::Paused))) {
        return;
    }
    mark_qoder_activity_check(app, id, now).await;
    let cred = match credential {
        Ok(Ok(cred)) => cred,
        Ok(Err(_)) | Err(_) => {
            let state = activity_state_from_outcome(id, &checkin::Outcome::AuthNeeded, None, now);
            record_channel_activity(app, id, checkin::ActivityState {
                status: "unknown".into(),
                detail: Some("本机暂未读到登录凭据".into()),
                ..state
            }, false).await;
            return;
        }
    };
    let client = checkin::direct_client();
    let (out, body) = checkin::call_with_body(&client, def, &cred, false, None).await;
    let activity = activity_state_from_outcome(id, &out, body.as_ref(), now_ts());
    let can_auto_claim = out == checkin::Outcome::Retry("ready".into());
    record_channel_activity(app, id, activity, true).await;

    // 平台侧已签(通常是用户自己在客户端点的):把这个**平台给的既成事实**记进今天的账。
    // 以前监测只把结论写进展示字段,不写 lastCode —— 2026-10-25 真机:00:04 在客户端
    // 签了,00:09 监测问到了 already,doneToday 却一直是 false,首页一直挂「今天还没签」。
    // 自动领取关着的人受益最大:在客户端签完后,挂件一小时内自己纠正,不用点任何东西。
    if let checkin::Outcome::Already(cr) = &out {
        record_observed_already(app, id, def, now, *cr).await;
    }
    drop(busy);

    if can_auto_claim {
        let should_claim = {
            let state = app.state::<AppState>();
            let cfg = lock_rw(&state.config);
            cfg.checkin.get(id).map(|c| c.enabled && c.auto).unwrap_or(false)
                && !cfg.hidden_channels.iter().any(|h| h == id)
        };
        if should_claim {
            if let Err(e) = run_checkin(app, id, false).await {
                eprintln!("{} 新活动自动领取跳过: {e}", def.name);
            }
        }
    }
}

/// 活动监测查到「平台侧已签」时,把它记成今天的既成事实(见 `monitor_standard_activity`
/// 里调用点的注释)。两条保护:
///   ① **只在 `last_day` 不是今天时写** —— 今天已有结论(run_checkin 或上一轮监测
///      写过)就不覆盖,`claimed`(我们领的,带核对单)绝不能被改写成 `already`;
///   ② **只在 `due_now` 之后记** —— 与自动领取同一道「平台的新一天已经开始」信任线
///      (00:23+抖动)。平台 0 点后可能短暂残留昨天的 `checked_in`,太早信它会把
///      「昨天已签」记成「今天已签」→ 当天静默丢分(与 Qoder `day_roll_hour` 同一条教训)。
///
/// attempts / next_try_at / arrival 一概不动:这是**观察**,不是我们发起的尝试。
pub(crate) async fn record_observed_already(
    app: &tauri::AppHandle,
    id: &str,
    def: &checkin::CheckinDef,
    now: i64,
    credits: Option<f64>,
) {
    let today = cycle_day_of(id, now);
    if !checkin::due_now(now, &today, id, def.day_roll_hour) {
        return;
    }
    let mut wrote = false;
    let state = app.state::<AppState>();
    let save_result = save_config_off_main(&state, |live| {
        let entry = live.checkin.entry(id.to_string()).or_default();
        if entry.last_day.as_deref() == Some(today.as_str()) {
            return; // 今天已有结论,不覆盖
        }
        entry.last_day = Some(today.clone());
        entry.last_code = Some("already".into());
        entry.last_credits = credits;
        entry.last_try_at = Some(now);
        entry.last_message = None;
        wrote = true;
    })
    .await;
    if let Err(e) = save_result {
        eprintln!("{} 监测结论落库失败: {e}", def.name);
        return;
    }
    if !wrote {
        return;
    }
    tokenscope_core::diaglog::write_keyed(
        "checkin",
        id,
        &format!(
            "监测发现平台已签 · 记入今日(未代领)· 数额={}",
            credits
                .map(|c| format!("{c}"))
                .unwrap_or_else(|| "平台未返回".into())
        ),
    );
    // 流水也留一行:「平台侧显示已签到(活动监测)」—— 与 run_checkin 的 already 行
    // 区分开,排查时能看出这条不是我们发起的尝试。
    let attempts = {
        let state = app.state::<AppState>();
        let a = lock_rw(&state.config)
            .checkin
            .get(id)
            .map(|c| c.attempts)
            .unwrap_or(0);
        a
    };
    {
        let store = lock_rw(&state.store);
        if let Err(e) = store.record_checkin(
            id,
            now,
            &today,
            "already",
            credits,
            attempts,
            "平台侧显示已签到(活动监测)",
        ) {
            eprintln!("{e}");
        }
    }
    let _ = app.emit("checkin-updated", serde_json::json!({ "id": id }));
}

/// 独立于每日领取的活动监测。签到开关开启即可每小时检查一次,自动领取关闭时也照常发现活动。
pub(crate) async fn monitor_qoder_activities(app: &tauri::AppHandle, id: &str) {    if UPDATING.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let now = now_ts();
    let (enabled, hidden, last_check) = {
        let state = app.state::<AppState>();
        let cfg = lock_rw(&state.config);
        let c = cfg.checkin.get(id).cloned().unwrap_or_default();
        (
            c.enabled,
            cfg.hidden_channels.iter().any(|h| h == id),
            c.last_activity_check_at,
        )
    };
    if !enabled || hidden || last_check.map(|t| now.saturating_sub(t) < QODER_ACTIVITY_INTERVAL_SEC).unwrap_or(false) {
        return;
    }
    let Ok(_busy) = CheckinBusy::acquire(id) else {
        return;
    };
    mark_qoder_activity_check(app, id, now).await;

    let Some(def) = checkin::find(id) else { return };
    let h = app.clone();
    let cred = match tokio::task::spawn_blocking(move || checkin_credential(&h, def)).await {
        Ok(Ok(cred)) => cred,
        _ => {
            let activity = checkin::ActivityState {
                key: format!("{}:{}:account", id, cycle_day_of(id, now)),
                title: "Qoder 活动状态".into(),
                status: "unknown".into(),
                scope: "account".into(),
                region: if id == "qoder-cn" { "CN" } else { "Global" }.into(),
                period: cycle_day_of(id, now),
                reward: None,
                observed_at: Some(now),
                detail: Some("本机暂未读到登录凭据".into()),
            };
            record_channel_activity(app, id, activity, false).await;
            return;
        }
    };
    let client = checkin::direct_client();
    let (out, body) = checkin::call_with_body(&client, def, &cred, false, None).await;
    let Some(body) = body else {
        let activity = checkin::ActivityState {
            key: format!("{}:{}:account", id, cycle_day_of(id, now)),
            title: "Qoder 活动状态".into(),
            status: "unknown".into(),
            scope: "account".into(),
            region: if id == "qoder-cn" { "CN" } else { "Global" }.into(),
            period: cycle_day_of(id, now),
            reward: None,
            observed_at: Some(now),
            detail: Some(out.verdict().into()),
        };
        record_channel_activity(app, id, activity, false).await;
        return;
    };
    let newly_claimable = record_qoder_activities(app, id, &body, now_ts()).await;
    let has_claim_id = checkin::qoder_pick(&body).claim_id.is_some();
    drop(_busy);

    if newly_claimable && has_claim_id {
        // 活动可能在每日首次签到之后才开放,发现新可领项时立刻复用同一领取链路。
        // 再读一次实时开关,避免用户刚关掉自动领取时仍发出 POST。
        let auto_enabled_now = {
            let state = app.state::<AppState>();
            let cfg = lock_rw(&state.config);
            cfg.checkin.get(id).map(|c| c.enabled && c.auto).unwrap_or(false)
                && !cfg.hidden_channels.iter().any(|h| h == id)
        };
        if auto_enabled_now {
            if let Err(e) = run_checkin(app, id, false).await {
                eprintln!("Qoder 新活动自动领取跳过: {e}");
            }
        }
    }
}

/// 每轮取数之后:把还没落定的到账核对单(pending)接着往下核。
///
/// 挂在 `refresh_now` 与 `poll_loop` 的取数完成点上 —— 快照已经写进库,
/// 这里只读库比增量,**零网络请求**。claim 当轮就能确认是常态(签到成功后
/// `run_checkin` 会立刻取一轮);这里兜的是"发放延迟 / 被同轮消耗抵消"的那部分,
/// 持续核到该渠道的签到周期日为止(`ArrivalInfo::observe` 里跨日落 unseen)。
///
/// 纪律:只改 `checkin[id].arrival` 这一项(N04 的锁纪律,不整份克隆覆盖),
/// 不碰 `last_code` / `outcome` —— 核对是证据,永不回改签到结论。
pub(crate) async fn settle_arrivals(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    let now = now_ts();
    // ① 读:谁有待落定的核对单。config 锁内只把「谁在等 + 等什么」抄下来,
    // **不嵌套 store 锁** —— 全局锁序是「先 store 后 config」(lock_rw 注释),
    // 持 config 再拿 store 会与 check_update 构成 AB-BA 死锁(报告 B2)。
    let pending: Vec<(String, checkin::ArrivalInfo, String)> = {
        let cfg = lock_rw(&state.config);
        checkin::CHECKINS
            .iter()
            .filter_map(|def| {
                let c = cfg.checkin.get(def.id)?;
                if !c.enabled {
                    return None; // 与 run_checkin 的闸门同口径:关了就不动
                }
                if cfg.hidden_channels.iter().any(|h| h == def.id) {
                    return None;
                }
                let a = c.arrival.as_ref()?;
                if a.status != "pending" {
                    return None;
                }
                Some((def.id.to_string(), a.clone(), cycle_day_of(def.id, now)))
            })
            .collect()
    };
    let mut updates: Vec<(String, checkin::ArrivalInfo)> = Vec::new();
    for (id, a, today) in pending {
        let snap = lock_rw(&state.store).latest_snapshot(&id).ok().flatten();
        let Some((_, bal)) = snap else { continue };
        let mut a2 = a.clone();
        a2.observe(bal, now, &today);
        if a2 != a {
            updates.push((id, a2));
        }
    }
    if updates.is_empty() {
        return;
    }
    // ② 写:只合并改动的那几项,过串行闸落盘(报告 B4)。诊断日志挪到闸外写,
    // 不占住配置临界区;配置损坏时不动配置(P1-16),核对结论留给下一轮。
    if refuse_if_config_broken().is_ok() {
        if let Err(e) = save_config_off_main(&state, |live| {
            for (id, a) in &updates {
                if let Some(e) = live.checkin.get_mut(id) {
                    e.arrival = Some(a.clone());
                }
            }
        })
        .await
        {
            eprintln!("到账核对落库失败: {e}");
        }
    }
    for (id, a) in &updates {
        tokenscope_core::diaglog::write_keyed(
            "checkin",
            id,
            &format!(
                "到账核对:{} · 基线 {}@{} · {}",
                a.status,
                a.baseline,
                a.baseline_at,
                a.delta
                    .map(|d| format!("增量 +{d}"))
                    .unwrap_or_else(|| "未见增量".into())
            ),
        );
    }
    // ③ 告诉前端:核对单落定了,渠道卡与设置页的文案要当场刷新
    for (id, _) in &updates {
        let _ = app.emit("checkin-updated", serde_json::json!({ "id": id }));
    }
}
