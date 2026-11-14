//! 独立 WebView2 profile 保留 Google 登录。仅结构化赠金通过导航桥返回本机。
use base64::Engine;
use std::sync::{LazyLock, Mutex};

use tauri::{Emitter, Manager};
use tokenscope_core::{fetch::now_ts, google_cloud as gc};

use crate::{lock_rw, AppState};

const LABEL: &str = "google-cloud-credits";
const SCRIPT: &str = include_str!("google_cloud_capture.js");

#[derive(Default)]
struct Session {
    nonce: String,
    expected: Option<String>,
    interactive: bool,
    complete: bool,
    started: i64,
}
static SESSION: LazyLock<Mutex<Session>> = LazyLock::new(|| Mutex::new(Session::default()));
static OPEN_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn url(account: Option<&str>) -> Result<tauri::Url, String> {
    let path = match account {
        Some(id) if gc::valid_account(id) => format!("{id}/credits/all"),
        Some(_) => return Err("结算账号格式无效".into()),
        None => "credits".into(),
    };
    format!("https://console.cloud.google.com/billing/{path}?hl=en")
        .parse()
        .map_err(|_| "结算地址无效".into())
}

fn script(nonce: &str) -> String {
    SCRIPT.replace("__CAPTURE_NONCE__", nonce)
}

fn begin(account: Option<String>, interactive: bool) -> Result<String, String> {
    let nonce = tokenscope_core::sync::crypto::random_source().map_err(|e| e.to_string())?;
    *lock_rw(&SESSION) = Session {
        nonce: nonce.clone(),
        expected: account,
        interactive,
        complete: false,
        started: now_ts(),
    };
    Ok(nonce)
}

fn notify(app: &tauri::AppHandle, error: &str) {
    let _ = app.emit("google-cloud-result", serde_json::json!({"error":error}));
}

fn receive(app: &tauri::AppHandle, target: &tauri::Url) {
    let nonce = target.path().strip_prefix("/google-cloud/").unwrap_or("");
    let Some(raw) = target.fragment() else {
        return;
    };
    if raw.len() > 150_000 {
        return;
    }
    let decoded = match base64::engine::general_purpose::STANDARD.decode(raw) {
        Ok(s) => s,
        Err(_) => return,
    };
    let data = match serde_json::from_slice::<gc::Capture>(&decoded) {
        Ok(d) => d,
        Err(_) => return,
    };
    // 与 disconnect / 开新窗口共用 session 锁，迟到的旧页面无法重新接管账号。
    let mut session = lock_rw(&SESSION);
    if nonce != session.nonce || session.complete {
        return;
    }
    let st = app.state::<AppState>();
    if lock_rw(&st.config)
        .hidden_channels
        .iter()
        .any(|id| id == gc::ID)
    {
        return;
    }
    let result = gc::capture(
        &lock_rw(&st.store),
        data,
        session.expected.as_deref(),
        now_ts(),
    );
    match result {
        Ok(()) => {
            session.complete = true;
            let interactive = session.interactive;
            let completed_nonce = session.nonce.clone();
            drop(session);
            st.fetcher.invalidate();
            let handle = app.clone();
            crate::on_main(app, move || {
                let current = lock_rw(&SESSION);
                if current.nonce != completed_nonce || !current.complete {
                    return;
                }
                if let Some(w) = handle.get_webview_window(LABEL) {
                    if let Err(e) = w.hide() {
                        notify(&handle, &format!("余额已保存，隐藏结算窗口失败: {e}"));
                    }
                }
            });
            let _ = app.emit(
                "google-cloud-result",
                serde_json::json!({"ok":true,"interactive":interactive}),
            );
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::refresh_now(&handle).await;
            });
        }
        Err(e) => {
            session.complete = true;
            if let Some(ref account) = session.expected {
                if let Err(write_error) = gc::set_error(&lock_rw(&st.store), account) {
                    notify(app, &write_error);
                }
            }
            drop(session);
            st.fetcher.invalidate();
            notify(app, &e);
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::refresh_now(&handle).await;
            });
        }
    }
}

async fn open(
    app: &tauri::AppHandle,
    account: Option<String>,
    visible: bool,
) -> Result<(), String> {
    let _gate = OPEN_GATE.lock().await;
    let st = app.state::<AppState>();
    if lock_rw(&st.config)
        .hidden_channels
        .iter()
        .any(|id| id == gc::ID)
    {
        return Err("渠道已隐藏，请恢复显示后连接".into());
    }
    if !visible && gc::load(&lock_rw(&st.store))?.selected != account {
        return Err("结算账号已变化，已取消旧刷新请求".into());
    }
    let target = url(account.as_deref())?;
    // 先使旧页面的导航和隐藏回调失效，失败重试沿用本次开始时间节流。
    *lock_rw(&SESSION) = Session {
        complete: true,
        started: now_ts(),
        ..Default::default()
    };
    if let Some(w) = app.get_webview_window(LABEL) {
        // 每次重建使 initialization_script 的 nonce 与本轮一致。
        w.destroy()
            .map_err(|e| format!("关闭上次结算窗口失败: {e}"))?;
    }
    gc::wait_for_window_release(
        || app.get_webview_window(LABEL).is_some(),
        std::time::Duration::from_secs(5),
    )
    .await?;
    let profile = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("google-cloud-profile");
    let handle = app.clone();
    let nonce = begin(account, visible)?;
    let result = tauri::WebviewWindowBuilder::new(app, LABEL, tauri::WebviewUrl::External(target))
        .title("Google Cloud 赠金 · 登录并选择结算账号")
        .inner_size(1150.0, 800.0)
        .min_inner_size(800.0, 600.0)
        .data_directory(profile)
        .visible(visible)
        .always_on_top(visible)
        .initialization_script(script(&nonce))
        .on_navigation(move |target| {
            if target.host_str() == Some("grandettoken.invalid") {
                receive(&handle, target);
                return false;
            }
            target.scheme() == "https"
        })
        .build()
        .map_err(|e| format!("Google 结算窗口打不开: {e}"));
    if let Err(error) = result {
        let mut session = lock_rw(&SESSION);
        if session.nonce == nonce {
            session.complete = true;
        }
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
pub async fn google_cloud_login(app: tauri::AppHandle) -> Result<(), String> {
    open(&app, None, true).await
}

#[tauri::command]
pub async fn google_cloud_refresh(app: tauri::AppHandle) -> Result<(), String> {
    let st = app.state::<AppState>();
    if lock_rw(&st.config)
        .hidden_channels
        .iter()
        .any(|id| id == gc::ID)
    {
        return Err("渠道已隐藏，请恢复显示后刷新".into());
    }
    let account = gc::load(&lock_rw(&st.store))?
        .selected
        .ok_or("请先连接 Google Cloud 结算账号")?;
    open(&app, Some(account), false).await
}

#[tauri::command]
pub async fn google_cloud_expiry_set(
    app: tauri::AppHandle,
    account: String,
    credit: String,
    at: Option<i64>,
) -> Result<(), String> {
    let st = app.state::<AppState>();
    gc::set_expiry(&lock_rw(&st.store), &account, &credit, at)?;
    st.fetcher.invalidate();
    crate::refresh_now(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn google_cloud_disconnect(app: tauri::AppHandle) -> Result<(), String> {
    let _gate = OPEN_GATE.lock().await;
    let st = app.state::<AppState>();
    {
        let mut session = lock_rw(&SESSION);
        gc::disconnect(&lock_rw(&st.store))?;
        *session = Session::default();
    }
    if let Some(w) = app.get_webview_window(LABEL) {
        w.destroy()
            .map_err(|e| format!("已停止连接，关闭结算窗口失败: {e}"))?;
    }
    st.fetcher.invalidate();
    crate::refresh_now(&app).await;
    Ok(())
}

/// 后台只在已连接且未隐藏时打开赠金页。页面加载失败会沿用旧余额并标记失败。
pub fn start(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let (hidden, interval, account, updated) = {
                let st = app.state::<AppState>();
                let cfg = lock_rw(&st.config).clone();
                let ledger = gc::load(&lock_rw(&st.store));
                let account = ledger.as_ref().ok().and_then(|l| l.selected.clone());
                let updated = ledger
                    .ok()
                    .and_then(|l| {
                        account
                            .as_ref()
                            .and_then(|id| l.accounts.get(id))
                            .map(|a| a.updated_at)
                    })
                    .unwrap_or(0);
                (
                    cfg.hidden_channels.iter().any(|id| id == gc::ID),
                    cfg.idle_interval_sec.max(60) as i64,
                    account,
                    updated,
                )
            };
            let (nonce, started, pending, interactive) = {
                let s = lock_rw(&SESSION);
                (
                    s.nonce.clone(),
                    s.started,
                    !s.complete && !s.nonce.is_empty(),
                    s.interactive,
                )
            };
            if hidden {
                let _gate = OPEN_GATE.lock().await;
                if !lock_rw(&app.state::<AppState>().config)
                    .hidden_channels
                    .iter()
                    .any(|id| id == gc::ID)
                {
                    continue;
                }
                // 隐藏中使迟到的页面结果失效，并终止网页网络活动。
                *lock_rw(&SESSION) = Session::default();
                if let Some(w) = app.get_webview_window(LABEL) {
                    let _ = w.destroy();
                }
            } else if pending && app.get_webview_window(LABEL).is_none() {
                let mut current = lock_rw(&SESSION);
                if current.nonce == nonce {
                    current.complete = true;
                }
            } else if pending && !interactive && now_ts() - started >= 90 {
                {
                    let mut current = lock_rw(&SESSION);
                    if current.nonce != nonce || current.complete {
                        continue;
                    }
                    current.complete = true;
                }
                if let Some(ref id) = account {
                    let st = app.state::<AppState>();
                    if let Err(e) = gc::set_error(&lock_rw(&st.store), id) {
                        notify(&app, &e);
                    }
                    st.fetcher.invalidate();
                    crate::refresh_now(&app).await;
                }
                notify(
                    &app,
                    "赠金表格尚未读取成功，请检查登录、清除表格过滤条件并显示全部记录",
                );
            } else if !pending && account.is_some() && now_ts() - updated.max(started) >= interval {
                if let Err(e) = open(&app, account, false).await {
                    notify(&app, &e);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
}
