use super::*;

// ─────────────── 本地密钥箱 ───────────────
//
// 设计见 `docs/design/design-vault-and-hidden-restore.html` 第三节(裁决 1A–7A):
//   · **密文在 Windows 凭据管理器**(服务名 `TokenScope:vault`),SQLite 只存索引(平台/标签/尾号/备注/时间)
//     —— 列表渲染**不需要解密**,只有「查看/复制」才读出那一条;
//   · 不导出、不自动使用、不同步上云、无主密码(边界 = 当前 Windows 账户)。

/// 界面提交的新建/编辑请求。`key` 为空 = 只改平台/标签/备注,不动密钥。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VaultInput {
    pub(crate) id: Option<String>,
    pub(crate) platform: String,
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) note: String,
    #[serde(default)]
    pub(crate) api_endpoint: String,
    #[serde(default)]
    pub(crate) website: String,
    #[serde(default)]
    pub(crate) key: String,
}

pub(crate) fn vault_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() { return Ok(String::new()); }
    let parsed = reqwest::Url::parse(value).map_err(|_| "请输入完整的网址（包含 http:// 或 https://）")?;
    let has_secret_param = parsed.query_pairs().any(|(name, _)| {
        let n = name.to_ascii_lowercase().replace('-', "_");
        n == "key" || n.contains("api_key") || n.contains("token") || n.contains("secret") || n.contains("password") || n.contains("authorization")
    });
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() || !parsed.username().is_empty() || parsed.password().is_some() || has_secret_param {
        return Err("网址需为不含密钥或凭据的 HTTP(S) 地址".into());
    }
    Ok(value.to_string())
}

// 渠道默认密钥导入与网关自动同步已拆至 vault_sync.rs。

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VaultListItem {
    #[serde(flatten)]
    pub(crate) item: tokenscope_core::store::VaultItem,
    pub(crate) default_channel: Option<String>,
    pub(crate) default_available: bool,
}
#[tauri::command]
pub(crate) async fn vault_list(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Vec<VaultListItem>, String> {
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    let import_app = app.clone();
    let out = tokio::task::spawn_blocking(move || {
        vault_sync::import_for_gateway(&import_app, &store, &cfg);
        let items = lock_rw(&store).vault_list()?;
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let default_channel = vault_default_channel(&lock_rw(&store), &item.id)?;
            let default_available = default_channel.as_ref().map(|id| secrets::channel_key(id).ok().flatten().map(|s| !s.trim().is_empty()).unwrap_or(false)).unwrap_or(true);
            out.push(VaultListItem { item, default_channel, default_available });
        }
        Ok::<_, String>(out)
    })
    .await
    .map_err(|e| format!("读取密钥箱失败: {e}"))??;
    Ok(out)
}

/// 新建或编辑一条；索引失败时恢复凭据的写入前状态。
#[tauri::command]
pub(crate) async fn vault_save(
    app: tauri::AppHandle,
    item: VaultInput,
) -> Result<tokenscope_core::store::VaultItem, String> {
    let store = app.state::<AppState>().store.clone();
    let cfg = lock_rw(&app.state::<AppState>().config).clone();
    let gateway = app.try_state::<gateway::Gateway>().map(|g| g.inner().clone());
    if let Some(g) = gateway.as_ref() { lock_rw(&g.inner).config_revision += 1; }
    let import_app = app.clone();
    let saved_result = tokio::task::spawn_blocking(move || -> Result<tokenscope_core::store::VaultItem, String> {
        use tokenscope_core::vault;
        refuse_while_updating("保存密钥")?;
        vault_sync::import_for_gateway(&import_app, &store, &cfg);
        let _guard = lock_rw(vault_defaults_lock());
        if let Some(id) = item.id.as_deref() { ensure_vault_mutable(&lock_rw(&store), id)?; }
        let platform = vault::valid_platform(&item.platform)?;
        let label = vault::valid_label(&item.label)?;
        let key = item.key.trim().to_string();
        let now = now_ts();

        // 库锁只圈住"读"这一段。下面要调 Windows 凭据管理器(外部 RPC,域账户/组策略
        // 出问题时可以卡到秒级),以前是持着 store 锁跨 RPC:那段时间里 channel_changes
        // 等主线程命令在同一把锁上排队,整个面板冻住(报告 P1-7)。
        let existing = { let s = lock_rw(&store); match item.id.as_deref() { Some(id) => s.vault_get(id)?, None => None } };
        // 校验必须发生在写入凭据之前，避免地址无效时密钥已被改掉。
        let api_endpoint = vault_url(&item.api_endpoint)?;
        let website = vault_url(&item.website)?;
        if item.id.is_some() && existing.is_none() {
            return Err("这条已经不在密钥箱里了(可能刚被删过)".into());
        }

        let id = existing
            .as_ref()
            .map(|e| e.id.clone())
            .unwrap_or_else(vault::new_id);
        let hint = if key.is_empty() {
            existing
                .as_ref()
                .map(|e| e.hint.clone())
                .unwrap_or_else(|| "••••".into())
        } else {
            secrets::mask_of(&key)
        };
        if existing.is_none() && key.is_empty() {
            return Err("新建时要填密钥".into());
        }
        let out = tokenscope_core::store::VaultItem {
            id: id.clone(),
            platform,
            label,
            hint,
            note: item.note.trim().to_string(),
            api_endpoint,
            website,
            created_at: existing.as_ref().map(|e| e.created_at).unwrap_or(now),
            updated_at: existing.as_ref().map(|e| now.max(e.updated_at.saturating_add(1))).unwrap_or(now),
        };
        if key.is_empty() {
            lock_rw(&store).vault_upsert(&out)?;
        } else {
            tokenscope_core::vault::transaction::commit(&tokenscope_core::vault::transaction::NativeVault, &id, Some(&key), || lock_rw(&store).vault_upsert(&out))?;
        }
        Ok(out)
    })
    .await;
    if let Some(g) = gateway.as_ref() { lock_rw(&g.inner).config_revision += 1; }
    let saved = saved_result.map_err(|_| "保存任务失败，密钥状态请核对后重试".to_string())??;
    gateway::spawn_vault_sync(&app, saved.id.clone());
    Ok(saved)
}

/// 删密钥箱条目的结果。
///
/// 为什么要有结构而不是 `Result<(),_>`(N07):删前拦截现在在**后端**做,
/// 而且扫的是真实指针 —— 有渠道在用时不直接报错,而是把清单交回去让界面确认。
/// 用错误字符串传清单是不可靠的(会被转义、会被吞),这里给结构化字段。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VaultDeleteResult {
    /// true = 真的删了;false = 有渠道在用,等用户确认(此时 referrers 非空)
    pub(crate) deleted: bool,
    /// 正引用这条的渠道 id 清单(**按真实指针扫出来的**,不是查索引)
    pub(crate) referrers: Vec<String>,
}

/// 按**真实指针**反查:有哪些渠道正引用这个箱子条目。
///
/// 这里问的是凭据管理器 —— **书架说了算**。索引读坏了、过期了、被删了,都不影响这个判断。
///
/// 渠道清单来自配置(内置渠道 + 自定义渠道)。已删除渠道留下的孤儿指针**扫不到**
/// (keyring 不提供枚举),这一点在设计稿 §6 里写明了。
pub(crate) async fn ref_channels_by_pointer(
    app: &tauri::AppHandle,
    vault_id: &str,
) -> Result<Vec<String>, String> {
    let ids = {
        let st = app.state::<AppState>();
        let c = lock_rw(&st.config);
        let mut v: Vec<String> = providers::PROVIDERS.iter().map(|d| d.id.to_string()).collect();
        v.extend(c.custom_channels.iter().map(|x| x.id.clone()));
        v
    };
    let vid = vault_id.to_string();
    tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        for id in ids {
            if let Ok(Some(v)) = secrets::link_get(&id) {
                if v == vid {
                    out.push(id);
                }
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| format!("反查引用失败: {e}"))?
}

/// 按真实指针**整表重抄**索引。设计稿 `product-vault-ref-reconcile-2026-10-21` 的动作 1。
///
/// 一句话:**书架说了算,索引只是副本**。所以重建的方向永远是"读指针 → 写索引",
/// 绝不会反过来(拿索引去改指针 —— 那就成了用副本校正正本)。
///
/// 什么时候跑:① 启动后台跑一次;② 删箱子条目之后立刻跑一次(界面要马上对)。
/// 索引写失败不该让任何用户操作失败 —— 这里只记一行日志,下次启动还会重来。
pub(crate) async fn rebuild_vault_refs(app: &tauri::AppHandle) -> Result<(), String> {
    let ids = {
        let st = app.state::<AppState>();
        let c = lock_rw(&st.config);
        let mut v: Vec<String> = providers::PROVIDERS.iter().map(|d| d.id.to_string()).collect();
        v.extend(c.custom_channels.iter().map(|x| x.id.clone()));
        v
    };
    let store = app.state::<AppState>().store.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for id in ids {
            if let Ok(Some(vid)) = secrets::link_get(&id) {
                pairs.push((id, vid));
            }
        }
        let n = pairs.len();
        lock_rw(&store).vault_ref_rebuild(&pairs)?;
        tokenscope_core::diaglog::write_keyed(
            "vault",
            "ref-rebuild",
            &format!("引用索引已按真实指针重抄 · {n} 条"),
        );
        Ok(())
    })
    .await
    .map_err(|e| format!("重抄引用索引失败: {e}"))?
}

/// 启动后跑一次索引对账(不挡界面)。
pub(crate) fn spawn_vault_ref_rebuild(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        // 让界面先画出来:对账要读 N 次凭据管理器,没必要和首屏抢
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if let Err(e) = rebuild_vault_refs(&app).await {
            tokenscope_core::diaglog::write_keyed("vault", "ref-rebuild-fail", &e);
        }
    });
}

#[tauri::command]
pub(crate) async fn vault_delete(
    app: tauri::AppHandle,
    id: String,
    force: Option<bool>,
) -> Result<VaultDeleteResult, String> {
    let import_store = app.state::<AppState>().store.clone();
    let import_cfg = lock_rw(&app.state::<AppState>().config).clone();
    let import_app = app.clone();
    tokio::task::spawn_blocking(move || vault_sync::import_for_gateway(&import_app, &import_store, &import_cfg))
        .await.map_err(|_| "检查默认密钥任务失败".to_string())?;
    ensure_vault_mutable(&lock_rw(&app.state::<AppState>().store), &id)?;
    // N07:删前拦截搬到后端,并且**扫真实指针**而不是查索引。
    // 以前拦截在浏览器里靠索引:索引读失败就静默放行、索引不准就拦了个寂寞。
    let referrers = ref_channels_by_pointer(&app, &id).await?;
    if !referrers.is_empty() && !force.unwrap_or(false) {
        return Ok(VaultDeleteResult { deleted: false, referrers });
    }
    let state = app.state::<AppState>();
    let store = state.store.clone();
    let gateway = app.try_state::<gateway::Gateway>().map(|g| g.inner().clone());
    tokio::task::spawn_blocking(move || {
        let _guard = lock_rw(vault_defaults_lock());
        ensure_vault_mutable(&lock_rw(&store), &id)?;
        // 先删索引(用户视角"这条没了"),再删凭据;凭据删失败也不该把索引变回来
        {
            let s = lock_rw(&store);
            s.vault_delete(&id)?;
        }
        let gateway_cleanup = gateway.map(|g| g.remove_vault(&id)).transpose();
        // 索引已经删了,凭据删不掉的话那把密钥的密文会**永远**留在凭据管理器里:
        // 界面再也列不出、也删不掉(vault_reveal 先校验索引)。以前只 eprintln 就当成功
        // (报告 P2-14)。重试一次,仍失败就把话说明白,让用户知道要去手动清。
        if let Err(e) = secrets::vault_delete(&id).or_else(|e| {
            std::thread::sleep(std::time::Duration::from_millis(200));
            secrets::vault_delete(&id).map_err(|_| e)
        }) {
            return Err(format!(
                "条目已从列表移除,但密文没能从 Windows 凭据管理器删掉:{e}。
                 请到「控制面板 → 凭据管理器 → 普通凭据」里删除以 TokenScope:vault 开头、
                 结尾为 {id} 的那一条。"
            ));
        }
        gateway_cleanup.map_err(|e| format!("条目和密文已删除，但网关映射清理失败，请刷新网关：{e}"))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("删除失败: {e}"))??;
    // N16:删完立刻重抄索引,并作废取数缓存 —— 渠道卡要**马上**显示"引用的条目已删除",
    // 而不是等下一轮轮询(最长 60 秒,用户会以为没删掉)。
    let _ = rebuild_vault_refs(&app).await;
    state.fetcher.invalidate();
    Ok(VaultDeleteResult {
        deleted: true,
        referrers,
    })
}

/// 取出明文(界面点「显示」时一次性返回,8 秒后界面自己收回)。
#[tauri::command]
pub(crate) async fn vault_reveal(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        let (exists, default_channel) = {
            let s = lock_rw(&store);
            (s.vault_get(&id)?.is_some(), vault_default_channel(&s, &id)?)
        };
        if !exists {
            return Err("这条已经不在密钥箱里了".into());
        }
        if let Some(channel) = default_channel {
            return secrets::channel_key(&channel)?.filter(|k| !k.trim().is_empty()).ok_or_else(|| "该渠道目前没有可用密钥".to_string());
        }
        match secrets::vault_get(&id)? {
            Some(k) => Ok(k),
            None => Err("索引还在但凭据丢了(可能被系统清理过),请重新录入".into()),
        }
    })
    .await
    .map_err(|e| format!("取密钥失败: {e}"))?
}

/// 复制:明文**不回前端**,后端读出来直接进剪贴板。
#[tauri::command]
pub(crate) async fn vault_copy(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let key = vault_reveal(state, id.clone()).await?;
    tokio::task::spawn_blocking(move || {
        copy_to_clipboard(&key).map(|_| "已复制到剪贴板".to_string())
    })
    .await
    .map_err(|e| format!("复制失败: {e}"))?
}

