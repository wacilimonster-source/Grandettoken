use super::*;

/// 行详情里的「最近变动」台账:展开时才取,不占轮询、不进视图。
#[tauri::command]
pub(crate) async fn channel_changes(
    app: tauri::AppHandle,
    id: String,
    days: i64,
) -> Result<Vec<tokenscope_core::store::ChangeEntry>, String> {
    key_allowed(&app.state::<AppState>(), &id)?;
    let days = days.clamp(1, 90);
    let store = {
        let st = app.state::<AppState>();
        st.read_store.clone()
    };
    let id2 = id.clone();
    tokio::task::spawn_blocking(move || {
        let fp = fp_of_any(&id2);
        let fpo = if fp.is_empty() { None } else { Some(fp.as_str()) };
        let s = lock_rw(&store);
        Ok(s.recent_changes(&id2, fpo, now_ts() - days * 86_400, 12)
            .unwrap_or_default())
    })
    .await
    .map_err(|e| format!("取变动台账失败: {e}"))?
}

/// 开机自启:写 HKCU 的 Run 键,不需要管理员权限。
/// 产物是绿色单文件 exe,可能被用户移动位置,所以 setup 里每次启动都按
/// `current_exe()` 重写一遍注册表 —— 路径永远指向当前这份 exe。
#[cfg(windows)]
pub(crate) mod autostart {
    use std::path::{Path, PathBuf};
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "Grandettoken";
    /// 改名前的键名:停用/同步时一并清掉,避免残留一个指向已删除 exe 的自启项
    const LEGACY_NAME: &str = "TokenScope";

    /// 是不是从构建目录里跑的开发版(`.../target/release/xxx.exe`)。
    fn in_build_tree(exe: &Path) -> bool {
        let p = exe.display().to_string().to_lowercase();
        p.contains(r"\target\release\") || p.contains(r"\target\debug\")
    }

    /// 注册自启键。**开发版直接报错**:把用户的开机项写到 target 目录里,一次
    /// `cargo clean` 就让自启指向不存在的路径,而 Windows 只会静默失败 —— 用户看
    /// 到的现象是"面板某天开机没出来"(本机踩过:键值指到 target/release/tokenscope.exe)。
    pub fn set(enabled: bool) -> Result<(), String> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(RUN_KEY)
            .map_err(|e| e.to_string())?;
        if enabled {
            // 开着自启就清遗留键:绿色版(TokenScope)时代开过自启、后升级到安装版的
            // 机器,否则注册表残留一个指向已删除 exe 的自启项,每次开机报错(报告 B7)。
            // 放在写自己的键之前:下面开发版那条 return Err 也要保证遗留键被清掉。
            let _ = key.delete_value(LEGACY_NAME);
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            if in_build_tree(&exe) {
                // 只拒写自己这条 `Grandettoken` 键 —— 它可能正指向一份安装版,开发版无权删
                return Err(format!(
                    "当前运行的是构建目录里的开发版,不会写入开机自启(那样路径会被 cargo clean 删掉)。请用已安装的版本开启:{}",
                    exe.display()
                ));
            }
            // 路径可能含空格,必须加引号;--hidden 让开机自启直接进托盘不弹窗
            let cmd = format!("\"{}\" --hidden", exe.display());
            key.set_value(NAME, &cmd).map_err(|e| e.to_string())?;
        } else {
            // 关掉时键可能本就不存在,删除报错属正常,忽略
            let _ = key.delete_value(NAME);
            let _ = key.delete_value(LEGACY_NAME);
        }
        Ok(())
    }

    /// 旧版遗留的 TokenScope 自启项;它指向的文件**确实还在磁盘上**才算数
    /// (只剩一个死键的话清掉就行,不必打扰用户)。
    fn legacy_left() -> Option<String> {
        let hk = RegKey::predef(HKEY_CURRENT_USER).open_subkey(RUN_KEY).ok()?;
        let val: String = hk.get_value(LEGACY_NAME).ok()?;
        // 顺序要紧:旧版写进去的值是 `"<exe 路径>" --hidden`(与本文件 set() 同格式)。
        // 以前先 trim 引号再 split,尾引号没被去掉 → exists() 为假 → 这条提醒永不出现。
        let raw = val.split(" --").next().unwrap_or("").trim().trim_matches('"').trim();
        let path = PathBuf::from(raw);
        if path.as_os_str().is_empty() || !path.exists() {
            return None;
        }
        Some(format!(
            "这台电脑上还有一份旧版({})。它每次开机都会自己把自启项写回来,新版本删不掉,所以开机会同时出现两个程序 —— 请卸载或改名那个旧目录。",
            path.display()
        ))
    }

    /// 开机时与注册表对齐(容忍失败),并把需要告知用户的问题返回出去。
    pub fn sync(enabled: bool) -> Option<String> {
        // 顺序要紧:legacy_left() 要在 set() 删掉那条键之前读
        let notice = legacy_left();
        let failed = match set(enabled) {
            Ok(()) => None,
            // 开发版跳过写入是一种"正当失败",其它注册表错误同样值得让用户看到
            Err(e) => Some(if in_self_build_tree() {
                e
            } else {
                format!("开机自启同步失败:{e}")
            }),
        };
        match (notice, failed) {
            (Some(a), Some(b)) => Some(format!("{a} 另外 {b}")),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// 开机自启项在不在、指向哪个 exe(体检只读,不改注册表)。
    /// 返回 Some(路径) = 键存在;None = 没有这条键。
    pub fn entry_present() -> Option<String> {
        let hk = RegKey::predef(HKEY_CURRENT_USER).open_subkey(RUN_KEY).ok()?;
        hk.get_value::<String, _>(NAME).ok()
    }

    fn in_self_build_tree() -> bool {
        std::env::current_exe().map(|p| in_build_tree(&p)).unwrap_or(false)
    }
}

#[cfg(not(windows))]
pub(crate) mod autostart {
    pub fn set(_enabled: bool) -> Result<(), String> {
        Err("开机自启仅支持 Windows".into())
    }
    pub fn sync(_enabled: bool) -> Option<String> {
        None
    }
    /// 非 Windows 没有注册表自启项(体检如实报"不适用")。
    pub fn entry_present() -> Option<String> {
        None
    }
}

#[tauri::command]
pub(crate) async fn set_autostart(enabled: bool) -> Result<(), String> {
    // HKCU 注册表读写:同步命令会占住主线程(报告 P1-14)
    tokio::task::spawn_blocking(move || autostart::set(enabled))
        .await
        .map_err(|e| format!("设置开机自启失败: {e}"))?
}

/// 查托盘所在显示器的实际小图标尺寸,而不是用主窗口所在显示器的 DPI。
/// 托盘隐藏在溢出区、Explorer 尚未就绪时,退回主显示器的缩放比例。
/// 这是异步命令:tray.rect() 要在主线程执行,不能在主线程等待自己。
pub(crate) fn tray_icon_size(app: &tauri::AppHandle) -> u32 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::{
            HiDpi::GetSystemMetricsForDpi,
            WindowsAndMessaging::{SM_CXSMICON, SM_CYSMICON},
        };

        let monitor = app
            .tray_by_id("main")
            .and_then(|tray| tray.rect().ok().flatten())
            .and_then(|rect| {
                // Windows 返回的是物理坐标,取中心点可避免图标恰好位于屏幕边缘。
                let pos = rect.position.to_physical::<i32>(1.0);
                let size = rect.size.to_physical::<u32>(1.0);
                app.monitor_from_point(
                    f64::from(pos.x) + f64::from(size.width) / 2.0,
                    f64::from(pos.y) + f64::from(size.height) / 2.0,
                )
                .ok()
                .flatten()
            })
            .or_else(|| app.primary_monitor().ok().flatten());
        let scale = monitor.map(|m| m.scale_factor()).unwrap_or(1.0);
        let dpi = (96.0 * scale).round().clamp(48.0, 384.0) as u32;
        // 系统托盘用小图标尺寸;若查询失败,按 16 个逻辑像素退回。
        let size = unsafe {
            GetSystemMetricsForDpi(SM_CXSMICON, dpi)
                .min(GetSystemMetricsForDpi(SM_CYSMICON, dpi))
        };
        if (16..=64).contains(&size) {
            size as u32
        } else {
            (16.0 * scale).round().clamp(16.0, 64.0) as u32
        }
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        32
    }
}

/// 托盘图像指纹:同尺寸、逐像素相同 → 同值。
pub(crate) fn tray_icon_digest(size: u32, rgba: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    size.hash(&mut h);
    rgba.hash(&mut h);
    h.finish()
}

#[tauri::command]
pub(crate) fn window_cmd(app: tauri::AppHandle, action: String, presentation: Option<hibernate::Presentation>) -> Result<(), String> {
    let window = || app.get_webview_window("main").ok_or("窗口不存在");
    match action.as_str() {
        "hide" => hibernate::hide_main_window(&app, &window()?).map_err(|e| e.to_string())?,
        "minimize" => window()?.minimize().map_err(|e| e.to_string())?,
        "presentation" => hibernate::presentation_of(&app, presentation.ok_or("缺少窗口形态")?),
        "quit" => {
            // 正在写客户端配置时立刻退出,进程会停在"已落盘、还没记账"的瞬间,
            // 重启后没有任何台账能回滚它(P1-15 那条窗口期)。走同一个等待逻辑。
            quit_when_idle(&app);
        }
        _ => return Err(format!("未知操作: {action}")),
    }
    Ok(())
}

/// 置顶的唯一写入口:窗口状态与配置一起改,不会出现"按钮亮了其实没置顶"。
/// 折叠形态(紧凑条/胶囊/贴边)没有置顶按钮,靠设置页里的同一个开关控制。
#[tauri::command]
pub(crate) async fn set_pin(app: tauri::AppHandle, state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    refuse_if_config_broken()?;
    let win = app.get_webview_window("main").ok_or("窗口不存在")?;
    // 先落库再动窗口:反过来会出现本函数注释声称"不可能"的状态 ——
    // 窗口实际置顶了而按钮不亮、下次启动也不置顶(报告 P3-27)。
    // 走 save_config_off_main:同步命令是在主线程上跑的,而这里要写 SQLite 并等 store 锁,
    // 90 天清理正持着那把锁时会整窗卡住(P1-14 的同一类问题)。
    save_config_off_main(&state, move |c| c.always_on_top = enabled).await?;
    if let Err(e) = win.set_always_on_top(enabled) {
        // 窗口没改成就把刚写的配置撤回,别让界面状态与真实置顶态背离
        let _ = save_config_off_main(&state, move |c| c.always_on_top = !enabled).await;
        return Err(e.to_string());
    }
    Ok(())
}

/// 记住窗口位置(报告 O-6)。只在自由态下记 —— 吸附/紧凑条/胶囊的坐标是布局
/// 算出来的,记下来反而会在下次启动覆盖正确的默认位置。
/// 前端在拖动结束后调用一次,不做高频写盘。
#[tauri::command]
pub(crate) async fn save_window_pos(state: State<'_, AppState>, x: i32, y: i32) -> Result<(), String> {
    refuse_if_config_broken()?;
    save_config_off_main(&state, move |c| {
        c.win_x = Some(x);
        c.win_y = Some(y);
    })
    .await
}

/// 把请求的窗口坐标夹进"至少有一条边露在外面"的范围。
/// 多屏环境下用户拔掉副屏后,记下的坐标会落在屏幕外,窗口看起来像消失了。
/// 至少要露出 KEEP_X × KEEP_Y 的可视区,保证还能拖回来。
pub(crate) fn clamp_to_monitors(app: &tauri::AppHandle, x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    const KEEP_X: i32 = 80;
    const KEEP_Y: i32 = 40;
    let Ok(monitors) = app.available_monitors() else {
        return (x, y);
    };
    if monitors.is_empty() {
        return (x, y);
    }
    // 完全落在某个显示器内 → 原样返回
    for m in &monitors {
        let (mx, my) = (m.position().x, m.position().y);
        let (mw, mh) = (m.size().width as i32, m.size().height as i32);
        if x >= mx && y >= my && x + w <= mx + mw && y + h <= my + mh {
            return (x, y);
        }
    }
    // 否则挑第一个显示器夹:左上角不越过 (mx+mw-KEEP) 边界
    let m = &monitors[0];
    let (mx, my) = (m.position().x, m.position().y);
    let (mw, mh) = (m.size().width as i32, m.size().height as i32);
    let cx = x.clamp(mx, (mx + mw - KEEP_X).max(mx));
    let cy = y.clamp(my, (my + mh - KEEP_Y).max(my));
    (cx, cy)
}

