use super::*;

// ───────────────────────── Trae(SQLite) ─────────────────────────

/// Trae 的自定义模型条目模板。字段取自 2026-10-12 真机取证的那条(用户在 Trae 里
/// 手配的 4SAPI 模型),逐字段对齐 —— 宁可多带几个 null,也不要少字段:
/// 少字段的条目在客户端里可能显示不全或不可选。
pub(super) fn trae_template(spec: &ModelSpec, custom_model_id: &str) -> Value {
    json!({
        "name": format!("custom_openai_compatible//{}", spec.id),
        "multimodal": spec.multimodal,
        "is_default": false,
        "custom_config": "",
        "display_name": spec.id,
        "prompt_max_tokens": null,
        "context_window_size": {"max": null, "default": null},
        "is_new": null,
        "is_beta": null,
        "core_memory_enable_type": null,
        "model_type": "chat_model",
        "is_preset": false,
        "client_connect": true,
        "provider": "custom_openai_compatible",
        "icon": {
            "dark": "https://lf-cdn.trae.com.cn/obj/trae-com-cn/model/default-custom-dark.svg",
            "light": "https://lf-cdn.trae.com.cn/obj/trae-com-cn/model/default-custom-light.svg"
        },
        "ak": spec.key,
        "base_url": spec.chat_completions_url(),
        "is_custom_base_url": true,
        "status": true,
        "custom_model_id": custom_model_id,
        "fee_model_level": null,
        "config_source": 3,
        "selectable": true,
        "commercial_info": null,
        "tob_commercial_info": null,
        "max_turns": {"default": null, "max": null},
        "tags": null,
        "sk": null,
        "auth_type": 0,
        "region": null,
        "thinking_enable": 0,
        "temperature": null,
        "top_p": null,
        "top_k": null,
        "saas_usage": {"max": null, "default": null},
        "features": {
            "provider": {"enable": true, "data": {"provider_name": "custom_openai_compatible"}},
            "context_windows": {
                "enable": true,
                "data": {
                    "dev_context": null, "max_context": null, "max_context_list": null,
                    "dev_turns": null, "max_turns": null
                }
            }
        }
    })
}

/// 新条目的 `custom_model_id`。**这是应用自己分配的数字,不是模型名的哈希**
/// (2026-10-12 试过 crc32/fnv1a/djb2/java-hash/adler32,都不等于现成样本 2607458050)。
/// 所以按「比现有最大值大、且不与现有冲突」生成 —— 唯一即可。
pub(super) fn next_custom_model_id(existing: &[u64]) -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut id = (now_ms % 10_000_000_000).max(existing.iter().copied().max().unwrap_or(0) + 1);
    while existing.contains(&id) {
        id += 1;
    }
    id.to_string()
}

/// Trae 的选中项 modelId 口径(2026-10-12 用真样本破译,单测里对着两个真样本回归):
///
/// ```text
/// 内置:  solo_agent_lite_1__DeepSeek-V4-Flash-Official_null
/// 自定义: solo_agent_3_custom_openai_compatible_custom_openai_compatible//claude-sonnet-5_2607458050
/// ```
///
/// 即 `<agentLabel>_<config_source>_<provider>_<name>_<custom_model_id 或 null>`,
/// 中段就是条目里的 `config_source`(内置=1、自定义=3),空字段自然形成连续下划线。
pub fn trae_model_id(
    agent: &str,
    name: &str,
    provider: &str,
    config_source: i64,
    custom_model_id: Option<&str>,
) -> String {
    let id = custom_model_id.unwrap_or("null");
    format!("{agent}_{config_source}_{provider}_{name}_{id}")
}

/// 从选中项的 modelId 里取出模型名(界面显示用)。
/// `...custom_openai_compatible//<模型>_<custom_model_id>` → `<模型>`
pub fn trae_selected_model(model_id: &str) -> Option<String> {
    let rest = model_id.split("custom_openai_compatible//").nth(1)?;
    match rest.rsplit_once('_') {
        Some((name, tail)) if tail.chars().all(|c| c.is_ascii_digit()) => Some(name.to_string()),
        _ => Some(rest.to_string()),
    }
}

pub(super) fn trae_write(
    db: &Path,
    spec: &ModelSpec,
    opts: &Opts,
) -> Result<SiteOutcome, String> {
    let hosts3 = spec.hosts();
    let mut conn = rusqlite::Connection::open(db)
        .map_err(|e| format!("打开 {} 失败(Trae 可能正在运行?): {e}", pretty(db)))?;
    // 键名前缀是「区域/租户号」,会随版本变,所以按后缀找,不写死
    let keys: Vec<String> = {
        let mut stmt = conn
            .prepare(
                // SQLite 的 LIKE 里 `_` 是单字符通配符,命中范围比看上去宽 ——
                // 取回后再按字面量复核一遍(报告 P2-9)
                "SELECT key FROM ItemTable WHERE key LIKE '%AI.agent.model.model_list_map'",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let found: Vec<String> = rows.filter_map(|r| r.ok()).collect();
        // SQLite 的 LIKE 把 `_` 当单字符通配符,命中范围比意图宽;取回后按字面量再筛一次
        // (报告 P2-9:模型表被认错时,一次写入能改到不该改的行)
        found
            .into_iter()
            .filter(|k| k.ends_with("AI.agent.model.model_list_map"))
            .collect()
    };
    if keys.is_empty() {
        return Err("没找到 Trae 的模型表(版本可能变了),本次不写".into());
    }

    let mut outcome = SiteOutcome::default();

    // 先摸清现有自定义条目:custom_model_id 号段,以及各 key 的 JSON
    let mut existing_ids: Vec<u64> = Vec::new();
    let mut maps: Vec<(String, Value)> = Vec::new();
    for k in &keys {
        let raw: String = conn
            .query_row("SELECT value FROM ItemTable WHERE key = ?1", [k], |r| {
                r.get(0)
            })
            .map_err(|e| format!("读 {k} 失败: {e}"))?;
        let v: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("解析 {k} 失败(结构可能已变,本次不写): {e}"))?;
        collect_custom_ids(&v, &mut existing_ids);
        maps.push((k.clone(), v));
    }

    // 全部 UPDATE 走一个事务:中间任何一条失败就整批回滚。
    // state.vscdb 装的是 Trae 整个 globalStorage,半途而废比不写更坏
    // (设计稿 4.4「写坏了的后果」明写:写入用事务、失败立即回滚、写前强制备份)。
    let tx = conn
        .transaction()
        .map_err(|e| format!("{} 开启事务失败(Trae 可能正在运行?): {e}", pretty(db)))?;

    let mut assigned: Option<String> = None;
    let mut touched = 0usize;
    for (key, map) in maps.iter_mut() {
        // 序列化一份"改之前"的文本,末尾对比:内容一致就跳过 UPDATE。
        // 以前是无条件写回,于是每点一次「写入」整个 state.vscdb 都被改写一遍
        // (时间戳变、产生新 -wal),而这一家偏偏没有备份(P0-2)(报告 P2-9)。
        let before_text = serde_json::to_string(map).map_err(|e| e.to_string())?;
        let Some(groups) = map.as_object_mut() else { continue };
        for (agent, arr) in groups.iter_mut() {
            let _ = agent;
            let Some(list) = arr.as_array_mut() else { continue };
            let pos = list.iter().position(|e| {
                trae_is_ours(e, &spec.id, &hosts3, &spec.base_url)
            });
            match pos {
                Some(i) => {
                    // 更新:保留原有 custom_model_id 与所有我们不认识的字段
                    let mut next = list[i].clone();
                    next["ak"] = json!(spec.key);
                    next["base_url"] = json!(spec.chat_completions_url());
                    next["multimodal"] = json!(spec.multimodal);
                    next["status"] = json!(true);
                    next["selectable"] = json!(true);
                    list[i] = next;
                    outcome.updated += 1;
                }
                None => {
                    let cid = assigned
                        .get_or_insert_with(|| next_custom_model_id(&existing_ids))
                        .clone();
                    // 同组已有自定义条目时**照它克隆**(版本差异靠它吸收:
                    // 客户端自己写的条目里带的字段,我们一个都不少);没有才用通用模板
                    let next = match list.iter().find(|e| {
                        e.get("provider").and_then(|p| p.as_str()) == Some("custom_openai_compatible")
                    }) {
                        Some(tpl) => {
                            let mut merged = tpl.clone();
                            if let Some(obj) = merged.as_object_mut() {
                                obj.insert(
                                    "name".into(),
                                    json!(format!("custom_openai_compatible//{}", spec.id)),
                                );
                                obj.insert("display_name".into(), json!(spec.id));
                                obj.insert("ak".into(), json!(spec.key));
                                obj.insert("base_url".into(), json!(spec.chat_completions_url()));
                                obj.insert("custom_model_id".into(), json!(cid));
                                obj.insert("multimodal".into(), json!(spec.multimodal));
                            }
                            merged
                        }
                        None => trae_template(spec, &cid),
                    };
                    list.push(next);
                    outcome.added += 1;
                }
            }
        }
        let text = serde_json::to_string(map).map_err(|e| e.to_string())?;
        if text != before_text {
            tx.execute(
                "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                rusqlite::params![text, key.as_str()],
            )
            .map_err(|e| format!("写 {key} 失败: {e}"))?;
            touched += 1;
        }
    }

    // 选中项:实验性开关。口径已破译,但客户端会不会立刻认这个选中项只有真机能验,
    // 所以默认关 —— 打开前请在 Trae 里手动确认过一次。
    let our_cid = assigned
        .clone()
        .or_else(|| find_our_custom_id(&maps, &spec.id));
    if opts.trae_auto_select {
        if let Some(cid) = our_cid.as_deref() {
            let sel_key: Option<String> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT key FROM ItemTable \
                         WHERE key LIKE '%recent_user_selection_by_agent_label'",
                    )
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(|e| e.to_string())?;
                let first = rows.filter_map(|r| r.ok()).next();
                first
            };
            if let Some(k) = sel_key {
                if let Ok(raw) = tx.query_row(
                    "SELECT value FROM ItemTable WHERE key = ?1",
                    [&k],
                    |r| r.get::<_, String>(0),
                ) {
                    if let Ok(mut sel) = serde_json::from_str::<Value>(&raw) {
                        if let Some(obj) = sel.as_object_mut() {
                            for (agent, entry) in obj.iter_mut() {
                                let name = format!("custom_openai_compatible//{}", spec.id);
                                let mid = trae_model_id(
                                    agent,
                                    &name,
                                    "custom_openai_compatible",
                                    3,
                                    Some(cid),
                                );
                                if let Some(o) = entry.as_object_mut() {
                                    o.insert("modelId".into(), json!(mid));
                                }
                            }
                            let text = serde_json::to_string(&sel).map_err(|e| e.to_string())?;
                            tx.execute(
                                "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                                rusqlite::params![text, k],
                            )
                            .map_err(|e| format!("写选中项失败: {e}"))?;
                            outcome.selected = true;
                        }
                    }
                }
            }
        }
    }
    if touched == 0 && outcome.added + outcome.updated > 0 {
        outcome.same = true;
        outcome.notes.push("内容已经在了,这次没动库。".into());
    }
    if !outcome.selected {
        outcome
            .notes
            .push("选中项没写:重启 Trae 后在模型列表里选一次即可".into());
    }
    // 一次性提交。中间任何一条 UPDATE 失败,tx 被丢弃时就地整批回滚,
    // 不会留下"模型表改了一半"的库(以前每条语句各自 autocommit)。
    tx.commit()
        .map_err(|e| format!("提交事务失败(本次改动已回滚): {e}"))?;
    Ok(outcome)
}

/// 把某份 model_list_map 里所有自定义条目的 custom_model_id 收进 existing(去重)。
pub(super) fn collect_custom_ids(v: &Value, existing: &mut Vec<u64>) {
    if let Some(groups) = v.as_object() {
        for arr in groups.values() {
            if let Some(list) = arr.as_array() {
                for e in list {
                    if e.get("provider").and_then(|p| p.as_str()) == Some("custom_openai_compatible") {
                        if let Some(id) = e.get("custom_model_id").and_then(|v| v.as_str()) {
                            if let Ok(n) = id.parse::<u64>() {
                                if !existing.contains(&n) {
                                    existing.push(n);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub(super) fn find_our_custom_id(maps: &[(String, Value)], model: &str) -> Option<String> {
    for (_, v) in maps {
        if let Some(groups) = v.as_object() {
            for arr in groups.values() {
                if let Some(list) = arr.as_array() {
                    for e in list {
                        if e.get("display_name").and_then(|d| d.as_str()) == Some(model)
                            && e.get("provider").and_then(|p| p.as_str())
                                == Some("custom_openai_compatible")
                        {
                            if let Some(id) = e.get("custom_model_id").and_then(|v| v.as_str()) {
                                return Some(id.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Trae:每个 agent 组各有一条,数一数;顺带看选中项指不指向它。
pub(super) fn trae_removal(db: &Path, spec: &ModelSpec) -> (bool, Option<Removal>) {
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return (false, None);
    };
    let mut keys: Vec<String> = Vec::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT key FROM ItemTable WHERE key LIKE '%AI.agent.model.model_list_map'")
    {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            keys = rows.flatten().collect();
        }
    }
    let hosts = spec.hosts();
    let mut count = 0usize;
    let mut groups = 0usize;
    for k in &keys {
        let Ok(raw) = conn.query_row("SELECT value FROM ItemTable WHERE key = ?1", [k], |r| {
            r.get::<_, String>(0)
        }) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(obj) = v.as_object() {
            for arr in obj.values() {
                if let Some(list) = arr.as_array() {
                    let n = list
                        .iter()
                        .filter(|e| trae_is_ours(e, &spec.id, &hosts, &spec.base_url))
                        .count();
                    if n > 0 {
                        count += n;
                        groups += 1;
                    }
                }
            }
        }
    }
    if count == 0 {
        return (false, None);
    }
    let mut clears = false;
    if let Some((mk, raw)) = selection_row(&conn) {
        let _ = mk;
        if let Ok(sel) = serde_json::from_str::<Value>(&raw) {
            if let Some(obj) = sel.as_object() {
                clears = obj.values().any(|e| {
                    e.get("modelId")
                        .and_then(|m| m.as_str())
                        .map(|m| m.contains(&format!("custom_openai_compatible//{}_", spec.id)))
                        .unwrap_or(false)
                });
            }
        }
    }
    (
        true,
        Some(Removal {
            what: format!("{count} 条自定义模型条目(分属 {groups} 个 agent 组)"),
            clears_selection: clears,
            count,
        }),
    )
}

/// 一条 Trae 条目是不是我们要找的那条自定义模型。
pub(super) fn trae_is_ours(e: &Value, model: &str, hosts: &[String], base_url: &str) -> bool {
    if e.get("provider").and_then(|p| p.as_str()) != Some("custom_openai_compatible") {
        return false;
    }
    if e.get("display_name").and_then(|d| d.as_str()) != Some(model) {
        return false;
    }
    if hosts.is_empty() && base_url.is_empty() {
        return true;
    }
    let url = e.get("base_url").and_then(|u| u.as_str()).unwrap_or("");
    url_is_ours(url, hosts, base_url)
}

/// Trae 的选中项那一行(键名 + 值)。
pub(super) fn selection_row(conn: &rusqlite::Connection) -> Option<(String, String)> {
    let mut stmt = conn
        .prepare(
            "SELECT key, value FROM ItemTable \
             WHERE key LIKE '%recent_user_selection_by_agent_label'",
        )
        .ok()?;
    let mut rows = stmt.query([]).ok()?;
    let row = rows.next().ok()??;
    Some((row.get(0).ok()?, row.get(1).ok()?))
}

pub(super) fn remove_trae_site(db: &Path, spec: &ModelSpec) -> Result<SiteOutcome, String> {
    let hosts2 = spec.hosts();
    let mut conn = rusqlite::Connection::open(db)
        .map_err(|e| format!("打开 {} 失败(Trae 可能正在运行?): {e}", pretty(db)))?;
    let keys: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT key FROM ItemTable WHERE key LIKE '%AI.agent.model.model_list_map'")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let found: Vec<String> = rows.filter_map(|r| r.ok()).collect();
        found
            .into_iter()
            .filter(|k| k.ends_with("AI.agent.model.model_list_map"))
            .collect()
    };
    if keys.is_empty() {
        return Err("没找到 Trae 的模型表(版本可能变了),本次不写".into());
    }
    let tx = conn
        .transaction()
        .map_err(|e| format!("{} 开启事务失败(Trae 可能正在运行?): {e}", pretty(db)))?;

    let mut outcome = SiteOutcome::default();
    for k in &keys {
        let raw: String = tx
            .query_row("SELECT value FROM ItemTable WHERE key = ?1", [k], |r| {
                r.get(0)
            })
            .map_err(|e| format!("读 {k} 失败: {e}"))?;
        let mut v: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("解析 {k} 失败(结构可能已变,本次不写): {e}"))?;
        let mut changed = false;
        if let Some(obj) = v.as_object_mut() {
            for arr in obj.values_mut() {
                if let Some(list) = arr.as_array_mut() {
                    let before = list.len();
                    list.retain(|e| !trae_is_ours(e, &spec.id, &hosts2, &spec.base_url));
                    let removed = before - list.len();
                    if removed > 0 {
                        outcome.removed += removed;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            let text = serde_json::to_string(&v).map_err(|e| e.to_string())?;
            tx.execute(
                "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                rusqlite::params![text, k.as_str()],
            )
            .map_err(|e| format!("写 {k} 失败: {e}"))?;
        }
    }

    if let Some((k, raw)) = selection_row(&tx) {
        if let Ok(mut sel) = serde_json::from_str::<Value>(&raw) {
            let mut changed = false;
            if let Some(obj) = sel.as_object_mut() {
                let hit: Vec<String> = obj
                    .iter()
                    .filter(|(_, e)| {
                        e.get("modelId")
                            .and_then(|m| m.as_str())
                            .map(|m| m.contains(&format!("custom_openai_compatible//{}_", spec.id)))
                            .unwrap_or(false)
                    })
                    .map(|(a, _)| a.clone())
                    .collect();
                for a in hit {
                    obj.remove(&a);
                    changed = true;
                    outcome.clears_selection = true;
                }
            }
            if changed {
                let text = serde_json::to_string(&sel).map_err(|e| e.to_string())?;
                tx.execute(
                    "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                    rusqlite::params![text, k.as_str()],
                )
                .map_err(|e| format!("写选中项失败: {e}"))?;
            }
        }
    }

    tx.commit()
        .map_err(|e| format!("提交事务失败(本次改动已回滚): {e}"))?;
    Ok(outcome)
}

pub(super) fn trae_read_back(db: &Path, spec: &ModelSpec) -> Option<(usize, usize)> {
    let hosts = spec.hosts();
    let conn = rusqlite::Connection::open(db).ok()?;
    let keys: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT key FROM ItemTable WHERE key LIKE '%AI.agent.model.model_list_map'")
            .ok()?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).ok()?;
        let found: Vec<String> = rows.filter_map(|r| r.ok()).collect();
        found
            .into_iter()
            .filter(|k| k.ends_with("AI.agent.model.model_list_map"))
            .collect()
    };
    let (mut arrays, mut ours) = (0usize, 0usize);
    for k in &keys {
        let Ok(raw) = conn.query_row("SELECT value FROM ItemTable WHERE key = ?1", [k], |r| {
            r.get::<_, String>(0)
        }) else {
            return None;
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(groups) = v.as_object() else { continue };
        for arr in groups.values() {
            let Some(list) = arr.as_array() else { continue };
            arrays += 1;
            if list.iter().any(|e| trae_is_ours(e, &spec.id, &hosts, &spec.base_url)) {
                ours += 1;
            }
        }
    }
    Some((arrays, ours))
}

pub(super) fn trae_verify_written(db: &Path, spec: &ModelSpec) -> bool {
    matches!(trae_read_back(db, spec), Some((a, o)) if a > 0 && o == a)
}

pub(super) fn trae_verify_removed(db: &Path, spec: &ModelSpec) -> bool {
    matches!(trae_read_back(db, spec), Some((_, 0)))
}

pub(super) fn trae_has_ours(db: &Path, spec: &ModelSpec) -> bool {
    matches!(trae_read_back(db, spec), Some((_, o)) if o > 0)
}
