use super::*;

// ───────────── 检查更新 ─────────────
// 更新逻辑放 Rust 而不是前端:前端是无打包器的静态页,拿不到
// @tauri-apps/plugin-updater 的 npm 包。返回值 + 事件这套路子与
// get_channels / channels-updated 一致 —— 前端只负责渲染,不做判断。
//
// 端点与公钥都在 tauri.conf.json 的 plugins.updater 里,插件自己读。
pub(crate) const UPDATE_CHECK_INTERVAL_SEC: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    /// 当前运行的版本
    pub current: String,
    /// 有新版本时为 true
    pub available: bool,
    pub version: String,
    pub notes: String,
    pub date: Option<String>,
    /// 这次是否真的去问过服务器。节流 / 关掉自动检查时都是 false。
    pub checked_now: bool,
    /// 只有真去问了才有值。失败原因只给手动点击看,自动检查一律静默。
    pub error: Option<String>,
}

impl UpdateState {
    fn idle(current: &str) -> Self {
        Self {
            current: current.into(),
            available: false,
            version: String::new(),
            notes: String::new(),
            date: None,
            checked_now: false,
            error: None,
        }
    }
}

// 注意:async 命令只要带了 State(借用)就必须返回 Result,否则 tauri 的代码生成
// 过不了(AsyncCommandMustReturnResult)。所以这里是 Result<UpdateState, String>,
// Err 只表示命令本身坏了,更新"没有可用版本"是 Ok 里的 available:false。
#[tauri::command]
pub(crate) async fn check_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    force: bool,
) -> Result<UpdateState, String> {
    use tauri_plugin_updater::UpdaterExt;
    let current = env!("CARGO_PKG_VERSION").to_string();

    let (auto, last_check_at, skipped) = {
        let cfg = lock_rw(&state.config);
        (
            cfg.auto_check_update,
            cfg.last_check_at,
            cfg.skipped_version.clone(),
        )
    };
    // 自动检查的闸门:开关关着、或距上次不足 24 小时,就直接闲置,不发请求。
    // 手动点「检查更新」(force=true)一律放行。
    if !force && !auto {
        return Ok(UpdateState::idle(&current));
    }
    if !force {
        if let Some(last) = last_check_at {
            if now_ts() - last < UPDATE_CHECK_INTERVAL_SEC {
                return Ok(UpdateState::idle(&current));
            }
        }
    }

    // 只要真的要发请求就先把时间戳写掉 —— 失败也算检查过,
    // 否则一次网络故障会变成每次启动都重试的请求风暴。
    {
        // 时间戳先记内存(失败也算检查过 —— 否则一次网络故障会变成每次启动都重试的
        // 请求风暴);落盘走串行闸,与其它配置写者排队(报告 B4)。配置损坏时**不能**
        // 整份写盘:内存里那份是 Config::default(),一写就把用户还能手工救回的原始
        // JSON 永久覆盖掉(报告 P1-16)。
        let now = now_ts();
        if refuse_if_config_broken().is_ok() {
            if let Err(e) = save_config_off_main(&state, |c| c.last_check_at = Some(now)).await {
                eprintln!("更新检查时间落库失败: {e}");
            }
        } else {
            lock_rw(&state.config).last_check_at = Some(now);
        }
    }

    let updater = match app.updater_builder().build() {
        Ok(u) => u,
        Err(e) => {
            return Ok(UpdateState {
                checked_now: true,
                error: Some(e.to_string()),
                ..UpdateState::idle(&current)
            })
        }
    };
    Ok(match updater.check().await {
        Ok(Some(u)) => {
            // 「跳过此版本」要尊重到底:手动点也不该把跳过的版本重新弹出来。
            // 但请求确实发出去了 —— checked_now 必须为真,前端据此才敢刷新
            // 「上次检查时间」;设成 false 会让周期轮询把它当成"没问到"而跳过记账。
            if skipped.as_deref() == Some(u.version.as_str()) {
                UpdateState {
                    checked_now: true,
                    ..UpdateState::idle(&current)
                }
            } else {
                UpdateState {
                    current,
                    available: true,
                    version: u.version.clone(),
                    // 更新说明在远端清单里可能没有,缺失就留空,前端会显示占位
                    notes: u.body.clone().unwrap_or_default(),
                    date: u.date.as_ref().map(|d| d.to_string()),
                    checked_now: true,
                    error: None,
                }
            }
        }
        Ok(None) => UpdateState {
            checked_now: true,
            ..UpdateState::idle(&current)
        },
        // 网络不通、限流、清单格式不对都走这里。自动检查时前端不会显示它
        Err(e) => UpdateState {
            checked_now: true,
            error: Some(updater_err(&e)),
            ..UpdateState::idle(&current)
        },
    })
}

/// updater 的错误原文是英文(如 "error downloading ..."):分类成一句中文给用户,
/// 原文写进诊断日志 —— 报障链路照样能搜到。
pub(crate) fn updater_err(e: impl std::fmt::Display) -> String {
    let raw = e.to_string();
    let low = raw.to_lowercase();
    let msg = if low.contains("signature") || low.contains("verify") || low.contains("integrity") {
        "签名校验没过,安装包可能不完整"
    } else if low.contains("network")
        || low.contains("connect")
        || low.contains("timeout")
        || low.contains("timed out")
    {
        "连不上更新服务器,检查网络后重试"
    } else if low.contains("download") || low.contains("request") || low.contains("send") {
        "下载没完成,点「重试更新」再试一次"
    } else {
        "更新没成,稍后重试"
    };
    tokenscope_core::diaglog::write_keyed("updater", "error", &raw);
    format!("更新没成: {msg}")
}

/// 下载并安装。Windows 上 install 那一步会直接退出进程,
/// 所以这里不会返回 "装好了" —— 前端别在它后面再弹确认框。
#[tauri::command]
pub(crate) async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    // 先在途写入收尾(最多等 20 秒),再进维护态
    for _ in 0..200 {
        if !INJECTING.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if INJECTING.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("有客户端配置正在写入,请等它结束后再更新(界面上那一步完成后重试)".to_string());
    }
    let _upd_guard = FlagGuard(&UPDATING);
    UPDATING.store(true, std::sync::atomic::Ordering::Relaxed);
    // 轮询现在按「下次重试到期」睡长觉(最长 900 秒),必须主动叫醒它才能立刻停取数
    UPDATING_NOTIFY.notify_waiters();
    let updater = app.updater_builder().build().map_err(updater_err)?;
    let update = updater
        .check()
        .await
        .map_err(updater_err)?
        .ok_or_else(|| "没有可用的更新".to_string())?;

    let mut received: u64 = 0;
    let mut total: u64 = 0;
    // 进度回调是**每个数据块**一次:几十 MB 会打出上万条事件,每条都带一次 JSON 序列化
    // + IPC + 前端全量重绘,面板与托盘在这期间明显顿(报告 P1-15)。按 200ms 节流。
    let h1 = app.clone();
    let mut last_emit = std::time::Instant::now();
    let h2 = app.clone();
    let _ = h1.emit(
        "update-progress",
        serde_json::json!({ "phase": "started", "received": 0, "total": 0 }),
    );
    update
        .download_and_install(
            move |chunk: usize, content_length: Option<u64>| {
                received += chunk as u64;
                if let Some(cl) = content_length {
                    total = cl;
                }
                if last_emit.elapsed() >= std::time::Duration::from_millis(200) || received == total {
                    last_emit = std::time::Instant::now();
                    let _ = h1.emit(
                        "update-progress",
                        serde_json::json!({ "phase": "downloading", "received": received, "total": total }),
                    );
                }
            },
            move || {
                let _ = h2.emit("update-progress", serde_json::json!({ "phase": "installing" }));
            },
        )
        .await
        .map_err(updater_err)?;
    let _ = app.emit("update-progress", serde_json::json!({ "phase": "finished" }));
    Ok(())
}

/// 记住「跳过此版本」。
#[tauri::command]
pub(crate) async fn skip_update_version(state: State<'_, AppState>, version: String) -> Result<(), String> {
    refuse_if_config_broken()?;
    save_config_off_main(&state, move |c| c.skipped_version = Some(version)).await
}

/// 当前版本号(前端底部显示用)。之前硬编码在 index.html 里,发版必改且会忘(报告 P2-15)。
#[tauri::command]
pub(crate) fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 取走启动期的非致命错误(配置损坏回落默认、快捷键注册失败)。
/// 取走即清空 —— 提示一次就够,别每次刷新都弹。
#[tauri::command]
pub(crate) fn take_startup_error(state: State<'_, AppState>) -> Option<String> {
    lock_rw(&state.startup_error).take()
}

