//! Tauri 侧的同步命令与后台发布。
//!
//! 这里只做胶水：取数、密钥环、配置落库、事件。加密/合并/传输都在
//! `tokenscope_core::sync`。特别注意：`std::sync::MutexGuard<Store>` 绝不跨
//! `.await` —— 发布与读取都拆成“同步准备 → 异步网络 → 同步提交”。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, Manager, State};
use tokenscope_core::config::Config;
use tokenscope_core::fetch::{now_ts, ChannelView};
use tokenscope_core::secrets;
use tokenscope_core::sync::{
    self, code::SyncCode, crypto, gist::GistClient, model::SyncSnapshot, profile, publish,
    FetchedSources, PublishOutcome, ReadOutcome,
};

use crate::{lock_rw, AppState, APP_VERSION};

/// 手动发布与后台发布共用一把锁;避免两次 PATCH 同一文件交错。
pub(crate) static PUBLISH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// 只串行同步配置;普通设置的整份保存不会写这条独立记录。
static SYNC_PROFILE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// 后台发布已经排队/在跑时不再重复排队。
static SYNC_SCHEDULED: AtomicBool = AtomicBool::new(false);
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub enabled: bool,
    pub relay_id: Option<String>,
    pub relay_kind: String,
    pub source: Option<String>,
    pub key_epoch: u8,
    pub interval_sec: u64,
    pub channels: Vec<String>,
    pub platform_labels: HashMap<String, String>,
    pub last_upload_at: Option<i64>,
    pub last_error: Option<String>,
    pub last_error_at: Option<i64>,
    pub next_allowed_at: Option<i64>,
    pub has_key: bool,
    pub has_github: bool,
    pub github_needs_reauth: bool,
    pub github_login: Option<String>,
    pub github_user_id: Option<i64>,
    pub oauth_client_configured: bool,
    pub pending_seq: Option<u64>,
    pub pending_bytes: Option<usize>,
    /// 由主密钥 + relayId + keyEpoch 现算;前端用来显示/复制。
    pub code_text: Option<String>,
    pub key_error: Option<String>,
}

pub(crate) fn err_str(e: sync::SyncError) -> String {
    format!("{}: {}", e.code, e.message)
}

pub(crate) fn cfg_of(state: &AppState) -> Result<Config, String> {
    let mut config = lock_rw(&state.config).clone();
    let profile = {
        let store = lock_rw(&state.store);
        sync::profile::load(&store)?
    };
    profile.apply_to(&mut config);
    Ok(config)
}

pub(crate) async fn save_sync_config(
    state: &AppState,
    mutate: impl FnOnce(&mut Config),
) -> Result<(), String> {
    let _guard = SYNC_PROFILE_LOCK.lock().await;
    let mut config = cfg_of(state)?;
    mutate(&mut config);
    let profile = sync::profile::SyncProfile::from_config(&config);
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        let store = lock_rw(&store);
        sync::profile::save(&store, &profile)
    })
    .await
    .map_err(|e| format!("保存同步设置失败: {e}"))?
}

pub(crate) async fn fetch_views(state: &AppState) -> Vec<ChannelView> {
    let store = state.store.clone();
    let reader = state.read_store.clone();
    let fetcher = state.fetcher.clone();
    fetcher.fetch_shared_using(&store, &reader).await
}

pub(crate) fn build_for(
    cfg: &Config,
    views: &[ChannelView],
    store: &tokenscope_core::store::Store,
    source: &str,
) -> Result<SyncSnapshot, String> {
    sync::builder::build_preview(cfg, views, store, source, now_ts(), APP_VERSION).map_err(err_str)
}

/// 读当前中继的主密钥与 relayId。PAT 不在这里读。
fn master_of(cfg: &Config) -> Result<([u8; crypto::KEY_LEN], [u8; crypto::RELAY_ID_LEN], String), String> {
    let gist_id = cfg
        .sync_relay_id
        .clone()
        .ok_or("还没有创建或加入同步组")?;
    let relay_id = sync::code::parse_gist_id_hex(&gist_id).map_err(err_str)?;
    let key_hex = secrets::sync_get(&secrets::sync_key_account(&gist_id))?
        .filter(|s| !s.trim().is_empty())
        .ok_or("本机没有这个同步组的主密钥")?;
    let bytes = crypto::hex_decode(key_hex.trim()).ok_or("同步主密钥格式不正确")?;
    if bytes.len() != crypto::KEY_LEN {
        return Err("同步主密钥长度不正确".into());
    }
    let mut master = [0u8; crypto::KEY_LEN];
    master.copy_from_slice(&bytes);
    Ok((master, relay_id, gist_id))
}

pub(crate) fn github_token() -> Result<String, String> {
    secrets::sync_get(secrets::SYNC_OAUTH_ACCOUNT)?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "本机还没有连接 GitHub;请先在同步页授权".to_string())
}

fn github_token_opt() -> Option<String> {
    secrets::sync_get(secrets::SYNC_OAUTH_ACCOUNT)
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
}

pub(crate) fn oauth_client_id(cfg: &Config) -> &str {
    sync::device_flow::desktop_client_id(
        cfg.sync_oauth_client_id.as_deref(),
        option_env!("GRANDETTOKEN_DESKTOP_CLIENT_ID"),
    )
}

pub(crate) fn ensure_configured(cfg: &Config) -> Result<(), String> {
    master_of(cfg)?;
    ensure_github_authorized(cfg)?;
    github_token()?;
    if cfg
        .sync_source
        .as_deref()
        .map_or(true, |s| crypto::normalize_source(s).is_none())
    {
        return Err("本机写入者 ID 缺失或格式不正确".into());
    }
    Ok(())
}

fn ensure_github_authorized(cfg: &Config) -> Result<(), String> {
    if profile::github_needs_reauth(cfg) {
        return Err("SYNC_AUTH: GitHub 授权已失效，请重新授权".into());
    }
    Ok(())
}

fn status_of(state: &AppState) -> Result<SyncStatus, String> {
    let cfg = cfg_of(state)?;
    let key = cfg.sync_relay_id.as_ref().map(|_| master_of(&cfg));
    let has_key = key.as_ref().is_some_and(|r| r.is_ok());
    let key_error = key.as_ref().and_then(|r| r.as_ref().err()).cloned();
    let has_github = github_token_opt().is_some();
    let github_needs_reauth = profile::github_needs_reauth(&cfg);
    let oauth_client_configured = !oauth_client_id(&cfg).is_empty();
    let pending = cfg.sync_source.as_deref().and_then(|source| {
        let store = state.store.clone();
        let s = lock_rw(&store);
        publish::load_state(&s, source)
            .ok()
            .and_then(|st| st.pending)
            .map(|p| (p.seq, p.envelope.len()))
    });
    let (pending_seq, pending_bytes) = match pending {
        Some((s, b)) => (Some(s), Some(b)),
        None => (None, None),
    };
    let code_text = key.and_then(Result::ok).map(|(master, relay_id, _)| {
        let mut code = SyncCode::new(relay_id, master);
        code.key_epoch = cfg.sync_key_epoch;
        code.encode()
    });
    Ok(SyncStatus {
        enabled: cfg.sync_enabled,
        relay_id: cfg.sync_relay_id,
        relay_kind: cfg.sync_relay_kind,
        source: cfg.sync_source,
        key_epoch: cfg.sync_key_epoch,
        interval_sec: cfg.sync_publish_interval_sec,
        channels: cfg.sync_channels,
        platform_labels: cfg.sync_platform_labels,
        last_upload_at: cfg.sync_last_upload_at,
        last_error: cfg.sync_last_error,
        last_error_at: cfg.sync_last_error_at,
        next_allowed_at: cfg.sync_next_allowed_at,
        has_key,
        has_github,
        github_needs_reauth,
        github_login: cfg.sync_github_login,
        github_user_id: cfg.sync_github_user_id,
        oauth_client_configured,
        pending_seq,
        pending_bytes,
        code_text,
        key_error,
    })
}

pub(crate) fn emit_status(app: &AppHandle) {
    let st = app.state::<AppState>();
    if let Ok(status) = status_of(st.inner()) {
        let _ = app.emit("sync-status", status);
    }
}

#[tauri::command]
pub async fn sync_status(state: State<'_, AppState>) -> Result<SyncStatus, String> {
    status_of(state.inner())
}

/// 同步码二维码：只在用户主动「显示」时调用;SVG 只进当前 WebView。
#[tauri::command]
pub async fn sync_qr_svg(state: State<'_, AppState>) -> Result<String, String> {
    let cfg = cfg_of(state.inner())?;
    let (master, relay_id, _) = master_of(&cfg)?;
    let mut code = SyncCode::new(relay_id, master);
    code.key_epoch = cfg.sync_key_epoch;
    sync::qr::svg(&code.encode()).map_err(err_str)
}

// ── 桌面 GitHub Device Flow ───────────────────────────────────────────────
//
// 桌面端不再手填 PAT：Rust 申请 device code,前端只看到 user_code 与验证地址;
// device_code 和最终 access token 都不进前端状态、日志或诊断。

#[tauri::command]
pub async fn sync_preview(state: State<'_, AppState>) -> Result<SyncSnapshot, String> {
    let views = fetch_views(state.inner()).await;
    let cfg = cfg_of(state.inner())?;
    let source = cfg
        .sync_source
        .clone()
        .or_else(|| crypto::random_source().ok())
        .ok_or("无法生成写入者 ID")?;
    let store = state.store.clone();
    let s = lock_rw(&store);
    build_for(&cfg, &views, &s, &source)
}

#[tauri::command]
pub async fn sync_publish_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PublishOutcome, String> {
    // 与自动发布(maybe_publish)同一条闸(报告 L14):配置损坏时手动发布会把默认
    // 配置的数据发出去,而 last_upload_at 又必然记不上 —— 下次还发,白烧 gist 写入。
    crate::refuse_if_config_broken()?;
    ensure_github_authorized(&cfg_of(state.inner())?)?;
    let views = fetch_views(state.inner()).await;
    publish_views(&app, &views, false).await
}

#[tauri::command]
pub async fn sync_read_group(state: State<'_, AppState>) -> Result<ReadOutcome, String> {
    let cfg = cfg_of(state.inner())?;
    let (master, relay_id, gist_id) = master_of(&cfg)?;
    // 与同文件其它命令同口径(报告 L14):没连 GitHub 就明说,别拿匿名请求去换一个
    // 误导性的 404「Gist 不存在」。
    ensure_github_authorized(&cfg)?;
    let client = GistClient::new(Some(github_token()?)).map_err(err_str)?;
    let fetched = sync::fetch_sources(
        &client,
        &gist_id,
        &master,
        &relay_id,
        cfg.sync_key_epoch,
        None,
    )
    .await
    .map_err(err_str)?;

    let (sources, mut failed_sources, etag, files) = match fetched {
        FetchedSources::NotModified { etag } => {
            return Ok(ReadOutcome::NotModified { etag });
        }
        FetchedSources::Loaded {
            sources,
            failed_sources,
            etag,
            files,
            owner_id: _,
        } => (sources, failed_sources, etag, files),
    };

    let (accepted, mut wm_failed, skipped_old) = {
        let store = state.store.clone();
        let s = lock_rw(&store);
        sync::apply_watermarks(&s, sources).map_err(err_str)?
    };
    wm_failed.extend(failed_sources.drain(..));
    let merged = sync::merge_snapshots(&accepted, wm_failed.clone()).map_err(err_str)?;
    Ok(ReadOutcome::Loaded {
        merged,
        etag,
        files,
        skipped_old,
        failed_sources: wm_failed,
    })
}

#[tauri::command]
pub async fn sync_set_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    crate::refuse_if_config_broken()?;
    let _guard = PUBLISH_LOCK.lock().await;
    let cfg = cfg_of(state.inner())?;
    if enabled {
        ensure_configured(&cfg)?;
    }
    save_sync_config(state.inner(), move |c| c.sync_enabled = enabled).await?;
    emit_status(&app);
    Ok(())
}

#[tauri::command]
pub async fn sync_set_channels(
    app: AppHandle,
    state: State<'_, AppState>,
    channels: Vec<String>,
) -> Result<(), String> {
    crate::refuse_if_config_broken()?;
    let mut channels: Vec<String> = channels
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    channels.sort();
    channels.dedup();
    save_sync_config(state.inner(), move |c| c.sync_channels = channels).await?;
    emit_status(&app);
    Ok(())
}

#[tauri::command]
pub async fn sync_set_interval(
    app: AppHandle,
    state: State<'_, AppState>,
    seconds: u64,
) -> Result<(), String> {
    crate::refuse_if_config_broken()?;
    let seconds = seconds.clamp(30, 3600);
    save_sync_config(state.inner(), move |c| c.sync_publish_interval_sec = seconds).await?;
    emit_status(&app);
    Ok(())
}

#[tauri::command]
pub async fn sync_set_platform_label(
    app: AppHandle,
    state: State<'_, AppState>,
    channel_id: String,
    label: String,
) -> Result<(), String> {
    crate::refuse_if_config_broken()?;
    let channel_id = channel_id.trim().to_string();
    if channel_id.is_empty() {
        return Err("渠道 id 不能为空".into());
    }
    let normalized = if label.trim().is_empty() {
        None
    } else {
        Some(
            sync::platform::normalize_label(&label)
                .ok_or("同步平台标签不合法(小写字母/数字/点/横线,最多 40 字)")?,
        )
    };
    save_sync_config(state.inner(), move |c| {
        match normalized {
            Some(v) => {
                c.sync_platform_labels.insert(channel_id.clone(), v);
            }
            None => {
                c.sync_platform_labels.remove(&channel_id);
            }
        }
    })
    .await?;
    emit_status(&app);
    Ok(())
}

/// 轮询复用本轮结果；重新授权恢复发布时按需取数。
pub(crate) fn maybe_publish(app: &AppHandle, views: Option<&[ChannelView]>) {
    if crate::refuse_if_config_broken().is_err() { return; }
    let state = app.state::<AppState>();
    let Ok(cfg) = cfg_of(state.inner()) else { return };
    if !profile::automatic_publish_due(&cfg, now_ts()) || github_token_opt().is_none() {
        return;
    }
    if SYNC_SCHEDULED.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    let views = views.map(<[ChannelView]>::to_vec);
    tauri::async_runtime::spawn(async move {
        let views = match views {
            Some(views) => views,
            None => fetch_views(app.state::<AppState>().inner()).await,
        };
        let _ = publish_views(&app, &views, true).await;
        SYNC_SCHEDULED.store(false, Ordering::SeqCst);
    });
}

async fn publish_views(app: &AppHandle, views: &[ChannelView], automatic: bool) -> Result<PublishOutcome, String> {
    let _guard = PUBLISH_LOCK.lock().await;
    let state = app.state::<AppState>();
    let cfg = cfg_of(state.inner())?;
    // 排队期间可能已手动发布、暂停同步或收到授权错误。
    if automatic && !profile::automatic_publish_due(&cfg, now_ts()) {
        return Err("当前无需自动发布".into());
    }
    ensure_github_authorized(&cfg)?;
    let (master, relay_id, gist_id) = master_of(&cfg)?;
    let token = github_token()?;
    let source = cfg
        .sync_source
        .clone()
        .ok_or("本机写入者 ID 缺失")?;
    let snapshot = {
        let store = state.store.clone();
        let s = lock_rw(&store);
        build_for(&cfg, views, &s, &source)?
    };
    let client = GistClient::new(Some(token)).map_err(err_str)?;
    let now = now_ts();

    let prepared_result = {
        let store = state.store.clone();
        let s = lock_rw(&store);
        sync::prepare_publish(&master, &relay_id, cfg.sync_key_epoch, &s, snapshot, now)
    };
    let prepared = match prepared_result {
        Ok(p) => p,
        Err(e) => return Err(record_publish_error(app, state.inner(), &e, now).await),
    };
    let sent = sync::send_prepared(&client, &gist_id, &prepared).await;
    let result: Result<PublishOutcome, sync::SyncError> = match sent {
        Ok(()) => {
            let store = state.store.clone();
            let s = lock_rw(&store);
            let r = sync::commit_publish(&s, &prepared, now);
            drop(s);
            r
        }
        Err(e) => Err(e),
    };

    match result {
        Ok(out) => {
            let at = out.at;
            let _ = save_sync_config(state.inner(), move |c| {
                c.sync_last_upload_at = Some(at);
                c.sync_last_error = None;
                c.sync_last_error_at = None;
                c.sync_next_allowed_at = None;
            })
            .await;
            emit_status(app);
            Ok(out)
        }
        Err(e) => Err(record_publish_error(app, state.inner(), &e, now).await),
    }
}

async fn record_publish_error(
    app: &AppHandle,
    state: &AppState,
    e: &sync::SyncError,
    now: i64,
) -> String {
    tokenscope_core::diaglog::write_keyed("sync", "publish", e.code);
    let retry_at = e
        .reset_at
        .or_else(|| e.retry_after_sec.map(|s| now + s as i64));
    let msg = err_str(e.clone());
    let msg2 = msg.clone();
    let _ = save_sync_config(state, move |c| {
        c.sync_last_error = Some(msg2.clone());
        c.sync_last_error_at = Some(now_ts());
        if retry_at.is_some() {
            c.sync_next_allowed_at = retry_at;
        }
    })
    .await;
    emit_status(app);
    let _ = app.emit("sync-error", &msg);
    msg
}
