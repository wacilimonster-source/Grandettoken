use super::*;

// ───────────────────────── ZCode ─────────────────────────

pub(super) fn write_zcode_site(site: &Site, spec: &ModelSpec) -> Result<SiteOutcome, String> {
    let cfg_path = site.root.join("config.json");
    let rules_path = site.root.join("provider_config.json");

    let Some(mut cfg) = read_json(&cfg_path)? else {
        return Err(format!("{} 读不出来", pretty(&cfg_path)));
    };
    let Some(mut rules) = read_json(&rules_path)? else {
        return Err(format!("{} 读不出来", pretty(&rules_path)));
    };
    // 版本号不认识就放弃 —— 硬写一份新版结构等于替用户改坏它
    let schema = rules
        .get("schemaVersion")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if schema != 1 {
        return Err(format!(
            "provider_config.json 的 schemaVersion={schema}(我们只认 1),结构可能已变,本次不写"
        ));
    }

    let mut outcome = SiteOutcome::default();

    // 找我们上次建的 provider:按名字(source=custom 且 name=4SAPI)
    let existing_pid = cfg
        .get("provider")
        .and_then(|p| p.as_object())
        .and_then(|m| {
            m.iter()
                .find(|(_, v)| {
                    v.get("name").and_then(|n| n.as_str()) == Some(spec.source_name.as_str())
                        && v.get("source").and_then(|s| s.as_str()) == Some("custom")
                })
                .map(|(k, _)| k.clone())
        });
    let pid = existing_pid.clone().unwrap_or_else(new_uuid);
    if existing_pid.is_none() {
        outcome.added = 1;
    } else {
        outcome.updated = 1;
    }

    // 模型条目:limit 只在知道上下文时给;output 一律不给(不编造能力上限,
    // 编大了会让客户端按不存在的上限发请求)。
    let mut model_entry = Map::new();
    if let Some(ctx) = spec.context_tokens {
        model_entry.insert("limit".into(), json!({ "context": ctx }));
    }
    let mut input = vec![json!("text")];
    if spec.multimodal {
        input.push(json!("image"));
    }
    model_entry.insert(
        "modalities".into(),
        json!({ "input": input, "output": ["text"] }),
    );
    model_entry.insert("zcode".into(), json!({ "modalitiesConfigured": true }));

    {
        let providers = cfg
            .get_mut("provider")
            .and_then(|p| p.as_object_mut())
            .ok_or("config.json 里没有 provider 对象")?;
        let entry = providers.entry(pid.clone()).or_insert_with(|| json!({}));
        let obj = entry.as_object_mut().ok_or("provider 条目不是对象")?;
        obj.insert("name".into(), json!(spec.source_name));
        obj.insert("kind".into(), json!("openai-compatible"));
        // 只覆盖我们约定的那几个键。以前整块替换 options,用户在 provider 上自配的
        // 自定义 header / organization / 超时 / 代理会在一次「写入」后凭空消失
        // (表现为某些模型突然不鉴权、走错组织)—— WorkBuddy 那边是保留未知字段的,
        // 同一个产品两套口径不该分叉(报告 P2-10)。
        merge_into(
            obj,
            "options",
            json!({
                "apiKey": spec.key,
                "baseURL": spec.base_url,
                "apiKeyRequired": true,
            }),
        );
        obj.insert("source".into(), json!("custom"));
        let models = obj.entry("models".to_string()).or_insert_with(|| json!({}));
        if let Some(m) = models.as_object_mut() {
            m.insert(spec.id.clone(), Value::Object(model_entry));
        }
        // 之前被用户删过的同名模型要解禁,否则写进去也不显示
        let zc = obj.entry("zcode".to_string()).or_insert_with(|| json!({}));
        if let Some(z) = zc.as_object_mut() {
            let del = z
                .entry("deletedModels".to_string())
                .or_insert_with(|| json!([]));
            if let Some(a) = del.as_array_mut() {
                a.retain(|v| v.as_str() != Some(spec.id.as_str()));
            }
        }
    }

    {
        let rules_root = rules
            .get_mut("config")
            .and_then(|c| c.get_mut("providerConfigRules"))
            .and_then(|c| c.get_mut("providerRules"))
            .and_then(|r| r.as_array_mut())
            .ok_or("provider_config.json 里没有 providerRules 数组")?;
        let i = match rules_root.iter().position(|r| {
            r.get("providerId").and_then(|v| v.as_str()) == Some(pid.as_str())
        }) {
            Some(i) => i,
            None => {
                rules_root.push(json!({
                    "providerId": pid,
                    "providerName": spec.source_name,
                    "config": {
                        "group": "standard-personal",
                        "access": {"type": "api-key", "apiKey": spec.key},
                        "api": {"type": "openai-chat-completions", "baseUrl": spec.base_url},
                        "personalModelIds": [],
                        "modelOrder": [],
                    }
                }));
                rules_root.len() - 1
            }
        };
        let rule = rules_root[i]
            .get_mut("config")
            .and_then(|c| c.as_object_mut())
            .ok_or("providerRule.config 不是对象")?;
        merge_into(rule, "access", json!({"type": "api-key", "apiKey": spec.key}));
        merge_into(
            rule,
            "api",
            json!({"type": "openai-chat-completions", "baseUrl": spec.base_url}),
        );
        if !rule.contains_key("group") {
            rule.insert("group".into(), json!("standard-personal"));
        }
        for key in ["personalModelIds", "modelOrder"] {
            let list = rule.entry(key.to_string()).or_insert_with(|| json!([]));
            if let Some(a) = list.as_array_mut() {
                if !a.iter().any(|v| v.as_str() == Some(spec.id.as_str())) {
                    a.push(json!(spec.id));
                }
            }
        }
    }

    // 上下文窗口:按 (modelId, providerId) upsert
    if let Some(ctx) = spec.context_tokens {
        if let Some(list) = rules
            .get_mut("config")
            .and_then(|c| c.get_mut("modelConfigRules"))
            .and_then(|c| c.get_mut("providerModelRules"))
            .and_then(|r| r.as_array_mut())
        {
            let pos = list.iter().position(|r| {
                r.get("modelId").and_then(|v| v.as_str()) == Some(spec.id.as_str())
                    && r.get("providerId").and_then(|v| v.as_str()) == Some(pid.as_str())
            });
            let entry = json!({
                "modelId": spec.id,
                "providerId": pid,
                "config": {"properties": {"contextWindow": ctx}}
            });
            match pos {
                Some(i) => list[i] = entry,
                None => list.push(entry),
            }
        }
    }

    let stamp = stamp_now();
    let (id, url, key) = (spec.id.clone(), spec.base_url.clone(), spec.key.clone());
    let name2 = spec.source_name.clone();
    let cfg_for_check = cfg_path.clone();
    outcome.backup = commit(
        site,
        &[cfg_path.clone(), rules_path.clone()],
        &stamp,
        move || {
            write_json(&cfg_path, &cfg, has_bom(&cfg_path), 2)?;
            write_json(&rules_path, &rules, has_bom(&rules_path), 2)?;
            Ok(())
        },
        move || {
            read_json(&cfg_for_check)
                .ok()
                .flatten()
                .and_then(|v| {
                    v.get("provider").and_then(|p| {
                        p.as_object().and_then(|m| {
                            m.iter()
                                .find(|(_, v)| {
                                    v.get("name").and_then(|n| n.as_str()) == Some(name2.as_str())
                                })
                                .map(|(_, v)| {
                                    v.get("options").and_then(|o| o.get("apiKey")).and_then(|k| k.as_str())
                                        == Some(key.as_str())
                                        && v.get("options").and_then(|o| o.get("baseURL")).and_then(|b| b.as_str())
                                            == Some(url.as_str())
                                        && v.get("models").and_then(|mm| mm.get(&id)).is_some()
                                })
                        })
                    })
                })
                .unwrap_or(false)
        },
    )?;
    outcome
        .notes
        .push(format!("备份:{}", pretty(&outcome.backup)));
    outcome.notes.push(format!(
        "provider id 复用同一个:重复写入不会堆出多个「{}」",
        spec.source_name
    ));
    Ok(outcome)
}

/// 生成一个 v4 形状的 uuid。不引第三方 crate:`RandomState` 的种子来自系统随机源,
/// 两次取样拼出 16 字节,再按 RFC 4122 打上版本/变体位。
pub(super) fn new_uuid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for chunk in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
        );
        h.write_u32(std::process::id());
        h.write_u64(chunk);
        bytes[chunk as usize * 8..chunk as usize * 8 + 8].copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let b = bytes;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

