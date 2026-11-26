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

