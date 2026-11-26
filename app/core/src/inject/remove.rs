use super::*;

/// 「取消写入」的预告:要移除哪一条、会不会顺带清掉客户端的选中项。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Removal {
    /// 人话描述要移除什么(界面直接显示,含条目名与指向的地址)
    pub what: String,
    /// 会顺带清掉客户端自己的「选中项」(它正指向这条模型时)
    pub clears_selection: bool,
    /// 要移掉几条(WorkBuddy=1;Trae=每个 agent 组各一条)
    pub count: usize,
}

/// ZCode 里我们这个来源的 provider id(source=custom 且 name=来源名)。
/// 每个来源一个 provider(「4SAPI」/「Hapi」/「OpenCode Go」),互不覆盖。
pub(super) fn zcode_pid(cfg: &Value, name: &str) -> Option<String> {
    cfg.get("provider")
        .and_then(|p| p.as_object())
        .and_then(|m| {
            m.iter()
                .find(|(_, v)| {
                    v.get("name").and_then(|n| n.as_str()) == Some(name)
                        && v.get("source").and_then(|s| s.as_str()) == Some("custom")
                })
                .map(|(k, _)| k.clone())
        })
}

/// WorkBuddy 类:有没有我们写的这一条,以及移除它会动到什么。
pub(super) fn wb_removal(site: &Site, spec: &ModelSpec) -> (bool, Option<Removal>) {
    let hosts = spec.hosts();
    let models = read_json(&site.root.join("models.json")).ok().flatten();
    let entry = models
        .as_ref()
        .and_then(|v| v.as_array())
        .and_then(|a| {
            a.iter().find(|e| {
                e.get("id").and_then(|i| i.as_str()) == Some(spec.id.as_str())
            })
        });
    let Some(e) = entry else {
        return (false, None);
    };
    if !wb_is_ours(e, &hosts, &spec.base_url) {
        // 同名但不属于这个来源(比如是别家地址) → 不当成"已配置"
        return (false, None);
    }
    let name = e
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or(spec.id.as_str());
    let url = e.get("url").and_then(|u| u.as_str()).unwrap_or("");
    let cur = read_json(&site.root.join("settings.json"))
        .ok()
        .flatten()
        .and_then(|s| {
            s.get(WB_SELECTION_KEY)
                .and_then(|m| m.as_str())
                .map(String::from)
        });
    let clears = cur
        .as_deref()
        .map(|v| v == spec.id || v == format!("{WB_PREFIX}{}", spec.id))
        .unwrap_or(false);
    (
        true,
        Some(Removal {
            what: format!("models.json 里的条目「{name}」({url})"),
            clears_selection: clears,
            count: 1,
        }),
    )
}

/// ZCode:模型在不在我们的 provider 里,以及移除后会剩下什么。
pub(super) fn zcode_removal(site: &Site, spec: &ModelSpec) -> (bool, Option<Removal>) {
    let cfg = match read_json(&site.root.join("config.json")) {
        Ok(Some(v)) => v,
        _ => return (false, None),
    };
    let Some(pid) = zcode_pid(&cfg, &spec.source_name) else {
        return (false, None);
    };
    let models = cfg
        .get("provider")
        .and_then(|p| p.get(&pid))
        .and_then(|p| p.get("models"))
        .and_then(|m| m.as_object());
    let Some(models) = models else {
        return (false, None);
    };
    if !models.contains_key(&spec.id) {
        return (false, None);
    }
    let left = models.len().saturating_sub(1);
    let what = if left == 0 {
        format!(
            "provider「{}」下的「{}」—— 这是它最后一个模型,连 provider 一起移除",
            spec.source_name, spec.id
        )
    } else {
        format!(
            "provider「{}」下的「{}」(provider 留下,还剩 {left} 个模型)",
            spec.source_name, spec.id
        )
    };
    (
        true,
        Some(Removal {
            what,
            clears_selection: false, // ZCode 的选中是会话级的,配置文件里没有
            count: 1,
        }),
    )
}

pub(super) fn remove_workbuddy_site(site: &Site, spec: &ModelSpec) -> Result<SiteOutcome, String> {
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
    // 只删「我们自己写的那一条」:同 id 但 url 属于别家来源、或用户手配的条目一律不动。
    // 写入契约第③条;以前是按 id 全表 retain,一次「取消写入」能把别人的同名条目一起删掉。
    let hosts = spec.hosts();
    if let Some(i) = arr.iter().position(|e| {
        e.get("id").and_then(|x| x.as_str()) == Some(spec.id.as_str())
            && wb_is_ours(e, &hosts, &spec.base_url)
    }) {
        arr.remove(i);
        outcome.removed = 1;
    }
    if outcome.removed == 0 {
        return Ok(outcome); // 本来就没有 —— 不写盘,连备份都不必做
    }

    // 选中项正指向它才清:删掉 model 键(回到"没选"的状态,与写入前一致)
    let mut settings_new: Option<Value> = None;
    if let Some(mut s) = read_json(&settings_path)? {
        if s.is_object() {
            let cur = s
                .get(WB_SELECTION_KEY)
                .and_then(|m| m.as_str())
                .map(String::from);
            if let Some(cur) = cur {
                if cur == spec.id || cur == format!("{WB_PREFIX}{}", spec.id) {
                    if let Some(o) = s.as_object_mut() {
                        o.remove(WB_SELECTION_KEY);
                    }
                    settings_new = Some(s);
                    outcome.clears_selection = true;
                }
            }
        }
    }

    let stamp = stamp_now();
    let models_next = Value::Array(arr);
    let settings_for_write = settings_new.clone();
    let models_for_check = models_path.clone();
    let settings_for_check = settings_path.clone();
    let id = spec.id.clone();
    let hosts_for_check = hosts;
    let base_for_check = spec.base_url.clone();
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
            let gone = read_json(&models_for_check)
                .ok()
                .flatten()
                .and_then(|v| {
                    v.as_array().map(|a| {
                        !a.iter().any(|e| {
                            e.get("id").and_then(|i| i.as_str()) == Some(id.as_str())
                                && wb_is_ours(e, &hosts_for_check, &base_for_check)
                        })
                    })
                })
                .unwrap_or(false);
            let sel_ok = read_json(&settings_for_check)
                .ok()
                .flatten()
                .and_then(|s| {
                    s.get(WB_SELECTION_KEY)
                        .and_then(|m| m.as_str())
                        .map(|v| v != id && v != format!("{WB_PREFIX}{id}"))
                })
                .unwrap_or(true);
            gone && sel_ok
        },
    )?;
    outcome
        .notes
        .push(format!("备份:{}", pretty(&outcome.backup)));
    Ok(outcome)
}

pub(super) fn remove_zcode_site(site: &Site, spec: &ModelSpec) -> Result<SiteOutcome, String> {
    let cfg_path = site.root.join("config.json");
    let rules_path = site.root.join("provider_config.json");
    let Some(mut cfg) = read_json(&cfg_path)? else {
        return Err(format!("{} 读不出来", pretty(&cfg_path)));
    };
    let Some(mut rules) = read_json(&rules_path)? else {
        return Err(format!("{} 读不出来", pretty(&rules_path)));
    };
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
    let Some(pid) = zcode_pid(&cfg, &spec.source_name) else {
        return Ok(outcome); // 没有我们的 provider → 没什么可删
    };

    let mut left = 0usize;
    if let Some(models) = cfg
        .get_mut("provider")
        .and_then(|p| p.get_mut(&pid))
        .and_then(|p| p.get_mut("models"))
        .and_then(|m| m.as_object_mut())
    {
        if models.remove(&spec.id).is_some() {
            outcome.removed += 1;
        }
        left = models.len();
    }

    // providerRules:把这条模型从 personalModelIds / modelOrder 里摘掉;模型空了连 provider 一起收走
    if let Some(list) = rules
        .get_mut("config")
        .and_then(|c| c.get_mut("providerConfigRules"))
        .and_then(|c| c.get_mut("providerRules"))
        .and_then(|r| r.as_array_mut())
    {
        if let Some(i) = list
            .iter()
            .position(|r| r.get("providerId").and_then(|v| v.as_str()) == Some(pid.as_str()))
        {
            if left == 0 {
                list.remove(i);
            } else if let Some(cfgobj) = list[i].get_mut("config").and_then(|c| c.as_object_mut()) {
                for key in ["personalModelIds", "modelOrder"] {
                    if let Some(a) = cfgobj.get_mut(key).and_then(|v| v.as_array_mut()) {
                        a.retain(|v| v.as_str() != Some(spec.id.as_str()));
                    }
                }
            }
        }
    }

    // providerModelRules:按 (modelId, providerId) 摘掉
    if let Some(list) = rules
        .get_mut("config")
        .and_then(|c| c.get_mut("modelConfigRules"))
        .and_then(|c| c.get_mut("providerModelRules"))
        .and_then(|r| r.as_array_mut())
    {
        list.retain(|r| {
            !(r.get("modelId").and_then(|v| v.as_str()) == Some(spec.id.as_str())
                && r.get("providerId").and_then(|v| v.as_str()) == Some(pid.as_str()))
        });
    }

    // 模型空了:provider 定义也一并移除(留一个空 provider 只会在选择器里碍事)
    if left == 0 {
        if let Some(p) = cfg.get_mut("provider").and_then(|p| p.as_object_mut()) {
            p.remove(&pid);
        }
    }

    if outcome.removed == 0 {
        return Ok(outcome);
    }

    let stamp = stamp_now();
    let (id, pid_for_check) = (spec.id.clone(), pid.clone());
    let name2 = spec.source_name.clone();
    let cfg_check = cfg_path.clone();
    let rules_check = rules_path.clone();
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
            let after = read_json(&cfg_check).ok().flatten();
            match after {
                Some(v) => match zcode_pid(&v, &name2) {
                    // provider 还在:模型必须已经不在它下面
                    Some(p) => v
                        .get("provider")
                        .and_then(|pr| pr.get(&p))
                        .and_then(|pr| pr.get("models"))
                        .and_then(|m| m.as_object())
                        .map(|m| !m.contains_key(&id))
                        .unwrap_or(true),
                    // provider 收了:确认我们这个 pid 在两份文件里都不再出现。
                    // 以前这里写的是「配置文本里不能再有 4SAPI 字样」—— 来源名被写死,
                    // 用户只要另外也配过 4SAPI,别家来源的取消就永远校验不过并自动回滚。
                    None => {
                        let in_cfg = v.to_string().contains(pid_for_check.as_str());
                        let in_rules = read_json(&rules_check)
                            .ok()
                            .flatten()
                            .map(|r| r.to_string().contains(pid_for_check.as_str()))
                            .unwrap_or(false);
                        !in_cfg && !in_rules
                    }
                },
                None => false,
            }
        },
    )?;
    outcome
        .notes
        .push(format!("备份:{}", pretty(&outcome.backup)));
    if left == 0 {
        outcome.notes.push(format!(
            "这是它最后一个模型:provider「{name_disp}」也一并移除了",
            name_disp = spec.source_name
        ));
    }
    Ok(outcome)
}

/// 取消写入:把这条模型从客户端里删掉。规矩与写入完全一样
/// (目标没运行 / 先备份 / 只动我们的条目 / 写完读回校验,不过就回滚)。
pub fn remove(client: Client, spec: &ModelSpec) -> Result<Vec<Report>, String> {
    let mut out = Vec::new();
    for adapter in ClientAdapter::discover(client)? {
        let site = &adapter.site;
        let stamp0 = stamp_now();
        let (cid, lbl) = (client.id().to_string(), site.label.clone());
        let mut local: Vec<Report> = Vec::new();
        let running_warn = &adapter.running_warning;
        // 与 apply 同口径(报告 P1-6):一套安装失败不该把已删成的另一家一起丢成"没记录"
        let res: Result<(), String> = (|| {
            adapter.require_readable()?;
            let stamp = stamp0.clone();
            let (client_id, label) = (cid.clone(), lbl.clone());
            let cli_cfg = CliTargetConfig::from_model_spec(spec);
            let o = match client {
            Client::Workbuddy | Client::WorkbuddyAi => remove_workbuddy_site(&site, spec)?,
            Client::Zcode => remove_zcode_site(&site, spec)?,
            Client::Trae => {
                let db = site.root.join("state.vscdb");
                if !trae_has_ours(&db, spec) {
                    // 本来就没有:与另三家同口径,不写盘、不建备份目录
                    SiteOutcome::default()
                } else {
                    // 与另三家走同一条 commit:先备份(含 -wal/-shm)→ 写 → 读回校验,不过就回滚
                    adapter.commit(
                        &stamp,
                        || remove_trae_site(&db, spec),
                        || trae_verify_removed(&db, spec),
                    )?
                }
            }
            Client::Dsh => remove_dsh_site(&site, spec)?,
            Client::ClaudeCode => remove_claude_code_site(&site, &cli_cfg)?,
            Client::Codex => remove_codex_site(&site)?,
            Client::OpenCode => remove_opencode_site(&site, &cli_cfg)?,
        };
            local.push(Report {
                client: client_id,
                label,
                stamp,
                backup: pretty(&o.backup),
                backup_path: o.backup.to_string_lossy().to_string(),
                added: 0,
                updated: 0,
                removed: o.removed,
            selected: false,
            effect: if client == Client::Dsh {
                "DSH 下一个请求就不在列表里了(不用重启)".to_string()
            } else if matches!(client, Client::ClaudeCode | Client::Codex | Client::OpenCode) {
                format!("下一个 {} 会话生效", site.label)
            } else {
                format!("重启 {} 后这条模型不再出现在列表里", site.label)
            },
            notes: {
                let mut n = o.notes;
                if o.removed == 0 {
                    n.push("本来就没有这条(可能已经删过了)".into());
                }
                if o.clears_selection {
                    n.push("它的「选中项」正指向这条模型,已一并清掉".into());
                }
                n
                },
                error: None,
            });
            Ok(())
        })();
        match res {
            Ok(()) => {
                for r in &mut local {
                    if let Some(w) = &running_warn {
                        r.notes.push(w.clone());
                    }
                    if matches!(client, Client::ClaudeCode | Client::Codex | Client::OpenCode) {
                        r.notes.push(inject_source::notice(client).into());
                    }
                }
                out.extend(local)
            }
            Err(e) => out.push(Report {
                client: cid,
                label: lbl,
                stamp: stamp0,
                backup: String::new(),
                backup_path: String::new(),
                added: 0,
                updated: 0,
                removed: 0,
                selected: false,
                effect: "这一家没删成(其余客户端的结果见下)".into(),
                notes: Vec::new(),
                error: Some(e),
            }),
        }
    }
    if !out.is_empty() && out.iter().all(|r| r.error.is_some()) {
        return Err(out[0]
            .error
            .clone()
            .unwrap_or_else(|| "取消写入失败".to_string()));
    }
    Ok(out)
}
