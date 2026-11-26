use super::*;
use crate::model_cmds::{inject_opts, model_spec_with, need_source};

/// 客户端状态 + 写入台账。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InjectOverview {
    pub(crate) clients: Vec<inject::ClientState>,
    pub(crate) log: Vec<inject::InjectRecord>,
}

#[tauri::command]
pub(crate) async fn inject_preview(
    state: State<'_, AppState>,
    client: String,
    source: Option<String>,
    model: String,
) -> Result<Vec<inject::Preview>, String> {
    let c = inject::Client::parse(&client).ok_or_else(|| format!("未知客户端: {client}"))?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        let src = need_source(source.clone())?;
        let spec = model_spec_with(&store, &cfg, &src, &model)?;
        Ok(inject::preview(c, &spec, &inject_opts(&cfg, c)))
    })
    .await
    .map_err(|e| format!("写入预告失败: {e}"))?
}

/// 执行写入。规矩(备份/校验/拒写运行中的目标)全在 core::inject 里。
#[tauri::command]
pub(crate) async fn inject_apply(
    state: State<'_, AppState>,
    client: String,
    source: Option<String>,
    model: String,
) -> Result<Vec<inject::Report>, String> {
    let c = inject::Client::parse(&client).ok_or_else(|| format!("未知客户端: {client}"))?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        let src = need_source(source.clone())?;
        let spec = model_spec_with(&store, &cfg, &src, &model)?;
        refuse_while_updating("写入客户端配置")?;
        let _busy = FlagGuard(&INJECTING);
        INJECTING.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut reports = inject::apply(c, &spec, &inject_opts(&cfg, c))?;
        // 记账:只记「写到哪、备份在哪、写了几条」——密钥不在其中
        {
            let s = lock_rw(&store);
            for r in &mut reports {
                // 没写成、或本来就没动盘:没有可回滚的东西,别往台账塞一条空备份路径
                // (那正是"历史里能看到、点回滚却报备份目录不存在"的来源)
                if r.error.is_some() || r.backup_path.is_empty() {
                    continue;
                }
                if let Err(e) = inject::push_log(
                    &s,
                    inject::InjectRecord {
                        // 读出来时会按目录是否还在重算,这里只需给个"刚写完当然在"的初值
                        backup_ok: true,
                        stamp: r.stamp.clone(),
                        at: now_ts(),
                        client: r.client.clone(),
                        label: r.label.clone(),
                        model: spec.id.clone(),
                        backup: r.backup.clone(),
                        backup_path: r.backup_path.clone(),
                        added: r.added,
                        updated: r.updated,
                        removed: r.removed,
                        selected: r.selected,
                    },
                    ) {
                        // 台账写不进去 = 这次以后在界面上回滚不了,必须说出来(P2-36)
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
    .map_err(|e| format!("写入失败: {e}"))?
}

/// 取消写入:把这条模型从客户端里删掉(写入的逆操作)。
/// 规矩完全一样:目标没运行、先备份、只动我们写的条目、写完读回校验,不过就回滚。
#[tauri::command]
pub(crate) async fn inject_remove(
    state: State<'_, AppState>,
    client: String,
    source: Option<String>,
    model: String,
) -> Result<Vec<inject::Report>, String> {
    let c = inject::Client::parse(&client).ok_or_else(|| format!("未知客户端: {client}"))?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        // 取消写入也要一个 spec:按 base_url / 名字认出"这条是不是我们写的"
        let src = need_source(source.clone())?;
        let spec = model_spec_with(&store, &cfg, &src, &model)?;
        refuse_while_updating("取消写入")?;
        let _busy = FlagGuard(&INJECTING);
        INJECTING.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut reports = inject::remove(c, &spec)?;
        {
            let s = lock_rw(&store);
            for r in &mut reports {
                if r.error.is_some() || r.backup_path.is_empty() {
                    continue;
                }
                if let Err(e) = inject::push_log(
                    &s,
                    inject::InjectRecord {
                        // 读出来时会按目录是否还在重算,这里只需给个"刚写完当然在"的初值
                        backup_ok: true,
                        stamp: r.stamp.clone(),
                        at: now_ts(),
                        client: r.client.clone(),
                        label: r.label.clone(),
                        model: spec.id.clone(),
                        backup: r.backup.clone(),
                        backup_path: r.backup_path.clone(),
                        added: 0,
                        updated: 0,
                        removed: r.removed,
                        selected: false,
                    },
                    ) {
                        r.notes.push(format!(
                            "台账没写进去:{e} —— 这次删除以后可能无法从界面回滚,备份目录仍在 {b}",
                            b = r.backup
                        ));
                    }
                }
            }
        Ok(reports)
    })
    .await
    .map_err(|e| format!("取消写入失败: {e}"))?
}

/// 四客户端状态 + 已写入的模型 + 台账。顺手把「选中项前缀口径」学下来并持久化:
/// 那是从客户端自己的文件里读出来的证据,不猜(见 core::inject::learn_prefix)。
#[tauri::command]
pub(crate) async fn inject_state(state: State<'_, AppState>) -> Result<InjectOverview, String> {
    refuse_if_config_broken()?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    let scanned = tokio::task::spawn_blocking(move || {
        let list = inject::state(&cfg.selection_prefix);
        let mut learned = cfg.selection_prefix.clone();
        let mut dirty = false;
        for st in &list {
            if let Some(p) = &st.learned_prefix {
                if learned.get(&st.client) != Some(p) {
                    learned.insert(st.client.clone(), p.clone());
                    dirty = true;
                }
            }
        }
        let log = {
            let s = lock_rw(&store);
            inject::load_log(&s)
        };
        (list, log, learned, dirty)
    })
    .await;
    let (clients, log, learned, dirty) = scanned.map_err(|e| {
        // 以前这里返回 Ok(空表):模型页显示成"这台机器没装 Trae / WorkBuddy / ZCode",
        // 用户去重装客户端甚至重装挂件,而真实原因(探测任务坏了)一点出口都没有(P2-35)
        format!("检测客户端状态失败:{e} —— 这是挂件的探测环节出问题,不代表你没装客户端")
    })?;
    if dirty && refuse_if_config_broken().is_ok() {
        // 学到的口径写回配置,过串行闸(报告 B4)。以前内存改完、克隆整份交给阻塞任务
        // 落盘,与 CFG_WRITE 闸内的写者可乱序 —— 旧 JSON 后落盘会把刚学到的口径拖回去。
        // 配置损坏时不动配置(P1-16):口径丢了没关系,下次检测会重新学回来。
        if let Err(e) = save_config_off_main(&state, |c| c.selection_prefix = learned).await {
            eprintln!("客户端口径落库失败: {e}");
        }
    }
    Ok(InjectOverview { clients, log })
}

/// 回滚一次写入:把备份目录里的文件放回原处。目标在跑就拒回滚(同写入的规矩)。
#[tauri::command]
pub(crate) async fn inject_rollback(state: State<'_, AppState>, backup: String) -> Result<String, String> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        let path = std::path::PathBuf::from(backup.trim());
        if !path.is_dir() {
            return Err(format!("备份目录不存在: {}", path.display()));
        }
        let log = {
            let s = lock_rw(&store);
            inject::load_log(&s)
        };
        // 归属由"这条备份落在哪个客户端配置目录的 backup-inject 下"决定,而不是只查台账:
        // 台账只留 20 条、以前 Trae 的记录备份路径干脆是空 —— 查不到时以前**直接跳过**
        // 运行检查(fail-open),等于给前端一个"把任意目录里的文件拷到别处"的原语(P2-36)。
        //
        // 旧实现是"字符串前缀 + 不解析 ..":`…\backup-inject\..\..\..\Temp\evil` 与
        // `…\backup-injectXYZ` 都能通过。现在:拒 `..`、要求父目录真叫 backup-inject、
        // 两侧归一成真实路径再比。
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("备份路径里不允许出现 ..".to_string());
        }
        let in_backup = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.eq_ignore_ascii_case("backup-inject"))
            .unwrap_or(false);
        if !in_backup {
            return Err(
                "这条备份不在任何客户端的 backup-inject 目录下,来历不明,挂件不执行回滚"
                    .to_string(),
            );
        }
        let canon = std::fs::canonicalize(&path).map_err(|e| format!("备份路径无效: {e}"))?;
        let owner = inject::sites().into_iter().find(|site| {
            std::fs::canonicalize(site.root.join("backup-inject"))
                .map(|b| canon.starts_with(&b))
                .unwrap_or(false)
        });
        let Some(site) = owner else {
            return Err(
                "这条备份不在任何客户端的 backup-inject 目录下,来历不明,挂件不执行回滚"
                    .to_string(),
            );
        };
        if site.running && site.client.blocks_when_running() {
            return Err(format!(
                "{} 正在运行,先退出它再回滚(否则回滚会被它的内存态覆盖)",
                site.label
            ));
        }
        let _ = &log;
        inject::restore(&path)
    })
    .await
    .map_err(|e| format!("回滚失败: {e}"))?
}
