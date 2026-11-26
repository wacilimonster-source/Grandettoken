use futures::{stream, StreamExt};
use tauri::{AppHandle, State};
use tokenscope_core::{
    fetch::now_ts, secrets,
    sync::{self, code::SyncCode, crypto, device_flow::github_user, gist::GistClient, publish,
        recovery::{self, RecoveryCandidate, RecoverySearch, VerifiedRecovery}, FetchedSources},
};
use crate::{lock_rw, AppState};
use crate::sync_cmds::*;

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
pub struct RecoveryResult {
    pub has_local_source: bool,
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
    let store = lock_rw(&state.store);
    recovery::verify(&store, fetched, owner_id)
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
    let (ids, truncated) = recovery::candidate_ids(ids, cfg.sync_relay_id.as_deref());
    let state_ref = state.inner();
    let owner_id = user.id;
    let checks = stream::iter(ids).map(|id| {
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
    Ok(recovery::collect_candidates(checks, truncated))
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
