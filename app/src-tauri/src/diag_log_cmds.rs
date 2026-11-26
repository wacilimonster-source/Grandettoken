use super::*;

/// 排查日志的落点与大小(设置页要显示"日志在哪儿、多大")。
/// 只报路径,不读内容 —— 打开/复制是另外两条命令,便于前端按需触发。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagLogInfo {
    pub path: String,
    pub dir: String,
    pub size: u64,
    /// 静默关闭了吗(`TOKENSCOPE_NODIAG=1`)。关着时界面上要如实说明,
    /// 否则用户以为"有日志",报问题时却什么都没有。
    pub enabled: bool,
    /// 「详细日志」现在开着吗(成功的请求也记)。界面据此回填开关。
    pub verbose: bool,
    /// 本次运行的会话 id(8 位十六进制)。用户报问题时说出它,就能定位那一次运行。
    pub session: String,
}

#[tauri::command]
pub(crate) async fn diag_log_info() -> Result<DiagLogInfo, String> {
    let dir = tokenscope_core::diaglog::log_dir().ok_or("拿不到日志目录")?;
    let path = tokenscope_core::diaglog::log_path().unwrap_or_else(|| dir.join(tokenscope_core::diaglog::LOG_FILE));
    Ok(DiagLogInfo {
        path: path.display().to_string(),
        dir: dir.display().to_string(),
        size: tokenscope_core::diaglog::size_bytes(),
        enabled: !tokenscope_core::diaglog::is_disabled(),
        verbose: tokenscope_core::diaglog::is_verbose(),
        session: tokenscope_core::diaglog::session_id(),
    })
}

/// 读日志尾巴(设置页预览 + 用户点「复制」时用)。只读文本,不含任何密钥
/// (写入侧已经做过 [`tokenscope_core::diaglog::redact`])。
#[tauri::command]
pub(crate) async fn diag_log_tail(lines: Option<usize>) -> Result<String, String> {
    let n = lines.unwrap_or(400).clamp(1, 5000);
    let got = tokenscope_core::diaglog::tail_lines(n);
    if got.is_empty() {
        return Ok("(日志还是空的 —— 等一轮取数之后再看,或者点一次「立即刷新」)".to_string());
    }
    Ok(got.join("\n"))
}

/// 在资源管理器里打开日志目录。**与 `open_path` 分开**:那条只允许打开客户端配置
/// 目录(白名单很窄),而这里只有"我们自己的日志目录"一个落点,不需要也不该复用。
#[tauri::command]
pub(crate) async fn open_log_dir(app: tauri::AppHandle) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        use tauri_plugin_opener::OpenerExt;
        let dir = tokenscope_core::diaglog::log_dir().ok_or("拿不到日志目录".to_string())?;
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| format!("建日志目录失败: {e}"))?;
        }
        app.opener()
            .open_path(dir.display().to_string(), None::<&str>)
            .map_err(|e| format!("打开失败: {e}"))
    })
    .await
    .map_err(|e| format!("打开失败: {e}"))?
}

/// 清空日志(用户报完问题、想重新观察一段时用)。
#[tauri::command]
pub(crate) async fn diag_log_clear() -> Result<String, String> {
    tokenscope_core::diaglog::clear()?;
    // 清完立刻记一行:让"新的一段"从明确的起点开始,不至于看起来像日志坏了
    tokenscope_core::diaglog::write("日志", "已清空(用户手动)");
    Ok("日志已清空".to_string())
}

/// 开关「详细日志」。
///
/// 默认关。打开后**成功的**请求也落盘 —— 排查"余额数字不对"这类问题必须看
/// 成功响应,那是唯一能看到"接口到底返了什么"的地方。绝大多数问题只需要失败
/// 现场(默认档就够),所以这个开关默认关,免得 99% 的人被日志量干扰。
///
/// 状态存进设置(重启后还在),但**读设置之前先用内存里的值** ——
/// 开关是"临时的调试手段",不该在配置还没加载出来时被打回默认。
#[tauri::command]
pub(crate) async fn diag_log_verbose(app: tauri::AppHandle, on: bool) -> Result<bool, String> {
    tokenscope_core::diaglog::set_verbose(on);
    tokenscope_core::diaglog::write(
        "日志",
        &format!("详细日志已{}", if on { "打开" } else { "关闭" }),
    );
    // 存进设置:下次启动还是这个状态
    let st = app.state::<AppState>();
    if let Err(e) = save_config_off_main(&st, move |c| c.diag_verbose = on).await {
        eprintln!("详细日志开关落库失败: {e}");
    }
    Ok(on)
}

/// 导出诊断包:环境 + 体检 + 事件日志 + 失败现场,合成**一份**可复制的文本。
///
/// 为什么合成一份:以前用户要贴两样东西(体检报告 + 日志),support 还要人工对。
/// 合成一份之后,用户只贴一样,support 也不用追问第二轮。
///
/// 纯文本而不是 zip:目标是"贴进聊天窗口",一段文本比一个压缩包好使。
#[tauri::command]
pub(crate) async fn diag_export(app: tauri::AppHandle) -> Result<String, String> {
    // 体检那一半要真打请求(最多十几秒)。`run_diagnostic` 本身是 async 的,
    // 里面的取数也是 await 的,直接等它就行 —— 真正占 CPU 的是 HTTP,不是这里。
    //
    // ⚠ 体检失败**不能带崩整个导出**:网络坏了正是用户最需要日志的时候,
    // 那时体检必然失败(要真打请求)—— 要是因此什么都拿不到,这个功能就废了。
    let report = match run_diagnostic(app.clone()).await {
        Ok(r) => r,
        Err(e) => format!("(体检没能跑完:{e})—— 下面是事件日志,先看这段"),
    };
    // 体检报告里含渠道名、地址、错误文案(可能带 URL),**也要过一遍脱敏** ——
    // 页脚写着"已脱敏",就得真的脱敏,不能只在日志那一段做。
    let report = tokenscope_core::diaglog::redact(&report);

    let session = tokenscope_core::diaglog::session_id();
    let version = env!("CARGO_PKG_VERSION");
    let now = tokenscope_core::fetch::local_datetime_string();
    let mut s = String::new();
    let bar = "=".repeat(60);
    s.push_str(&bar);
    s.push_str(&format!(
        "\n Grandettoken 诊断包 · {now} · v{version}\n 会话 ID: {session}\n"
    ));
    s.push_str(&bar);
    s.push_str("\n\n【1】环境\n");
    s.push_str(&diag_env_section());
    s.push_str("\n\n【2】体检(逐渠道真打一次)\n");
    s.push_str(&report);
    s.push_str("\n\n【3】事件日志(最近的记录)\n");
    let events = tokenscope_core::diaglog::tail_lines(400);
    if events.is_empty() {
        s.push_str("(日志还是空的 —— 等一轮取数之后再看)\n");
    } else {
        s.push_str(&events.join("\n"));
        s.push('\n');
    }
    s.push_str("\n【4】说明\n");
    s.push_str("本包已做脱敏:密钥、Cookie、Token 一律不落原文。\n");
    s.push_str("但路径、域名、渠道名仍是敏感信息 —— 请当作敏感信息处理。\n");
    Ok(s)
}

/// 诊断包里的环境段。
pub(crate) fn diag_env_section() -> String {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<取不到>".into());
    let dir = tokenscope_core::diaglog::log_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "<取不到>".into());
    let drives = tokenscope_core::workbuddy::drive_letters()
        .iter()
        .map(|d| format!("{d}:"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "  Grandettoken {} · exe={exe}\n  日志目录={dir}\n  本机盘符: {drives}",
        env!("CARGO_PKG_VERSION")
    )
}
