use super::*;

#[tauri::command]
pub(crate) fn get_cached_channels(state: State<'_, AppState>) -> Option<ChannelsPush> {
    let views = state.fetcher.last_views()?;
    let home = home_view_of(&lock_rw(&state.config), &views, now_ts());
    Some(ChannelsPush { views, home })
}

#[tauri::command]
pub(crate) async fn get_channels(state: State<'_, AppState>, force: Option<bool>) -> Result<Vec<ChannelView>, String> {
    let store = state.store.clone();
    let reader = state.read_store.clone();
    let fetcher = state.fetcher.clone();
    // fetch_shared:与轮询/托盘刷新共用闸门,手动刷新不再叠加出一份并发请求(报告 O-4)
    if force.unwrap_or(false) { fetcher.request_refresh(); }
    Ok(fetcher.fetch_shared_using(&store, &reader).await)
}

/// 首页四种形态的**统一视图**:一行一个 `FormRow`,加一份排好的顺序与几个计数。
///
/// 存在的理由见 `core::view` 的头注:四种形态以前各自解释一次 `valid / limited /
/// updatedAt`,同一个渠道能给出三种结论。这一条命令之后,前端只负责"画"。
///
/// 取数走 `fetch_shared`(与 `get_channels` 同一闸门、同一份 5 秒内的近期结果),
/// 所以前端连着调这两条不会多打一次账务接口。
#[tauri::command]
pub(crate) async fn home_view(state: State<'_, AppState>) -> Result<view::HomeView, String> {
    let store = state.store.clone();
    let reader = state.read_store.clone();
    let fetcher = state.fetcher.clone();
    let views = fetcher.fetch_shared_using(&store, &reader).await;

    let cfg = lock_rw(&state.config).clone();
    Ok(home_view_of(&cfg, &views, now_ts()))
}

#[tauri::command]
pub(crate) fn get_config(state: State<'_, AppState>) -> Config {
    lock_rw(&state.config).clone()
}

/// 按配置重注册全局快捷键(空串 = 只注销)。先注册新键、成功后才撤旧键:
/// 新键被其它应用占用时保持现状,不会出现「旧键没了、新键也没有」。
/// 行为回调挂在插件 Builder 的 with_handler 上(见 run())。
pub(crate) fn apply_hotkey(app: &tauri::AppHandle, state: &AppState, want: &str) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let want = want.trim().to_string();
    let mut cur = lock_rw(&state.hotkey);
    if cur.as_deref().unwrap_or("") == want.as_str() {
        return Ok(()); // 没变,别动注册器
    }
    let manager = app.global_shortcut();
    if want.is_empty() {
        if let Some(old) = cur.take() {
            let _ = manager.unregister(old.as_str());
        }
        return Ok(());
    }
    manager
        .register(want.as_str())
        .map_err(|e| {
            // {e} 是全局热键注册表的英文原文:用户面给结论,原文进日志
            tokenscope_core::diaglog::write_keyed("hotkey", "register", &format!("注册 {want} 失败: {e}"));
            format!("快捷键 {want} 注册失败,可能被其它程序占用")
        })?;
    if let Some(old) = cur.take() {
        let _ = manager.unregister(old.as_str());
    }
    *cur = Some(want);
    Ok(())
}

#[tauri::command]
pub(crate) async fn set_config(app: tauri::AppHandle, state: State<'_, AppState>, config: Config) -> Result<(), String> {
    refuse_if_config_broken()?;
    let hibernate_changed = lock_rw(&state.config).hibernate_webview_on_hide != config.hibernate_webview_on_hide;
    // 前端提交的是它启动时那份配置的快照。更新流程(跳过版本/上次检查时间)和
    // 窗口位置是后端单方面往前推的字段 —— 前端拿不到最新值,整份覆盖会把它们
    // 抹回 None(报告 P1-3:跳过某版本后,下次保存设置让该版本又重新弹窗)。
    let mut incoming = config;
    {
        let cur = lock_rw(&state.config);
        if incoming.skipped_version.is_none() {
            incoming.skipped_version = cur.skipped_version.clone();
        }
        if incoming.last_check_at.is_none() {
            incoming.last_check_at = cur.last_check_at;
        }
        // 「详细日志」是**后端单方面推进的字段**(只有 diag_log_verbose 命令能改它)。
        // 前端手里永远是开机那一刻的快照,整份覆盖会把用户的开关抹回默认值 ——
        // 与「跳过版本被覆盖」是同一个坑(报告 P1-3),这里直接由后端说了算。
        incoming.diag_verbose = cur.diag_verbose;
        if incoming.win_x.is_none() {
            incoming.win_x = cur.win_x;
        }
        if incoming.win_y.is_none() {
            incoming.win_y = cur.win_y;
        }
        // 选中项口径是后端从客户端文件里读出来的(前端手里永远是旧的):
        // 整份覆盖会把刚学会的抹掉,所以后端已知的键以库里为准。
        let mut merged = cur.selection_prefix.clone();
        for (k, v) in &incoming.selection_prefix {
            merged.entry(k.clone()).or_insert_with(|| v.clone());
        }
        incoming.selection_prefix = merged;

        // 签到:只有 enabled / auto 是用户意图,其余七个字段(last_day / last_code / attempts /
        // next_try_at / last_try_at / last_credits / last_message)是后端进度。前端提交的永远是
        // 开机那一刻的快照,整份覆盖会把当天「已领 +100」抹回「今天还没签」、退避计数一起清零,
        // 节拍器随后在同一天再签一次(报告 P1-13;与它修掉的「跳过版本被覆盖」是同一个坑)。
        // enabled_since 也归后端:在 false→true 的那一跳记时刻(签到中心靠它区分
        // "没签"与"开启之前无数据"),关闭时清掉,再开重新起算。
        let mut ck = cur.checkin.clone();
        for (id, inc) in &incoming.checkin {
            match ck.get_mut(id) {
                Some(c) => {
                    if inc.enabled && !c.enabled {
                        c.enabled_since = Some(now_ts());
                    } else if !inc.enabled {
                        c.enabled_since = None;
                    }
                    c.enabled = inc.enabled;
                    c.auto = inc.auto;
                }
                None => {
                    let mut fresh = checkin::CheckinCfg::default();
                    fresh.enabled = inc.enabled;
                    fresh.auto = inc.auto;
                    if inc.enabled {
                        fresh.enabled_since = Some(now_ts());
                    }
                    ck.insert(id.clone(), fresh);
                }
            }
        }
        incoming.checkin = ck;

        // 自定义渠道只能由 custom_add / custom_remove 改:前端手里永远是开机那一刻的
        // 快照(custom_add 之后不重取 get_config),整份覆盖会把刚加的渠道删掉、
        // 把刚删的复活(密钥成孤儿)。与上面「跳过版本被覆盖」是同一个坑(报告 B1)。
        incoming.custom_channels = cur.custom_channels.clone();
    }

    // 先应用快捷键:注册失败(如被占用)就让整次保存失败,前端好回滚旧值
    let old_hotkey = lock_rw(&state.hotkey).clone();
    if let Err(e) = apply_hotkey(&app, &state, &incoming.hotkey) {
        return Err(e);
    }
    let next = incoming;
    // MiniMax 托管开关立即生效(set_config 不走 sanitize,不能等下次配置加载)
    tokenscope_core::minimax::set_writeback_enabled(next.minimax_writeback);
    tokenscope_core::antigravity::set_cli_path(next.antigravity_cli_path.clone());
    if let Err(e) = save_config_off_main(&state, move |c| *c = next).await {
        // 落库失败:快捷键严格回到保存前。以前只在"原来有键"时回滚,
        // 第一次设键又落库失败时,那个组合键已被挂件抢走却显示为空(报告 P3-34)。
        let _ = apply_hotkey(&app, &state, old_hotkey.as_deref().unwrap_or(""));
        return Err(e);
    }
    // 配置变了(隐藏渠道 / 间隔 / 阈值…)就作废"5 秒内复用上次结果"的窗口 ——
    // 否则保存后立刻取数会拿到旧视图,界面看起来像"点了没反应"(用户实测:隐藏渠道恢复)。
    state.fetcher.invalidate();
    tray::wake(&app);
    if hibernate_changed {
        hibernate::config_changed(&app);
    }
    google_cloud_web::wake();
    Ok(())
}

/// 只回显尾 4 位,用于确认每个渠道存的到底是哪把 key(存错 key 却以为是接口坏了,
/// 是最难排查的一类问题)。不回显完整密钥。复用本机应用登录态的渠道没有密钥可回显;
/// 网页登录型(WorkBuddy)存的就是令牌本身,同样值得回显尾 4 位。
#[tauri::command]
pub(crate) async fn key_hint(app: tauri::AppHandle, id: String) -> Result<Option<String>, String> {
    let builtin_ok = matches!(
        providers::find(&id).map(|d| d.auth),
        Some(providers::AuthKind::Keyring | providers::AuthKind::WebSession)
    );
    // 自定义渠道的密钥也在凭据管理器里(账户名 = id),尾号照样值得回显 ——
    // 以前这里对 custom-N 返回 Ok(None),卡片上的尾号永远空着(报告 B3)。
    let custom_ok = !builtin_ok && {
        let state = app.state::<AppState>();
        is_custom_id(&id)
            && lock_rw(&state.config).custom_channels.iter().any(|x| x.id == id)
    };
    if !builtin_ok && !custom_ok {
        return Ok(None);
    }
    // 这是个 RPC,而且每轮渲染会对每个已配密钥的渠道各发一次 —— 以前是同步命令,
    // 等于把最多 6 次凭据管理器调用串在主线程上(报告 P1-14)。
    // 另外不再用 secrets::masked:它把"凭据库坏了"吞成 None(报告 P2-15)。
    // 走 resolve 而不是 get:引用模式的渠道要显示**箱子那把**的尾号,否则界面说
    // "未配置密钥"而取数其实正常(两份真相的典型症状)。
    tokio::task::spawn_blocking(move || match secrets::resolve(&id) {
        Ok(r) => Ok(r.key().map(secrets::mask_of)),
        Err(e) => Err(e),
    })
    .await
    .map_err(|e| format!("读取凭据失败: {e}"))?
}

/// 这个 id 是不是自定义渠道(custom-1、custom-2…)。
pub(crate) fn is_custom_id(id: &str) -> bool {
    id.starts_with("custom-")
}

/// 这个渠道"当前密钥的指纹":内置渠道读它自己的凭据,自定义渠道的凭据名就是 id。
/// **阻塞**(读凭据管理器),只能在非主线程上调。
pub(crate) fn fp_of_any(id: &str) -> String {
    if let Some(def) = providers::find(id) {
        return tokenscope_core::fetch::current_key_fp(def);
    }
    match secrets::channel_key(id) {
        Ok(Some(k)) if !k.trim().is_empty() => tokenscope_core::fetch::key_fp(&k),
        _ => String::new(),
    }
}

/// 这个渠道能不能填/删密钥:内置渠道(非 App 型)或**存在的**自定义渠道。
/// 自定义渠道的密钥也在同一个凭据管理器里,账户名就是渠道 id。
pub(crate) fn key_allowed(state: &AppState, id: &str) -> Result<(), String> {
    if providers::find(id).is_some() {
        return Ok(());
    }
    let c = lock_rw(&state.config);
    if is_custom_id(id) && c.custom_channels.iter().any(|x| x.id == id) {
        return Ok(());
    }
    Err(format!("未知渠道: {id}"))
}

#[tauri::command]
pub(crate) async fn set_key(app: tauri::AppHandle, id: String, key: String) -> Result<(), String> {
    key_allowed(&app.state::<AppState>(), &id)?;
    if let Some(def) = providers::find(&id) {
        if let providers::AuthKind::App(app) = def.auth {
            return Err(format!(
                "{} 不需要填密钥:凭据直接读本机已登录的 {} 客户端",
                def.name,
                app.label()
            ));
        }
    }
    // 凭据管理器写 = RPC:不能在主线程做(报告 P1-14)
    //
    // 互斥(裁决 7):填了值就把引用清掉。否则读路径**优先看引用**,
    // 用户填的新值会被完全忽略 —— 表现为"保存成功但还在用旧的",最难查的那种。
    //
    // ⚠ **顺序:先写值、后清引用**(2026-10-21 修 N03)。反过来的话,
    // 写值这一步会因为"密钥为空"被拒(`secrets::set` 里 `if key.is_empty()`),
    // 而那时引用**已经被清掉了** —— 用户看到"保存失败",但原来那份能用的引用没了,
    // 渠道变成"既没引用也没值"。改成先写后清:写失败时旧引用还在,渠道继续正常工作;
    // 而"两份都在"的短暂状态完全无害,因为读路径优先看引用。
    let cid = id.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        secrets::set(&cid, &key)?;
        secrets::link_clear(&cid)
    })
    .await
    .map_err(|e| format!("保存密钥失败: {e}"))??;
    // 反查索引同步清掉(它只是索引,权威来源是上面那个指针)
    let store = app.state::<AppState>().store.clone();
    lock_rw(&store).vault_ref_clear(&id)?;
    // 与删除同理:刚换过凭据就别再复用 5 秒内的旧结果,否则"保存了却还显示未配置/旧数值"
    app.state::<AppState>().fetcher.invalidate();
    sync_channel_to_gateway(&app, &id);
    Ok(())
}

/// 返回值是给界面直接显示的一句话(成功也要说清"删了什么"),不是 Result<(),_>:
/// 清浏览器会话失败时以前是 `let _ =` 一吞,命令照样 Ok、界面照样说"已删除密钥",
/// 而下一轮取数又读到了那个 Cookie —— 用户看到"清掉了却还显示已登录"。
#[tauri::command]
pub(crate) async fn delete_key(app: tauri::AppHandle, id: String) -> Result<String, String> {
    key_allowed(&app.state::<AppState>(), &id)?;
    // 自定义渠道:只有凭据管理器里那一份,没有网页会话可清
    if providers::find(&id).is_none() {
        purge_channel_credentials(&app, &id).await?;
        let state = app.state::<AppState>();
        state.fetcher.forget_channel(&lock_rw(&state.store), &id)?;
        return Ok("已删除这个自定义渠道的密钥(渠道定义还在,换一把可继续用)".into());
    }
    let def = providers::find(&id).ok_or_else(|| format!("未知渠道: {id}"))?;
    if let providers::AuthKind::App(app) = def.auth {
        return Err(format!("{} 没有本应用保存的密钥(用的是 {} 的登录态)", def.name, app.label()));
    }
    // 网页登录型的会话住在浏览器 profile 里,不在凭据管理器 —— 一并清掉。
    // 不清的话这个按钮点了等于没点:下一轮取数照样读得到 Cookie(2026-10-08)。
    // 清理会等待 WebView 的异步响应，放到阻塞线程，给运行时留出处理其它请求的线程。
    let is_web = matches!(def.auth, providers::AuthKind::WebSession);
    let mut warn = String::new();
    if is_web {
        // 各渠道的会话住在各自域的 Cookie 罐里:WorkBuddy 清工作台两域,豆包清
        // doubao.com —— 用错清单会把别人的会话误伤或漏清
        let handle = app.clone();
        let cleared = tokio::task::spawn_blocking(move || match def.id {
            WB_CHANNEL_ID => wb_clear_session(&handle),
            "doubao" => doubao_web::clear_session(&handle),
            _ => Ok(()),
        })
        .await
        .map_err(|error| error.to_string())?;
        if let Err(e) = cleared {
            warn = format!(";浏览器登录会话可能没清干净({e}),请在客户端里退出登录一次");
        }
    }
    purge_channel_credentials(&app, &id).await?;
    // 凭据刚变过:作废"5 秒内复用上次结果",否则紧接着的刷新还会把旧结果(带旧凭据
    // 取到的那份)端上来,用户看到"清掉了却还显示已登录"。
    let state = app.state::<AppState>();
    state.fetcher.forget_channel(&lock_rw(&state.store), &id)?;
    sync_channel_to_gateway(&app, &id);
    Ok(if is_web {
        format!("已清除 {} 的登录会话{warn}", def.name)
    } else {
        format!("已删除 {} 的密钥{warn}", def.name)
    })
}

/// 把一个渠道的**全部**密钥痕迹清干净:直接填的值 + 引用指针 + 反查索引行。
///
/// 2026-10-21 修 N01/N02:以前各处只删 `TokenScope/<id>`,而密钥箱打通之后
/// 一个渠道的密钥有**两个来源**,读取时**优先看引用** —— 于是"删除密钥"点了等于没点:
/// 弹窗说已删除,渠道照样取得到数。这不是文案问题,是凭据真的还在。
///
/// 三样必须一起清,少一样就留一条孤儿:
///   · `secrets::delete`     —— 直接填的值
///   · `secrets::link_clear` —— 引用指针(**读取时优先级最高的那个**)
///   · `vault_ref_clear`     —— SQLite 反查索引(不清的话密钥箱还显示"正被它使用")
pub(crate) async fn purge_channel_credentials(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let cid = id.to_string();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        secrets::delete(&cid)?;
        secrets::link_clear(&cid)
    })
    .await
    .map_err(|e| format!("删除密钥任务失败: {e}"))??;
    let store = app.state::<AppState>().store.clone();
    lock_rw(&store).vault_ref_clear(id)?;
    Ok(())
}

