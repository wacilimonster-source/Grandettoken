//! 本地网关进阶能力 Tauri 命令（路由组、台账统计、Prompt 脱敏、CLI/客户端网关接入）。

use crate::gateway::get_gateway;
use crate::{lock_rw, model_cmds, refuse_if_config_broken, refuse_while_updating, AppState, FlagGuard, INJECTING};
use serde::Deserialize;
use serde_json::Value;
use tauri::{Manager, State};
use tokenscope_core::fetch::now_ts;
use tokenscope_core::gateway::routing::{RouteGroup, RouteTarget, RoutingStrategy};
use tokenscope_core::gateway::{effective_models, lock};
use tokenscope_core::gateway_proto::{masking::MaskingSettings, normalize_alias};
use tokenscope_core::inject::{self, GatewayInjectSpec};
use tokenscope_core::store_gateway::GatewayUsageSummary;

fn default_true() -> bool {
    true
}
fn default_sticky_ttl() -> u64 {
    1800
}
fn default_cooldown_secs() -> u64 {
    60
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteGroupInput {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub strategy: String,
    #[serde(default = "default_true")]
    pub sticky: bool,
    #[serde(default = "default_sticky_ttl")]
    pub sticky_ttl_secs: u64,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    #[serde(default)]
    pub targets: Vec<RouteTarget>,
}

#[tauri::command]
pub async fn gateway_save_group(
    app: tauri::AppHandle,
    input: RouteGroupInput,
) -> Result<Value, String> {
    let g = get_gateway(&app)?;
    let id = normalize_alias(&input.id)
        .map_err(|e| format!("路由组 ID 不合法（将用于 group/<id>）：{e}"))?;
    let name = if input.name.trim().is_empty() {
        id.clone()
    } else {
        input.name.trim().to_string()
    };
    let strategy = RoutingStrategy::parse(&input.strategy);
    let sticky_ttl_secs = input.sticky_ttl_secs.clamp(60, 86_400);
    let cooldown_secs = input.cooldown_secs.clamp(5, 600);
    let targets: Vec<RouteTarget> = input
        .targets
        .into_iter()
        .filter(|t| !t.channel_id.trim().is_empty() && !t.model.trim().is_empty())
        .map(|t| RouteTarget {
            channel_id: t.channel_id.trim().to_string(),
            model: t.model.trim().to_string(),
        })
        .collect();
    if targets.is_empty() {
        return Err("请至少添加一个上游渠道与模型成员".into());
    }

    {
        let mut inner = lock(&g.inner);
        for t in &targets {
            let Some(ch) = inner
                .settings
                .channels
                .iter()
                .find(|c| c.channel_id == t.channel_id)
            else {
                return Err(format!("成员渠道 {} 不存在或已下架", t.channel_id));
            };
            let eff = effective_models(ch);
            if !eff.iter().any(|m| m == &t.model) {
                return Err(format!("渠道「{}」未开放模型 {}", ch.alias, t.model));
            }
        }
        let group = RouteGroup {
            id: id.clone(),
            name,
            strategy,
            sticky: input.sticky,
            sticky_ttl_secs,
            cooldown_secs,
            targets,
        };
        let mut next = inner.settings.clone();
        if let Some(pos) = next
            .groups
            .iter()
            .position(|gr| gr.id.eq_ignore_ascii_case(&id))
        {
            next.groups[pos] = group;
        } else {
            next.groups.push(group);
        }
        g.commit_settings(&mut inner, next)?;
    }
    crate::gateway::gateway_status(app).await
}

#[tauri::command]
pub async fn gateway_delete_group(app: tauri::AppHandle, id: String) -> Result<Value, String> {
    let g = get_gateway(&app)?;
    let target = id.trim().to_string();
    if target.is_empty() {
        return Err("缺少路由组 ID".into());
    }
    {
        let mut inner = lock(&g.inner);
        let mut next = inner.settings.clone();
        let before = next.groups.len();
        next.groups.retain(|gr| !gr.id.eq_ignore_ascii_case(&target));
        if next.groups.len() == before {
            return Err("路由组不存在或已删除".into());
        }
        g.commit_settings(&mut inner, next)?;
        inner.routing.rr_cursors.remove(&target);
    }
    crate::gateway::gateway_status(app).await
}

#[tauri::command]
pub async fn gateway_set_masking(
    app: tauri::AppHandle,
    mut input: MaskingSettings,
) -> Result<Value, String> {
    let g = get_gateway(&app)?;
    input.custom_patterns = input
        .custom_patterns
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() >= 3)
        .take(20)
        .collect();
    {
        let mut inner = lock(&g.inner);
        let mut next = inner.settings.clone();
        next.masking = input;
        g.commit_settings(&mut inner, next)?;
    }
    crate::gateway::gateway_status(app).await
}

#[tauri::command]
pub async fn gateway_usage_summary(
    app: tauri::AppHandle,
    days: Option<i64>,
    recent_limit: Option<usize>,
) -> Result<GatewayUsageSummary, String> {
    let g = get_gateway(&app)?;
    let d = days.unwrap_or(7).clamp(1, 90);
    let since_ts = tokenscope_core::gateway::http::now_unix() - d * 86_400;
    let limit = recent_limit.unwrap_or(40).clamp(1, 200);
    g.flush_ledger().await?;
    let store = app.state::<AppState>().read_store.clone();
    tokio::task::spawn_blocking(move || lock(&store).gateway_usage_summary(since_ts, limit))
        .await
        .map_err(|e| format!("读取网关台账失败: {e}"))?
}

#[tauri::command]
pub async fn gateway_clear_usage(app: tauri::AppHandle) -> Result<(), String> {
    let g = get_gateway(&app)?;
    g.clear_ledger().await
}

#[tauri::command]
pub(crate) async fn gateway_inject_clients(
    state: State<'_, AppState>,
) -> Result<crate::inject_cmds::InjectOverview, String> {
    refuse_if_config_broken()?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        let clients = inject::state(&cfg.selection_prefix);
        let log = {
            let s = lock_rw(&store);
            inject::load_log(&s)
        };
        crate::inject_cmds::InjectOverview { clients, log }
    })
    .await
    .map_err(|e| format!("读取客户端状态失败: {e}"))
}

async fn resolve_gateway_spec(
    app: &tauri::AppHandle,
    model_id: &str,
    is_group: bool,
) -> Result<GatewayInjectSpec, String> {
    let g = get_gateway(app)?;
    let clean_model = model_id.trim().to_string();
    if clean_model.is_empty() {
        return Err("请选择要接入网关的模型或虚拟路由组".into());
    }
    let port = lock(&g.inner).settings.port;
    let token = g.access_secret(false).await?;
    Ok(GatewayInjectSpec::new(port, &token, &clean_model, is_group))
}

#[tauri::command]
pub(crate) async fn gateway_inject_preview(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    client: String,
    model_id: String,
    is_group: bool,
) -> Result<Vec<inject::Preview>, String> {
    let c = inject::Client::parse(&client).ok_or_else(|| format!("未知客户端: {client}"))?;
    let spec = resolve_gateway_spec(&app, &model_id, is_group).await?;
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        Ok(inject::preview_gateway(
            c,
            &spec,
            &model_cmds::inject_opts(&cfg, c),
        ))
    })
    .await
    .map_err(|e| format!("生成写入预告失败: {e}"))?
}

#[tauri::command]
pub(crate) async fn gateway_inject_apply(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    client: String,
    model_id: String,
    is_group: bool,
) -> Result<Vec<inject::Report>, String> {
    let c = inject::Client::parse(&client).ok_or_else(|| format!("未知客户端: {client}"))?;
    let spec = resolve_gateway_spec(&app, &model_id, is_group).await?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        refuse_while_updating("写入本地网关客户端配置")?;
        let _busy = FlagGuard(&INJECTING);
        INJECTING.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut reports = inject::apply_gateway(c, &spec, &model_cmds::inject_opts(&cfg, c))?;
        {
            let s = lock_rw(&store);
            for r in &mut reports {
                if r.error.is_some() || r.backup_path.is_empty() {
                    continue;
                }
                if let Err(e) = inject::push_log(
                    &s,
                    inject::InjectRecord {
                        backup_ok: true,
                        stamp: r.stamp.clone(),
                        at: now_ts(),
                        client: r.client.clone(),
                        label: r.label.clone(),
                        model: spec.model_id.clone(),
                        backup: r.backup.clone(),
                        backup_path: r.backup_path.clone(),
                        added: r.added,
                        updated: r.updated,
                        removed: r.removed,
                        selected: r.selected,
                    },
                ) {
                    r.notes.push(format!(
                        "台账没写进去:{e} —— 这次写入以后可能无法从界面回滚,备份目录仍在 {b}",
                        b = r.backup
                    ));
                }
            }
        }
        Ok(reports)
    })
    .await
    .map_err(|e| format!("写入本地网关配置失败: {e}"))?
}

#[tauri::command]
pub(crate) async fn gateway_inject_remove(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    client: String,
    model_id: String,
    is_group: bool,
) -> Result<Vec<inject::Report>, String> {
    let c = inject::Client::parse(&client).ok_or_else(|| format!("未知客户端: {client}"))?;
    let spec = resolve_gateway_spec(&app, &model_id, is_group).await?;
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        refuse_while_updating("移除本地网关客户端配置")?;
        let _busy = FlagGuard(&INJECTING);
        INJECTING.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut reports = inject::remove_gateway(c, &spec)?;
        {
            let s = lock_rw(&store);
            for r in &mut reports {
                if r.error.is_some() || r.backup_path.is_empty() {
                    continue;
                }
                if let Err(e) = inject::push_log(
                    &s,
                    inject::InjectRecord {
                        backup_ok: true,
                        stamp: r.stamp.clone(),
                        at: now_ts(),
                        client: r.client.clone(),
                        label: r.label.clone(),
                        model: spec.model_id.clone(),
                        backup: r.backup.clone(),
                        backup_path: r.backup_path.clone(),
                        added: 0,
                        updated: 0,
                        removed: r.removed,
                        selected: false,
                    },
                ) {
                    r.notes.push(format!(
                        "台账没写进去:{e} —— 这次取消写入以后可能无法从界面回滚,备份目录仍在 {b}",
                        b = r.backup
                    ));
                }
            }
        }
        Ok(reports)
    })
    .await
    .map_err(|e| format!("移除本地网关配置失败: {e}"))?
}

#[tauri::command]
pub(crate) async fn gateway_inject_drift() -> Result<Vec<Value>, String> {
    tokio::task::spawn_blocking(|| {
        tokenscope_core::inject::drift_all()
            .into_iter()
            .map(|(client, label, message)| {
                serde_json::json!({"client": client, "label": label, "message": message})
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| format!("读取接入漂移失败: {e}"))
}
