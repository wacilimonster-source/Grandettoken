//! Codex 重置顾问的 Tauri 命令层。
//!
//! 这里只做三件事：校验 `id`、按当前 Codex 账号 scope 读写 profile、把当前 `ChannelView`
//! 交给 `codex_reset::advise`。不读配额快照里的卡数据、不发网络请求、不兑换卡。

use tauri::State;
use tokenscope_core::codex_reset::{
    advise, local_today_end, Advice, AdviceInput, Profile, ProfilePatch,
};
use tokenscope_core::fetch::now_ts;
use tokenscope_core::recovery::Snapshot;
use tokenscope_core::store::Store;

use crate::{lock_rw, AppState};

const PROFILE_PREFIX: &str = "codex_reset_profile_v1";

fn ensure_id(id: &str) -> Result<(), String> {
    if id == "codex" {
        Ok(())
    } else {
        Err("重置顾问只支持 Codex".into())
    }
}

fn profile_key(scope: &str) -> String {
    format!("{PROFILE_PREFIX}:{scope}")
}

fn scope_of(store: &Store, id: &str) -> Option<String> {
    Snapshot::stored_scope(store, id)
}

fn load_profile(store: &Store, id: &str) -> Result<(Profile, Option<String>), String> {
    let scope = scope_of(store, id);
    let Some(scope) = scope else {
        return Ok((Profile::default(), None));
    };
    let raw = store.get_setting(&profile_key(&scope))?;
    let mut profile = match raw {
        // 旧版或损坏的本地设置按未填写处理，允许用户重新录入并覆盖。
        Some(raw) => serde_json::from_str::<Profile>(&raw).unwrap_or_default(),
        None => Profile::default(),
    };
    profile.scope = Some(scope.clone());
    Ok((profile, Some(scope)))
}

fn save_profile(store: &Store, scope: &str, profile: &Profile) -> Result<(), String> {
    let raw = serde_json::to_string(profile).map_err(|e| format!("序列化重置顾问设置失败: {e}"))?;
    store.set_setting(&profile_key(scope), &raw)
}

#[tauri::command]
pub fn codex_reset_profile_get(
    state: State<'_, AppState>,
    id: String,
) -> Result<Profile, String> {
    ensure_id(&id)?;
    let store = lock_rw(&state.store);
    let (profile, _scope) = load_profile(&store, &id)?;
    Ok(profile)
}

#[tauri::command]
pub fn codex_reset_profile_set(
    state: State<'_, AppState>,
    id: String,
    profile: ProfilePatch,
) -> Result<Profile, String> {
    ensure_id(&id)?;
    let store = lock_rw(&state.store);
    let (mut current, scope) = load_profile(&store, &id)?;
    let Some(scope) = scope else {
        return Err("没找到 Codex 账号范围，订阅到期日只在本轮生效，未落盘".into());
    };
    if profile.scope.as_deref() != Some(scope.as_str()) {
        return Err("Codex 账号已变化，请刷新后重新设置".into());
    }
    if profile.clear_sub_end {
        current.sub_end_at = None;
    } else if let Some(at) = profile.sub_end_at {
        if at <= 0 {
            return Err("订阅到期日必须是有效日期".into());
        }
        current.sub_end_at = Some(at);
    }
    if let Some(need) = profile.need {
        current.need = need;
    }
    current.updated_at = now_ts();
    save_profile(&store, &scope, &current)?;
    Ok(current)
}

/// 给当前 Codex 详情卡算一次建议。
///
/// `input` 由前端从同一份 ChannelView 构造；scope 指纹用于拒绝换号后迟到的请求。
#[tauri::command]
pub fn codex_reset_advice(
    state: State<'_, AppState>,
    id: String,
    input: AdviceInput,
) -> Result<Advice, String> {
    ensure_id(&id)?;
    let now = now_ts();
    let store = lock_rw(&state.store);
    let scope = scope_of(&store, &id);
    if input.scope != scope {
        return Err("Codex 账号已变化，请刷新后重新计算".into());
    }
    // 写操作和读判定分开：命令层不碰卡，也不改 profile；纯函数只读输入。
    // 「今天要用」的当天边界由本机时区补齐；纯函数不读时钟以外的环境。
    let mut input = input;
    if input.today_end_at.is_none() {
        input.today_end_at = local_today_end(now);
    }
    Ok(advise(&input, now))
}
