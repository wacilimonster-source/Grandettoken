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
mod sync_group_cmds;
mod sync_oauth_cmds;
mod download;
mod gateway;
mod gateway_ext;
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
    sync_preview, sync_publish_now, sync_qr_svg, sync_read_group,
    sync_set_channels, sync_set_enabled, sync_set_interval, sync_set_platform_label, sync_status,
};
pub use sync_group_cmds::{
    sync_create, sync_disconnect_github, sync_forget, sync_join, sync_recovery_candidates,
    sync_restore_group, sync_rotate,
};
pub use sync_oauth_cmds::{sync_oauth_start, sync_oauth_poll, sync_oauth_cancel, sync_set_oauth_client_id};

pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub read_store: Arc<Mutex<Store>>,
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
mod gateway_test_cmds;
mod checkin_scheduler;
mod checkin_history_cmd;
mod diag_log_cmds;
mod promo_cmds;
mod os_cmds;
mod inject_cmds;
mod startup;
mod hibernate;
mod tray;
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
pub use diag_log_cmds::DiagLogInfo;
use os_cmds::copy_to_clipboard;
use checkin_scheduler::*;
use model_cmds::source_base;
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
    let now = now_ts();
    let cfg = {
        let state = app.state::<AppState>();
        // 先落到局部变量:直接 return 这个表达式的话,MutexGuard 的临时值
        // 会在 `state` 之后才析构,借用比 state 活得久(E0597)。
        let c = lock_rw(&state.config).clone();
        c
    };
    if let Some(gw) = app.try_state::<gateway::Gateway>() {
        let mut signals = std::collections::BTreeMap::new();
        for v in views {
            let remaining_ratio = if v.limited {
                Some(0.0)
            } else if let (Some(r), Some(t)) = (v.remaining, v.total) {
                if t > 0.0 {
                    Some((r / t).clamp(0.0, 1.0))
                } else {
                    None
                }
            } else {
                v.windows
                    .iter()
                    .find(|w| w.main)
                    .or_else(|| v.windows.first())
                    .map(|w| (w.remain_percent / 100.0).clamp(0.0, 1.0))
            };
            let soonest_expiry_ts = v
                .expiring
                .iter()
                .filter(|e| e.amount > 0.0 && e.at > now)
                .map(|e| e.at)
                .min();
            let has_expiring_7d = soonest_expiry_ts.is_some_and(|at| at <= now + 7 * 86_400);
            signals.insert(
                v.id.to_ascii_lowercase(),
                tokenscope_core::gateway::routing::ChannelSignal {
                    provider_id: v.id.clone(),
                    remaining_ratio,
                    has_expiring_7d,
                    soonest_expiry_ts,
                    is_deepseek: v.id.eq_ignore_ascii_case("deepseek"),
                    updated_at: now,
                },
            );
        }
        gw.update_channel_signals(signals);
    }
    tray::publish(app, views);
    if hibernate::has_live_webview(app) {
        if let Some(window) = app.get_webview_window("main") {
            let home = home_view_of(&cfg, views, now);
            let _ = window.emit("channels-updated", ChannelsPush { views: views.to_vec(), home });
        }
    }
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
    // 落盘成功后才发布设置，避免保存失败时后台继续使用已回滚的开关。
    let mut next = lock_rw(&state.config).clone();
    mutate(&mut next);
    let json = serde_json::to_string(&next).map_err(|e| format!("配置序列化失败: {e}"))?;
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        let s = lock_rw(&store);
        s.set_setting("config", &json)
    })
    .await
    .map_err(|e| format!("保存设置失败: {e}"))??;
    *lock_rw(&state.config) = next;
    Ok(())
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
pub(crate) async fn with_store<T: Send + 'static>(
    store: Arc<Mutex<Store>>,
    operation: impl FnOnce(&Store) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(move || operation(&lock_rw(&store)))
        .await.map_err(|e| format!("数据库任务失败: {e}"))?
}

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
            hibernate::ensure_and_show_main_window(app);
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
                    hibernate::toggle_main_window(app);
                })
                .build(),
        )
        // 公钥与端点都在 tauri.conf.json 的 plugins.updater 里,插件自己读配置
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(startup::setup)
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
            config_cmds::get_cached_channels,
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
            gateway_test_cmds::gateway_test_chat,
            gateway_test_cmds::gateway_cancel_test_chat,
            gateway_test_cmds::gateway_diagnostics,
            gateway_test_cmds::gateway_clear_diagnostics,
            gateway_ext::gateway_save_group,
            gateway_ext::gateway_delete_group,
            gateway_ext::gateway_set_masking,
            gateway_ext::gateway_usage_summary,
            gateway_ext::gateway_clear_usage,
            gateway_ext::gateway_inject_clients,
            gateway_ext::gateway_inject_drift,
            gateway_ext::gateway_inject_preview,
            gateway_ext::gateway_inject_apply,
            gateway_ext::gateway_inject_remove,
            inject_cmds::inject_preview,
            inject_cmds::inject_apply,
            inject_cmds::inject_remove,
            inject_cmds::inject_state,
            inject_cmds::inject_rollback,
            model_cmds::copy_model_bundle,
            model_cmds::model_info,
            os_cmds::copy_text,
            download::download_link,
            os_cmds::open_path,
            os_cmds::open_url,
            promo_cmds::fetch_promo_page,
            promo_cmds::open_promo_url,
            diag_log_cmds::diag_log_info,
            diag_log_cmds::diag_log_tail,
            diag_log_cmds::open_log_dir,
            diag_log_cmds::diag_log_clear,
            diag_log_cmds::diag_log_verbose,
            diag_log_cmds::diag_export,
            checkin_cmds::checkin_state,
            checkin_cmds::checkin_now,
            checkin_history_cmd::checkin_history,
            weblogin_cmds::qoderweb_login,
            checkin_cmds::credit_grants,
            os_cmds::key_reveal,
            os_cmds::key_copy,
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
            google_cloud_web::google_cloud_disconnect,
        ])
        .build(tauri::generate_context!())
        .unwrap_or_else(|e| {
            // 以前是 expect:abort 之后连一行字都看不到(GUI 子系统 + panic=abort)
            let msg = report_fatal("运行期", &e.to_string());
            eprintln!("{msg}");
            std::process::exit(1);
        })
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { code: None, api, .. } = event {
                if !hibernate::has_live_webview(app) {
                    api.prevent_exit();
                }
            }
        });
}
