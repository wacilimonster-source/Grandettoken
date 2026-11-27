use super::*;

/// 复制模型信息的文本字段。密钥复制由 key_copy 处理。
#[tauri::command]
pub(crate) async fn copy_text(text: String) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("没有可复制的内容".into());
    }
    tokio::task::spawn_blocking(move || copy_to_clipboard(&text))
        .await
        .map_err(|e| format!("复制失败: {e}"))?
}

/// 渠道密钥的明文(用户点「显示」时一次性返回)。**这是产品里唯一把密钥回前端的路径**,
/// 所以只有用户显式点击才会走到这里;界面侧 8 秒自动收回,不留驻。密钥型渠道以外没有可取的。
/// 渠道的显示名:内置渠道查静态表,自定义渠道查配置(报告 B3)。
/// 错误信息与「已复制」回执都要用它 —— key_reveal 放行自定义渠道后,
/// 不能再用 `providers::find(&id).expect("上面校验过")`(对 custom-N 会 panic)。
pub(crate) fn channel_display_name(state: &AppState, id: &str) -> String {
    if let Some(def) = providers::find(id) {
        return def.name.to_string();
    }
    lock_rw(&state.config)
        .custom_channels
        .iter()
        .find(|x| x.id == id)
        .map(|x| x.name.clone())
        .unwrap_or_else(|| id.to_string())
}

#[tauri::command]
pub(crate) async fn key_reveal(app: tauri::AppHandle, id: String) -> Result<String, String> {
    let state = app.state::<AppState>();
    // 放行口径与 set_key / delete_key 相同(报告 B3):自定义渠道的密钥账户名也是
    // 渠道 id,以前只认静态表,卡片上的「复制 / 显示」对自定义渠道必报"未知渠道"。
    key_allowed(&state, &id)?;
    if let Some(def) = providers::find(&id) {
        if !matches!(def.auth, providers::AuthKind::Keyring) {
            return Err(format!("{} 没有可复制的密钥(用的是本机登录态或网页会话)", def.name));
        }
    }
    let name = channel_display_name(&state, &id);
    // 凭据管理器读是 RPC:与 key_hint/set_key 同一条纪律,不下沉就会占住一个 tokio worker
    let rid = id.clone();
    match tokio::task::spawn_blocking(move || secrets::channel_key(&rid))
        .await
        .map_err(|e| format!("读取凭据失败: {e}"))?
    {
        Ok(Some(k)) if !k.trim().is_empty() => Ok(k),
        Ok(_) => Err(format!("{name} 还没有保存密钥")),
        Err(e) => Err(e),
    }
}

/// 复制渠道密钥:明文**不回前端**,后端读出来直接进剪贴板(与「复制三件套」同一条路)。
#[tauri::command]
pub(crate) async fn key_copy(app: tauri::AppHandle, id: String) -> Result<String, String> {
    let state = app.state::<AppState>();
    let name = channel_display_name(&state, &id);
    let key = key_reveal(app, id).await?;
    tokio::task::spawn_blocking(move || copy_to_clipboard(&key).map(|_| format!("已复制 {name} 的密钥")))
        .await
        .map_err(|e| format!("复制失败: {e}"))?
}

/// 用系统 `clip.exe` 写剪贴板(经 stdin,密钥不进命令行)。
/// 输入按 UTF-16LE:clip 按控制台代码页解释字节流,中文标签走 UTF-8 会被切坏。
pub(crate) fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    let system = std::env::var_os("SystemRoot").ok_or("无法定位系统剪贴板工具")?;
    let mut cmd = std::process::Command::new(std::path::PathBuf::from(system).join("System32").join("clip.exe"));
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // GUI 程序默认会给子进程配一个新的控制台窗口 —— clip.exe 也会闪一下黑框
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("调用 clip.exe 失败: {e}"))?;
    {
        let mut si = child.stdin.take().ok_or("拿不到 clip 的标准输入")?;
        let utf16: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        si.write_all(&utf16)
            .map_err(|e| format!("写入剪贴板失败: {e}"))?;
    }
    let st = child
        .wait()
        .map_err(|e| format!("clip.exe 未正常结束: {e}"))?;
    if !st.success() {
        return Err("clip.exe 返回失败".into());
    }
    Ok(())
}

/// 在资源管理器里打开一个目录(备份目录 / 客户端配置目录)。
/// 界面上的路径是 `~/…` 缩写(见 `inject::pretty`),这里先展开成真实路径再打开 ——
/// 否则资源管理器会去找一个叫 `~` 的目录。
#[tauri::command]
pub(crate) async fn open_path(app: tauri::AppHandle, path: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        use tauri_plugin_opener::OpenerExt;
        let raw = path.trim();
        let expanded = match raw.strip_prefix('~') {
            Some(rest) => {
                let home = std::env::var("USERPROFILE")
                    .or_else(|_| std::env::var("HOME"))
                    .map_err(|_| "拿不到用户目录,无法展开 ~".to_string())?;
                format!("{}{}", home, rest.replace('/', "\\"))
            }
            None => raw.to_string(),
        };
        let target = std::path::PathBuf::from(&expanded);
        if !target.exists() {
            return Err(format!("路径不存在: {raw}"));
        }
        // 词法上的 `..` 一律拒绝:starts_with 是按组件比、**不解析父目录**的,
        // 于是 `<site.root>\..\..\..\..\Windows\System32\calc.exe` 也能过白名单 ——
        // 这条命令就退化成"把任意 exe 交给资源管理器"的原语(P3-24 要挡的正是这个)。
        if target
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("路径里不允许出现 ..".to_string());
        }
        // 只允许打开挂件认识的客户端配置目录(含各自的 backup-inject/):
        // 两侧都归一成真实路径再比,短名 / 大小写 / 符号链接都绕不过去。
        let canon = std::fs::canonicalize(&target).map_err(|e| format!("路径无效: {e}"))?;
        let allowed = inject::sites().iter().any(|site| {
            std::fs::canonicalize(&site.root)
                .map(|r| canon.starts_with(&r))
                .unwrap_or(false)
        });
        if !allowed {
            return Err("只能打开客户端自己的配置目录与备份目录".to_string());
        }
        app.opener()
            .open_path(expanded, None::<&str>)
            .map_err(|e| format!("打开失败: {e}"))
    })
    .await
    .map_err(|e| format!("打开失败: {e}"))?
}

/// 只允许打开 GitHub 官方设备授权页。同步授权流程之外的 URL 一律拒绝。
#[tauri::command]
pub(crate) async fn open_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let allowed = [
        "https://github.com/login/device",
        "https://github.com/settings/developers",
    ];
    if !allowed.contains(&url.as_str()) {
        return Err("不允许打开这个地址".into());
    }
    tokio::task::spawn_blocking(move || {
        use tauri_plugin_opener::OpenerExt;
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|e| format!("打开失败: {e}"))
    })
    .await
    .map_err(|e| format!("打开失败: {e}"))?
}
