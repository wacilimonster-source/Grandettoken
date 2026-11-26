use crate::{
    sync_cmds::{cfg_of, emit_status, oauth_client_id, save_sync_config},
    AppState,
};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};
use tokenscope_core::{
    fetch::now_ts,
    secrets,
    sync::device_flow::{self, DeviceSession, OAuthPoll, OAuthStart, TokenDecision},
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

fn clear(session: &DeviceSession) {
    let mut active = flow();
    if active
        .as_ref()
        .is_some_and(|attempt| attempt.same_attempt(session))
    {
        *active = None;
    }
}

#[tauri::command]
pub async fn sync_oauth_start(state: State<'_, AppState>) -> Result<OAuthStart, String> {
    let cfg = cfg_of(state.inner())?;
    let client_id = oauth_client_id(&cfg).ok_or_else(|| {
        "SYNC_NOT_CONFIGURED: 桌面 OAuth App 未配置 client_id,开发者需先在 GitHub 建桌面 OAuth App".to_string()
    })?;
    let (session, started) = device_flow::start(&client_id).await?;
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
    let Some(client_id) = oauth_client_id(&cfg) else {
        return Ok(OAuthPoll::Error {
            code: tokenscope_core::sync::error::NOT_CONFIGURED.into(),
            message: "桌面 OAuth App 未配置 client_id,请重新获取".into(),
        });
    };
    let decision = device_flow::poll(&session, &client_id).await?;
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
    if !is_current(&session) {
        return Ok(OAuthPoll::NoSession);
    }
    if let Err(message) = secrets::sync_set(secrets::SYNC_OAUTH_ACCOUNT, &access_token) {
        clear(&session);
        return Ok(OAuthPoll::Error {
            code: "SYNC_STORE".into(),
            message,
        });
    }
    let login = user.login.clone();
    let user_id = user.id;
    let state = app.state::<AppState>();
    if let Err(e) = save_sync_config(state.inner(), move |cfg| {
        cfg.sync_github_login = Some(login);
        cfg.sync_github_user_id = Some(user_id);
        cfg.sync_last_error = None;
        cfg.sync_last_error_at = None;
        cfg.sync_next_allowed_at = None;
    })
    .await
    {
        let _ = secrets::sync_delete(secrets::SYNC_OAUTH_ACCOUNT);
        clear(&session);
        return Ok(OAuthPoll::Error {
            code: "SYNC_STORE".into(),
            message: format!("保存 GitHub 登录状态失败: {e}"),
        });
    }

    clear(&session);
    emit_status(&app);
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
    if value.chars().count() > 100 || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("Client ID 格式不正确(不能包含空格或控制字符)".into());
    }
    let opt = if value.is_empty() { None } else { Some(value) };
    save_sync_config(state.inner(), move |c| c.sync_oauth_client_id = opt).await?;
    emit_status(&app);
    Ok(())
}
