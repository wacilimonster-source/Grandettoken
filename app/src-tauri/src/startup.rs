use super::*;

pub(crate) fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let dir = app.path().app_data_dir().map_err(|e| {
        let m = format!("无法定位数据目录: {e}");
        report_fatal("定位数据目录", &m)
    })?;
    let db_path = dir.join("tokenscope.db");
    let store = Store::open(&db_path).map_err(|e| {
        let m = format!(
            "打开数据文件失败: {e}(路径 {});若还有 Grandettoken.exe 在运行,先结束它",
            db_path.display()
        );
        report_fatal("打开数据库", &m)
    })?;
    // 配置坏了不能静默用默认值:那会把用户的隐藏渠道、快捷键、轮询间隔
    // 全部悄悄还原(报告 P2-14)。取默认值继续启动,但把原因留给前端提示。
    let (config, mut startup_error) = match Config::try_load(&store) {
        Ok(c) => (c, None),
        Err(e) => {
            // 用默认值继续启动是既定取舍(P2-14),但必须把后果说清、并且别再让
            // 一次随手保存把还能手工救回的那行 JSON 覆盖掉(报告 P1-16)。
            CONFIG_BROKEN.store(true, std::sync::atomic::Ordering::Relaxed);
            let msg = format!(
                "{e} —— 现已按默认设置运行:你之前隐藏的渠道恢复了取数。设置保存已暂停,以免覆盖还能修复的原始配置"
            );
            (Config::default(), Some(msg))
        }
    };

    // 同步组使用独立记录，避免签到、窗口和其他设置的旧快照覆盖它。
    // 配置损坏时不能用默认值迁移，保留原数据供恢复。
    if !CONFIG_BROKEN.load(std::sync::atomic::Ordering::Relaxed) {
        if let Err(e) = tokenscope_core::sync::profile::migrate(&store, &config) {
            startup_error = note(startup_error, format!("同步设置读取失败: {e}"));
        }
    }

    // 注册表与配置对齐:开机自启以配置为准,并刷新为当前 exe 路径,
    // 用户挪动 exe 后无需重新设置自启。构建目录里的开发版不写键,
    // 旧版残留的自启项要让用户看得见(光靠新版删会被旧版开机时写回来)。
    if let Some(msg) = autostart::sync(config.autostart) {
        startup_error = note(startup_error, msg);
    }

    // 置顶 + 上次位置是持久化设置:启动时按配置应用,折叠形态没有置顶按钮也生效
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_always_on_top(config.always_on_top);
        // 先恢复尺寸,再用**新尺寸**夹位置:否则宽窗口在屏幕边缘会被
        // 按旧尺寸裁进屏幕,恢复位置后右/下边又探出去。
        if config.form == "panel" {
            // 面板只有「单行窗 / 双行窗」两个固定尺寸,不再恢复用户拖动过的尺寸。
            let u = match config.font_scale.as_str() {
                "lg" => 1.15,
                "xl" => 1.3,
                _ => 1.0,
            };
            let (cw, ch) = if config.panel_layout == "double" {
                (860.0, 820.0)
            } else {
                (480.0, 820.0)
            };
            let _ = w.set_resizable(false);
            let _ = w.set_size(tauri::LogicalSize::new(
                cw * u + 20.0,
                ch * u + 20.0,
            ));
        }
        // 恢复窗口位置(报告 O-6)。多屏环境下副屏拔掉后旧坐标落在屏幕外,
        // 窗口"消失"又无从找回 —— 所以按当前可用显示器裁一遍。
        if let (Some(x), Some(y)) = (config.win_x, config.win_y) {
            if let Ok(size) = w.outer_size() {
                let (cx, cy) =
                    clamp_to_monitors(app.handle(), x, y, size.width as i32, size.height as i32);
                if cx != x || cy != y {
                    eprintln!("窗口位置 ({x},{y}) 不在任何显示器内,已修正为 ({cx},{cy})");
                }
                let _ = w.set_position(tauri::PhysicalPosition::new(cx, cy));
            }
        }
    }

    // 排查用日志:目录与 SQLite 同一个文件夹(用户找得着、也跟着数据一起备份/清理)。
    // **必须在 manage 与起轮询之前**:第一轮取数的失败链要能落进文件,否则
    // "装上就取不到数"那种场景又只剩一句界面文案(2026-10-20 用户报的那类)。
    tokenscope_core::diaglog::set_dir(dir.clone());
    // 「详细日志」要在横幅**之前**生效:否则用户上次开着详细日志,
    // 这一轮的启动事件就只记了"关着",横幅与实际对不上。
    tokenscope_core::diaglog::set_verbose(config.diag_verbose);
    tokenscope_core::diaglog::startup_banner(env!("CARGO_PKG_VERSION"));

    let store = Arc::new(Mutex::new(store));
    let read_store = match Store::open_readonly(&db_path) {
        Ok(reader) => Arc::new(Mutex::new(reader)),
        Err(e) => {
            startup_error = note(startup_error, format!("只读数据库连接不可用，查询将共用写连接：{e}"));
            store.clone()
        }
    };
    app.manage(AppState {
        store,
        read_store,
        config: Mutex::new(config.clone()),
        fetcher: Arc::new(Fetcher::new()),
        hotkey: Mutex::new(None),
        startup_error: Mutex::new(None),
    });
    app.manage(hibernate::Hibernate::default());

    match gateway::Gateway::new(app.state::<AppState>().store.clone()) {
        Ok(gw) => {
            app.manage(gw);
            let gateway_app = app.handle().clone();
            tauri::async_runtime::spawn(async move { gateway::autostart(gateway_app).await; });
        }
        // TLS 后端初始化失败这类罕见故障:不再 panic 硬退(报告 L16)——
        // 记进启动错误,余额监控与签到照常跑,只是本地网关这一项不可用。
        Err(e) => {
            let msg = format!("本地网关初始化失败:{e}(余额监控不受影响,仅本地 API 网关不可用)");
            eprintln!("{msg}");
            startup_error = note(startup_error, msg);
        }
    }
    // 后台一次性导入现有渠道密钥；不阻塞首屏，也不更改渠道引用。
    let import_app = app.handle().clone();
    let import_store = app.state::<AppState>().store.clone();
    let import_cfg = config.clone();
    tauri::async_runtime::spawn(async move {
        let worker = import_app.clone();
        if tokio::task::spawn_blocking(move || vault_sync::import_for_gateway(&worker, &import_store, &import_cfg)).await.is_err() {
            gateway::report_sync_error(&import_app, "导入渠道密钥任务失败".into());
        }
    });

    // 网页登录型渠道的会话来源:每轮取数时现从 webview 的 Cookie 罐里读。
    // 回调在取数路径的 spawn_blocking 里调用，避免主线程阻塞读 WebView。
    let session_app = app.handle().clone();
    app.state::<AppState>()
        .fetcher
        .set_web_credential_source(Box::new(move |id| web_session_of(&session_app, id)));

    let progress_app = app.handle().clone();
    google_cloud_web::start(app.handle().clone());
    app.state::<AppState>().fetcher.set_progress_sink(Arc::new(move |views| {
        emit_channels(&progress_app, &views);
    }));

    // 启动时注册已保存的全局快捷键;被占用不拦启动,但要把原因留给界面
    // (之前只 eprintln,绿色版从托盘启动时没人看得到 stderr —— 报告 P2-14)
    if !config.hotkey.is_empty() {
        let st = app.state::<AppState>();
        if let Err(e) = apply_hotkey(app.handle(), &st, &config.hotkey) {
            eprintln!("全局快捷键注册失败: {e}");
            startup_error = note(startup_error, e);
        }
    }
    if let Some(msg) = startup_error {
        *lock_rw(&app.state::<AppState>().startup_error) = Some(msg);
    }

    let refresh = MenuItem::with_id(app, "refresh", "刷新", true, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "显示面板", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &refresh, &sep, &quit])?;
    app.manage(tray::TrayState::default());

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Grandettoken")
        .menu(&menu)
        // 左键只切换显示/隐藏(下面的事件处理),菜单留给右键。
        // 不设这个开关时 Tauri 在 Windows 上左键也会弹菜单。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| match ev.id().as_ref() {
            // 正在写客户端配置时立刻退出会留下"改了没记账"的中间态,先等它写完(P1-15)
            "quit" => quit_when_idle(app),
            "show" => hibernate::ensure_and_show_main_window(app),
            "refresh" => {
                let handle = app.clone();
                tauri::async_runtime::spawn(async move { refresh_now(&handle).await });
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                hibernate::toggle_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    tauri::async_runtime::spawn(tray::run(app.handle().clone()));
    tauri::async_runtime::spawn(hibernate::run(app.handle().clone()));

    let handle = app.handle().clone();
    let store = app.state::<AppState>().store.clone();
    tauri::async_runtime::spawn(async move {
        prune_snapshots(store).await;
        poll_loop(handle).await;
    });
    // 签到节拍器:独立于取数轮询(签到是写操作,节奏要分开)。
    // **必须在原有 setup 里起** —— Tauri 的 Builder::setup 只有一个槽位,
    // 再加一个会把 manage(AppState) 那套顶掉,应用会在第一条命令就 panic。
    spawn_checkin_ticker(app.handle().clone());

    // 引用索引对账(设计稿 product-vault-ref-reconcile-2026-10-21):
    // 启动后台按真实指针重抄一遍索引 —— 无论上次是怎么走偏的,下次启动自己就好了。
    spawn_vault_ref_rebuild(app.handle().clone());

    // 窗口配置为 visible:false,这里统一控制显隐:
    // 正常启动立即显示;开机自启带 --hidden,直接进托盘等用户点开
    if std::env::args().any(|a| a == "--hidden") {
        hibernate::arm_hidden(app.handle());
    } else {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }

    Ok(())
}
