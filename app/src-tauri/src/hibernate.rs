//! The tray and backend tasks outlive the main window. All window transitions run on the UI thread.
use std::{sync::Mutex, time::Duration};

use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};
use tokio::{sync::Notify, time::Instant};

use crate::{google_cloud_web, lock_rw, note, refresh_if_stale, AppState};

const IDLE: Duration = Duration::from_secs(300);
const SESSION_IDS: [&str; 4] = ["workbuddy", "doubao", "qoder", "qoder-cn"];

#[derive(Clone, Copy, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum Form {
    #[default]
    Panel,
    Compact,
    Pill,
}

#[derive(Clone, Copy, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, serde::Deserialize, serde::Serialize)]
struct Dock {
    side: Side,
    y: i32,
    open: bool,
}

#[derive(Clone, Copy, Default, serde::Deserialize, serde::Serialize)]
pub(crate) struct Presentation {
    form: Form,
    dock: Option<Dock>,
}

#[derive(Clone, Copy)]
struct Geometry {
    size: PhysicalSize<u32>,
    position: PhysicalPosition<i32>,
    pinned: bool,
}

#[derive(Default)]
enum Phase {
    #[default]
    Live,
    Hidden(Instant),
    Released {
        geometry: Geometry,
        waiting: bool,
    },
}

#[derive(Default)]
pub(crate) struct Hibernate {
    phase: Mutex<Phase>,
    presentation: Mutex<Presentation>,
    web_sessions: Mutex<[Option<bool>; SESSION_IDS.len()]>,
    changed: Notify,
}

pub(crate) fn presentation_of(app: &AppHandle, value: Presentation) {
    *lock_rw(&app.state::<Hibernate>().presentation) = value;
}

pub(crate) fn has_live_webview(app: &AppHandle) -> bool {
    !app.try_state::<Hibernate>()
        .is_some_and(|state| matches!(*lock_rw(&state.phase), Phase::Released { .. }))
}

pub(crate) fn observe_session(app: &AppHandle, id: &str, present: Option<bool>) {
    if present.is_some() {
        if let Some(index) = SESSION_IDS.iter().position(|candidate| *candidate == id) {
            lock_rw(&app.state::<Hibernate>().web_sessions)[index] = present;
        }
    }
}

fn report(app: &AppHandle, operation: &str, error: &str) {
    let message = format!("省内存模式：{operation}失败（{error}）");
    tokenscope_core::diaglog::write_keyed("window", "hibernate", &message);
    let state = app.state::<AppState>();
    let mut startup_error = lock_rw(&state.startup_error);
    *startup_error = note(startup_error.take(), message);
}

fn enabled(app: &AppHandle) -> bool {
    lock_rw(&app.state::<AppState>().config).hibernate_webview_on_hide
}

pub(crate) fn arm_hidden(app: &AppHandle) {
    let state = app.state::<Hibernate>();
    let mut phase = lock_rw(&state.phase);
    if !matches!(*phase, Phase::Released { .. }) {
        *phase = if enabled(app) {
            Phase::Hidden(Instant::now() + IDLE)
        } else {
            Phase::Live
        };
        state.changed.notify_one();
    }
}

pub(crate) fn config_changed(app: &AppHandle) {
    dispatch(app, |app| {
        if app
            .get_webview_window("main")
            .is_some_and(|window| window.is_visible().ok() == Some(false))
        {
            arm_hidden(app);
        }
    });
}

pub(crate) fn hide_main_window(app: &AppHandle, window: &WebviewWindow) -> tauri::Result<()> {
    window.hide()?;
    arm_hidden(app);
    let _ = window.emit("window-hidden", ());
    Ok(())
}

fn show(app: &AppHandle, window: &WebviewWindow, max_age_sec: i64) -> tauri::Result<()> {
    let was_visible = window.is_visible()?;
    window.show()?;
    window.set_focus()?;
    if !was_visible {
        let _ = window.emit("window-shown", ());
    }
    refresh_if_stale(app, max_age_sec);
    Ok(())
}

pub(crate) fn ensure_and_show_main_window(app: &AppHandle) {
    dispatch(app, ensure_on_ui);
}

pub(crate) fn toggle_main_window(app: &AppHandle) {
    dispatch(app, |app| {
        if has_live_webview(app) {
            if let Some(window) = app
                .get_webview_window("main")
                .filter(|w| w.is_visible().ok() == Some(true))
            {
                if let Err(error) = hide_main_window(app, &window) {
                    report(app, "隐藏主窗口", &error.to_string());
                }
                return;
            }
        }
        ensure_on_ui(app);
    });
}

fn ensure_on_ui(app: &AppHandle) {
    if let Err(error) = ensure(app) {
        report(app, "显示主窗口", &error);
    }
}

fn dispatch(app: &AppHandle, action: impl FnOnce(&AppHandle) + Send + 'static) -> bool {
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || action(&handle)) {
        report(app, "调度主窗口", &error.to_string());
        return false;
    }
    true
}

fn ensure(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Hibernate>();
    {
        let mut phase = lock_rw(&state.phase);
        match &mut *phase {
            Phase::Live | Phase::Hidden(_) => {
                *phase = Phase::Live;
                state.changed.notify_one();
            }
            Phase::Released { geometry, waiting } => {
                if !*waiting {
                    *waiting = true;
                    let presentation = *lock_rw(&state.presentation);
                    tauri::async_runtime::spawn(reopen(app.clone(), *geometry, presentation));
                }
                return Ok(());
            }
        }
    }
    let window = app.get_webview_window("main").ok_or("主窗口不存在")?;
    show(app, &window, 60).map_err(|e| e.to_string())
}

async fn reopen(app: AppHandle, geometry: Geometry, presentation: Presentation) {
    let limit = Instant::now() + Duration::from_secs(3);
    while app.get_webview_window("main").is_some() && Instant::now() < limit {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let result = if app.get_webview_window("main").is_some() {
        Err("旧主窗口的标签仍未释放".into())
    } else {
        // Windows WebView2 creation can deadlock if run inside a window event or synchronous IPC.
        let handle = app.clone();
        tokio::task::spawn_blocking(move || recreate(&handle, geometry, presentation))
            .await
            .map_err(|e| e.to_string())
            .and_then(|result| result)
    };
    dispatch(&app, move |app| {
        let state = app.state::<Hibernate>();
        match result {
            Ok(window) => {
                *lock_rw(&state.phase) = Phase::Live;
                crate::checkin_scheduler::CHECKIN_WAKE.notify_one();
                if let Err(error) = show(app, &window, 0) {
                    report(app, "显示重建的主窗口", &error.to_string());
                }
            }
            Err(error) => {
                if let Phase::Released { waiting, .. } = &mut *lock_rw(&state.phase) {
                    *waiting = false;
                }
                report(app, "重建主窗口", &error);
            }
        }
    });
}

fn recreate(
    app: &AppHandle,
    geometry: Geometry,
    mut presentation: Presentation,
) -> Result<WebviewWindow, String> {
    let Geometry {
        mut position,
        mut size,
        pinned,
    } = geometry;
    if let Some(monitor) = app
        .monitor_from_point(
            f64::from(position.x) + f64::from(size.width) / 2.0,
            f64::from(position.y) + f64::from(size.height) / 2.0,
        )
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten())
    {
        let area = monitor.work_area();
        size.width = size.width.min(area.size.width);
        size.height = size.height.min(area.size.height);
        let x_max = area.position.x + (area.size.width - size.width) as i32;
        let y_max = area.position.y + (area.size.height - size.height) as i32;
        position.x = position.x.clamp(area.position.x, x_max);
        position.y = position.y.clamp(area.position.y, y_max);
        if let Some(dock) = &mut presentation.dock {
            position.x = match dock.side {
                Side::Left => area.position.x,
                Side::Right => x_max,
            };
            dock.y = dock
                .y
                .clamp(area.position.y, area.position.y + area.size.height as i32);
        }
    }
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == "main")
        .ok_or("找不到主窗口配置")?;
    let script = format!(
        "window.__GT_RESUME_MAIN = {};",
        serde_json::to_string(&presentation).map_err(|e| e.to_string())?
    );
    let window = tauri::WebviewWindowBuilder::from_config(app, config)
        .and_then(|builder| {
            builder
                .initialization_script(script)
                .always_on_top(pinned)
                .visible(false)
                .resizable(false)
                .build()
        })
        .map_err(|e| e.to_string())?;
    if let Err(error) = window
        .set_size(size)
        .and_then(|()| window.set_position(position))
    {
        // 新标签已注册，后续唤回应显示这个窗口。尺寸失败时仍可由界面重排恢复。
        report(app, "恢复窗口位置或尺寸", &error.to_string());
    }
    Ok(window)
}

/// Whether an extra webview window must keep the main WebView alive.
///
/// The Google Cloud credit reader is a background capture window: it is hidden
/// (not destroyed) after each refresh and recreated on demand by its scheduler.
/// Keeping it counted here would make the memory-saving mode unreachable for
/// every Google Cloud user. A *visible* Google Cloud window is an interactive
/// login and must still be preserved.
fn window_blocks_release(label: &str, visible: Option<bool>) -> bool {
    if label == "main" {
        return false;
    }
    if label == google_cloud_web::LABEL {
        return visible != Some(false);
    }
    true
}

fn needs_webview(app: &AppHandle, state: &Hibernate) -> bool {
    if app
        .webview_windows()
        .iter()
        .any(|(label, window)| window_blocks_release(label, window.is_visible().ok()))
    {
        return true;
    }
    let data = app.state::<AppState>();
    let observed = *lock_rw(&state.web_sessions);
    let last_views = data.fetcher.last_views();
    let config = lock_rw(&data.config);
    SESSION_IDS.iter().zip(observed).any(|(id, result)| {
        if config.hidden_channels.iter().any(|hidden| hidden == id) {
            return false;
        }
        match result {
            Some(present) => present,
            // 启动尚未出数时先保留窗口。已出数但渠道从未配置时，不必等到会话读成功。
            None => last_views.as_ref().is_none_or(|views| {
                views.iter().any(|view| view.id == *id && view.has_key)
            }),
        }
    })
}

fn release(app: &AppHandle, deadline: Instant) -> tauri::Result<()> {
    let state = app.state::<Hibernate>();
    let mut phase = lock_rw(&state.phase);
    if !matches!(*phase, Phase::Hidden(at) if at == deadline && Instant::now() >= at) {
        return Ok(());
    }
    *phase = Phase::Live;
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    if !enabled(app) || window.is_visible()? {
        return Ok(());
    }
    if needs_webview(app, &state) {
        *phase = Phase::Hidden(Instant::now() + Duration::from_secs(60));
        return Ok(());
    }
    let geometry = Geometry {
        size: window.inner_size()?,
        position: window.outer_position()?,
        pinned: window.is_always_on_top()?,
    };
    *phase = Phase::Released {
        geometry,
        waiting: false,
    };
    drop(phase);
    // Release the background Google Cloud capture window as well: it holds its
    // own WebView2 processes, so destroying only `main` would neither reach the
    // memory target nor clear the guard above. Its scheduler recreates it on the
    // next refresh; a visible window means an interactive login, so keep it.
    if let Some(background) = app.get_webview_window(google_cloud_web::LABEL) {
        match background.is_visible() {
            Ok(false) => {
                if let Err(error) = background.destroy() {
                    report(app, "释放 Google Cloud 后台窗口", &error.to_string());
                }
            }
            _ => {
                *lock_rw(&state.phase) = Phase::Hidden(Instant::now() + Duration::from_secs(60));
                return Ok(());
            }
        }
    }
    if let Err(error) = window.destroy() {
        *lock_rw(&state.phase) = Phase::Live;
        return Err(error);
    }
    Ok(())
}

pub(crate) async fn run(app: AppHandle) {
    let state = app.state::<Hibernate>();
    loop {
        let deadline = match *lock_rw(&state.phase) {
            Phase::Hidden(at) => Some(at),
            _ => None,
        };
        let Some(deadline) = deadline else {
            state.changed.notified().await;
            continue;
        };
        tokio::select! {
            _ = state.changed.notified() => {},
            _ = tokio::time::sleep_until(deadline) => {
                if !dispatch(&app, move |app| {
                    if let Err(error) = release(app, deadline) {
                        report(app, "释放主窗口", &error.to_string());
                    }
                    app.state::<Hibernate>().changed.notify_one();
                }) {
                    return;
                }
                // 唯有 UI 线程能确定是否释放或延期，等它提交状态再读到期时刻。
                state.changed.notified().await;
            }
        }
    }
}
