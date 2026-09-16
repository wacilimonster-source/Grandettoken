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
}

#[tauri::command]
async fn get_channels(state: State<'_, AppState>) -> Result<Vec<ChannelView>, String> {
    // 先克隆 Arc 再 await:不让 State 的借用跨越 await 点
    let store = state.store.clone();
    let fetcher = state.fetcher.clone();
    Ok(fetcher.fetch_all(&store).await)
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Config {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
fn set_config(state: State<'_, AppState>, config: Config) -> Result<(), String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    config.save(&store)?;
    drop(store);
    *state.config.lock().unwrap() = config;
    Ok(())
}

#[tauri::command]
fn set_key(id: String, key: String) -> Result<(), String> {
    if providers::find(&id).is_none() {
        return Err(format!("未知渠道: {id}"));
    }
    secrets::set(&id, &key)
}

#[tauri::command]
fn delete_key(id: String) -> Result<(), String> {
    secrets::delete(&id)
}

/// 只回显尾 4 位,用于界面确认存的是哪把 key,不泄露完整密钥。
#[tauri::command]
fn key_hint(id: String) -> Option<String> {
    secrets::masked(&id)
}

#[tauri::command]
fn get_series(state: State<'_, AppState>, id: String, hours: i64) -> Vec<f64> {
    let from = now_ts() - hours * 3600;
    state
        .store
        .lock()
        .ok()
        .and_then(|s| s.series(&id, from, 24).ok())
        .unwrap_or_default()
}

#[tauri::command]
fn provider_meta() -> Vec<serde_json::Value> {
    providers::PROVIDERS
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "name": p.name,
                "short": p.short,
                "color": p.color,
                "url": p.url,
                "unstable": p.unstable,
            })
        })
        .collect()
}

#[tauri::command]
fn window_cmd(app: tauri::AppHandle, action: String) -> Result<(), String> {
    let win = app.get_webview_window("main").ok_or("窗口不存在")?;
    match action.as_str() {
        "hide" => win.hide().map_err(|e| e.to_string())?,
        "minimize" => win.minimize().map_err(|e| e.to_string())?,
        "pin" => {
            let cur = win.is_always_on_top().unwrap_or(false);
            win.set_always_on_top(!cur).map_err(|e| e.to_string())?;
        }
        "quit" => app.exit(0),
        _ => return Err(format!("未知操作: {action}")),
    }
    Ok(())
}

/// 后台轮询。四个渠道都是账务接口,查询余额不消耗 token,所以间隔可以压得比较短;
/// 唯一约束是对方的限流礼貌性,所以失败时退避。
async fn poll_loop(app: tauri::AppHandle) {
    let mut failures = 0u32;

    loop {
        let (active_sec, idle_sec, backoff_sec) = {
            let state = app.state::<AppState>();
            let c = state.config.lock().unwrap();
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
        let views = fetcher.fetch_all(&store).await;

        if views.iter().any(|v| v.valid) {
            failures = 0;
        } else {
            failures += 1;
        }

        let _ = app.emit("channels-updated", &views);
    }
}

async fn refresh_now(app: &tauri::AppHandle) {
    let (store, fetcher) = {
        let state = app.state::<AppState>();
        (state.store.clone(), state.fetcher.clone())
    };
    let views = fetcher.fetch_all(&store).await;
    let _ = app.emit("channels-updated", &views);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("无法定位数据目录: {e}"))?;
            let store = Store::open(&dir.join("tokenscope.db"))?;
            let config = Config::load(&store);

            let _ = store.prune(now_ts() - 90 * 86400);

            app.manage(AppState {
                store: Arc::new(Mutex::new(store)),
                config: Mutex::new(config),
                fetcher: Arc::new(Fetcher::new()),
            });

            let refresh = MenuItem::with_id(app, "refresh", "立即刷新", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "显示面板", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &refresh, &sep, &quit])?;

            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("TokenScope")
                .menu(&menu)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
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
                            }
                        }
                    }
                })
                .build(app)?;

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { poll_loop(handle).await });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_channels,
            get_config,
            set_config,
            set_key,
            delete_key,
            key_hint,
            get_series,
            provider_meta,
            window_cmd,
        ])
        .run(tauri::generate_context!())
        .expect("TokenScope 启动失败");
}
