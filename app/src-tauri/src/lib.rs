//! TokenScope 的 Tauri 外壳:窗口、托盘、命令、轮询调度。
//!
//! 取数逻辑全在 tokenscope-core,这里只做胶水。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;
use tokenscope_core::config::Config;
use tokenscope_core::fetch::{now_ts, ChannelView, Fetcher};
use tokenscope_core::{appauth, checkin, inject, models, providers, secrets, store::Store, view, workbuddy};

mod sync_cmds;
mod download;
mod gateway;
mod vault_sync;
use vault_sync::{
    ensure_vault_mutable, sync_channel_to_gateway, vault_default_channel,
    vault_defaults_lock,
};
mod doubao_web;
mod codex_reset_cmd;
mod google_cloud_web;
mod webview_cookies;
pub use sync_cmds::{
    sync_create, sync_disconnect_github, sync_forget,
    sync_join, sync_oauth_cancel, sync_oauth_poll, sync_oauth_start, sync_preview,
    sync_publish_now, sync_qr_svg, sync_read_group, sync_recovery_candidates, sync_restore_group,
    sync_rotate, sync_set_channels, sync_set_enabled,
    sync_set_interval, sync_set_oauth_client_id, sync_set_platform_label, sync_status,
};

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

mod checkin_activity;
mod checkin_cmds;
mod config_cmds;
mod custom_cmds;
mod diag_cmds;
mod key_ref_cmds;
mod model_cmds;
mod notify;
mod poll;
mod update_cmds;
mod vault_cmds;
mod weblogin_cmds;
mod window_cmds;
use checkin_activity::*;
use checkin_cmds::*;
use config_cmds::*;
use diag_cmds::*;
pub use model_cmds::DiagLogInfo;
use model_cmds::*;
use notify::*;
use poll::*;
pub use update_cmds::UpdateState;
use vault_cmds::*;
use weblogin_cmds::*;
use window_cmds::*;

/// 把**这一份**取数结果算成首页统一视图。
///
/// `home_view` 命令与轮询推送(`channels-updated`)共用它。两条路径必须用同一个
/// 函数、喂同一份 views —— 2026-10-26 真机:推送只带了 views,前端手里的视图停在
/// 启动那一刻,于是面板数字一直在动、胶囊(主数字取自 `headline`)一直是启动时的余额。
pub(crate) fn home_view_of(cfg: &Config, views: &[ChannelView], now: i64) -> view::HomeView {
    // 签到的"今天签完没"由后端判定(Qoder 的周期日从 10:00 起算,前端算必错),
    // 这里沿用 `checkin_state` 那条链路上的同一个函数。
    // 隐藏的渠道不必在这里排除:`build_home` 只数排进行列表的那些行,而隐藏的
    // 渠道根本不进 `rows`(与前端 `ckActiveIds` 的口径仍然是同一个)。
    let checks: Vec<view::CheckinInput> = checkin::CHECKINS
        .iter()
        .map(|d| {
            let c = cfg.checkin.get(d.id).cloned().unwrap_or_default();
            let activity_ready = activity_ready_today(&c, &cycle_day_of(d.id, now));
            view::CheckinInput {
                id: d.id.to_string(),
                enabled: c.enabled,
                done_today: c.blocks_today(&cycle_day_of(d.id, now), APP_VERSION) && !activity_ready,
            }
        })
        .collect();

    view::build_home(views, &checks, &view::ViewInput::from_cfg(cfg, now))
}

/// 一轮取数的推送载荷:**数据 + 由这份数据算出的统一视图**。
///
/// 视图跟着数据一起推,前端就不必再为"补一份视图"多调一次 IPC(那还可能多打一轮账务接口)。
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChannelsPush {
    views: Vec<ChannelView>,
    home: view::HomeView,
}

/// 推送一轮取数结果。两处轮询出口(定时循环 / 手动刷新)都走它,免得再漏掉视图。
pub(crate) fn emit_channels(app: &tauri::AppHandle, views: &[ChannelView]) {
    let cfg = {
        let state = app.state::<AppState>();
        // 先落到局部变量:直接 return 这个表达式的话,MutexGuard 的临时值
        // 会在 `state` 之后才析构,借用比 state 活得久(E0597)。
        let c = lock_rw(&state.config).clone();
        c
    };
    let home = home_view_of(&cfg, views, now_ts());
    let _ = app.emit(
        "channels-updated",
        ChannelsPush { views: views.to_vec(), home },
    );
}

/// 配置那行 JSON 只是**格式坏了**、内容还可能手工救回来的时候,绝不能让任何一次保存把
/// 默认配置整份 UPSERT 覆盖上去(报告 P1-16:拖一下窗口/点一次置顶就会永久丢失隐藏清单、
/// 快捷键、申请制规则)。启动时置位,所有写配置的命令都先问一句。
pub(crate) static CONFIG_BROKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 配置写入的串行闸:这些命令以前都在主线程上跑,两次保存天然排队;改成 async 后
/// 必须自己排,否则"后改的先落库"会把配置倒回旧值(P3-35 的一小半)。
pub(crate) static CFG_WRITE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn save_config_off_main(
    state: &AppState,
    mutate: impl FnOnce(&mut Config),
) -> Result<(), String> {
    let _ser = CFG_WRITE.lock().await;
    // 内存里改 + 序列化(快),落库(慢)交给阻塞任务。
    let json = {
        let mut cfg = lock_rw(&state.config);
        mutate(&mut cfg);
        serde_json::to_string(&*cfg).map_err(|e| format!("配置序列化失败: {e}"))?
    };
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        let s = lock_rw(&store);
        s.set_setting("config", &json)
    })
    .await
    .map_err(|e| format!("保存设置失败: {e}"))?
}

/// 更新维护态与"客户端配置正在写"两个标志(报告 P1-15)。
/// Windows 上 updater 的 install 这一步会直接结束进程:如果正好落在 inject 已落盘、
/// 还没记账的那一瞬间,客户端配置就停在半写状态,重启后没有任何记录能回滚它。
/// 所以:更新前先等在途的写入;更新期间停轮询、停签到节拍、拒绝新的写入与密钥箱保存。
pub(crate) static UPDATING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// 「开始更新」的立即唤醒信号。
///
/// 轮询改成按「下一次失败重试到期」精确睡长觉之后,不能再靠每秒轮询来发现
/// `UPDATING` 翻转(那正是本次要消掉的 86,400 次/天空转)。开始更新时必须主动叫醒它,
/// 否则 60~900 秒的睡眠会让「更新期间停取数」这件事晚生效 —— 那是行为变化,不是优化。
pub(crate) static UPDATING_NOTIFY: tokio::sync::Notify = tokio::sync::Notify::const_new();
pub(crate) static INJECTING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) struct FlagGuard(pub(crate) &'static std::sync::atomic::AtomicBool);
impl Drop for FlagGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

pub(crate) fn refuse_while_updating(what: &str) -> Result<(), String> {
    if UPDATING.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(format!("正在更新挂件,{what}已暂缓 —— 等更新结束后再点"));
    }
    Ok(())
}

/// 正在跑签到的渠道(节拍器与手动点击之间的互斥)。
///
/// 以前没有任何在途标记:"今天已经处理过/退避中"这两个闸门只在**所有网络往返结束之后**
/// 才写配置,而一次签到要串 2~3 个请求(每个最多 20 秒)。用户在节拍器正跑的时候点一次
/// 「领积分」就能同时发出第二个 claim,两条路的 attempts/last_day/next_try_at 还会互相覆盖。
/// 只存 id、不跨 await 持锁,拿不到就明确告诉用户"稍等几秒"。
pub(crate) static CHECKIN_RUNNING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub(crate) struct CheckinBusy(pub(crate) String);
impl CheckinBusy {
    fn acquire(id: &str) -> Result<Self, String> {
        let mut g = CHECKIN_RUNNING.lock().unwrap_or_else(|p| p.into_inner());
        if g.iter().any(|x| x == id) {
            return Err(id.to_string());
        }
        g.push(id.to_string());
        Ok(CheckinBusy(id.to_string()))
    }
}
impl Drop for CheckinBusy {
    fn drop(&mut self) {
        let mut g = CHECKIN_RUNNING.lock().unwrap_or_else(|p| p.into_inner());
        g.retain(|x| x != &self.0);
    }
}

/// 把启动期的致命错误写成文件。release 是 GUI 子系统 + `panic = "abort"`:
/// `?` 与 `expect` 的信息只进 stderr,而 stderr 在这种构建下没人看得见 ——
/// 用户只会看到"双击了没反应"(报告 P2-38)。
pub(crate) fn report_fatal(stage: &str, e: &str) -> String {
    let line = format!(
        "启动失败 · {stage}:{e}
建议:确认是否还有 Grandettoken.exe 在任务管理器里运行,结束后再打开
"
    );
    for dir in [
        std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("com.wacil.tokenscope")),
        Some(std::env::temp_dir()),
    ]
    .into_iter()
    .flatten()
    {
        let _ = std::fs::create_dir_all(&dir);
        if std::fs::write(dir.join("startup-error.log"), &line).is_ok() {
            return format!("{line}(详情见 {})", dir.join("startup-error.log").display());
        }
    }
    line
}

pub(crate) fn refuse_if_config_broken() -> Result<(), String> {
    if CONFIG_BROKEN.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(
            "配置读取失败,现在按默认设置运行(你隐藏的渠道也在按默认取数)。\
             为保住库里还能修的那份,已暂停保存设置:请先修复或确认重置,再改设置。"
                .to_string(),
        );
    }
    Ok(())
}

/// 全局锁序约定:**先 store 后 config**。所有同时拿两把锁的命令都必须按这个
/// 顺序,否则与按此序加锁的命令交错时会 AB-BA 死锁。
/// (check_update 与 settle_arrivals 曾经违反:前者已改走 save_config_off_main 串行闸、
/// 后者已把「config 内抄清单 / 放锁后读 store」拆开 —— 2026-11-10 报告 B2/B4。
/// 新代码请优先走串行闸,实在要同时拿两把锁,顺序不得颠倒。)
/// 锁一律走 `lock_rw` 毒化恢复:一次 panic 不该把后续所有命令永久卡死。
pub(crate) fn lock_rw<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// 往"启动期非致命问题"这条串上再接一条。多条都要让用户看到,
/// 而不是后面那条把前面那条悄悄顶掉。
pub(crate) fn note(prev: Option<String>, msg: String) -> Option<String> {
    Some(match prev {
        Some(p) => format!("{p};另外 {msg}"),
        None => msg,
    })
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
                // 第二实例把窗口唤回来时也补一轮,别让人看到半天前的数字(P3-37)
                refresh_if_stale(app, 60);
            }
        }))
        .plugin(tauri_plugin_opener::init())
        // 系统通知(裁决 3A)。前端没有打包器拿不到插件的 JS 包,所以通知**全在
        // Rust 侧发**(poll_loop → maybe_notify_crit),这里只负责把能力挂上。
        .plugin(tauri_plugin_notification::init())
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
                            if w.hide().is_ok() { let _ = app.emit("window-hidden", ()); }
                        } else {
                            let _ = w.show();
                            let _ = w.set_focus();
                            // 真正「隐藏 → 显示」的那一刻告知前端(重放入场动画)。
                            // 前端不能靠 focus 事件区分 alt-tab 与唤回(报告 B8)
                            let _ = app.emit("window-shown", ());
                            refresh_if_stale(app, 60);
                        }
                    }
                })
                .build(),
        )
        // 公钥与端点都在 tauri.conf.json 的 plugins.updater 里,插件自己读配置
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let dir = app.path().app_data_dir().map_err(|e| {
                let m = format!("无法定位数据目录: {e}");
                report_fatal("定位数据目录", &m)
            })?;
            let store = Store::open(&dir.join("tokenscope.db")).map_err(|e| {
                let m = format!(
                    "打开数据文件失败: {e}(路径 {});若还有 Grandettoken.exe 在运行,先结束它",
                    dir.join("tokenscope.db").display()
                );
                report_fatal("打开数据库", &m)
            })?;
            // 配置坏了不能静默用默认值:那会把用户的隐藏渠道、快捷键、轮询间隔
            // 全部悄悄还原(报告 P2-14)。取默认值继续启动,但把原因留给前端提示。
            let (config, mut startup_error) = match Config::try_load(&store) {
                Ok(c) => (c, None),
                Err(e) => {
                    // 用默认值继续启动是既定取舍(P2-14),但必须把后果说清、并且别再让
                    // 一次随手保存把还能手工救回的那行 JSON 覆盖掉(报告 P1-16)。
                    CONFIG_BROKEN.store(true, std::sync::atomic::Ordering::Relaxed);
                    let msg = format!(
                        "{e} —— 现已按默认设置运行:你之前隐藏的渠道恢复了取数。设置保存已暂停,以免覆盖还能修复的原始配置"
                    );
                    (Config::default(), Some(msg))
                }
            };

            // 同步组使用独立记录，避免签到、窗口和其他设置的旧快照覆盖它。
            // 配置损坏时不能用默认值迁移，保留原数据供恢复。
            if !CONFIG_BROKEN.load(std::sync::atomic::Ordering::Relaxed) {
                if let Err(e) = tokenscope_core::sync::profile::migrate(&store, &config) {
                    startup_error = note(startup_error, format!("同步设置读取失败: {e}"));
                }
            }

            // 注册表与配置对齐:开机自启以配置为准,并刷新为当前 exe 路径,
            // 用户挪动 exe 后无需重新设置自启。构建目录里的开发版不写键,
            // 旧版残留的自启项要让用户看得见(光靠新版删会被旧版开机时写回来)。
            if let Some(msg) = autostart::sync(config.autostart) {
                startup_error = note(startup_error, msg);
            }

            // 置顶 + 上次位置是持久化设置:启动时按配置应用,折叠形态没有置顶按钮也生效
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_always_on_top(config.always_on_top);
                // 先恢复尺寸,再用**新尺寸**夹位置:否则宽窗口在屏幕边缘会被
                // 按旧尺寸裁进屏幕,恢复位置后右/下边又探出去。
                if config.form == "panel" {
                    // 面板只有「单行窗 / 双行窗」两个固定尺寸,不再恢复用户拖动过的尺寸。
                    let u = match config.font_scale.as_str() {
                        "lg" => 1.15,
                        "xl" => 1.3,
                        _ => 1.0,
                    };
                    let (cw, ch) = if config.panel_layout == "double" {
                        (860.0, 820.0)
                    } else {
                        (480.0, 820.0)
                    };
                    let _ = w.set_resizable(false);
                    let _ = w.set_size(tauri::LogicalSize::new(
                        cw * u + 20.0,
                        ch * u + 20.0,
                    ));
                }
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

            let _ = store.prune(now_ts() - 90 * 86400, tokenscope_core::fetch::local_tz_offset());

            // 排查用日志:目录与 SQLite 同一个文件夹(用户找得着、也跟着数据一起备份/清理)。
            // **必须在 manage 与起轮询之前**:第一轮取数的失败链要能落进文件,否则
            // "装上就取不到数"那种场景又只剩一句界面文案(2026-10-20 用户报的那类)。
            tokenscope_core::diaglog::set_dir(dir.clone());
            // 「详细日志」要在横幅**之前**生效:否则用户上次开着详细日志,
            // 这一轮的启动事件就只记了"关着",横幅与实际对不上。
            tokenscope_core::diaglog::set_verbose(config.diag_verbose);
            tokenscope_core::diaglog::startup_banner(env!("CARGO_PKG_VERSION"));

            app.manage(AppState {
                store: Arc::new(Mutex::new(store)),
                config: Mutex::new(config.clone()),
                fetcher: Arc::new(Fetcher::new()),
                hotkey: Mutex::new(None),
                startup_error: Mutex::new(None),
            });

            match gateway::Gateway::new(app.state::<AppState>().store.clone()) {
                Ok(gw) => {
                    app.manage(gw);
                    let gateway_app = app.handle().clone();
                    tauri::async_runtime::spawn(async move { gateway::autostart(gateway_app).await; });
                }
                // TLS 后端初始化失败这类罕见故障:不再 panic 硬退(报告 L16)——
                // 记进启动错误,余额监控与签到照常跑,只是本地网关这一项不可用。
                Err(e) => {
                    let msg = format!("本地网关初始化失败:{e}(余额监控不受影响,仅本地 API 网关不可用)");
                    eprintln!("{msg}");
                    startup_error = note(startup_error, msg);
                }
            }
            // 后台一次性导入现有渠道密钥；不阻塞首屏，也不更改渠道引用。
            let import_app = app.handle().clone();
            let import_store = app.state::<AppState>().store.clone();
            let import_cfg = config.clone();
            tauri::async_runtime::spawn(async move {
                let worker = import_app.clone();
                if tokio::task::spawn_blocking(move || vault_sync::import_for_gateway(&worker, &import_store, &import_cfg)).await.is_err() {
                    gateway::report_sync_error(&import_app, "导入渠道密钥任务失败".into());
                }
            });

            // 网页登录型渠道的会话来源:每轮取数时现从 webview 的 Cookie 罐里读。
            // 回调在取数路径的 spawn_blocking 里调用，避免主线程阻塞读 WebView。
            let session_app = app.handle().clone();
            app.state::<AppState>()
                .fetcher
                .set_web_credential_source(Box::new(move |id| web_session_of(&session_app, id)));

            let progress_app = app.handle().clone();
            google_cloud_web::start(app.handle().clone());
            app.state::<AppState>().fetcher.set_progress_sink(Arc::new(move |views| {
                emit_channels(&progress_app, &views);
            }));

            // 启动时注册已保存的全局快捷键;被占用不拦启动,但要把原因留给界面
            // (之前只 eprintln,绿色版从托盘启动时没人看得到 stderr —— 报告 P2-14)
            if !config.hotkey.is_empty() {
                let st = app.state::<AppState>();
                if let Err(e) = apply_hotkey(app.handle(), &st, &config.hotkey) {
                    eprintln!("全局快捷键注册失败: {e}");
                    startup_error = note(startup_error, e);
                }
            }
            if let Some(msg) = startup_error {
                *lock_rw(&app.state::<AppState>().startup_error) = Some(msg);
            }

            let refresh = MenuItem::with_id(app, "refresh", "刷新", true, None::<&str>)?;
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
                    // 正在写客户端配置时立刻退出会留下"改了没记账"的中间态,先等它写完(P1-15)
                    "quit" => quit_when_idle(app),
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let was = w.is_visible().unwrap_or(false);
                            let _ = w.show();
                            let _ = w.set_focus();
                            if !was {
                                let _ = app.emit("window-shown", ());
                            }
                            refresh_if_stale(app, 60);
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
                                if w.hide().is_ok() { let _ = app.emit("window-hidden", ()); }
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                                let _ = app.emit("window-shown", ());
                                refresh_if_stale(app, 60);
                            }
                        }
                    }
                })
                .build(app)?;

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { poll_loop(handle).await });
            // 签到节拍器:独立于取数轮询(签到是写操作,节奏要分开)。
            // **必须在原有 setup 里起** —— Tauri 的 Builder::setup 只有一个槽位,
            // 再加一个会把 manage(AppState) 那套顶掉,应用会在第一条命令就 panic。
            spawn_checkin_ticker(app.handle().clone());

    // 引用索引对账(设计稿 product-vault-ref-reconcile-2026-10-21):
    // 启动后台按真实指针重抄一遍索引 —— 无论上次是怎么走偏的,下次启动自己就好了。
    spawn_vault_ref_rebuild(app.handle().clone());

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
        // WorkBuddy 登录窗被用户自己关掉(没抓到会话)时,要让界面把「登录中」收回去,
        // 否则会一直卡在等令牌的样子。抓到会话那一次是我们主动关窗的,由 WB_LOGIN_DONE
        // 标位区分,不该再报一次"已取消"。观察者也靠 WB_LOGIN_CLOSED 收工 ——
        // 它因此不用去问一个正在销毁的窗口。
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                // WorkBuddy 登录窗被用户自己关掉(没抓到会话)时,要让界面把「登录中」
                // 收回去。抓到会话那一次是我们主动关窗的,由 DONE 标位区分。
                if window.label() == WB_LOGIN_LABEL {
                    WB_LOGIN_CLOSED.store(true, std::sync::atomic::Ordering::Relaxed);
                    if WB_LOGIN_DONE.swap(false, std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let _ = window.emit(
                        "workbuddy-login",
                        serde_json::json!({ "ok": false, "cancelled": true }),
                    );
                }
                // Qoder 网页登录窗同一条规矩(国际版/国内版共用这套判定,按 label 反推
                // 渠道,拆成两个分支而不是两个闭包 —— on_window_event 只能挂一次)
                if let Some(id) = qoder_label_id(window.label()) {
                    let (done, closed, _) = qoder_login_flags(id);
                    closed.store(true, std::sync::atomic::Ordering::Relaxed);
                    if done.swap(false, std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let _ = window.emit(
                        "qoderweb-login",
                        serde_json::json!({ "ok": false, "cancelled": true, "id": id }),
                    );
                }
                // 豆包登录窗:同一条规矩,逻辑在模块里(标位与事件都在那侧)
                if window.label() == doubao_web::LOGIN_LABEL {
                    doubao_web::on_login_window_destroyed(window);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            config_cmds::get_channels,
            config_cmds::home_view,
            config_cmds::get_config,
            config_cmds::set_config,
            config_cmds::set_key,
            config_cmds::delete_key,
            weblogin_cmds::workbuddy_login,
            weblogin_cmds::doubao_login,
            window_cmds::channel_changes,
            window_cmds::set_autostart,
            window_cmds::set_pin,
            window_cmds::set_tray_icon,
            window_cmds::set_tray_tooltip,
            window_cmds::tray_icon_size,
            config_cmds::key_hint,
            window_cmds::window_cmd,
            update_cmds::check_update,
            update_cmds::install_update,
            update_cmds::skip_update_version,
            window_cmds::save_window_pos,
            update_cmds::app_version,
            update_cmds::take_startup_error,
            model_cmds::get_model_catalog,
            gateway::gateway_status,
            gateway::gateway_start,
            gateway::gateway_stop,
            gateway::gateway_restart,
            gateway::gateway_secret,
            gateway::gateway_publish,
            gateway::gateway_update_channel,
            gateway::gateway_refresh_channel,
            gateway::gateway_unpublish,
            gateway::gateway_test_channel,
            gateway::gateway_test_chat,
            gateway::gateway_cancel_test_chat,
            gateway::gateway_diagnostics,
            gateway::gateway_clear_diagnostics,
            model_cmds::inject_preview,
            model_cmds::inject_apply,
            model_cmds::inject_remove,
            model_cmds::inject_state,
            model_cmds::inject_rollback,
            model_cmds::copy_model_bundle,
            model_cmds::model_info,
            model_cmds::copy_text,
            download::download_link,
            model_cmds::open_path,
            model_cmds::open_url,
            model_cmds::fetch_promo_feed,
            model_cmds::open_promo_url,
            model_cmds::diag_log_info,
            model_cmds::diag_log_tail,
            model_cmds::open_log_dir,
            model_cmds::diag_log_clear,
            model_cmds::diag_log_verbose,
            model_cmds::diag_export,
            checkin_cmds::checkin_state,
            checkin_cmds::checkin_now,
            checkin_cmds::checkin_history,
            weblogin_cmds::qoderweb_login,
            checkin_cmds::credit_grants,
            model_cmds::key_reveal,
            model_cmds::key_copy,
            vault_cmds::vault_list,
            vault_cmds::vault_save,
            vault_cmds::vault_delete,
            vault_cmds::vault_reveal,
            vault_cmds::vault_copy,
            // 渠道密钥 ↔ 密钥箱(设计稿 product-vault-channel-bridge)
            key_ref_cmds::key_source,
            key_ref_cmds::set_key_ref,
            key_ref_cmds::clear_key_ref,
            key_ref_cmds::vault_ref_map,
            diag_cmds::probe_channel,
            diag_cmds::run_diagnostic,
            custom_cmds::custom_test,
            custom_cmds::custom_add,
            custom_cmds::custom_remove,
            sync_status,
            sync_preview,
            sync_create,
            sync_join,
            sync_publish_now,
            sync_read_group,
            sync_recovery_candidates,
            sync_restore_group,
            sync_set_enabled,
            sync_set_channels,
            sync_set_interval,
            sync_set_platform_label,
            sync_rotate,
            sync_forget,
            sync_disconnect_github,
            sync_oauth_start,
            sync_oauth_poll,
            sync_oauth_cancel,
            sync_set_oauth_client_id,
            sync_qr_svg,
            codex_reset_cmd::codex_reset_advice,
            codex_reset_cmd::codex_reset_profile_get,
            codex_reset_cmd::codex_reset_profile_set,
            google_cloud_web::google_cloud_login,
            google_cloud_web::google_cloud_refresh,
            google_cloud_web::google_cloud_expiry_set,
            google_cloud_web::google_cloud_disconnect,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            // 以前是 expect:abort 之后连一行字都看不到(GUI 子系统 + panic=abort)
            let msg = report_fatal("运行期", &e.to_string());
            eprintln!("{msg}");
            std::process::exit(1);
        });
}

#[cfg(test)]
mod vault_url_tests {
    use super::vault_url;
    #[test]
    fn default_records_are_protected_without_blocking_other_keys() {
        let path = std::env::temp_dir().join(format!("vault-default-test-{}.db", tokenscope_core::vault::new_id()));
        let store = tokenscope_core::store::Store::open(&path).unwrap();
        store.set_setting("vault_default_channel:4sapi", "default-1").unwrap();
        assert_eq!(super::vault_default_channel(&store, "default-1").unwrap().as_deref(), Some("4sapi"));
        assert!(super::ensure_vault_mutable(&store, "default-1").is_err());
        assert!(super::ensure_vault_mutable(&store, "normal-key").is_ok());
        drop(store);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn accepts_plain_addresses_and_rejects_embedded_credentials() {
        assert_eq!(vault_url(" https://example.com/v1 ").unwrap(), "https://example.com/v1");
        assert_eq!(vault_url("").unwrap(), "");
        assert_eq!(vault_url("https://example.com/v1?api-version=1").unwrap(), "https://example.com/v1?api-version=1");
        assert!(vault_url("https://user:password@example.com/v1").is_err());
        assert!(vault_url("https://example.com/v1?key=secret").is_err());
        assert!(vault_url("javascript:alert(1)").is_err());
    }
}

#[cfg(test)]
mod tray_digest_tests {
    use super::tray_icon_digest;

    /// 去重的前提:同尺寸、同像素必须得到同一个指纹;任一不同都必须变。
    #[test]
    fn digest_matches_only_identical_frames() {
        let a = [1u8, 2, 3, 4];
        assert_eq!(tray_icon_digest(8, &a), tray_icon_digest(8, &a));
        assert_ne!(tray_icon_digest(8, &a), tray_icon_digest(16, &a));
        assert_ne!(tray_icon_digest(8, &a), tray_icon_digest(8, &[1, 2, 3, 5]));
        assert_ne!(tray_icon_digest(8, &a), tray_icon_digest(8, &[1, 2, 3]));
    }
}
