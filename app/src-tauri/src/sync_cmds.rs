//! Tauri 侧的同步命令与后台发布。
//!
//! 这里只做胶水：取数、密钥环、配置落库、事件。加密/合并/传输都在
//! `tokenscope_core::sync`。特别注意：`std::sync::MutexGuard<Store>` 绝不跨
//! `.await` —— 发布与读取都拆成“同步准备 → 异步网络 → 同步提交”。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use futures::{stream, StreamExt};
use reqwest::header::ACCEPT;
use tauri::{AppHandle, Emitter, Manager, State};
use tokenscope_core::config::Config;
use tokenscope_core::fetch::{now_ts, ChannelView};
use tokenscope_core::secrets;
use tokenscope_core::sync::{
    self, code::SyncCode, crypto, gist::GistClient, model::SyncSnapshot, publish, FetchedSources,
    PublishOutcome, ReadOutcome,
};

use crate::{lock_rw, AppState, APP_VERSION};

const GITHUB_DEVICE_CODE: &str = "https://github.com/login/device/code";
const GITHUB_TOKEN: &str = "https://github.com/login/oauth/access_token";
const GITHUB_USER: &str = "https://api.github.com/user";
const GITHUB_VERIFY: &str = "https://github.com/login/device";

/// 手动发布与后台发布共用一把锁;避免两次 PATCH 同一文件交错。
static PUBLISH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// 只串行同步配置;普通设置的整份保存不会写这条独立记录。
static SYNC_PROFILE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// 后台发布已经排队/在跑时不再重复排队。
static SYNC_SCHEDULED: AtomicBool = AtomicBool::new(false);
/// 桌面 Device Flow 的在途会话。device_code 只留 Rust 内存,不进前端/日志。
static OAUTH: Mutex<Option<OAuthSession>> = Mutex::new(None);

#[derive(Debug, Clone)]
struct OAuthSession {
    device_code: String,
    expires_at: i64,
    interval: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthStart {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: i64,
    pub interval: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OAuthPoll {
    Pending { interval: u64 },
    Authorized { login: String, user_id: i64 },
    Error { code: String, message: String },
    NoSession,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubStatus {
    pub configured: bool,
    pub connected: bool,
    pub login: Option<String>,
    pub user_id: Option<i64>,
}

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
    pub github_login: Option<String>,
    pub github_user_id: Option<i64>,
    pub oauth_client_configured: bool,
    pub pending_seq: Option<u64>,
    pub pending_bytes: Option<usize>,
    /// 由主密钥 + relayId + keyEpoch 现算;前端用来显示/复制。
    pub code_text: Option<String>,
    pub key_error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncCreateResult {
    pub gist_id: String,
    pub sync_code: String,
    pub seq: u64,
    pub bytes: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRotateResult {
    pub gist_id: String,
    pub sync_code: String,
    pub seq: u64,
    pub bytes: usize,
    pub old_relay_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCandidate {
    pub gist_id: String,
    pub key_epoch: u8,
    pub has_local_source: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverySearch {
    pub candidates: Vec<RecoveryCandidate>,
    pub unverified: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryResult {
    pub has_local_source: bool,
}

fn err_str(e: sync::SyncError) -> String {
    format!("{}: {}", e.code, e.message)
}

fn cfg_of(state: &AppState) -> Result<Config, String> {
    let mut config = lock_rw(&state.config).clone();
    let profile = {
        let store = lock_rw(&state.store);
        sync::profile::load(&store)?
    };
    profile.apply_to(&mut config);
    Ok(config)
}

async fn save_sync_config(
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

async fn fetch_views(state: &AppState) -> Vec<ChannelView> {
    let store = state.store.clone();
    let fetcher = state.fetcher.clone();
    fetcher.fetch_shared(&store).await
}

fn build_for(
    cfg: &Config,
    views: &[ChannelView],
    store: &tokenscope_core::store::Store,
    source: &str,
) -> Result<SyncSnapshot, String> {
    sync::build_snapshot(sync::BuildInput {
        cfg,
        views,
        store,
        source,
        seq: 1,
        now: now_ts(),
        app: APP_VERSION,
    })
    .map_err(err_str)
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

fn github_token() -> Result<String, String> {
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

/// 桌面 OAuth App 的公开 client_id：优先配置覆盖,其次编译期环境变量。
fn oauth_client_id(cfg: &Config) -> Option<String> {
    cfg.sync_oauth_client_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            option_env!("GRANDETTOKEN_DESKTOP_CLIENT_ID")
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty())
        })
}

fn ensure_configured(cfg: &Config) -> Result<(), String> {
    master_of(cfg)?;
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

fn status_of(state: &AppState) -> Result<SyncStatus, String> {
    let cfg = cfg_of(state)?;
    let key = cfg.sync_relay_id.as_ref().map(|_| master_of(&cfg));
    let has_key = key.as_ref().is_some_and(|r| r.is_ok());
    let key_error = key.as_ref().and_then(|r| r.as_ref().err()).cloned();
    let has_github = github_token_opt().is_some();
    let oauth_client_configured = oauth_client_id(&cfg).is_some();
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
        github_login: cfg.sync_github_login,
        github_user_id: cfg.sync_github_user_id,
        oauth_client_configured,
        pending_seq,
        pending_bytes,
        code_text,
        key_error,
    })
}

fn emit_status(app: &AppHandle) {
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

#[derive(serde::Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: i64,
    interval: u64,
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    scope: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
}

#[derive(serde::Deserialize)]
struct GithubUserResponse {
    id: i64,
    login: String,
}

/// 同步用的 HTTP 客户端:**进程内复用一份**。
///
/// 一次同步要串好几个 GitHub 请求(用户信息 / gist 读写 / 设备流),原来每个请求
/// 都新建一套连接池与 TLS 上下文,连接也无法复用。参数、超时、UA 与错误码逐字不变。
fn http_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .user_agent("Grandettoken-Desktop/1")
                .build()
                .map_err(|e| format!("SYNC_NETWORK: HTTP 客户端初始化失败: {e}"))
        })
        .as_ref()
        .map_err(|e| e.clone())
}

async fn github_user(token: &str) -> Result<GithubUserResponse, String> {
    let client = http_client()?;
    let resp = client
        .get(GITHUB_USER)
        .header(ACCEPT, "application/vnd.github+json")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "SYNC_NETWORK: 请求 GitHub 用户信息失败".to_string())?;
    if resp.status().as_u16() == 401 {
        return Err("SYNC_AUTH: GitHub 授权已失效,请重新授权".into());
    }
    if !resp.status().is_success() {
        return Err(format!("SYNC_UPSTREAM: GitHub 用户信息返回 {}", resp.status()));
    }
    let user: GithubUserResponse = resp
        .json()
        .await
        .map_err(|_| "SYNC_UPSTREAM: GitHub 用户信息不是预期 JSON".to_string())?;
    if user.id <= 0 || user.login.trim().is_empty() {
        return Err("SYNC_UPSTREAM: GitHub 用户信息不完整".into());
    }
    Ok(user)
}

#[tauri::command]
pub async fn sync_github_status(
    state: State<'_, AppState>,
) -> Result<GithubStatus, String> {
    let cfg = cfg_of(state.inner())?;
    Ok(GithubStatus {
        configured: oauth_client_id(&cfg).is_some(),
        connected: github_token_opt().is_some(),
        login: cfg.sync_github_login,
        user_id: cfg.sync_github_user_id,
    })
}

#[tauri::command]
pub async fn sync_oauth_start(
    state: State<'_, AppState>,
) -> Result<OAuthStart, String> {
    let cfg = cfg_of(state.inner())?;
    let client_id = oauth_client_id(&cfg).ok_or_else(|| {
        "SYNC_NOT_CONFIGURED: 桌面 OAuth App 未配置 client_id,开发者需先在 GitHub 建桌面 OAuth App".to_string()
    })?;
    let client = http_client()?;
    let resp = client
        .post(GITHUB_DEVICE_CODE)
        .header(ACCEPT, "application/json")
        .form(&[("client_id", client_id.as_str()), ("scope", "gist")])
        .send()
        .await
        .map_err(|_| "SYNC_NETWORK: 无法申请 GitHub 授权码".to_string())?;
    if !resp.status().is_success() {
        return Err(format!(
            "SYNC_UPSTREAM: GitHub 授权码接口返回 {}",
            resp.status()
        ));
    }
    let body: DeviceCodeResponse = resp
        .json()
        .await
        .map_err(|_| "SYNC_UPSTREAM: GitHub 授权码响应不是预期 JSON".to_string())?;
    if body.device_code.is_empty() || body.user_code.is_empty() || body.interval == 0 {
        return Err("SYNC_UPSTREAM: GitHub 授权码响应不完整".into());
    }
    if !body.verification_uri.starts_with("https://github.com/") {
        return Err("SYNC_UPSTREAM: GitHub 返回了非官方授权地址".into());
    }

    let session = OAuthSession {
        device_code: body.device_code,
        expires_at: now_ts() + body.expires_in.max(60),
        interval: body.interval.max(5),
    };
    *OAUTH.lock().unwrap_or_else(|p| p.into_inner()) = Some(session);

    Ok(OAuthStart {
        user_code: body.user_code,
        verification_uri: GITHUB_VERIFY.to_string(),
        expires_in: body.expires_in.max(60),
        interval: body.interval.max(5),
    })
}

#[tauri::command]
pub async fn sync_oauth_poll(app: AppHandle) -> Result<OAuthPoll, String> {
    let session = {
        let guard = OAUTH.lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_ref() {
            None => return Ok(OAuthPoll::NoSession),
            Some(s) => s.clone(),
        }
    };
    if now_ts() >= session.expires_at {
        let _ = OAUTH.lock().map(|mut g| *g = None);
        return Ok(OAuthPoll::Error {
            code: "SYNC_OAUTH_EXPIRED".into(),
            message: "授权码已过期,请重新获取".into(),
        });
    }

    let cfg = cfg_of(app.state::<AppState>().inner())?;
    let Some(client_id) = oauth_client_id(&cfg) else {
        return Ok(OAuthPoll::Error {
            code: "SYNC_NOT_CONFIGURED".into(),
            message: "桌面 OAuth App 未配置 client_id".into(),
        });
    };
    let client = http_client()?;
    let resp = client
        .post(GITHUB_TOKEN)
        .header(ACCEPT, "application/json")
        .form(&[
            ("client_id", client_id.as_str()),
            ("device_code", session.device_code.as_str()),
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:device_code",
            ),
        ])
        .send()
        .await
        .map_err(|_| "SYNC_NETWORK: 轮询 GitHub 授权失败".to_string())?;
    if !resp.status().is_success() {
        return Ok(OAuthPoll::Error {
            code: "SYNC_UPSTREAM".into(),
            message: format!("GitHub 授权接口返回 {}", resp.status()),
        });
    }
    let token: TokenResponse = resp
        .json()
        .await
        .map_err(|_| "SYNC_UPSTREAM: GitHub 授权响应不是预期 JSON".to_string())?;

    if let Some(err) = token.error.as_deref() {
        return match err {
            "authorization_pending" => Ok(OAuthPoll::Pending {
                interval: session.interval,
            }),
            "slow_down" => {
                let next = token.interval.unwrap_or(session.interval + 5).max(5);
                if let Ok(mut guard) = OAUTH.lock() {
                    if let Some(s) = guard.as_mut() {
                        if s.device_code == session.device_code {
                            s.interval = next;
                        }
                    }
                }
                Ok(OAuthPoll::Pending { interval: next })
            }
            "access_denied" => {
                let _ = OAUTH.lock().map(|mut g| *g = None);
                Ok(OAuthPoll::Error {
                    code: "SYNC_OAUTH_DENIED".into(),
                    message: "GitHub 授权被拒绝".into(),
                })
            }
            "expired_token" => {
                let _ = OAUTH.lock().map(|mut g| *g = None);
                Ok(OAuthPoll::Error {
                    code: "SYNC_OAUTH_EXPIRED".into(),
                    message: "授权码已过期,请重新获取".into(),
                })
            }
            other => {
                let _ = OAUTH.lock().map(|mut g| *g = None);
                Ok(OAuthPoll::Error {
                    code: format!("SYNC_OAUTH_{}", other.to_ascii_uppercase()),
                    message: token
                        .error_description
                        .unwrap_or_else(|| "GitHub 授权失败".into())
                        .chars()
                        .take(200)
                        .collect(),
                })
            }
        };
    }

    let access_token = match token.access_token {
        Some(t) if !t.trim().is_empty() => t,
        _ => {
            return Ok(OAuthPoll::Error {
                code: "SYNC_UPSTREAM".into(),
                message: "GitHub 没有返回 access token".into(),
            })
        }
    };

    // 取消竞态：如果弹层已经取消/换了一次授权,迟到的 token 不保存。
    {
        let guard = OAUTH.lock().unwrap_or_else(|p| p.into_inner());
        let still_current = guard
            .as_ref()
            .map(|s| s.device_code == session.device_code)
            .unwrap_or(false);
        if !still_current {
            return Ok(OAuthPoll::NoSession);
        }
    }

    // 只接受恰好 gist 的 scope;缺 gist 或多出写权限都不完成连接。
    let scopes: Vec<&str> = token
        .scope
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .collect();
    if !scopes.contains(&"gist") || scopes.iter().any(|s| *s != "gist") {
        let _ = OAUTH.lock().map(|mut g| *g = None);
        return Ok(OAuthPoll::Error {
            code: "SYNC_SCOPE_INVALID".into(),
            message: "GitHub 返回的授权范围不是仅 gist;请在 GitHub 撤销授权后重试".into(),
        });
    }

    let user = match github_user(&access_token).await {
        Ok(u) => u,
        Err(e) => {
            let _ = OAUTH.lock().map(|mut g| *g = None);
            return Ok(OAuthPoll::Error {
                code: "SYNC_AUTH".into(),
                message: e,
            });
        }
    };

    if let Err(e) = secrets::sync_set(secrets::SYNC_OAUTH_ACCOUNT, &access_token) {
        let _ = OAUTH.lock().map(|mut g| *g = None);
        return Ok(OAuthPoll::Error {
            code: "SYNC_STORE".into(),
            message: e,
        });
    }
    let login = user.login.clone();
    let user_id = user.id;
    let state_ref = app.state::<AppState>();
    if let Err(e) = save_sync_config(state_ref.inner(), move |c| {
        c.sync_github_login = Some(login);
        c.sync_github_user_id = Some(user_id);
        c.sync_last_error = None;
        c.sync_last_error_at = None;
        c.sync_next_allowed_at = None;
    })
    .await
    {
        let _ = secrets::sync_delete(secrets::SYNC_OAUTH_ACCOUNT);
        let _ = OAUTH.lock().map(|mut g| *g = None);
        return Ok(OAuthPoll::Error {
            code: "SYNC_STORE".into(),
            message: format!("保存 GitHub 登录状态失败: {e}"),
        });
    }

    let _ = OAUTH.lock().map(|mut g| *g = None);
    emit_status(&app);
    Ok(OAuthPoll::Authorized {
        login: user.login,
        user_id: user.id,
    })
}

#[tauri::command]
pub async fn sync_oauth_cancel() -> Result<(), String> {
    let _ = OAUTH.lock().map(|mut g| *g = None);
    Ok(())
}

/// 开发用：把公开的桌面 OAuth App client_id 写进本机配置。
/// 正式发布时应优先用编译期 `GRANDETTOKEN_DESKTOP_CLIENT_ID` 内置。
#[tauri::command]
pub async fn sync_set_oauth_client_id(
    app: AppHandle,
    state: State<'_, AppState>,
    client_id: String,
) -> Result<(), String> {
    crate::refuse_if_config_broken()?;
    let value = client_id.trim().to_string();
    if value.chars().count() > 100
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err("Client ID 格式不正确(不能包含空格或控制字符)".into());
    }
    let opt = if value.is_empty() { None } else { Some(value) };
    save_sync_config(state.inner(), move |c| c.sync_oauth_client_id = opt).await?;
    emit_status(&app);
    Ok(())
}

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
pub async fn sync_create(
    app: AppHandle,
    state: State<'_, AppState>,
    allow_existing: bool,
) -> Result<SyncCreateResult, String> {
    crate::refuse_if_config_broken()?;
    let _guard = PUBLISH_LOCK.lock().await;
    let cfg = cfg_of(state.inner())?;
    if cfg.sync_relay_id.is_some() {
        return Err("已有同步组;要换新的用「重新生成同步码」".into());
    }
    if !allow_existing && !secrets::sync_list_relay_ids()?.is_empty() {
        return Err("SYNC_OLD_GROUP_FOUND: 本机保留着旧同步组,请先尝试找回".into());
    }
    let token = github_token()?;
    let user = github_user(&token).await?;
    let views = fetch_views(state.inner()).await;
    let source = crypto::random_source().map_err(err_str)?;
    let master = crypto::random_master_key().map_err(err_str)?;
    let snapshot = {
        let store = state.store.clone();
        let s = lock_rw(&store);
        build_for(&cfg, &views, &s, &source)?
    };
    let client = GistClient::new(Some(token)).map_err(err_str)?;
    let now = now_ts();
    let outcome = sync::create_relay(sync::CreateRelayInput {
        client: &client,
        master: &master,
        snapshot,
        now,
        description: "Grandettoken sync relay",
    })
    .await
    .map_err(err_str)?;

    secrets::sync_set(
        &secrets::sync_key_account(&outcome.gist_id),
        &crypto::hex_encode(&master),
    )?;
    {
        let store = state.store.clone();
        let s = lock_rw(&store);
        sync::init_relay_state(&s, &outcome, now).map_err(err_str)?;
    }

    let gist_id = outcome.gist_id.clone();
    let source = outcome.source.clone();
    let login = user.login;
    let user_id = user.id;
    save_sync_config(state.inner(), move |c| {
        c.sync_enabled = true;
        c.sync_relay_kind = "gist".into();
        c.sync_relay_id = Some(gist_id.clone());
        c.sync_source = Some(source.clone());
        c.sync_key_epoch = 0;
        c.sync_publish_interval_sec = c.sync_publish_interval_sec.clamp(30, 3600);
        c.sync_last_upload_at = Some(now);
        c.sync_last_error = None;
        c.sync_last_error_at = None;
        c.sync_next_allowed_at = None;
        c.sync_github_login = Some(login);
        c.sync_github_user_id = Some(user_id);
    })
    .await?;
    emit_status(&app);

    Ok(SyncCreateResult {
        gist_id: outcome.gist_id,
        sync_code: outcome.code_text,
        seq: outcome.seq,
        bytes: outcome.bytes,
    })
}

#[tauri::command]
pub async fn sync_join(
    app: AppHandle,
    state: State<'_, AppState>,
    code: String,
) -> Result<SyncCreateResult, String> {
    crate::refuse_if_config_broken()?;
    let _guard = PUBLISH_LOCK.lock().await;
    let cfg = cfg_of(state.inner())?;
    if cfg.sync_relay_id.is_some() {
        return Err("已有同步组;要切换用「找回旧同步组」".into());
    }
    let code = SyncCode::decode(&code).map_err(err_str)?;
    let token = github_token()?;
    let user = github_user(&token).await?;
    let gist_id = code.gist_id_hex();
    let views = fetch_views(state.inner()).await;
    let source = crypto::random_source().map_err(err_str)?;
    let snapshot = {
        let store = state.store.clone();
        let s = lock_rw(&store);
        build_for(&cfg, &views, &s, &source)?
    };
    let client = GistClient::new(Some(token)).map_err(err_str)?;

    // 空组不能证明密钥正确：必须先看到至少一份能解开的快照;owner 必须是当前账号。
    let fetched = sync::fetch_sources(
        &client,
        &gist_id,
        &code.key,
        &code.relay_id,
        code.key_epoch,
        None,
    )
    .await
    .map_err(err_str)?;
    let (has_sources, owner_id) = match fetched {
        FetchedSources::Loaded {
            sources, owner_id, ..
        } => (!sources.is_empty(), owner_id),
        FetchedSources::NotModified { .. } => (false, None),
    };
    if !has_sources {
        return Err("同步组还没有可验证快照;请先在已建组的电脑上发布一次".into());
    }
    if owner_id != Some(user.id) {
        return Err(
            "SYNC_ACCOUNT_MISMATCH: 请使用创建该同步组的 GitHub 账号重新授权".into(),
        );
    }

    secrets::sync_set(
        &secrets::sync_key_account(&gist_id),
        &crypto::hex_encode(&code.key),
    )?;

    let now = now_ts();
    let prepared = {
        let store = state.store.clone();
        let s = lock_rw(&store);
        sync::prepare_publish(
            &code.key,
            &code.relay_id,
            code.key_epoch,
            &s,
            snapshot,
            now,
        )
        .map_err(err_str)?
    };
    sync::send_prepared(&client, &gist_id, &prepared)
        .await
        .map_err(err_str)?;
    let out = {
        let store = state.store.clone();
        let s = lock_rw(&store);
        sync::commit_publish(&s, &prepared, now).map_err(err_str)?
    };

    let save_source = source.clone();
    let save_gist = gist_id.clone();
    let key_epoch = code.key_epoch;
    let login = user.login;
    let user_id = user.id;
    save_sync_config(state.inner(), move |c| {
        c.sync_enabled = true;
        c.sync_relay_kind = "gist".into();
        c.sync_relay_id = Some(save_gist.clone());
        c.sync_source = Some(save_source.clone());
        c.sync_key_epoch = key_epoch;
        c.sync_last_upload_at = Some(now);
        c.sync_last_error = None;
        c.sync_last_error_at = None;
        c.sync_next_allowed_at = None;
        c.sync_github_login = Some(login);
        c.sync_github_user_id = Some(user_id);
    })
    .await?;
    emit_status(&app);

    Ok(SyncCreateResult {
        gist_id,
        sync_code: code.encode(),
        seq: out.seq,
        bytes: out.bytes,
    })
}

struct VerifiedRecovery {
    source: Option<String>,
    last_upload_at: Option<i64>,
}

/// 找回来源时必须把本机发布状态与远端密文水位对应上,不能猜另一台电脑的来源。
async fn verify_recovery(
    state: &AppState,
    token: &str,
    owner_id: i64,
    code: &SyncCode,
) -> Result<VerifiedRecovery, String> {
    let client = GistClient::new(Some(token.to_string())).map_err(err_str)?;
    let fetched = sync::fetch_sources(
        &client,
        &code.gist_id_hex(),
        &code.key,
        &code.relay_id,
        code.key_epoch,
        None,
    ).await.map_err(err_str)?;
    let sources = match fetched {
        FetchedSources::Loaded { sources, owner_id: owner, .. } => {
            if owner != Some(owner_id) {
                return Err("旧同步组不属于当前 GitHub 账号,请连接原账号".into());
            }
            sources
        }
        FetchedSources::NotModified { .. } => Vec::new(),
    };
    if sources.is_empty() {
        return Err("无法验证旧同步码,请确认旧组仍可访问且有有效数据".into());
    }
    let (source, last_upload_at) = {
        let store = lock_rw(&state.store);
        publish::local_recovery_source(&store, &sources).map_err(err_str)?
    };
    Ok(VerifiedRecovery { source, last_upload_at })
}

fn stored_code(gist_id: &str, key_epoch: u8) -> Result<SyncCode, String> {
    let relay = sync::code::parse_gist_id_hex(gist_id).map_err(err_str)?;
    let key_hex = secrets::sync_get(&secrets::sync_key_account(gist_id))?
        .ok_or("本机没有这个旧组的密钥")?;
    let bytes = crypto::hex_decode(key_hex.trim()).ok_or("本机旧组密钥格式不正确")?;
    let key: [u8; crypto::KEY_LEN] = bytes.try_into().map_err(|_| "本机旧组密钥长度不正确")?;
    let mut code = SyncCode::new(relay, key);
    code.key_epoch = key_epoch;
    Ok(code)
}

#[tauri::command]
pub async fn sync_recovery_candidates(state: State<'_, AppState>) -> Result<RecoverySearch, String> {
    let cfg = cfg_of(state.inner())?;
    let token = github_token()?;
    let user = github_user(&token).await?;
    let ids = tokio::task::spawn_blocking(secrets::sync_list_relay_ids)
        .await.map_err(|_| "读取本机同步凭据列表失败")??;
    let ids: Vec<_> = ids.into_iter()
        .filter(|id| cfg.sync_relay_id.as_deref() != Some(id.as_str()))
        .collect();
    const LIMIT: usize = 24;
    let truncated = ids.len() > LIMIT;
    let state_ref = state.inner();
    let owner_id = user.id;
    let checks = stream::iter(ids.into_iter().take(LIMIT)).map(|id| {
        let token = &token;
        async move {
            // v1 电脑创建的组 epoch 均为 0;其他代数可通过手动输入旧码恢复。
            let code = stored_code(&id, 0)?;
            let verified = verify_recovery(state_ref, token, owner_id, &code).await?;
            Ok::<_, String>(RecoveryCandidate {
                gist_id: id,
                key_epoch: code.key_epoch,
                has_local_source: verified.source.is_some(),
            })
        }
    }).buffer_unordered(4).collect::<Vec<_>>().await;
    let mut candidates = Vec::new();
    let mut unverified = 0;
    for result in checks {
        match result {
            Ok(candidate) => candidates.push(candidate),
            Err(_) => unverified += 1,
        }
    }
    candidates.sort_by(|a, b| a.gist_id.cmp(&b.gist_id));
    Ok(RecoverySearch { candidates, unverified, truncated })
}

/// 先验证所有权与密钥,再切换;保留原组及密钥,恢复后始终暂停自动发布。
#[tauri::command]
pub async fn sync_restore_group(
    app: AppHandle,
    state: State<'_, AppState>,
    gist_id: Option<String>,
    code: Option<String>,
    key_epoch: Option<u8>,
    expected_current: Option<String>,
) -> Result<RecoveryResult, String> {
    crate::refuse_if_config_broken()?;
    let _guard = PUBLISH_LOCK.lock().await;
    if cfg_of(state.inner())?.sync_relay_id != expected_current {
        return Err("当前同步组已变化,请重新选择要恢复的组".into());
    }
    let token = github_token()?;
    let user = github_user(&token).await?;
    let code = match code.filter(|c| !c.trim().is_empty()) {
        Some(text) => SyncCode::decode(&text).map_err(err_str)?,
        None => stored_code(gist_id.as_deref().ok_or("请选择旧同步组")?, key_epoch.unwrap_or(0))?,
    };
    let verified = verify_recovery(state.inner(), &token, user.id, &code).await?;
    let id = code.gist_id_hex();
    secrets::sync_set(&secrets::sync_key_account(&id), &crypto::hex_encode(&code.key))?;
    let source = verified.source.clone();
    let has_local_source = source.is_some();
    let last_upload_at = verified.last_upload_at;
    let epoch = code.key_epoch;
    save_sync_config(state.inner(), move |c| {
        c.sync_enabled = false;
        c.sync_relay_id = Some(id);
        c.sync_source = source;
        c.sync_key_epoch = epoch;
        c.sync_last_upload_at = last_upload_at;
        c.sync_last_error = None;
        c.sync_last_error_at = None;
        c.sync_next_allowed_at = None;
        c.sync_github_login = Some(user.login);
        c.sync_github_user_id = Some(user.id);
    }).await?;
    emit_status(&app);
    Ok(RecoveryResult { has_local_source })
}

#[tauri::command]
pub async fn sync_publish_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PublishOutcome, String> {
    // 与自动发布(maybe_publish)同一条闸(报告 L14):配置损坏时手动发布会把默认
    // 配置的数据发出去,而 last_upload_at 又必然记不上 —— 下次还发,白烧 gist 写入。
    crate::refuse_if_config_broken()?;
    let views = fetch_views(state.inner()).await;
    publish_views(&app, &views, false).await
}

#[tauri::command]
pub async fn sync_read_group(state: State<'_, AppState>) -> Result<ReadOutcome, String> {
    let cfg = cfg_of(state.inner())?;
    let (master, relay_id, gist_id) = master_of(&cfg)?;
    // 与同文件其它命令同口径(报告 L14):没连 GitHub 就明说,别拿匿名请求去换一个
    // 误导性的 404「Gist 不存在」。
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

#[tauri::command]
pub async fn sync_rotate(
    app: AppHandle,
    state: State<'_, AppState>,
    expected_relay_id: String,
) -> Result<SyncRotateResult, String> {
    crate::refuse_if_config_broken()?;
    let _guard = PUBLISH_LOCK.lock().await;
    let cfg = cfg_of(state.inner())?;
    if cfg.sync_relay_id.as_deref() != Some(expected_relay_id.as_str()) {
        return Err("当前同步组已变化,请确认后重试".into());
    }
    let token = github_token()?;
    let old_relay_id = cfg.sync_relay_id.clone();
    let views = fetch_views(state.inner()).await;
    let source = crypto::random_source().map_err(err_str)?;
    let master = crypto::random_master_key().map_err(err_str)?;
    let snapshot = {
        let store = state.store.clone();
        let s = lock_rw(&store);
        build_for(&cfg, &views, &s, &source)?
    };
    let client = GistClient::new(Some(token)).map_err(err_str)?;
    let now = now_ts();
    let outcome = sync::create_relay(sync::CreateRelayInput {
        client: &client,
        master: &master,
        snapshot,
        now,
        description: "Grandettoken sync relay (rotated)",
    })
    .await
    .map_err(err_str)?;

    secrets::sync_set(
        &secrets::sync_key_account(&outcome.gist_id),
        &crypto::hex_encode(&master),
    )?;
    {
        let store = state.store.clone();
        let s = lock_rw(&store);
        sync::init_relay_state(&s, &outcome, now).map_err(err_str)?;
    }

    let gist_id = outcome.gist_id.clone();
    let source = outcome.source.clone();
    save_sync_config(state.inner(), move |c| {
        c.sync_enabled = true;
        c.sync_relay_id = Some(gist_id.clone());
        c.sync_source = Some(source.clone());
        c.sync_key_epoch = 0;
        c.sync_last_upload_at = Some(now);
        c.sync_last_error = None;
        c.sync_last_error_at = None;
        c.sync_next_allowed_at = None;
    })
    .await?;
    emit_status(&app);

    Ok(SyncRotateResult {
        gist_id: outcome.gist_id,
        sync_code: outcome.code_text,
        seq: outcome.seq,
        bytes: outcome.bytes,
        old_relay_id,
    })
}

#[tauri::command]
pub async fn sync_forget(app: AppHandle, state: State<'_, AppState>, expected_relay_id: String) -> Result<(), String> {
    crate::refuse_if_config_broken()?;
    let _guard = PUBLISH_LOCK.lock().await;
    let cfg = cfg_of(state.inner())?;
    if cfg.sync_relay_id.as_deref() != Some(expected_relay_id.as_str()) {
        return Err("当前同步组已变化,请确认后重试".into());
    }
    save_sync_config(state.inner(), |c| {
        c.sync_enabled = false;
        c.sync_relay_id = None;
        c.sync_source = None;
        c.sync_key_epoch = 0;
        c.sync_last_upload_at = None;
        c.sync_last_error = None;
        c.sync_last_error_at = None;
        c.sync_next_allowed_at = None;
    })
    .await?;
    if let Some(source) = cfg.sync_source.as_deref() {
        {
            let store = state.store.clone();
            let s = lock_rw(&store);
            let _ = publish::clear_state(&s, source);
        }
    }
    if let Some(gist) = cfg.sync_relay_id.as_deref() {
        let _ = secrets::sync_delete(&secrets::sync_key_account(gist));
    }
    emit_status(&app);
    Ok(())
}

#[tauri::command]
pub async fn sync_disconnect_github(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let _guard = PUBLISH_LOCK.lock().await;
    // 先存「已断开」再删凭据(报告 L14):顺序反过来的话,保存一旦失败,配置里还
    // 指着一把已经删掉的 token,后续发布会一直 401。反过来失败也只是留下一个
    // 孤儿凭据(下次断开重删即可),状态是自洽的。
    save_sync_config(state.inner(), |c| {
        c.sync_enabled = false;
        c.sync_github_login = None;
        c.sync_github_user_id = None;
        c.sync_next_allowed_at = None;
        c.sync_last_error = Some("GitHub 授权已断开;重新授权后才能发布(同步组与主密钥保留)".into());
        c.sync_last_error_at = Some(now_ts());
    })
    .await?;
    secrets::sync_delete(secrets::SYNC_OAUTH_ACCOUNT)?;
    emit_status(&app);
    Ok(())
}

#[tauri::command]
pub async fn sync_delete_relay(
    app: AppHandle,
    state: State<'_, AppState>,
    gist_id: String,
) -> Result<(), String> {
    let _guard = PUBLISH_LOCK.lock().await;
    let token = github_token()?;
    let client = GistClient::new(Some(token)).map_err(err_str)?;
    client.delete_gist(&gist_id).await.map_err(err_str)?;
    let cfg = cfg_of(state.inner())?;
    if cfg.sync_relay_id.as_deref() == Some(gist_id.as_str()) {
        if let Some(source) = cfg.sync_source.as_deref() {
            let store = state.store.clone();
            let s = lock_rw(&store);
            let _ = publish::clear_state(&s, source);
        }
        let _ = secrets::sync_delete(&secrets::sync_key_account(&gist_id));
        save_sync_config(state.inner(), |c| {
            c.sync_enabled = false;
            c.sync_relay_id = None;
            c.sync_source = None;
            c.sync_key_epoch = 0;
            c.sync_last_upload_at = None;
            c.sync_next_allowed_at = None;
        })
        .await?;
    } else {
        let _ = secrets::sync_delete(&secrets::sync_key_account(&gist_id));
    }
    emit_status(&app);
    Ok(())
}

/// 轮询循环每轮调用：到点就后台发布,不阻塞取数。
pub fn maybe_publish(app: &AppHandle, views: &[ChannelView]) {
    if crate::refuse_if_config_broken().is_err() { return; }
    let state = app.state::<AppState>();
    let Ok(cfg) = cfg_of(state.inner()) else { return };
    if !cfg.sync_enabled || cfg.sync_relay_id.is_none() || cfg.sync_source.is_none() {
        return;
    }
    if github_token_opt().is_none() {
        return;
    }
    // 401 之后不再自动重试,等用户重新授权(授权成功会清掉 last_error)。
    if cfg
        .sync_last_error
        .as_deref()
        .map_or(false, |e| e.starts_with("SYNC_AUTH"))
    {
        return;
    }
    if cfg
        .sync_next_allowed_at
        .map_or(false, |t| t > now_ts())
    {
        return;
    }
    let last = cfg.sync_last_upload_at.unwrap_or(0);
    if now_ts() - last < cfg.sync_publish_interval_sec as i64 {
        return;
    }
    if SYNC_SCHEDULED.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    let views = views.to_vec();
    tauri::async_runtime::spawn(async move {
        let _ = publish_views(&app, &views, true).await;
        SYNC_SCHEDULED.store(false, Ordering::SeqCst);
    });
}

async fn publish_views(app: &AppHandle, views: &[ChannelView], automatic: bool) -> Result<PublishOutcome, String> {
    let _guard = PUBLISH_LOCK.lock().await;
    let state = app.state::<AppState>();
    let cfg = cfg_of(state.inner())?;
    // 排队期间用户可能暂停或恢复了另一个组,不能再自动发布。
    if automatic && !cfg.sync_enabled {
        return Err("自动发布已暂停".into());
    }
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
