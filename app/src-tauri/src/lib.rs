//! TokenScope 的 Tauri 外壳:窗口、托盘、命令、轮询调度。
//!
//! 取数逻辑全在 tokenscope-core,这里只做胶水。

use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, State};
use tokenscope_core::config::Config;
use tokenscope_core::fetch::{now_ts, ChannelView, Fetcher};
use tokenscope_core::{providers, secrets, store::Store};

pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub config: Mutex<Config>,
    pub fetcher: Arc<Fetcher>,
    /// 当前已注册的全局快捷键(空 = 未注册)。换键时先注册新的、成功后再撤旧的。
    pub hotkey: Mutex<Option<String>>,
    /// 启动期的非致命错误(快捷键被占用、配置损坏回落默认…)。之前只 eprintln,
    /// 绿色版从托盘启动时没人看得到 stderr(报告 P2-14)。前端取走即清空,只提示一次。
    pub startup_error: Mutex<Option<String>>,
}

#[tauri::command]
async fn get_channels(state: State<'_, AppState>) -> Result<Vec<ChannelView>, String> {
    // 先克隆 Arc 再 await:不让 State 的借用跨越 await 点
    let store = state.store.clone();
    let fetcher = state.fetcher.clone();
    // fetch_shared:与轮询/托盘刷新共用闸门,手动刷新不再叠加出一份并发请求(报告 O-4)
    Ok(fetcher.fetch_shared(&store).await)
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Config {
    lock_rw(&state.config).clone()
}

/// 按配置重注册全局快捷键(空串 = 只注销)。先注册新键、成功后才撤旧键:
/// 新键被其它应用占用时保持现状,不会出现「旧键没了、新键也没有」。
/// 行为回调挂在插件 Builder 的 with_handler 上(见 run())。
fn apply_hotkey(app: &tauri::AppHandle, state: &AppState, want: &str) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let want = want.trim().to_string();
    let mut cur = lock_rw(&state.hotkey);
    if cur.as_deref().unwrap_or("") == want.as_str() {
        return Ok(()); // 没变,别动注册器
    }
    let manager = app.global_shortcut();
    if want.is_empty() {
        if let Some(old) = cur.take() {
            let _ = manager.unregister(old.as_str());
        }
        return Ok(());
    }
    manager
        .register(want.as_str())
        .map_err(|e| format!("注册 {want} 失败(可能已被其它程序占用): {e}"))?;
    if let Some(old) = cur.take() {
        let _ = manager.unregister(old.as_str());
    }
    *cur = Some(want);
    Ok(())
}

#[tauri::command]
fn set_config(app: tauri::AppHandle, state: State<'_, AppState>, config: Config) -> Result<(), String> {
    // 前端提交的是它启动时那份配置的快照。更新流程(跳过版本/上次检查时间)和
    // 窗口位置是后端单方面往前推的字段 —— 前端拿不到最新值,整份覆盖会把它们
    // 抹回 None(报告 P1-3:跳过某版本后,下次保存设置让该版本又重新弹窗)。
    let mut incoming = config;
    {
        let cur = lock_rw(&state.config);
        if incoming.skipped_version.is_none() {
            incoming.skipped_version = cur.skipped_version.clone();
        }
        if incoming.last_check_at.is_none() {
            incoming.last_check_at = cur.last_check_at;
        }
        if incoming.win_x.is_none() {
            incoming.win_x = cur.win_x;
        }
        if incoming.win_y.is_none() {
            incoming.win_y = cur.win_y;
        }
    }

    // 先应用快捷键:注册失败(如被占用)就让整次保存失败,前端好回滚旧值
    let old_hotkey = lock_rw(&state.hotkey).clone();
    if let Err(e) = apply_hotkey(&app, &state, &incoming.hotkey) {
        return Err(e);
    }
    let store = lock_rw(&state.store);
    if let Err(e) = incoming.save(&store) {
        // 落库失败:把快捷键回滚,否则界面显示"保存失败"而快捷键却已经换了
        if let Some(old) = old_hotkey {
            let _ = apply_hotkey(&app, &state, &old);
        }
        return Err(e);
    }
    drop(store);
    *lock_rw(&state.config) = incoming;
    Ok(())
}

/// 只回显尾 4 位,用于确认每个渠道存的到底是哪把 key(存错 key 却以为是接口坏了,
/// 是最难排查的一类问题)。不回显完整密钥。复用本机应用登录态的渠道没有密钥可回显。
#[tauri::command]
fn key_hint(id: String) -> Option<String> {
    if !matches!(
        providers::find(&id).map(|d| d.auth),
        Some(providers::AuthKind::Keyring)
    ) {
        return None;
    }
    secrets::masked(&id)
}

#[tauri::command]
fn set_key(id: String, key: String) -> Result<(), String> {
    let def = providers::find(&id).ok_or_else(|| format!("未知渠道: {id}"))?;
    if let providers::AuthKind::App(app) = def.auth {
        return Err(format!(
            "{} 不需要填密钥:凭据直接读本机已登录的 {} 客户端",
            def.name,
            app.label()
        ));
    }
    secrets::set(&id, &key)
}

#[tauri::command]
fn delete_key(id: String) -> Result<(), String> {
    let def = providers::find(&id).ok_or_else(|| format!("未知渠道: {id}"))?;
    if let providers::AuthKind::App(app) = def.auth {
        return Err(format!("{} 没有本应用保存的密钥(用的是 {} 的登录态)", def.name, app.label()));
    }
    secrets::delete(&id)
}

#[tauri::command]
fn get_series(state: State<'_, AppState>, id: String, hours: i64) -> Vec<f64> {
    let from = now_ts() - hours * 3600;
    // lock_rw:锁被毒化时也要出图,不能让取数线程的一次 panic 之后所有曲线都空掉
    let s = lock_rw(&state.store);
    s.series(&id, from, 24).unwrap_or_default()
}

/// 开机自启:写 HKCU 的 Run 键,不需要管理员权限。
/// 产物是绿色单文件 exe,可能被用户移动位置,所以 setup 里每次启动都按
/// `current_exe()` 重写一遍注册表 —— 路径永远指向当前这份 exe。
#[cfg(windows)]
mod autostart {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "Grandettoken";
    /// 改名前的键名:停用/同步时一并清掉,避免残留一个指向已删除 exe 的自启项
    const LEGACY_NAME: &str = "TokenScope";

    pub fn set(enabled: bool) -> Result<(), String> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(RUN_KEY)
            .map_err(|e| e.to_string())?;
        if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            // 路径可能含空格,必须加引号;--hidden 让开机自启直接进托盘不弹窗
            let cmd = format!("\"{}\" --hidden", exe.display());
            key.set_value(NAME, &cmd).map_err(|e| e.to_string())?;
            // 开着自启也要清遗留键:绿色版(TokenScope)时代开过自启、后升级到安装版
            // 的机器,否则注册表残留一个指向已删除 exe 的自启项,每次开机报错(报告 B7)
            let _ = key.delete_value(LEGACY_NAME);
        } else {
            // 关掉时键可能本就不存在,删除报错属正常,忽略
            let _ = key.delete_value(NAME);
            let _ = key.delete_value(LEGACY_NAME);
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod autostart {
    pub fn set(_enabled: bool) -> Result<(), String> {
        Err("开机自启仅支持 Windows".into())
    }
}

#[tauri::command]
fn set_autostart(enabled: bool) -> Result<(), String> {
    autostart::set(enabled)
}

/// 托盘角标。前端用 canvas 画好 32×32 的 RGBA(图标同款造型 + 状态色角标),
/// 这里换成托盘图标并把文字放进 tooltip —— 托盘图标只有 16px,数字放 tooltip,
/// 和胶囊共用同一套渠道与轮播逻辑。
///
/// 像素走 base64 而不是 `Vec<u8>`:Tauri 把命令参数序列化成 JSON,4096 字节的
/// RGBA 会变成两万多个字符的数组,每几秒轮播一次白耗流量(报告 O-9)。
/// 尺寸不符直接报错:图标画错时宁可保留上一帧,也不要把花屏贴到托盘上。
#[tauri::command]
fn set_tray_icon(
    app: tauri::AppHandle,
    rgba_b64: String,
    size: u32,
    tooltip: String,
) -> Result<(), String> {
    use base64::Engine as _;
    let tray = app.tray_by_id("main").ok_or("托盘不存在")?;
    let rgba = base64::engine::general_purpose::STANDARD
        .decode(rgba_b64.as_bytes())
        .map_err(|e| format!("托盘图标 base64 解码失败: {e}"))?;
    let want = size as usize * size as usize * 4;
    if size == 0 || rgba.len() != want {
        return Err(format!("托盘图标尺寸不符: {size}x{size} 需要 {want} 字节,收到 {}", rgba.len()));
    }
    let img = tauri::image::Image::new_owned(rgba, size, size);
    tray.set_icon(Some(img)).map_err(|e| e.to_string())?;
    tray.set_tooltip(Some(&tooltip)).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn window_cmd(app: tauri::AppHandle, action: String) -> Result<(), String> {
    let win = app.get_webview_window("main").ok_or("窗口不存在")?;
    match action.as_str() {
        "hide" => win.hide().map_err(|e| e.to_string())?,
        "minimize" => win.minimize().map_err(|e| e.to_string())?,
        "quit" => app.exit(0),
        _ => return Err(format!("未知操作: {action}")),
    }
    Ok(())
}

/// 全局锁序约定:**先 store 后 config**。所有同时拿两把锁的命令都必须按这个
/// 顺序,否则与 check_update / skip_update_version(store→config)交错时会 AB-BA 死锁。
/// 锁一律走 `lock_rw` 毒化恢复:一次 panic 不该把后续所有命令永久卡死。
fn lock_rw<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// 置顶的唯一写入口:窗口状态与配置一起改,不会出现"按钮亮了其实没置顶"。
/// 折叠形态(紧凑条/胶囊/贴边)没有置顶按钮,靠设置页里的同一个开关控制。
#[tauri::command]
fn set_pin(app: tauri::AppHandle, state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    let win = app.get_webview_window("main").ok_or("窗口不存在")?;
    win.set_always_on_top(enabled).map_err(|e| e.to_string())?;
    let store = lock_rw(&state.store);
    let mut cfg = lock_rw(&state.config);
    cfg.always_on_top = enabled;
    cfg.save(&store)
}

/// 后台轮询。渠道都是账务接口,查询余额不消耗 token,所以间隔可以压得比较短;
/// 唯一约束是对方的限流礼貌性,所以失败时退避。
async fn poll_loop(app: tauri::AppHandle) {
    let mut failures = 0u32;
    // 90 天清理原来只在启动时做一次:这台机器常年不重启的话快照表就一直长(报告 O-2)
    let mut last_prune = now_ts();

    loop {
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
        tokio::time::sleep(std::time::Duration::from_secs(wait + jitter)).await;

        // 先把 Arc 取出来,避免 State 借用跨越 await
        let (store, fetcher) = {
            let state = app.state::<AppState>();
            (state.store.clone(), state.fetcher.clone())
        };

        if now_ts() - last_prune >= 86_400 {
            last_prune = now_ts();
            let s = lock_rw(&store);
            if let Err(e) = s.prune(now_ts() - 90 * 86_400) {
                eprintln!("定期清理快照失败: {e}");
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

        let _ = app.emit("channels-updated", &views);
    }
}

async fn refresh_now(app: &tauri::AppHandle) {
    let (store, fetcher) = {
        let state = app.state::<AppState>();
        (state.store.clone(), state.fetcher.clone())
    };
    let views = fetcher.fetch_shared(&store).await;
    let _ = app.emit("channels-updated", &views);
}

// ───────────── 检查更新 ─────────────
// 更新逻辑放 Rust 而不是前端:前端是无打包器的静态页,拿不到
// @tauri-apps/plugin-updater 的 npm 包。返回值 + 事件这套路子与
// get_channels / channels-updated 一致 —— 前端只负责渲染,不做判断。
//
// 端点与公钥都在 tauri.conf.json 的 plugins.updater 里,插件自己读。
const UPDATE_CHECK_INTERVAL_SEC: i64 = 24 * 60 * 60;

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
async fn check_update(
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
        let store = state.store.lock();
        if let Ok(s) = store {
            // 锁序样板:这里 store→config,set_pin 等命令必须同序(见 lock_rw 注释)
            let mut cfg = lock_rw(&state.config);
            cfg.last_check_at = Some(now_ts());
            let _ = cfg.save(&s);
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
            error: Some(e.to_string()),
            ..UpdateState::idle(&current)
        },
    })
}

/// 下载并安装。Windows 上 install 那一步会直接退出进程,
/// 所以这里不会返回 "装好了" —— 前端别在它后面再弹确认框。
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater_builder().build().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "没有可用的更新".to_string())?;

    let mut received: u64 = 0;
    let mut total: u64 = 0;
    let h1 = app.clone();
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
                let _ = h1.emit(
                    "update-progress",
                    serde_json::json!({ "phase": "downloading", "received": received, "total": total }),
                );
            },
            move || {
                let _ = h2.emit("update-progress", serde_json::json!({ "phase": "installing" }));
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit("update-progress", serde_json::json!({ "phase": "finished" }));
    Ok(())
}

/// 记住「跳过此版本」。
#[tauri::command]
fn skip_update_version(state: State<'_, AppState>, version: String) -> Result<(), String> {
    let store = lock_rw(&state.store);
    let mut cfg = lock_rw(&state.config);
    cfg.skipped_version = Some(version);
    cfg.save(&store)
}

/// 记住窗口位置(报告 O-6)。只在自由态下记 —— 吸附/紧凑条/胶囊的坐标是布局
/// 算出来的,记下来反而会在下次启动覆盖正确的默认位置。
/// 前端在拖动结束后调用一次,不做高频写盘。
#[tauri::command]
fn save_window_pos(state: State<'_, AppState>, x: i32, y: i32) -> Result<(), String> {
    let store = lock_rw(&state.store);
    let mut cfg = lock_rw(&state.config);
    cfg.win_x = Some(x);
    cfg.win_y = Some(y);
    cfg.save(&store)
}

/// 把请求的窗口坐标夹进"至少有一条边露在外面"的范围。
/// 多屏环境下用户拔掉副屏后,记下的坐标会落在屏幕外,窗口看起来像消失了。
/// 至少要露出 KEEP_X × KEEP_Y 的可视区,保证还能拖回来。
fn clamp_to_monitors(app: &tauri::AppHandle, x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    const KEEP_X: i32 = 80;
    const KEEP_Y: i32 = 40;
    let Ok(monitors) = app.available_monitors() else {
        return (x, y);
    };
    if monitors.is_empty() {
        return (x, y);
    }
    // 完全落在某个显示器内 → 原样返回
    for m in &monitors {
        let (mx, my) = (m.position().x, m.position().y);
        let (mw, mh) = (m.size().width as i32, m.size().height as i32);
        if x >= mx && y >= my && x + w <= mx + mw && y + h <= my + mh {
            return (x, y);
        }
    }
    // 否则挑第一个显示器夹:左上角不越过 (mx+mw-KEEP) 边界
    let m = &monitors[0];
    let (mx, my) = (m.position().x, m.position().y);
    let (mw, mh) = (m.size().width as i32, m.size().height as i32);
    let cx = x.clamp(mx, (mx + mw - KEEP_X).max(mx));
    let cy = y.clamp(my, (my + mh - KEEP_Y).max(my));
    (cx, cy)
}

/// 当前版本号(前端底部显示用)。之前硬编码在 index.html 里,发版必改且会忘(报告 P2-15)。
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 取走启动期的非致命错误(配置损坏回落默认、快捷键注册失败)。
/// 取走即清空 —— 提示一次就够,别每次刷新都弹。
#[tauri::command]
fn take_startup_error(state: State<'_, AppState>) -> Option<String> {
    lock_rw(&state.startup_error).take()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 必须是第一个插件:第二个实例不再往下走,直接把已开实例的窗口唤到前台。
        // 之前双开会出现两个托盘图标、两个轮询一起打账务接口,而且两套 SQLite
        // 连接写同一个库(报告 O-3)。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let was = w.is_visible().unwrap_or(false);
                let _ = w.show();
                let _ = w.set_focus();
                if !was {
                    let _ = app.emit("window-shown", ());
                }
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                // 全插件共用这一个回调:按下的那一刻切换主窗口显隐(与托盘左键同语义)
                .with_handler(|app, _shortcut, ev| {
                    use tauri_plugin_global_shortcut::ShortcutState;
                    if ev.state != ShortcutState::Pressed {
                        return;
                    }
                    if let Some(w) = app.get_webview_window("main") {
                        if w.is_visible().unwrap_or(false) {
                            let _ = w.hide();
                        } else {
                            let _ = w.show();
                            let _ = w.set_focus();
                            // 真正「隐藏 → 显示」的那一刻告知前端(重放入场动画)。
                            // 前端不能靠 focus 事件区分 alt-tab 与唤回(报告 B8)
                            let _ = app.emit("window-shown", ());
                        }
                    }
                })
                .build(),
        )
        // 公钥与端点都在 tauri.conf.json 的 plugins.updater 里,插件自己读配置
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("无法定位数据目录: {e}"))?;
            let store = Store::open(&dir.join("tokenscope.db"))?;
            // 配置坏了不能静默用默认值:那会把用户的隐藏渠道、快捷键、轮询间隔
            // 全部悄悄还原(报告 P2-14)。取默认值继续启动,但把原因留给前端提示。
            let (config, mut startup_error) = match Config::try_load(&store) {
                Ok(c) => (c, None),
                Err(e) => (Config::default(), Some(e)),
            };

            // 注册表与配置对齐:开机自启以配置为准,并刷新为当前 exe 路径,
            // 用户挪动 exe 后无需重新设置自启
            let _ = autostart::set(config.autostart);

            // 置顶 + 上次位置是持久化设置:启动时按配置应用,折叠形态没有置顶按钮也生效
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_always_on_top(config.always_on_top);
                // 恢复窗口位置(报告 O-6)。多屏环境下副屏拔掉后旧坐标落在屏幕外,
                // 窗口"消失"又无从找回 —— 所以按当前可用显示器裁一遍。
                if let (Some(x), Some(y)) = (config.win_x, config.win_y) {
                    if let Ok(size) = w.outer_size() {
                        let (cx, cy) =
                            clamp_to_monitors(app.handle(), x, y, size.width as i32, size.height as i32);
                        if cx != x || cy != y {
                            eprintln!("窗口位置 ({x},{y}) 不在任何显示器内,已修正为 ({cx},{cy})");
                        }
                        let _ = w.set_position(tauri::PhysicalPosition::new(cx, cy));
                    }
                }
            }

            let _ = store.prune(now_ts() - 90 * 86400);

            app.manage(AppState {
                store: Arc::new(Mutex::new(store)),
                config: Mutex::new(config.clone()),
                fetcher: Arc::new(Fetcher::new()),
                hotkey: Mutex::new(None),
                startup_error: Mutex::new(None),
            });

            // 启动时注册已保存的全局快捷键;被占用不拦启动,但要把原因留给界面
            // (之前只 eprintln,绿色版从托盘启动时没人看得到 stderr —— 报告 P2-14)
            if !config.hotkey.is_empty() {
                let st = app.state::<AppState>();
                if let Err(e) = apply_hotkey(app.handle(), &st, &config.hotkey) {
                    eprintln!("全局快捷键注册失败: {e}");
                    startup_error = Some(match startup_error {
                        Some(prev) => format!("{prev};另外 {e}"),
                        None => e,
                    });
                }
            }
            if let Some(msg) = startup_error {
                *lock_rw(&app.state::<AppState>().startup_error) = Some(msg);
            }

            let refresh = MenuItem::with_id(app, "refresh", "立即刷新", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "显示面板", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &refresh, &sep, &quit])?;

            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Grandettoken")
                .menu(&menu)
                // 左键只切换显示/隐藏(下面的事件处理),菜单留给右键。
                // 不设这个开关时 Tauri 在 Windows 上左键也会弹菜单。
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let was = w.is_visible().unwrap_or(false);
                            let _ = w.show();
                            let _ = w.set_focus();
                            if !was {
                                let _ = app.emit("window-shown", ());
                            }
                        }
                    }
                    "refresh" => {
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move { refresh_now(&handle).await });
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                                let _ = app.emit("window-shown", ());
                            }
                        }
                    }
                })
                .build(app)?;

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { poll_loop(handle).await });

            // 窗口配置为 visible:false,这里统一控制显隐:
            // 正常启动立即显示;开机自启带 --hidden,直接进托盘等用户点开
            if !std::env::args().any(|a| a == "--hidden") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_channels,
            get_config,
            set_config,
            set_key,
            delete_key,
            get_series,
            set_autostart,
            set_pin,
            set_tray_icon,
            key_hint,
            window_cmd,
            check_update,
            install_update,
            skip_update_version,
            save_window_pos,
            app_version,
            take_startup_error,
        ])
        .run(tauri::generate_context!())
        .expect("TokenScope 启动失败");
}
