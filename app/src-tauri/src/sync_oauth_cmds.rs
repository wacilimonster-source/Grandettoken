use crate::{
    sync_cmds::{cfg_of, emit_status, maybe_publish, oauth_client_id, save_sync_config, PUBLISH_LOCK},
    AppState,
};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};
use tokenscope_core::{
    fetch::now_ts,
    secrets,
    sync::{
        device_flow::{self, DeviceSession, OAuthPoll, OAuthStart, TokenDecision},
        profile, GetOutcome, GistClient, SyncError,
    },
};

static OAUTH: Mutex<Option<DeviceSession>> = Mutex::new(None);

fn flow() -> std::sync::MutexGuard<'static, Option<DeviceSession>> {
    OAUTH.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn is_current(session: &DeviceSession) -> bool {
    flow()
        .as_ref()
        .is_some_and(|active| active.same_attempt(session))
}

fn clear(session: &DeviceSession) -> bool {
    let mut active = flow();
    if active
        .as_ref()
        .is_some_and(|attempt| attempt.same_attempt(session))
    {
        *active = None;
        return true;
    }
    false
}

async fn verify_owner(gist_id: &str, token: &str, user_id: i64) -> Result<(), SyncError> {
    let client = GistClient::new(Some(token.to_owned()))?;
    match client.get(gist_id, None).await? {
        GetOutcome::Loaded(gist) if gist.owner_id == Some(user_id) => Ok(()),
        _ => Err(SyncError::new(
            "SYNC_ACCOUNT_MISMATCH",
            "请使用创建该同步组的 GitHub 账号重新授权",
        )),
    }
}

#[tauri::command]
pub async fn sync_oauth_start(state: State<'_, AppState>) -> Result<OAuthStart, String> {
    let cfg = cfg_of(state.inner())?;
    let (session, started) = device_flow::start(oauth_client_id(&cfg)).await?;
    *flow() = Some(session);
    Ok(started)
}

#[tauri::command]
pub async fn sync_oauth_poll(app: AppHandle) -> Result<OAuthPoll, String> {
    let session = flow().as_ref().cloned();
    let Some(session) = session else {
        return Ok(OAuthPoll::NoSession);
    };
    if session.expired(now_ts()) {
        clear(&session);
        return Ok(OAuthPoll::Error {
            code: "SYNC_OAUTH_EXPIRED".into(),
            message: "授权码已过期,请重新获取".into(),
        });
    }

    let cfg = cfg_of(app.state::<AppState>().inner())?;
    let decision = device_flow::poll(&session, oauth_client_id(&cfg)).await?;
    if !is_current(&session) {
        return Ok(OAuthPoll::NoSession);
    }
    let access_token = match decision {
        TokenDecision::Pending {
            interval,
            slow_down,
        } => {
            if let Some(active) = flow()
                .as_mut()
                .filter(|s| slow_down && s.same_attempt(&session))
            {
                active.set_interval(interval);
            }
            return Ok(OAuthPoll::Pending { interval });
        }
        TokenDecision::Error {
            code,
            message,
            clear: cancel,
        } => {
            if cancel {
                clear(&session);
            }
            return Ok(OAuthPoll::Error { code, message });
        }
        TokenDecision::Ready(token) => token,
    };

    let user = match device_flow::github_user(&access_token).await {
        Ok(user) => user,
        Err(message) => {
            clear(&session);
            return Ok(OAuthPoll::Error {
                code: "SYNC_AUTH".into(),
                message,
            });
        }
    };
    // 旧凭据的在途发布可能返回 401，必须先让它写完状态再替换凭据。
    let publish_guard = PUBLISH_LOCK.lock().await;
    if !is_current(&session) {
        return Ok(OAuthPoll::NoSession);
    }
    let state = app.state::<AppState>();
    let config = cfg_of(state.inner())?;
    if let Some(gist_id) = config.sync_relay_id.as_deref() {
        if let Err(e) = verify_owner(gist_id, &access_token, user.id).await {
            if !clear(&session) {
                return Ok(OAuthPoll::NoSession);
            }
            return Ok(OAuthPoll::Error { code: e.code.into(), message: e.message });
        }
    }
    if !clear(&session) {
        return Ok(OAuthPoll::NoSession);
    }
    let previous_token = secrets::sync_get(secrets::SYNC_OAUTH_ACCOUNT);
    let previous_token = match previous_token.and_then(|previous| {
        secrets::sync_set(secrets::SYNC_OAUTH_ACCOUNT, &access_token)?;
        Ok(previous)
    }) {
        Ok(previous) => previous,
        Err(message) => return Ok(OAuthPoll::Error { code: "SYNC_STORE".into(), message }),
    };
    let login = user.login.clone();
    let user_id = user.id;
    if let Err(e) = save_sync_config(state.inner(), move |cfg| {
        cfg.sync_github_login = Some(login);
        cfg.sync_github_user_id = Some(user_id);
        cfg.sync_last_error = None;
        cfg.sync_last_error_at = None;
        cfg.sync_next_allowed_at = None;
    })
    .await
    {
        let rollback = match previous_token.as_deref() {
            Some(token) => secrets::sync_set(secrets::SYNC_OAUTH_ACCOUNT, token),
            None => secrets::sync_delete(secrets::SYNC_OAUTH_ACCOUNT),
        };
        let mut message = format!("保存 GitHub 登录状态失败: {e}");
        if let Err(rollback) = rollback {
            message.push_str(&format!("，恢复原授权失败: {rollback}"));
        }
        return Ok(OAuthPoll::Error {
            code: "SYNC_STORE".into(),
            message,
        });
    }

    drop(publish_guard);
    emit_status(&app);
    if profile::github_needs_reauth(&config) {
        maybe_publish(&app, None);
    }
    Ok(OAuthPoll::Authorized {
        login: user.login,
        user_id,
    })
}

#[tauri::command]
pub async fn sync_oauth_cancel() -> Result<(), String> {
    *flow() = None;
    Ok(())
}

/// 旧安装版可保存公开 Client ID，定制构建也可用本机配置覆盖默认值。
#[tauri::command]
pub async fn sync_set_oauth_client_id(
    app: AppHandle,
    state: State<'_, AppState>,
    client_id: String,
) -> Result<(), String> {
    crate::refuse_if_config_broken()?;
    let value = client_id.trim().to_string();
    if value.chars().count() > 100 || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("Client ID 格式不正确(不能包含空格或控制字符)".into());
    }
    let opt = if value.is_empty() { None } else { Some(value) };
    save_sync_config(state.inner(), move |c| c.sync_oauth_client_id = opt).await?;
    emit_status(&app);
    Ok(())
}
