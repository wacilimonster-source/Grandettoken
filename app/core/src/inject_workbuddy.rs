use super::*;

// ───────────────────────── WorkBuddy / WorkBuddy AI ─────────────────────────

/// 我们要写的那条条目长这样(与用户手配的那条逐字段对齐)。
pub(super) fn wb_entry(spec: &ModelSpec, existing: Option<&Map<String, Value>>) -> Value {
    // 更新时不改用户自己起的名字(他可能已经把 4S- 改成别的)
    let name = existing
        .and_then(|m| m.get("name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
        .unwrap_or_else(|| spec.display_name.clone());

    let mut m = Map::new();
    m.insert("id".into(), json!(spec.id));
    m.insert("name".into(), json!(name));
    m.insert("vendor".into(), json!("Custom"));
    m.insert("url".into(), json!(spec.base_url));
    m.insert("apiKey".into(), json!(spec.key));
    m.insert("supportsToolCall".into(), json!(true));
    m.insert("supportsImages".into(), json!(spec.multimodal));
    m.insert("supportsReasoning".into(), json!(true));
    m.insert("useCustomProtocol".into(), json!(false));
    m.insert(
        "reasoning".into(),
        json!({"supportedEfforts": ["low", "medium", "high", "xhigh", "max"]}),
    );
    if let Some(ctx) = spec.context_tokens {
        m.insert("maxInputTokens".into(), json!(ctx));
    }
    // 用户自己可能加过别的字段(别名之类):更新时原样保留
    if let Some(old) = existing {
        for (k, v) in old {
            if !m.contains_key(k) {
                m.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(m)
}

/// 一条 url 里的**主机名**:剥掉 scheme / userinfo / 端口 / 路径 / 查询串,小写。
/// 与 `models::source_of_url`(读路径,早就是精确主机名比对)同一套解析。
pub(super) fn url_host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .rsplit('@')
        .next()
        .unwrap_or(rest);
    let host = if let Some(h) = authority.strip_prefix('[') {
        // IPv6 字面量 [::1]:8080:方括号里的冒号不是端口分隔符
        h.split(']').next().unwrap_or(h)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// 这条地址是不是**这个来源**的条目地址。判据两条:
/// ① 主机名与来源域名**精确**相同(不能 `contains`:那样 `https://mirror-4sapi.com/v1`
///    与 `https://not4sapi.com.evil/v1` 都会算成"我们写的",于是「取消写入」会删掉
///    用户自己的条目、写入时也不再报"同名属于别家"的冲突);
/// ② 或者整串等于我们会写下去的那个 base_url(用户把 `model_base_url` 配到自定义域名时,
///    只有这一条能把他写进去的条目认回来;Trae 写的是 `…/chat/completions`,一并认)。
/// `hosts` 为空 = 这个来源没有域名表,那就只剩 ② 可判。
pub(super) fn url_is_ours(url: &str, hosts: &[String], base_url: &str) -> bool {
    if !base_url.is_empty()
        && (url == base_url || url == format!("{base_url}/chat/completions"))
    {
        return true;
    }
    let Some(host) = url_host_of(url) else {
        return false;
    };
    hosts.iter().any(|h| host == h.trim().to_ascii_lowercase())
}

/// 这个名字像不像我们起的(三家前缀任意一个)。只在**地址认不出来时**作旁证用 ——
/// 地址能认出来就一律以地址为准。
pub fn name_is_ours(name: &str) -> bool {
    ["4S-", "HP-", "OG ", "GW-"].iter().any(|p| name.starts_with(p))
}

/// 一条 models.json 条目是不是「我们(4SAPI)写的」。判据:[`url_is_ours`] ——
/// **只有 url 空/缺**时才退回名字前缀(`4S-` / `HP-` / `OG `)。**不能只看 id** ——
/// 别的中转站的同名模型有同样的 id;也不能"名字带前缀就算" —— 用户手配的 `4S-`
/// 条目地址指向别家时不是我们的,删了就是删用户的东西(写入侧的冲突提示同理)。
pub(super) fn wb_is_ours(e: &Value, hosts: &[String], base_url: &str) -> bool {
    let url = e.get("url").and_then(|u| u.as_str()).unwrap_or("");
    if url_is_ours(url, hosts, base_url) {
        return true;
    }
    if url.trim().is_empty() {
        return e
            .get("name")
            .and_then(|n| n.as_str())
            .map(name_is_ours)
            .unwrap_or(false);
    }
    false
}

/// 写入后的读回校验:盘上那条必须与我们**这次写下去的内容逐字段一致**
/// (id / url / apiKey / name),写过的选中项也要落上。
///
/// 以前只比 id + url —— 两者在写之前就成立,于是被截断/写坏的行、或客户端把
/// apiKey 抹掉/换了,校验一律通过(与 ZCode 那边校验 apiKey+baseURL 的口径看齐)。
pub(super) fn wb_verify(models_path: &Path, settings_path: &Path, expect: &Value, selection: Option<&str>) -> bool {
    let same = |e: &Value, k: &str| {
        e.get(k).and_then(|v| v.as_str()) == expect.get(k).and_then(|v| v.as_str())
    };
    let entry_ok = read_json(models_path)
        .ok()
        .flatten()
        .and_then(|v| {
            v.as_array().map(|a| {
                a.iter().any(|e| {
                    same(e, "id")
                        && same(e, "url")
                        && same(e, "apiKey")
                        && same(e, "name")
                })
            })
        })
        .unwrap_or(false);
    if !entry_ok {
        return false;
    }
    match selection {
        None => true,
        Some(want) => read_json(settings_path)
            .ok()
            .flatten()
            .and_then(|s| s.get(WB_SELECTION_KEY).and_then(|v| v.as_str()).map(String::from))
            .as_deref()
            == Some(want),
    }
}

pub(super) fn write_workbuddy_site(
    site: &Site,
    spec: &ModelSpec,
    opts: &Opts,
) -> Result<SiteOutcome, String> {
    let hosts = spec.hosts();
    let models_path = site.root.join("models.json");
    let settings_path = site.root.join("settings.json");

    let Some(models) = read_json(&models_path)? else {
        return Err(format!("{} 不存在或是空的", pretty(&models_path)));
    };
    let Value::Array(mut arr) = models else {
        return Err(format!(
            "{} 不是数组(结构可能已变,本次不写)",
            pretty(&models_path)
        ));
    };

    let mut outcome = SiteOutcome::default();

    // upsert:同 id 的条目就地更新,否则追加
    let idx = arr.iter().position(|e| {
        e.get("id")
            .and_then(|i| i.as_str())
            .map(|i| i == spec.id)
            .unwrap_or(false)
    });
    if let Some(i) = idx {
        let old = arr[i].as_object().cloned().ok_or("条目不是对象")?;
        if !wb_is_ours(&arr[i], &hosts, &spec.base_url) {
            outcome.foreign_conflict = Some(
                arr[i]
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("(无 url)")
                    .to_string(),
            );
        }
        let old_key = old.get("apiKey").and_then(|k| k.as_str()).unwrap_or("");
        let old_url = old.get("url").and_then(|u| u.as_str()).unwrap_or("");
        if old_url == spec.base_url && old_key == spec.key {
            outcome.same = true;
        } else {
            outcome.updated = 1;
            if old_key != spec.key {
                outcome.notes.push("密钥与现有条目不同(确认后会覆盖)".into());
            }
            arr[i] = wb_entry(spec, Some(&old));
        }
    } else {
        arr.push(wb_entry(spec, None));
        outcome.added = 1;
    }

    // 选中项:只有学到前缀才写。学不到就只写条目 ——
    // 猜另一个程序的键格式,会埋下「看着配好了其实没生效」的坑。
    let mut settings_new: Option<Value> = None;
    if let Some(prefix) = opts.selection_prefix.as_deref() {
        if let Some(s) = read_json(&settings_path)? {
            if s.is_object() {
                let mut s = s;
                s[WB_SELECTION_KEY] = json!(format!("{prefix}{}", spec.id));
                settings_new = Some(s);
                outcome.selected = true;
            }
        }
    }

    if outcome.same && settings_new.is_none() {
        // 内容和盘上一致:不写盘,连备份都不必做
        return Ok(outcome);
    }

    let stamp = stamp_now();
    let models_next = Value::Array(arr);
    let settings_for_write = settings_new.clone();
    let models_for_check = models_path.clone();
    let settings_for_check = settings_path.clone();
    // 读回校验要按**这次实际写下去的那一条**逐字段比对(名字可能沿用了用户自己起的)
    let expect = models_next
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|e| e.get("id").and_then(|i| i.as_str()) == Some(spec.id.as_str()))
        })
        .cloned()
        .unwrap_or(Value::Null);
    let want_selection = settings_new
        .as_ref()
        .and_then(|s| s.get(WB_SELECTION_KEY))
        .and_then(|v| v.as_str())
        .map(String::from);
    outcome.backup = commit(
        site,
        &[models_path.clone(), settings_path.clone()],
        &stamp,
        move || {
            write_json(&models_path, &models_next, has_bom(&models_path), 2)?;
            if let Some(s) = settings_for_write {
                write_json(&settings_path, &s, has_bom(&settings_path), 2)?;
            }
            Ok(())
        },
        move || {
            wb_verify(
                &models_for_check,
                &settings_for_check,
                &expect,
                want_selection.as_deref(),
            )
        },
    )?;
    outcome
        .notes
        .push(format!("备份:{}", pretty(&outcome.backup)));
    Ok(outcome)
}

/// 「同名条目被覆盖」这条提示里该怎么称呼**原本那条**的来源:按它的实际地址认
/// (`models::source_of_url`),认不出才是"别家来源"。
/// 以前这句话写死"不是 4SAPI":写 Hapi / OpenCode Go 时意思正好反了(P2)。
pub(super) fn foreign_source_label(url: &str) -> &'static str {
    crate::models::source_of_url(url)
        .and_then(crate::models::find_source)
        .map(|d| d.name)
        .unwrap_or("别家来源")
}

/// 从 WorkBuddy 当前选中值反推前缀口径。返回 None = 没有可用的证据(不猜)。
///
/// 证据两种:
/// - 值带 `custom-local:` 前缀 → 服务端特性开关是开的,照此写;
/// - 值是裸 id 且命中我们自己写的条目 → 开关是关的,照此写。
///
/// 值是个内置模型(如 `gpt-6-luna`)时两种都不是 —— 内置模型永远不带前缀,
/// 由它推不出开关状态,所以返回 None(宁可不写选中,也不写错)。
pub fn learn_prefix(current: Option<&str>, our_ids: &[String]) -> Option<String> {
    let v = current?.trim();
    if v.is_empty() {
        return None;
    }
    if v.starts_with(WB_PREFIX) {
        return Some(WB_PREFIX.to_string());
    }
    if our_ids.iter().any(|id| id == v) {
        return Some(String::new());
    }
    None
}

/// 剥掉已知前缀,给出「当前选中的模型 id」用于界面显示。
pub fn strip_prefix(current: &str) -> &str {
    current.trim().strip_prefix(WB_PREFIX).unwrap_or(current.trim())
}

