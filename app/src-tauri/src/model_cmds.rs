use super::*;

// ─────────────── 4SAPI 可用模型 + 一键配置到客户端 ───────────────
//
// 设计与取证见 `docs/design/design-4sapi-models.html`。三条纪律:
//   1. 模型清单是**元数据接口**,不产生 token 计费(与余额取数同一条原则);
//   2. 写进客户端一律「先备份 → 写 → 读回校验」,目标在跑就拒写(规矩在 core::inject 里);
//   3. 密钥只从凭据管理器**现读现写**,不回前端、不落盘、不进日志。

/// 来源芯片要的信息。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceInfo {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) has_price: bool,
    pub(crate) list_public: bool,
}

pub(crate) fn source_infos() -> Vec<SourceInfo> {
    models::SOURCES
        .iter()
        .map(|d| SourceInfo {
            id: d.id.to_string(),
            name: d.name.to_string(),
            has_price: d.pricing_path.is_some(),
            list_public: d.list_public,
        })
        .collect()
}

/// 模型页要的整份视图。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogView {
    /// 来源 id("4sapi" / "hapi" / "opencode-go")
    pub(crate) source: String,
    /// 来源的界面名
    pub(crate) source_name: String,
    /// 全部来源(界面上的芯片;没取到的来源也要列出来,标「—」)
    pub(crate) sources: Vec<SourceInfo>,
    /// 这个来源有没有价格(只有 4SAPI 有)。false → 界面不显示单价、也不按价排序
    pub(crate) has_price: bool,
    /// 列表是否公开(Go 是:没配密钥也能列出模型)
    pub(crate) list_public: bool,
    /// 价目表取数的域名
    pub(crate) host: String,
    /// 要写进客户端的接入地址(挂件实测可用的那个)
    pub(crate) base_url: String,
    pub(crate) version: String,
    pub(crate) fetched_at: i64,
    /// 可用集合的取回时间;0 = 从没取到过(界面据此把「可用」退成「未知」)
    pub(crate) usable_at: i64,
    pub(crate) has_key: bool,
    pub(crate) group: String,
    pub(crate) groups: Vec<models::Group>,
    pub(crate) total: usize,
    pub(crate) chat_count: usize,
    pub(crate) usable_count: usize,
    pub(crate) rows: Vec<models::RowView>,
    /// 刷新失败时的真实原因(界面显示上次快照并用它说明)
    pub(crate) error: Option<String>,
}

/// 客户端状态 + 写入台账。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InjectOverview {
    pub(crate) clients: Vec<inject::ClientState>,
    pub(crate) log: Vec<inject::InjectRecord>,
}

/// 写进客户端的接入地址:用户指定 > 实测可用的域名 > 主域名。
/// 写进客户端的接入地址。
/// 4SAPI:用户指定 > 实测可用的域名 > 主域名(它有几个域名,得挑一个真通的);
/// 其它来源:用来源定义里的固定地址。
pub(crate) fn source_base(cfg: &Config, cache: Option<&models::ModelCache>, def: &models::SourceDef) -> String {
    if def.id != "4sapi" {
        return def.base_url.to_string();
    }
    if let Some(u) = cfg.model_base_url.as_deref() {
        let t = u.trim().trim_end_matches('/');
        if !t.is_empty() {
            return t.to_string();
        }
    }
    match cache {
        Some(c) => c.base_url(),
        None => def.base_url.to_string(),
    }
}

/// 该来源的密钥。Go 的列表公开,但写进客户端必须带密钥。
/// Err = 凭据管理器真失败,必须如实往上传,别吞成「没配密钥」
/// —— 那会把用户支去重填密钥,可病根本不在密钥上(报告 L9,P2-15 同款)。
pub(crate) fn source_key(id: &str) -> Result<Option<String>, String> {
    secrets::channel_key(id).map(|k| k.filter(|k| !k.trim().is_empty()))
}


/// 入口顺序:配置里指定的域名优先,其次上次成功的(可用集合 > 价目表),最后默认顺序。
pub(crate) fn host_order(cfg: &Config, cache: Option<&models::ModelCache>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |h: String| {
        if !h.is_empty() && !out.iter().any(|x| x == &h) {
            out.push(h);
        }
    };
    if let Some(base) = cfg.model_base_url.as_deref() {
        if let Some(h) = providers::url_host(base) {
            push(h);
        }
    }
    if let Some(c) = cache {
        if c.usable_at > 0 {
            push(c.usable_host.clone());
        }
        push(c.host.clone());
    }
    for h in providers::four_s_api_hosts() {
        push(h);
    }
    out
}

/// 一个模型写进客户端时的全部信息。上下文/多模态从价目表缓存里取;
/// 缓存里没有就留空 —— 不编造能力上限。
///
/// **这里是阻塞活**(凭据管理器 / 读缓存),调用方负责把它放进 `spawn_blocking` ——
/// 见 `inject_preview` 上面那段注释。
pub(crate) fn model_spec_with(
    store: &Arc<Mutex<Store>>,
    cfg: &Config,
    source: &str,
    model: &str,
) -> Result<inject::ModelSpec, String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("没有指定模型".into());
    }
    let def = models::find_source(source).ok_or_else(|| format!("未知模型来源: {source}"))?;
    let key = match source_key(def.id) {
        Ok(Some(k)) => k,
        Ok(None) => {
            return Err(format!(
                "还没配置 {} 密钥:先在「渠道」页签里填一次",
                def.name
            ))
        }
        Err(e) => return Err(format!("{} 的凭据库读取失败:{e}", def.name)),
    };
    let cache = {
        let s = lock_rw(store);
        models::ModelCache::load(&s)
    };
    let row = cache
        .as_ref()
        .and_then(|c| c.catalog().ok())
        .and_then(|cat| cat.find(model).cloned());
    let context = row
        .as_ref()
        .and_then(|r| r.context.as_deref())
        .and_then(models::context_tokens);
    let multimodal = row.as_ref().map(|r| r.multimodal).unwrap_or(false);
    let base = source_base(cfg, cache.as_ref(), def);
    Ok(inject::ModelSpec::new(
        def.id,
        model,
        &base,
        &key,
        context,
        multimodal,
    )
    .with_api_protocol(models::api_protocol(def.id, model)))
}

pub(crate) fn inject_opts(cfg: &Config, client: inject::Client) -> inject::Opts {
    inject::Opts {
        // 学到的口径才写选中项;没学到就只写条目(core::inject::learn_prefix)
        selection_prefix: cfg.selection_prefix.get(client.id()).cloned(),
        trae_auto_select: cfg.trae_auto_select,
    }
}

/// 取 4SAPI 的可用模型:公开价目表 + 这把 key 能调的集合。
/// 两者都是元数据,不产生 token 计费;缓存一天,失败时退回上次快照并如实报错。
#[tauri::command]
pub(crate) async fn get_model_catalog(
    state: State<'_, AppState>,
    source: Option<String>,
    force: bool,
) -> Result<CatalogView, String> {
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    let client = state.fetcher.http.clone();
    let now = now_ts();
    let want = source.unwrap_or_default();
    let def = models::find_source(if want.is_empty() { "4sapi" } else { &want })
        .ok_or_else(|| format!("未知模型来源: {want}"))?;

    let mut cache = {
        let s = lock_rw(&store);
        models::ModelCache::load(&s)
    };
    let mut error: Option<String> = None;

    // ── 只有 id 列表的来源(Hapi / OpenCode Go):列表给什么就是什么,没有价、没有标签 ──
    if def.pricing_path.is_none() {
        // 凭据管理器读是 RPC,别占着 tokio worker(其余调用点都在 spawn_blocking 里)
        let sid = def.id;
        let key = tokio::task::spawn_blocking(move || source_key(sid))
            .await
            .map_err(|e| format!("读取密钥失败: {e}"))?
            .map_err(|e| format!("凭据库读取失败: {e}"))?;
        let need = force
            || cache
                .as_ref()
                .and_then(|c| c.sources.get(def.id))
                .map(|sc| sc.stale(now) || sc.list_path != def.list_path)
                .unwrap_or(true);
        if need {
            match models::fetch_source_ids(&client, def, key.as_deref()).await {
                Ok((ids, host)) => {
                    let c = cache.get_or_insert_with(Default::default);
                    let sc = c.sources.entry(def.id.to_string()).or_default();
                    sc.ids = ids.clone();
                    sc.fetched_at = now;
                    sc.host = host;
                    sc.list_path = def.list_path.to_string();
                    // 对这个来源而言,"能用"就是列表本身(公开列表 = 它提供这些模型)
                    sc.usable = ids;
                    sc.usable_at = now;
                }
                Err(e) => error = Some(e),
            }
        }
        if let Some(c) = cache.as_ref() {
            let s = lock_rw(&store);
            if let Err(e) = c.save(&s) {
                eprintln!("模型缓存写入失败: {e}");
            }
        }
        let sc = cache.as_ref().and_then(|c| c.sources.get(def.id));
        let (ids, usable, usable_at) = match sc {
            Some(sc) => (sc.ids.clone(), sc.usable.clone(), sc.usable_at),
            None => (Vec::new(), Vec::new(), 0),
        };
        let rows = models::rows_from_source_ids(def.id, &ids, &usable, usable_at > 0);
        return Ok(CatalogView {
            source: def.id.to_string(),
            source_name: def.name.to_string(),
            sources: source_infos(),
            has_price: false,
            list_public: def.list_public,
            host: sc.map(|s| s.host.clone()).unwrap_or_default(),
            base_url: def.base_url.to_string(),
            version: String::new(),
            fetched_at: sc.map(|s| s.fetched_at).unwrap_or(0),
            usable_at,
            has_key: key.is_some(),
            group: String::new(),
            groups: Vec::new(),
            total: rows.len(),
            chat_count: rows.len(),
            usable_count: rows.iter().filter(|r| r.usable == Some(true)).count(),
            rows: models::row_views_of(rows, &[], ""),
            error,
        });
    }

    // ── 4SAPI:价目表 + 可用集合(它有四个域名,挑真通的)──
    let hosts = host_order(&cfg, cache.as_ref());

    let needs_catalog = force || cache.as_ref().map(|c| c.stale(now)).unwrap_or(true);
    if needs_catalog {
        match models::fetch_catalog(&client, &hosts).await {
            Ok(f) => {
                let c = cache.get_or_insert_with(Default::default);
                c.version = f.catalog.version.clone();
                c.fetched_at = now;
                c.pricing = f.raw.to_string();
                c.host = f.host;
            }
            // 失败不把价目表清空:显示上次快照 + 真实原因(与余额取数同一契约)
            Err(e) => error = Some(e),
        }
    }

    // 凭据管理器读是 RPC,下沉到阻塞任务(模型页虽然是人点的,但一次要串好几个请求)
    let key = tokio::task::spawn_blocking(|| secrets::channel_key("4sapi"))
        .await
        .map_err(|e| format!("读取 4SAPI 密钥失败: {e}"))?
        .ok()
        .flatten()
        .filter(|k| !k.trim().is_empty());
    let needs_usable = cache
        .as_ref()
        .map(|c| force || c.usable_at <= 0 || now - c.usable_at > models::CACHE_TTL_SEC)
        .unwrap_or(true);
    if let (Some(k), true) = (key.as_ref(), needs_usable) {
        match models::fetch_usable(&client, &hosts, k).await {
            Ok((ids, host)) => {
                if let Some(c) = cache.as_mut() {
                    c.usable = ids;
                    c.usable_at = now;
                    c.usable_host = host;
                }
            }
            Err(e) => {
                if error.is_none() {
                    error = Some(e);
                }
            }
        }
    }

    if let Some(c) = cache.as_ref() {
        let s = lock_rw(&store);
        if let Err(e) = c.save(&s) {
            eprintln!("模型缓存写入失败: {e}");
        }
    }

    let Some(cache) = cache else {
        return Ok(CatalogView {
            source: def.id.to_string(),
            source_name: def.name.to_string(),
            sources: source_infos(),
            has_price: true,
            list_public: def.list_public,
            host: String::new(),
            base_url: source_base(&cfg, None, def),
            version: String::new(),
            fetched_at: 0,
            usable_at: 0,
            has_key: key.is_some(),
            group: String::new(),
            groups: Vec::new(),
            total: 0,
            chat_count: 0,
            usable_count: 0,
            rows: Vec::new(),
            error: error.or_else(|| Some("还没有价目表缓存".into())),
        });
    };

    let mut catalog = cache.catalog()?;
    if cache.usable_known() {
        // 只有**成功取到过**才打「不可用」标记:一次网络失败不该让 160 个模型全变红
        catalog.mark_usable(&cache.usable);
    }
    let group = cfg
        .model_group
        .clone()
        .filter(|g| !g.trim().is_empty())
        .unwrap_or_else(|| catalog.default_group());
    let rows = models::row_views(&catalog, &group);
    Ok(CatalogView {
        source: def.id.to_string(),
        source_name: def.name.to_string(),
        sources: source_infos(),
        has_price: true,
        list_public: def.list_public,
        host: cache.host.clone(),
        base_url: source_base(&cfg, Some(&cache), def),
        version: cache.version.clone(),
        fetched_at: cache.fetched_at,
        usable_at: cache.usable_at,
        has_key: key.is_some(),
        group,
        groups: catalog.groups.clone(),
        total: catalog.rows.len(),
        chat_count: catalog.chat_count(),
        usable_count: catalog.rows.iter().filter(|r| r.usable == Some(true)).count(),
        rows,
        error,
    })
}

/// 写入预告(纯读,绝不写盘)。
///
/// `async` + `spawn_blocking` 不是风格问题:**非 async 的 Tauri 命令跑在主线程上**,
/// 而这条路上要起 `tasklist` 探进程、读客户端配置、读凭据管理器 —— 全是阻塞活。
/// 真机实测过一次卡住:整个挂件不再响应任何命令(WebView 的 IPC 一起排队),
/// 连 WorkBuddy 渠道的会话读取都跟着超时。项目里早有同一条教训(取数路径的
/// 文件 IO / keyring 都下沉 spawn_blocking),这里照同一套办。
/// 界面上「当前来源」必须由前端一起传进来。
/// 以前缺省静默落到 "4sapi":用户在 Hapi / OpenCode Go 的模型上操作,拿到的却是
/// 4SAPI 的接入地址 + 4SAPI 的密钥(报告 P0-5)。缺参数就该失败,不该替用户猜。
pub(crate) fn need_source(source: Option<String>) -> Result<String, String> {
    let s = source.unwrap_or_default().trim().to_string();
    if s.is_empty() {
        return Err("没指定模型来源:请在模型行的来源芯片上操作".to_string());
    }
    Ok(s)
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

/// 「复制三件套」。密钥由后端拼进字符串,**不回前端**。
/// 注意:写入剪贴板对其他程序(含剪贴板历史)可见 —— 界面必须写明这一点。
#[tauri::command]
pub(crate) async fn copy_model_bundle(
    state: State<'_, AppState>,
    source: Option<String>,
    model: String,
) -> Result<String, String> {
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        let src = need_source(source.clone())?;
        let spec = model_spec_with(&store, &cfg, &src, &model)?;
        let protocol = spec
            .api_protocol
            .ok_or_else(|| format!("官网未公布 {} 的 API 协议,不能生成可靠的接入配置", spec.id))?;
        let endpoint = spec
            .endpoint_url()
            .ok_or_else(|| format!("{} 没有可用的完整端点", spec.id))?;
        let text = format!(
            "接口协议:{}\n接入地址:{}\n完整端点:{}\n模型名称:{}\nAPI Key:{}",
            protocol.label(), spec.base_url, endpoint, spec.id, spec.key
        );
        copy_to_clipboard(&text)?;
        // 前端在 msg 后统一拼「· 剪贴板对其他程序可见」,这里别再重复「已复制」
        Ok("已复制 接口地址 + 模型名 + 密钥".to_string())
    })
    .await
    .map_err(|e| format!("复制失败: {e}"))?
}

/// 模型基本信息:手工在客户端里新建自定义模型时该填的那几个值。
///
/// 为什么要有这条:自动写入有时客户端不认(真机:Trae 的日志把条目收进了 custom_models,
/// 界面列表却没出现),那时候用户得自己在这个客户端的"自定义模型"里手填一遍。
/// 所以他得能看见这些值 —— 密钥在这里**只给后四位**,明文走 key_reveal 那条专用路。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelInfoView {
    /// 模型标识(OpenAI 兼容接口的 model 参数;Trae 里也是它)
    pub(crate) id: String,
    /// 客户端里的显示名口径(`4S-<id>` / `HP-<id>` / `OG <目录名>`;按来源不同)
    pub(crate) display_name: String,
    pub(crate) source: String,
    pub(crate) source_name: String,
    /// 当前模型实际使用的协议。
    pub(crate) api_type: String,
    /// 该模型应填写的 base_url;OpenCode Go 按模型协议分别设置。
    pub(crate) base_url: String,
    /// 完整请求端点。
    pub(crate) endpoint: String,
    pub(crate) context_tokens: Option<i64>,
    pub(crate) multimodal: bool,
    pub(crate) has_key: bool,
    /// 密钥后四位(取不到就是 None)
    pub(crate) key_hint: Option<String>,
}

#[tauri::command]
pub(crate) async fn model_info(
    state: State<'_, AppState>,
    source: Option<String>,
    model: String,
) -> Result<ModelInfoView, String> {
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        let src = need_source(source)?;
        let def = models::find_source(&src).ok_or_else(|| format!("未知模型来源: {src}"))?;
        let model = model.trim().to_string();
        if model.is_empty() {
            return Err("没有指定模型".into());
        }
        // 与写入用同一份清单:上下文/多模态从价目表来,未知就不编(不要求必须配了密钥 ——
        // 没配密钥的来源也要能把地址和模型名给人看)
        let cache = {
            let s = lock_rw(&store);
            models::ModelCache::load(&s)
        };
        let row = cache
            .as_ref()
            .and_then(|c| c.catalog().ok())
            .and_then(|cat| cat.find(&model).cloned());
        let protocol = models::api_protocol(def.id, &model);
        let base = match protocol {
            Some(p) if def.id == "opencode-go" => p.base_url().to_string(),
            Some(_) => source_base(&cfg, cache.as_ref(), def),
            None => String::new(),
        };
        let key = source_key(def.id).map_err(|e| format!("凭据库读取失败: {e}"))?;
        Ok(ModelInfoView {
            id: model.clone(),
            display_name: inject::display_name_of(def.id, &model),
            source: def.id.to_string(),
            source_name: def.name.to_string(),
            api_type: protocol
                .map(models::ApiProtocol::label)
                .unwrap_or("官网未标注协议，暂停自动接入")
                .to_string(),
            endpoint: protocol
                .map(|p| p.endpoint(&base, &model))
                .unwrap_or_default(),
            base_url: base,
            context_tokens: row
                .as_ref()
                .and_then(|r| r.context.as_deref())
                .and_then(models::context_tokens),
            multimodal: row.as_ref().map(|r| r.multimodal).unwrap_or(false),
            has_key: key.is_some(),
            key_hint: key.as_deref().map(secrets::mask_of),
        })
    })
    .await
    .map_err(|e| format!("取模型信息失败: {e}"))?
}

/// 复制任意一段文本到剪贴板(模型信息的逐项复制)。**明文密钥不走这里** ——
/// 那一条有 key_copy 专用路(明文不回前端)。
#[tauri::command]
pub(crate) async fn copy_text(text: String) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("没有可复制的内容".into());
    }
    tokio::task::spawn_blocking(move || copy_to_clipboard(&text))
        .await
        .map_err(|e| format!("复制失败: {e}"))?
}

/// 渠道密钥的明文(用户点「显示」时一次性返回)。**这是产品里唯一把密钥回前端的路径**,
/// 所以只有用户显式点击才会走到这里;界面侧 8 秒自动收回,不留驻。密钥型渠道以外没有可取的。
/// 渠道的显示名:内置渠道查静态表,自定义渠道查配置(报告 B3)。
/// 错误信息与「已复制」回执都要用它 —— key_reveal 放行自定义渠道后,
/// 不能再用 `providers::find(&id).expect("上面校验过")`(对 custom-N 会 panic)。
pub(crate) fn channel_display_name(state: &AppState, id: &str) -> String {
    if let Some(def) = providers::find(id) {
        return def.name.to_string();
    }
    lock_rw(&state.config)
        .custom_channels
        .iter()
        .find(|x| x.id == id)
        .map(|x| x.name.clone())
        .unwrap_or_else(|| id.to_string())
}

#[tauri::command]
pub(crate) async fn key_reveal(app: tauri::AppHandle, id: String) -> Result<String, String> {
    let state = app.state::<AppState>();
    // 放行口径与 set_key / delete_key 相同(报告 B3):自定义渠道的密钥账户名也是
    // 渠道 id,以前只认静态表,卡片上的「复制 / 显示」对自定义渠道必报"未知渠道"。
    key_allowed(&state, &id)?;
    if let Some(def) = providers::find(&id) {
        if !matches!(def.auth, providers::AuthKind::Keyring) {
            return Err(format!("{} 没有可复制的密钥(用的是本机登录态或网页会话)", def.name));
        }
    }
    let name = channel_display_name(&state, &id);
    // 凭据管理器读是 RPC:与 key_hint/set_key 同一条纪律,不下沉就会占住一个 tokio worker
    let rid = id.clone();
    match tokio::task::spawn_blocking(move || secrets::channel_key(&rid))
        .await
        .map_err(|e| format!("读取凭据失败: {e}"))?
    {
        Ok(Some(k)) if !k.trim().is_empty() => Ok(k),
        Ok(_) => Err(format!("{name} 还没有保存密钥")),
        Err(e) => Err(e),
    }
}

/// 复制渠道密钥:明文**不回前端**,后端读出来直接进剪贴板(与「复制三件套」同一条路)。
#[tauri::command]
pub(crate) async fn key_copy(app: tauri::AppHandle, id: String) -> Result<String, String> {
    let state = app.state::<AppState>();
    let name = channel_display_name(&state, &id);
    let key = key_reveal(app, id).await?;
    tokio::task::spawn_blocking(move || copy_to_clipboard(&key).map(|_| format!("已复制 {name} 的密钥")))
        .await
        .map_err(|e| format!("复制失败: {e}"))?
}

/// 用系统 `clip.exe` 写剪贴板(经 stdin,密钥不进命令行)。
/// 输入按 UTF-16LE:clip 按控制台代码页解释字节流,中文标签走 UTF-8 会被切坏。
pub(crate) fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut cmd = std::process::Command::new("clip");
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // GUI 程序默认会给子进程配一个新的控制台窗口 —— clip.exe 也会闪一下黑框
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("调用 clip.exe 失败: {e}"))?;
    {
        let mut si = child.stdin.take().ok_or("拿不到 clip 的标准输入")?;
        let utf16: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        si.write_all(&utf16)
            .map_err(|e| format!("写入剪贴板失败: {e}"))?;
    }
    let st = child
        .wait()
        .map_err(|e| format!("clip.exe 未正常结束: {e}"))?;
    if !st.success() {
        return Err("clip.exe 返回失败".into());
    }
    Ok(())
}

/// 在资源管理器里打开一个目录(备份目录 / 客户端配置目录)。
/// 界面上的路径是 `~/…` 缩写(见 `inject::pretty`),这里先展开成真实路径再打开 ——
/// 否则资源管理器会去找一个叫 `~` 的目录。
#[tauri::command]
pub(crate) async fn open_path(app: tauri::AppHandle, path: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        use tauri_plugin_opener::OpenerExt;
        let raw = path.trim();
        let expanded = match raw.strip_prefix('~') {
            Some(rest) => {
                let home = std::env::var("USERPROFILE")
                    .or_else(|_| std::env::var("HOME"))
                    .map_err(|_| "拿不到用户目录,无法展开 ~".to_string())?;
                format!("{}{}", home, rest.replace('/', "\\"))
            }
            None => raw.to_string(),
        };
        let target = std::path::PathBuf::from(&expanded);
        if !target.exists() {
            return Err(format!("路径不存在: {raw}"));
        }
        // 词法上的 `..` 一律拒绝:starts_with 是按组件比、**不解析父目录**的,
        // 于是 `<site.root>\..\..\..\..\Windows\System32\calc.exe` 也能过白名单 ——
        // 这条命令就退化成"把任意 exe 交给资源管理器"的原语(P3-24 要挡的正是这个)。
        if target
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("路径里不允许出现 ..".to_string());
        }
        // 只允许打开挂件认识的客户端配置目录(含各自的 backup-inject/):
        // 两侧都归一成真实路径再比,短名 / 大小写 / 符号链接都绕不过去。
        let canon = std::fs::canonicalize(&target).map_err(|e| format!("路径无效: {e}"))?;
        let allowed = inject::sites().iter().any(|site| {
            std::fs::canonicalize(&site.root)
                .map(|r| canon.starts_with(&r))
                .unwrap_or(false)
        });
        if !allowed {
            return Err("只能打开客户端自己的配置目录与备份目录".to_string());
        }
        app.opener()
            .open_path(expanded, None::<&str>)
            .map_err(|e| format!("打开失败: {e}"))
    })
    .await
    .map_err(|e| format!("打开失败: {e}"))?
}

/// 只允许打开 GitHub 官方设备授权页。同步授权流程之外的 URL 一律拒绝。
#[tauri::command]
pub(crate) async fn open_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let allowed = [
        "https://github.com/login/device",
        "https://github.com/settings/developers",
    ];
    if !allowed.contains(&url.as_str()) {
        return Err("不允许打开这个地址".into());
    }
    tokio::task::spawn_blocking(move || {
        use tauri_plugin_opener::OpenerExt;
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|e| format!("打开失败: {e}"))
    })
    .await
    .map_err(|e| format!("打开失败: {e}"))?
}

/// 优惠列表每半小时刷新，复用连接池并把客户端初始化错误交给界面。
static PROMO_HTTP: std::sync::OnceLock<Result<reqwest::Client, String>> =
    std::sync::OnceLock::new();

fn promo_client() -> Result<&'static reqwest::Client, String> {
    PROMO_HTTP
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "优惠网络客户端初始化失败".into())
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// 官网公开优惠列表的某一页，由后端拼固定地址。页码只用于取更早的优惠。
#[tauri::command]
pub(crate) async fn fetch_promo_page(page: u32) -> Result<String, String> {
    const MAX_BYTES: usize = 2_000_000;
    if !(1..=100).contains(&page) {
        return Err("优惠页码超出范围".into());
    }
    let url = if page == 1 {
        "https://www.aipromonow.com/deals".to_string()
    } else {
        format!("https://www.aipromonow.com/deals?page={page}")
    };
    let client = promo_client()?;
    let mut response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| if e.is_timeout() { "连接优惠来源超时" } else { "无法连接优惠来源" })?;
    if !response.status().is_success() {
        return Err(format!("优惠来源返回 HTTP {}", response.status().as_u16()));
    }
    if response.content_length().unwrap_or(0) > MAX_BYTES as u64 {
        return Err("优惠页面内容过大".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "读取优惠页面失败")? {
        if chunk.len() > MAX_BYTES - bytes.len() {
            return Err("优惠页面内容过大".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| "优惠页面编码无效".into())
}

#[tauri::command]
pub(crate) async fn open_promo_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let parsed = reqwest::Url::parse(&url).map_err(|_| "地址无效")?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("www.aipromonow.com") || !parsed.path().starts_with("/promos/") {
        return Err("不允许打开这个地址".into());
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(url, None::<&str>).map_err(|e| format!("打开失败: {e}"))
}

/// 排查日志的落点与大小(设置页要显示"日志在哪儿、多大")。
/// 只报路径,不读内容 —— 打开/复制是另外两条命令,便于前端按需触发。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagLogInfo {
    pub path: String,
    pub dir: String,
    pub size: u64,
    /// 静默关闭了吗(`TOKENSCOPE_NODIAG=1`)。关着时界面上要如实说明,
    /// 否则用户以为"有日志",报问题时却什么都没有。
    pub enabled: bool,
    /// 「详细日志」现在开着吗(成功的请求也记)。界面据此回填开关。
    pub verbose: bool,
    /// 本次运行的会话 id(8 位十六进制)。用户报问题时说出它,就能定位那一次运行。
    pub session: String,
}

#[tauri::command]
pub(crate) async fn diag_log_info() -> Result<DiagLogInfo, String> {
    let dir = tokenscope_core::diaglog::log_dir().ok_or("拿不到日志目录")?;
    let path = tokenscope_core::diaglog::log_path().unwrap_or_else(|| dir.join(tokenscope_core::diaglog::LOG_FILE));
    Ok(DiagLogInfo {
        path: path.display().to_string(),
        dir: dir.display().to_string(),
        size: tokenscope_core::diaglog::size_bytes(),
        enabled: !tokenscope_core::diaglog::is_disabled(),
        verbose: tokenscope_core::diaglog::is_verbose(),
        session: tokenscope_core::diaglog::session_id(),
    })
}

/// 读日志尾巴(设置页预览 + 用户点「复制」时用)。只读文本,不含任何密钥
/// (写入侧已经做过 [`tokenscope_core::diaglog::redact`])。
#[tauri::command]
pub(crate) async fn diag_log_tail(lines: Option<usize>) -> Result<String, String> {
    let n = lines.unwrap_or(400).clamp(1, 5000);
    let got = tokenscope_core::diaglog::tail_lines(n);
    if got.is_empty() {
        return Ok("(日志还是空的 —— 等一轮取数之后再看,或者点一次「立即刷新」)".to_string());
    }
    Ok(got.join("\n"))
}

/// 在资源管理器里打开日志目录。**与 `open_path` 分开**:那条只允许打开客户端配置
/// 目录(白名单很窄),而这里只有"我们自己的日志目录"一个落点,不需要也不该复用。
#[tauri::command]
pub(crate) async fn open_log_dir(app: tauri::AppHandle) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        use tauri_plugin_opener::OpenerExt;
        let dir = tokenscope_core::diaglog::log_dir().ok_or("拿不到日志目录".to_string())?;
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| format!("建日志目录失败: {e}"))?;
        }
        app.opener()
            .open_path(dir.display().to_string(), None::<&str>)
            .map_err(|e| format!("打开失败: {e}"))
    })
    .await
    .map_err(|e| format!("打开失败: {e}"))?
}

/// 清空日志(用户报完问题、想重新观察一段时用)。
#[tauri::command]
pub(crate) async fn diag_log_clear() -> Result<String, String> {
    tokenscope_core::diaglog::clear()?;
    // 清完立刻记一行:让"新的一段"从明确的起点开始,不至于看起来像日志坏了
    tokenscope_core::diaglog::write("日志", "已清空(用户手动)");
    Ok("日志已清空".to_string())
}

/// 开关「详细日志」。
///
/// 默认关。打开后**成功的**请求也落盘 —— 排查"余额数字不对"这类问题必须看
/// 成功响应,那是唯一能看到"接口到底返了什么"的地方。绝大多数问题只需要失败
/// 现场(默认档就够),所以这个开关默认关,免得 99% 的人被日志量干扰。
///
/// 状态存进设置(重启后还在),但**读设置之前先用内存里的值** ——
/// 开关是"临时的调试手段",不该在配置还没加载出来时被打回默认。
#[tauri::command]
pub(crate) async fn diag_log_verbose(app: tauri::AppHandle, on: bool) -> Result<bool, String> {
    tokenscope_core::diaglog::set_verbose(on);
    tokenscope_core::diaglog::write(
        "日志",
        &format!("详细日志已{}", if on { "打开" } else { "关闭" }),
    );
    // 存进设置:下次启动还是这个状态
    let st = app.state::<AppState>();
    if let Err(e) = save_config_off_main(&st, move |c| c.diag_verbose = on).await {
        eprintln!("详细日志开关落库失败: {e}");
    }
    Ok(on)
}

/// 导出诊断包:环境 + 体检 + 事件日志 + 失败现场,合成**一份**可复制的文本。
///
/// 为什么合成一份:以前用户要贴两样东西(体检报告 + 日志),support 还要人工对。
/// 合成一份之后,用户只贴一样,support 也不用追问第二轮。
///
/// 纯文本而不是 zip:目标是"贴进聊天窗口",一段文本比一个压缩包好使。
#[tauri::command]
pub(crate) async fn diag_export(app: tauri::AppHandle) -> Result<String, String> {
    // 体检那一半要真打请求(最多十几秒)。`run_diagnostic` 本身是 async 的,
    // 里面的取数也是 await 的,直接等它就行 —— 真正占 CPU 的是 HTTP,不是这里。
    //
    // ⚠ 体检失败**不能带崩整个导出**:网络坏了正是用户最需要日志的时候,
    // 那时体检必然失败(要真打请求)—— 要是因此什么都拿不到,这个功能就废了。
    let report = match run_diagnostic(app.clone()).await {
        Ok(r) => r,
        Err(e) => format!("(体检没能跑完:{e})—— 下面是事件日志,先看这段"),
    };
    // 体检报告里含渠道名、地址、错误文案(可能带 URL),**也要过一遍脱敏** ——
    // 页脚写着"已脱敏",就得真的脱敏,不能只在日志那一段做。
    let report = tokenscope_core::diaglog::redact(&report);

    let session = tokenscope_core::diaglog::session_id();
    let version = env!("CARGO_PKG_VERSION");
    let now = tokenscope_core::fetch::local_datetime_string();
    let mut s = String::new();
    let bar = "=".repeat(60);
    s.push_str(&bar);
    s.push_str(&format!(
        "\n Grandettoken 诊断包 · {now} · v{version}\n 会话 ID: {session}\n"
    ));
    s.push_str(&bar);
    s.push_str("\n\n【1】环境\n");
    s.push_str(&diag_env_section());
    s.push_str("\n\n【2】体检(逐渠道真打一次)\n");
    s.push_str(&report);
    s.push_str("\n\n【3】事件日志(最近的记录)\n");
    let events = tokenscope_core::diaglog::tail_lines(400);
    if events.is_empty() {
        s.push_str("(日志还是空的 —— 等一轮取数之后再看)\n");
    } else {
        s.push_str(&events.join("\n"));
        s.push('\n');
    }
    s.push_str("\n【4】说明\n");
    s.push_str("本包已做脱敏:密钥、Cookie、Token 一律不落原文。\n");
    s.push_str("但路径、域名、渠道名仍是敏感信息 —— 请当作敏感信息处理。\n");
    Ok(s)
}

/// 诊断包里的环境段。
pub(crate) fn diag_env_section() -> String {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<取不到>".into());
    let dir = tokenscope_core::diaglog::log_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "<取不到>".into());
    let drives = tokenscope_core::workbuddy::drive_letters()
        .iter()
        .map(|d| format!("{d}:"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "  Grandettoken {} · exe={exe}\n  日志目录={dir}\n  本机盘符: {drives}",
        env!("CARGO_PKG_VERSION")
    )
}

