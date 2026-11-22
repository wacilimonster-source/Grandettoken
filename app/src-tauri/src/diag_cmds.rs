use super::*;

// ───────────────────────── 渠道体检(裁决 2A) ─────────────────────────

/// 按**显示宽度**补空格:中文一个字占两列,`{:<12}` 会把报告排歪。
pub(crate) fn pad_display(s: &str, width: usize) -> String {
    let w: usize = s
        .chars()
        .map(|c| if (c as u32) > 0x2E7F { 2 } else { 1 })
        .sum();
    let mut out = s.to_string();
    for _ in w..width {
        out.push(' ');
    }
    out
}

pub(crate) fn host_of(url: &Option<String>) -> String {
    match url {
        Some(u) => {
            let rest = u.split_once("://").map(|(_, r)| r).unwrap_or(u);
            rest.split(['/', '?']).next().unwrap_or(rest).to_string()
        }
        None => "(未记录)".into(),
    }
}

/// 系统(WinINET)注册表代理的状态:返回"(是否启用, 给人看的一句话)"。
///
/// 为什么要单独读它:挂件取数用的 reqwest 0.12.28 的 default feature 里带着
/// `system-proxy`,hyper-util 会读 `HKCU\Software\Microsoft\Windows\CurrentVersion\
/// Internet Settings` 的 ProxyEnable —— 也就是说**注册表代理是真的影响挂件的**。
/// 体检报告以前只查环境变量,查不到就印"未检测到(全部直连)":在本机
/// (ProxyEnable=1、ProxyServer=127.0.0.1:7897、环境变量为空)是一句错话,
/// 会把"取数为什么连不上/走的哪个出口"的排查方向带歪(2026-10-19 取证结掉第一轮 P2-12)。
#[cfg(windows)]
pub(crate) fn system_proxy_state() -> (bool, String) {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let Ok(k) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY) else {
        return (false, "注册表里没有 Internet Settings 项".to_string());
    };
    let on = k.get_value::<u32, _>("ProxyEnable").unwrap_or(0) != 0;
    if !on {
        return (false, "未启用".to_string());
    }
    let server = k.get_value::<String, _>("ProxyServer").unwrap_or_default();
    let pac = k.get_value::<String, _>("AutoConfigURL").unwrap_or_default();
    let server = server.trim();
    let pac = pac.trim();
    (
        true,
        if !server.is_empty() {
            format!("已启用 → {server}")
        } else if !pac.is_empty() {
            format!("已启用(PAC)→ {pac}")
        } else {
            "已启用,但注册表里没写地址".to_string()
        },
    )
}

#[cfg(not(windows))]
pub(crate) fn system_proxy_state() -> (bool, String) {
    (false, "非 Windows,注册表代理不适用".to_string())
}

/// 一个渠道的「测试连接」:真打一次接口,把过程如实报出来。
/// 与轮询共用同一条取数链路,但**不写快照**(手动测试不该篡改观测缺口)。
#[tauri::command]
pub(crate) async fn probe_channel(
    app: tauri::AppHandle,
    id: String,
) -> Result<tokenscope_core::fetch::ProbeReport, String> {
    // 自定义渠道:用自己的定义 + 凭据(账户名 = id)打一次,报告口径与内置一致
    if providers::find(&id).is_none() {
        let ch = {
            let state = app.state::<AppState>();
            key_allowed(&state, &id)?;
            let c = lock_rw(&state.config);
            c.custom_channels
                .iter()
                .find(|x| x.id == id)
                .cloned()
                .ok_or_else(|| format!("没有这个自定义渠道:{id}"))?
        };
        let cid = id.clone();
        let key = tokio::task::spawn_blocking(move || secrets::channel_key(&cid))
            .await
            .map_err(|e| format!("读取凭据失败: {e}"))?;
        let started = std::time::Instant::now();
        // 「没填密钥」(Ok(None))与「凭据库读不出来」(Err)是两件事,不能吞成一个:
        // 后者说"还没填"会把用户支去重填一遍根本治不了病(报告 L9,P2-15 同款)。
        let k = match key {
            Ok(k) => k,
            Err(e) => {
                return Ok(tokenscope_core::fetch::ProbeReport {
                    id: ch.id.clone(),
                    name: ch.name.clone(),
                    ok: false,
                    auth_label: "凭据库读取失败".into(),
                    url_used: None,
                    elapsed_ms: 0,
                    remaining: None,
                    used: None,
                    total: None,
                    unit: ch.normalized_unit(),
                    error: Some(format!("凭据管理器读取失败: {e}")),
                    snapshot_last_at: None,
                    snapshot_count_7d: 0,
                    accounts: 1,
                });
            }
        };
        let Some(k) = k else {
            return Ok(tokenscope_core::fetch::ProbeReport {
                id: ch.id.clone(),
                name: ch.name.clone(),
                ok: false,
                auth_label: "未配置密钥".into(),
                url_used: None,
                elapsed_ms: 0,
                remaining: None,
                used: None,
                total: None,
                unit: ch.normalized_unit(),
                error: Some("还没填密钥".into()),
                snapshot_last_at: None,
                snapshot_count_7d: 0,
                accounts: 1,
            });
        };
        let r = {
            let http = app.state::<AppState>().fetcher.http.clone();
            tokenscope_core::custom::fetch_custom(&http, &ch, &k).await
        };
        return Ok(tokenscope_core::fetch::ProbeReport {
            id: ch.id.clone(),
            name: ch.name.clone(),
            ok: r.valid,
            auth_label: "自定义渠道 · 密码管理器".into(),
            url_used: r.url_used,
            elapsed_ms: started.elapsed().as_millis() as u64,
            remaining: r.remaining,
            used: None,
            total: None,
            unit: ch.normalized_unit(),
            error: r.error,
            snapshot_last_at: None,
            snapshot_count_7d: 0,
            accounts: 1,
        });
    }
    let def = providers::find(&id).ok_or_else(|| format!("没有这个渠道:{id}"))?;
    let (fetcher, store) = {
        let state = app.state::<AppState>();
        (state.fetcher.clone(), state.store.clone())
    };
    Ok(fetcher.probe_report(def, &store).await)
}

/// 一键体检:逐渠道真打一次 + 本机四项检查,产出一份**可复制**的纯文本。
/// 报告里只有域名、状态、数值与路径 —— 没有密钥、没有完整响应体。
#[tauri::command]
pub(crate) async fn run_diagnostic(app: tauri::AppHandle) -> Result<String, String> {
    let (fetcher, store) = {
        let state = app.state::<AppState>();
        (state.fetcher.clone(), state.store.clone())
    };
    // 隐藏清单读内存里的 live 配置(报告 L13):以前重新从库里 try_load,配置损坏
    // 回落默认值后 hidden_channels 为空,体检会对「隐藏 = 不发请求」红线的渠道真打。
    let config_broken = refuse_if_config_broken().is_err();
    let cfg = {
        let state = app.state::<AppState>();
        let c = lock_rw(&state.config).clone();
        c
    };
    let mut lines: Vec<String> = vec![format!(
        "Grandettoken 体检 · {}",
        tokenscope_core::fetch::local_datetime_string()
    )];
    let total = providers::PROVIDERS.len();
    let mut need = 0usize;
    if config_broken {
        lines.push(
            "配置损坏:渠道体检降级 —— 只查本机,不外呼(隐藏清单不可信,守不住「隐藏渠道不发请求」红线)".into(),
        );
    }
    for (i, def) in providers::PROVIDERS.iter().enumerate() {
        let idx = format!("[{}/{}]", i + 1, total);
        if config_broken {
            continue;
        }
        if cfg.hidden_channels.iter().any(|h| h == def.id) {
            lines.push(format!(
                "{} {} 已隐藏(不发请求,跳过)",
                idx,
                pad_display(def.name, 12)
            ));
            continue;
        }
        let r = fetcher.probe_report(def, &store).await;
        if r.ok {
            let val = match r.remaining {
                Some(v) if r.unit == "%" => format!("{v:.0}%"),
                Some(v) if r.unit == "CNY" => format!("¥{v:.2}"),
                Some(v) => format!("{v:.0}"),
                None => "—".to_string(),
            };
            lines.push(format!(
                "{} {} ✓ {}ms · {} · {}",
                idx,
                pad_display(def.name, 12),
                r.elapsed_ms,
                host_of(&r.url_used),
                val
            ));
        } else {
            need += 1;
            lines.push(format!(
                "{} {} ✗ {} —— 显示的是本地快照,取数未通",
                idx,
                pad_display(def.name, 12),
                r.error.unwrap_or_else(|| "失败".into())
            ));
        }
    }
    // 本机那一半:网络通了不等于记录在写,两半各证一半
    // 自定义渠道:逐个真打一次(它们不在 PROVIDERS 里,单独一轮)
    {
        let custom = { lock_rw(&app.state::<AppState>().config).custom_channels.clone() };
        for (i, ch) in custom.iter().enumerate() {
            let idx = format!("[自{}/{}]", i + 1, custom.len());
            if config_broken {
                continue;
            }
            if cfg.hidden_channels.iter().any(|h| h == &ch.id) {
                lines.push(format!("{} {} 已隐藏(不发请求,跳过)", idx, pad_display(&ch.name, 12)));
                continue;
            }
            let cid = ch.id.clone();
            let key = tokio::task::spawn_blocking(move || secrets::channel_key(&cid))
                .await
                .map_err(|e| format!("读取凭据失败: {e}"))?;
            match key {
                // 「没填」与「凭据库读不出来」分开说(报告 L9):吞成一个会让人去
                // 重填密钥,可病根本不在密钥上。
                Err(e) => {
                    need += 1;
                    lines.push(format!(
                        "{} {} ✗ 凭据库读取失败:{e}",
                        idx,
                        pad_display(&ch.name, 12)
                    ));
                }
                Ok(None) => {
                    need += 1;
                    lines.push(format!(
                        "{} {} ✗ 还没填密钥 —— 在渠道卡里填一次",
                        idx,
                        pad_display(&ch.name, 12)
                    ));
                }
                Ok(Some(k)) => {
                    let http = app.state::<AppState>().fetcher.http.clone();
                    let r = tokenscope_core::custom::fetch_custom(&http, ch, &k).await;
                    if r.valid {
                        let val = match r.remaining {
                            Some(v) if ch.normalized_unit() == "%" => format!("{v:.0}%"),
                            Some(v) if ch.normalized_unit() == "CNY" => format!("¥{v:.2}"),
                            Some(v) => format!("{v:.0}"),
                            None => "—".to_string(),
                        };
                        lines.push(format!(
                            "{} {} ✓ {}{}",
                            idx,
                            pad_display(&ch.name, 12),
                            host_of(&r.url_used),
                            if r.probe_only {
                                " · 探活通过(不解析余额)".to_string()
                            } else {
                                format!(" · 读到 {val}")
                            }
                        ));
                    } else {
                        need += 1;
                        lines.push(format!(
                            "{} {} ✗ {}",
                            idx,
                            pad_display(&ch.name, 12),
                            r.error.unwrap_or_else(|| "失败".into())
                        ));
                    }
                }
            }
        }
    }
    let db_err = {
        let s = lock_rw(&store);
        s.set_setting("diag_ping", &now_ts().to_string()).err()
    };
    lines.push(format!(
        "本机:快照库可写 {}",
        if db_err.is_none() { "✓" } else { "✗" }
    ));
    let auto = autostart::entry_present();
    let auto_txt = match (auto, cfg.autostart) {
        (Some(_), true) => "✓ 已注册".to_string(),
        (Some(_), false) => "注册表里有,但设置里是关的(点一次开关即可对齐)".to_string(),
        (None, true) => "✗ 设置里开着,注册表里没有(点一次开关重写)".to_string(),
        (None, false) => "未启用".to_string(),
    };
    lines.push(format!("      开机自启 {auto_txt}"));
    let envp: Vec<String> = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()).map(|v| format!("{k}={v}")))
        .collect();
    let (sys_on, sys_txt) = system_proxy_state();
    lines.push(format!(
        "      代理 · 环境变量 {}",
        if envp.is_empty() {
            "无".to_string()
        } else {
            envp.join(" · ")
        }
    ));
    lines.push(format!("      代理 · 系统(注册表){sys_txt}"));
    lines.push(format!(
        "      取数是否走代理:{}",
        if !envp.is_empty() || sys_on {
            "是(上面两行任一开着,reqwest 都会走它;每日签到例外,走强制直连)"
        } else {
            "否,直连(注意:代理软件若是 TUN/全局模式,系统层面仍会接管,这里看不出来)"
        }
    ));
    // ── WorkBuddy 专项:这条链最长、失败点最多,而且是"装上就取不到数"的最常见来源。
    //    用户报问题时把这几行贴过来,基本就能定位到"没装 / 没找到 / 拿不到密钥 / 过期 / 被拒"。
    lines.push("      ── WorkBuddy 凭据链 ──".to_string());
    for l in tokenscope_core::workbuddy::diag_summary(now_ts()) {
        lines.push(format!("      {l}"));
    }
    if let Ok(dir) = app.path().app_data_dir() {
        lines.push(format!("      数据目录 {}", dir.display()));
    }
    if let Some(e) = lock_rw(&app.state::<AppState>().startup_error).clone() {
        lines.push(format!("      启动期错误 {e}"));
    }
    lines.push(if need == 0 {
        "结论:全部正常".to_string()
    } else {
        format!("结论:{need} 项需要处理(上面对应行标 ✗)")
    });
    Ok(lines.join("\n"))
}

