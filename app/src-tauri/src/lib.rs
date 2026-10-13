//! TokenScope 的 Tauri 外壳:窗口、托盘、命令、轮询调度。
//!
//! 取数逻辑全在 tokenscope-core,这里只做胶水。

use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, State};
use tokenscope_core::config::Config;
use tokenscope_core::fetch::{now_ts, ChannelView, Fetcher};
use tokenscope_core::{appauth, checkin, inject, models, providers, secrets, store::Store};

pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub config: Mutex<Config>,
    pub fetcher: Arc<Fetcher>,
    /// 当前已注册的全局快捷键(空 = 未注册)。换键时先注册新的、成功后再撤旧的。
    pub hotkey: Mutex<Option<String>>,
    /// 启动期的非致命错误(快捷键被占用、配置损坏回落默认…)。之前只 eprintln,
    /// 绿色版从托盘启动时没人看得到 stderr(报告 P2-14)。前端取走即清空,只提示一次。
    pub startup_error: Mutex<Option<String>>,
}

#[tauri::command]
async fn get_channels(state: State<'_, AppState>) -> Result<Vec<ChannelView>, String> {
    // 先克隆 Arc 再 await:不让 State 的借用跨越 await 点
    let store = state.store.clone();
    let fetcher = state.fetcher.clone();
    // fetch_shared:与轮询/托盘刷新共用闸门,手动刷新不再叠加出一份并发请求(报告 O-4)
    Ok(fetcher.fetch_shared(&store).await)
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Config {
    lock_rw(&state.config).clone()
}

/// 按配置重注册全局快捷键(空串 = 只注销)。先注册新键、成功后才撤旧键:
/// 新键被其它应用占用时保持现状,不会出现「旧键没了、新键也没有」。
/// 行为回调挂在插件 Builder 的 with_handler 上(见 run())。
fn apply_hotkey(app: &tauri::AppHandle, state: &AppState, want: &str) -> Result<(), String> {
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
        .map_err(|e| format!("注册 {want} 失败(可能已被其它程序占用): {e}"))?;
    if let Some(old) = cur.take() {
        let _ = manager.unregister(old.as_str());
    }
    *cur = Some(want);
    Ok(())
}

#[tauri::command]
fn set_config(app: tauri::AppHandle, state: State<'_, AppState>, config: Config) -> Result<(), String> {
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
    }

    // 先应用快捷键:注册失败(如被占用)就让整次保存失败,前端好回滚旧值
    let old_hotkey = lock_rw(&state.hotkey).clone();
    if let Err(e) = apply_hotkey(&app, &state, &incoming.hotkey) {
        return Err(e);
    }
    let store = lock_rw(&state.store);
    if let Err(e) = incoming.save(&store) {
        // 落库失败:把快捷键回滚,否则界面显示"保存失败"而快捷键却已经换了
        if let Some(old) = old_hotkey {
            let _ = apply_hotkey(&app, &state, &old);
        }
        return Err(e);
    }
    drop(store);
    *lock_rw(&state.config) = incoming;
    Ok(())
}

/// 只回显尾 4 位,用于确认每个渠道存的到底是哪把 key(存错 key 却以为是接口坏了,
/// 是最难排查的一类问题)。不回显完整密钥。复用本机应用登录态的渠道没有密钥可回显;
/// 网页登录型(WorkBuddy)存的就是令牌本身,同样值得回显尾 4 位。
#[tauri::command]
fn key_hint(id: String) -> Option<String> {
    if !matches!(
        providers::find(&id).map(|d| d.auth),
        Some(providers::AuthKind::Keyring | providers::AuthKind::WebSession)
    ) {
        return None;
    }
    secrets::masked(&id)
}

#[tauri::command]
fn set_key(app: tauri::AppHandle, id: String, key: String) -> Result<(), String> {
    let def = providers::find(&id).ok_or_else(|| format!("未知渠道: {id}"))?;
    if let providers::AuthKind::App(app) = def.auth {
        return Err(format!(
            "{} 不需要填密钥:凭据直接读本机已登录的 {} 客户端",
            def.name,
            app.label()
        ));
    }
    secrets::set(&id, &key)?;
    // 与删除同理:刚换过凭据就别再复用 5 秒内的旧结果,否则"保存了却还显示未配置/旧数值"
    app.state::<AppState>().fetcher.invalidate();
    Ok(())
}

#[tauri::command]
async fn delete_key(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let def = providers::find(&id).ok_or_else(|| format!("未知渠道: {id}"))?;
    if let providers::AuthKind::App(app) = def.auth {
        return Err(format!("{} 没有本应用保存的密钥(用的是 {} 的登录态)", def.name, app.label()));
    }
    // 网页登录型的会话住在浏览器 profile 里,不在凭据管理器 —— 一并清掉。
    // 不清的话这个按钮点了等于没点:下一轮取数照样读得到 Cookie(2026-10-08)。
    // 这条命令因此必须是 async:清 Cookie 要同步问 webview,不能占着主线程等。
    if matches!(def.auth, providers::AuthKind::WebSession) {
        wb_clear_session(&app);
    }
    secrets::delete(&id)?;
    // 凭据刚变过:作废"5 秒内复用上次结果",否则紧接着的刷新还会把旧结果(带旧凭据
    // 取到的那份)端上来,用户看到"清掉了却还显示已登录"。
    app.state::<AppState>().fetcher.invalidate();
    Ok(())
}

// ───────────── WorkBuddy 网页登录 ─────────────
//
// WorkBuddy 桌面端 5.6.2 起把本机凭据文件里的 accessToken 写成 AES-GCM 信封
// (实测 {"$wbEncrypted":1,"envelope":…},静态钥编译期内置、本机不可解 —— 2026-10-02 取证),
// 以前那套"只读复用客户端登录态"彻底读不到了,改走挂件内网页登录。
//
// **真正要抄的不是令牌,是会话 Cookie**(2026-10-08 真机取证,第一版去抓 Bearer 白做了):
// 控制台自己打 /billing/meter/* 时既没有 Authorization 头、sessionStorage 里也没有
// growth-center-token,认证全在 httpOnly 的 session / session_2 两个 Cookie 上;
// 而网关(openresty)还按 User-Agent 分流 —— 同一批 Cookie,浏览器 UA 200、客户端 UA 401。
// 所以这里的流程是:
//   1) 登录窗停在「个人中心 / 控制台」= 服务端确实认了这份会话(没认会被挡回 /login/);
//   2) 从 webview 的 Cookie 罐里现读(httpOnly 也读得到;登录窗与主窗口共用同一个 profile);
//   3) **先用挂件自己的 HTTP 栈真打一次接口**,通了才关窗报「已登录」(见 Fetcher::probe);
//   4) 什么都不落盘:会话留在浏览器 profile 里,取数时现读,平台轮换 Cookie 也能跟上
//      (Windows 凭据管理器也放不下 —— 实测 blob 上限约 1280 字符,两个 Cookie 近 6 KB)。
// 判定规则(哪些 Cookie 算会话、落在什么路径才算登录完成)都在 core::weblogin,那边有单测。

/// 登录窗 label:同一时刻只允许一个
const WB_LOGIN_LABEL: &str = "wb-login";
/// 只有这条路走网页登录;手动粘贴的密钥仍存这个条目(Windows 凭据管理器)
const WB_CHANNEL_ID: &str = "workbuddy";
/// 登录页。redirect_uri 就是登录完成后回落的用量页 —— 也是判"登录完成"的路径之一。
const WB_LOGIN_URL: &str = "https://www.workbuddy.cn/login?platform=usercenter&state=0&redirect_uri=https%3A%2F%2Fwww.workbuddy.cn%2Fprofile%2Fplans-usage";
/// 读/删会话 Cookie 时用的地址(只要域名对得上,path 随便给一个能命中的)
const WB_SESSION_URL: &str = "https://www.workbuddy.cn/";

/// 成功抓到会话后置位,用于区分"用户自己关掉了窗口"和"我们主动关窗" ——
/// 前者要让界面把「登录中」状态收回去,后者不该再报一次取消。
static WB_LOGIN_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 登录窗已经销毁(用户自己关的,或者我们收尾时关的)。观察者靠它收工 ——
/// 有了这个标志,就不必去问那个可能正在销毁的窗口(问它会 panic,见 on_main_with_timeout)。
static WB_LOGIN_CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 页面是否已经落到"登录完成"的路径上(判定规则在 core::weblogin)。由 on_navigation
/// 在主线程上置位 —— 那里拿到 URL 不需要任何跨线程派发,比事后去问窗口安全得多。
static WB_LANDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 把一段"要碰 webview"的代码投到主线程去跑,不等结果(关闭窗口这类不需要回信)。
fn on_main(app: &tauri::AppHandle, f: impl FnOnce() + Send + 'static) {
    let _ = app.run_on_main_thread(f);
}

/// 把一段"要碰 webview"的代码投到主线程去跑,带**自己的**超时把结果等回来。
///
/// 为什么不能直接在外壳里调 `cookies_for_url`:它内部走的是**阻塞派发** ——
/// 从 worker 线程发消息给主线程,然后在 `rx.recv().unwrap()` 上等回信。目标 webview
/// 正在销毁、或者应用正在退出时,那条回信永远不会来,`unwrap` 直接 panic;而 release
/// 是 `panic = "abort"`,一次派发失败等于整个挂件原地消失(2026-10-08 踩到:刚关掉
/// 登录窗就去读 Cookie,日志里留下 RecvError panic)。
/// 这里改成"投递任务 + 自己带超时地等",拿不到就当作没读到,让这一轮取数退回"未登录",
/// 绝不让挂件崩。投递本身在主线程上是就地执行,不存在回信问题。
fn on_main_with_timeout<T: Send + 'static>(
    app: &tauri::AppHandle,
    f: impl FnOnce() -> T + Send + 'static,
    timeout: std::time::Duration,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })
    .ok()?;
    rx.recv_timeout(timeout).ok()
}

/// 从 webview 的 Cookie 罐里取会话。
///
/// **只读主窗口**:它从启动活到退出,而登录窗随时可能被用户关掉(读一个正在销毁的
/// 窗口会走到上面那个 panic 上去)。Cookie 是按 profile 存的,主窗口读到的和登录窗
/// 读到的是同一份 —— 2026-10-08 实测(httpOnly 的 session / session_2 都读得到)。
fn wb_session_of(app: &tauri::AppHandle, id: &str) -> Option<String> {
    if id != WB_CHANNEL_ID {
        return None; // 只有 WorkBuddy 走这条路
    }
    let handle = app.clone();
    on_main_with_timeout(
        app,
        move || {
            let w = handle.get_webview_window("main")?;
            let url: tauri::Url = WB_SESSION_URL.parse().ok()?;
            let jar = w.cookies_for_url(url).ok()?;
            tokenscope_core::weblogin::cookie_credential(jar.iter().map(|c| (c.name(), c.value())))
        },
        std::time::Duration::from_secs(3),
    )
    .flatten()
}

/// 清掉宿主浏览器里的 WorkBuddy 会话(「清除登录」用)。
/// 会话不在凭据库里,不清这里那个按钮点了等于没点:下一轮取数照样读得到 Cookie。
fn wb_clear_session(app: &tauri::AppHandle) {
    let handle = app.clone();
    let _ = on_main_with_timeout(
        app,
        move || {
            let Some(w) = handle.get_webview_window("main") else {
                return;
            };
            let Ok(url) = WB_SESSION_URL.parse::<tauri::Url>() else {
                return;
            };
            let Ok(jar) = w.cookies_for_url(url) else {
                return;
            };
            for c in jar {
                if tokenscope_core::weblogin::is_session_cookie(c.name()) {
                    let _ = w.delete_cookie(c);
                }
            }
        },
        std::time::Duration::from_secs(3),
    );
}

/// 会话可用吗:用挂件自己的 HTTP 栈真打一次接口(见 Fetcher::probe)。
/// 不验证就报成功的话,一份被平台拒掉的会话会变成"状态显示已登录、却一直取不到数"。
async fn wb_session_works(app: &tauri::AppHandle, session: &str) -> bool {
    let fetcher = app.state::<AppState>().fetcher.clone();
    match providers::find(WB_CHANNEL_ID) {
        Some(def) => fetcher.probe(def, session).await,
        None => false,
    }
}

/// 收尾:关窗 → 通知界面 → 立刻刷一轮。会话本身不用搬走 —— 它就在浏览器 profile 里,
/// 取数时现读(见文件顶部第 4 条)。
async fn wb_finish_login(app: &tauri::AppHandle) {
    // 先置位再关窗:Destroyed 监听靠这个标志区分"登录成功"与"用户自己关了"
    WB_LOGIN_DONE.store(true, std::sync::atomic::Ordering::Relaxed);
    let handle = app.clone();
    on_main(app, move || {
        if let Some(w) = handle.get_webview_window(WB_LOGIN_LABEL) {
            let _ = w.close();
        }
    });
    let _ = app.emit("workbuddy-login", serde_json::json!({"ok": true}));
    app.state::<AppState>().fetcher.invalidate(); // 作废"5 秒内复用上次结果",否则这次刷新拿的还是旧结果
    refresh_now(app).await;
}

/// 登录窗开着的时候盯着它,直到会话可用或者窗口被用户关掉。
///
/// 为什么轮询而不是拦导航:控制台是 SPA,登录落地那一跳可能走 pushState,不触发导航
/// 事件;1.2 秒一次的开销只是读一次 Cookie,而且**只有**会话内容变了、或者页面落到了
/// 「个人中心 / 控制台」,才发真请求去验一次。
///
/// 判"能不能收工"靠**验证**,不靠"cookie 罐里有没有东西":没登录的访客也可能有
/// session Cookie。反过来,已经登录过的人再点一次「登录」,页面会停在「选择账号」上
/// 等人点 —— 那时会话其实已经可用,验证一过就直接收工,不让用户对着一个"没反应"的
/// 窗口发呆(2026-10-08 真机踩到)。
///
/// 两条状态都由主线程侧的钩子置位(窗口事件、导航事件),**不去碰登录窗的任何 API**:
/// 那类调用是阻塞派发,窗口正在销毁时会让挂件 panic-abort(见 on_main_with_timeout)。
fn spawn_login_watcher(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tried = String::new();
        let mut probes = 0u32;
        let mut ticks = 0u32;
        loop {
            if WB_LOGIN_CLOSED.load(std::sync::atomic::Ordering::Relaxed) {
                return; // 用户自己把窗口关了
            }
            let landed = WB_LANDED.load(std::sync::atomic::Ordering::Relaxed);
            // 读 Cookie 是阻塞调用(要同步问 webview),下沉到 spawn_blocking:
            // 别占着 tokio worker(与取数那边同一个理由,报告 O-5)。
            let probe_app = app.clone();
            let session = tokio::task::spawn_blocking(move || wb_session_of(&probe_app, WB_CHANNEL_ID))
                .await
                .ok()
                .flatten();
            if let Some(session) = session {
                let fresh = session != tried;
                if fresh {
                    tried = session.clone();
                    probes = 0;
                }
                // 新会话立刻验;同一份会话每 ~6 秒补验一次(用户可能刚点完「选择账号」,
                // Cookie 没变但账号上下文变了),最多 10 次,不对着平台一直打。
                if fresh || (probes < 10 && ticks % 5 == 0) {
                    probes += 1;
                    if wb_session_works(&app, &session).await {
                        wb_finish_login(&app).await;
                        return;
                    }
                    // 验不过:确实落到登录完成页、又刚换了会话(用户刚登录完)才说话;
                    // 还停在登录页说明人家正要登录,别抢着报错。
                    if landed && fresh {
                        let _ = app.emit(
                            "workbuddy-login",
                            serde_json::json!({
                                "ok": false,
                                "reason": "登录是成功了,但挂件用这份会话取数被平台拒绝。可改用访问密钥:在 www.workbuddy.cn/keys 建一份,贴进卡片的输入框。"
                            }),
                        );
                    }
                }
            }
            ticks += 1;
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        }
    });
}

/// 打开 WorkBuddy 登录窗。已经开着就把它拉到前台(不重复开第二个)。
///
/// **这条命令必须是 async**,别改成同步的:同步命令由 Tauri 在主线程上就地执行,
/// 而它是在 WebView2 的 WebMessageReceived 回调里被调用的 —— 在 COM 回调里再同步
/// 建一个 webview,CreateCoreWebView2Controller 的完成回调就再也送不到主线程上
/// (2026-10-08 实测:窗口壳出来了、页面停在 about:blank、之后连别的 IPC 一起冻死)。
/// async 命令跑在 async runtime 的线程上,窗口创建被当成一条消息投进事件循环,
/// 与启动时建主窗口走的是同一种上下文 —— 那条路是通的。
#[tauri::command]
async fn workbuddy_login(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(WB_LOGIN_LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    WB_LOGIN_DONE.store(false, std::sync::atomic::Ordering::Relaxed);
    WB_LANDED.store(false, std::sync::atomic::Ordering::Relaxed);
    WB_LOGIN_CLOSED.store(false, std::sync::atomic::Ordering::Relaxed);
    let url: tauri::Url = WB_LOGIN_URL
        .parse()
        .map_err(|e| format!("登录地址有问题: {e}"))?;
    // 不注入任何脚本、不拦任何导航:登录页是第三方的,挂件只负责看它走到哪儿。
    // on_navigation 只当信号用(记下"已经落到个人中心了"),一律放行。
    tauri::WebviewWindowBuilder::new(&app, WB_LOGIN_LABEL, tauri::WebviewUrl::External(url))
        .title("登录 WorkBuddy 账号（国内版）")
        .inner_size(560.0, 780.0)
        .min_inner_size(420.0, 560.0)
        .resizable(true)
        // 面板是置顶的,登录窗不置顶就会被压在下面,用户看不见
        .always_on_top(true)
        .on_navigation(|nav| {
            if tokenscope_core::weblogin::is_post_login(nav.as_str()) {
                WB_LANDED.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            true
        })
        .build()
        .map_err(|e| format!("登录窗口打不开: {e}"))?;
    spawn_login_watcher(app.clone());
    Ok(())
}

#[tauri::command]
fn get_series(state: State<'_, AppState>, id: String, hours: i64) -> Vec<f64> {
    let from = now_ts() - hours * 3600;
    // lock_rw:锁被毒化时也要出图,不能让取数线程的一次 panic 之后所有曲线都空掉
    let s = lock_rw(&state.store);
    s.series(&id, from, 24).unwrap_or_default()
}

/// 开机自启:写 HKCU 的 Run 键,不需要管理员权限。
/// 产物是绿色单文件 exe,可能被用户移动位置,所以 setup 里每次启动都按
/// `current_exe()` 重写一遍注册表 —— 路径永远指向当前这份 exe。
#[cfg(windows)]
mod autostart {
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
        let path = PathBuf::from(val.trim_matches('"').split(" --").next().unwrap_or(""));
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

    fn in_self_build_tree() -> bool {
        std::env::current_exe().map(|p| in_build_tree(&p)).unwrap_or(false)
    }
}

#[cfg(not(windows))]
mod autostart {
    pub fn set(_enabled: bool) -> Result<(), String> {
        Err("开机自启仅支持 Windows".into())
    }
    pub fn sync(_enabled: bool) -> Option<String> {
        None
    }
}

#[tauri::command]
fn set_autostart(enabled: bool) -> Result<(), String> {
    autostart::set(enabled)
}

/// 托盘角标。前端用 canvas 画好 32×32 的 RGBA(图标同款造型 + 状态色角标),
/// 这里换成托盘图标并把文字放进 tooltip —— 托盘图标只有 16px,数字放 tooltip,
/// 和胶囊共用同一套渠道与轮播逻辑。
///
/// 像素走 base64 而不是 `Vec<u8>`:Tauri 把命令参数序列化成 JSON,4096 字节的
/// RGBA 会变成两万多个字符的数组,每几秒轮播一次白耗流量(报告 O-9)。
/// 尺寸不符直接报错:图标画错时宁可保留上一帧,也不要把花屏贴到托盘上。
#[tauri::command]
fn set_tray_icon(
    app: tauri::AppHandle,
    rgba_b64: String,
    size: u32,
    tooltip: String,
) -> Result<(), String> {
    use base64::Engine as _;
    let tray = app.tray_by_id("main").ok_or("托盘不存在")?;
    let rgba = base64::engine::general_purpose::STANDARD
        .decode(rgba_b64.as_bytes())
        .map_err(|e| format!("托盘图标 base64 解码失败: {e}"))?;
    let want = size as usize * size as usize * 4;
    if size == 0 || rgba.len() != want {
        return Err(format!("托盘图标尺寸不符: {size}x{size} 需要 {want} 字节,收到 {}", rgba.len()));
    }
    let img = tauri::image::Image::new_owned(rgba, size, size);
    tray.set_icon(Some(img)).map_err(|e| e.to_string())?;
    tray.set_tooltip(Some(&tooltip)).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn window_cmd(app: tauri::AppHandle, action: String) -> Result<(), String> {
    let win = app.get_webview_window("main").ok_or("窗口不存在")?;
    match action.as_str() {
        "hide" => win.hide().map_err(|e| e.to_string())?,
        "minimize" => win.minimize().map_err(|e| e.to_string())?,
        "quit" => app.exit(0),
        _ => return Err(format!("未知操作: {action}")),
    }
    Ok(())
}

/// 全局锁序约定:**先 store 后 config**。所有同时拿两把锁的命令都必须按这个
/// 顺序,否则与 check_update / skip_update_version(store→config)交错时会 AB-BA 死锁。
/// 锁一律走 `lock_rw` 毒化恢复:一次 panic 不该把后续所有命令永久卡死。
fn lock_rw<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// 往"启动期非致命问题"这条串上再接一条。多条都要让用户看到,
/// 而不是后面那条把前面那条悄悄顶掉。
fn note(prev: Option<String>, msg: String) -> Option<String> {
    Some(match prev {
        Some(p) => format!("{p};另外 {msg}"),
        None => msg,
    })
}

/// 置顶的唯一写入口:窗口状态与配置一起改,不会出现"按钮亮了其实没置顶"。
/// 折叠形态(紧凑条/胶囊/贴边)没有置顶按钮,靠设置页里的同一个开关控制。
#[tauri::command]
fn set_pin(app: tauri::AppHandle, state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    let win = app.get_webview_window("main").ok_or("窗口不存在")?;
    win.set_always_on_top(enabled).map_err(|e| e.to_string())?;
    let store = lock_rw(&state.store);
    let mut cfg = lock_rw(&state.config);
    cfg.always_on_top = enabled;
    cfg.save(&store)
}

/// 后台轮询。渠道都是账务接口,查询余额不消耗 token,所以间隔可以压得比较短;
/// 唯一约束是对方的限流礼貌性,所以失败时退避。
async fn poll_loop(app: tauri::AppHandle) {
    let mut failures = 0u32;
    // 90 天清理原来只在启动时做一次:这台机器常年不重启的话快照表就一直长(报告 O-2)
    let mut last_prune = now_ts();

    loop {
        let (active_sec, idle_sec, backoff_sec) = {
            let state = app.state::<AppState>();
            let c = lock_rw(&state.config);
            (
                c.active_interval_sec,
                c.idle_interval_sec,
                c.backoff_interval_sec,
            )
        };

        let visible = app
            .get_webview_window("main")
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false);

        let base = if visible { active_sec } else { idle_sec };
        let wait = if failures >= 3 { backoff_sec } else { base };

        // 抖动,避免多渠道同秒并发撞上对方限流
        let jitter = (now_ts() % 5) as u64;
        tokio::time::sleep(std::time::Duration::from_secs(wait + jitter)).await;

        // 先把 Arc 取出来,避免 State 借用跨越 await
        let (store, fetcher) = {
            let state = app.state::<AppState>();
            (state.store.clone(), state.fetcher.clone())
        };

        if now_ts() - last_prune >= 86_400 {
            last_prune = now_ts();
            let s = lock_rw(&store);
            if let Err(e) = s.prune(now_ts() - 90 * 86_400) {
                eprintln!("定期清理快照失败: {e}");
            }
        }

        // 走 fetch_shared:用户点「立即刷新」撞上轮询时排队等结果,
        // 而不是对同一批账务接口发出双份请求(报告 O-4)
        let views = fetcher.fetch_shared(&store).await;

        if views.iter().any(|v| v.valid) {
            failures = 0;
        } else if views.iter().any(|v| v.has_key) {
            failures += 1;
        }
        // 一个渠道都没配凭据时不算失败:那是全新安装,退避到 5 分钟只会让
        // 用户「填好 key 却半天看不到数字」

        let _ = app.emit("channels-updated", &views);
    }
}

async fn refresh_now(app: &tauri::AppHandle) {
    let (store, fetcher) = {
        let state = app.state::<AppState>();
        (state.store.clone(), state.fetcher.clone())
    };
    let views = fetcher.fetch_shared(&store).await;
    let _ = app.emit("channels-updated", &views);
}

// ───────────── 检查更新 ─────────────
// 更新逻辑放 Rust 而不是前端:前端是无打包器的静态页,拿不到
// @tauri-apps/plugin-updater 的 npm 包。返回值 + 事件这套路子与
// get_channels / channels-updated 一致 —— 前端只负责渲染,不做判断。
//
// 端点与公钥都在 tauri.conf.json 的 plugins.updater 里,插件自己读。
const UPDATE_CHECK_INTERVAL_SEC: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    /// 当前运行的版本
    pub current: String,
    /// 有新版本时为 true
    pub available: bool,
    pub version: String,
    pub notes: String,
    pub date: Option<String>,
    /// 这次是否真的去问过服务器。节流 / 关掉自动检查时都是 false。
    pub checked_now: bool,
    /// 只有真去问了才有值。失败原因只给手动点击看,自动检查一律静默。
    pub error: Option<String>,
}

impl UpdateState {
    fn idle(current: &str) -> Self {
        Self {
            current: current.into(),
            available: false,
            version: String::new(),
            notes: String::new(),
            date: None,
            checked_now: false,
            error: None,
        }
    }
}

// 注意:async 命令只要带了 State(借用)就必须返回 Result,否则 tauri 的代码生成
// 过不了(AsyncCommandMustReturnResult)。所以这里是 Result<UpdateState, String>,
// Err 只表示命令本身坏了,更新"没有可用版本"是 Ok 里的 available:false。
#[tauri::command]
async fn check_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    force: bool,
) -> Result<UpdateState, String> {
    use tauri_plugin_updater::UpdaterExt;
    let current = env!("CARGO_PKG_VERSION").to_string();

    let (auto, last_check_at, skipped) = {
        let cfg = lock_rw(&state.config);
        (
            cfg.auto_check_update,
            cfg.last_check_at,
            cfg.skipped_version.clone(),
        )
    };
    // 自动检查的闸门:开关关着、或距上次不足 24 小时,就直接闲置,不发请求。
    // 手动点「检查更新」(force=true)一律放行。
    if !force && !auto {
        return Ok(UpdateState::idle(&current));
    }
    if !force {
        if let Some(last) = last_check_at {
            if now_ts() - last < UPDATE_CHECK_INTERVAL_SEC {
                return Ok(UpdateState::idle(&current));
            }
        }
    }

    // 只要真的要发请求就先把时间戳写掉 —— 失败也算检查过,
    // 否则一次网络故障会变成每次启动都重试的请求风暴。
    {
        let store = state.store.lock();
        if let Ok(s) = store {
            // 锁序样板:这里 store→config,set_pin 等命令必须同序(见 lock_rw 注释)
            let mut cfg = lock_rw(&state.config);
            cfg.last_check_at = Some(now_ts());
            let _ = cfg.save(&s);
        }
    }

    let updater = match app.updater_builder().build() {
        Ok(u) => u,
        Err(e) => {
            return Ok(UpdateState {
                checked_now: true,
                error: Some(e.to_string()),
                ..UpdateState::idle(&current)
            })
        }
    };
    Ok(match updater.check().await {
        Ok(Some(u)) => {
            // 「跳过此版本」要尊重到底:手动点也不该把跳过的版本重新弹出来。
            // 但请求确实发出去了 —— checked_now 必须为真,前端据此才敢刷新
            // 「上次检查时间」;设成 false 会让周期轮询把它当成"没问到"而跳过记账。
            if skipped.as_deref() == Some(u.version.as_str()) {
                UpdateState {
                    checked_now: true,
                    ..UpdateState::idle(&current)
                }
            } else {
                UpdateState {
                    current,
                    available: true,
                    version: u.version.clone(),
                    // 更新说明在远端清单里可能没有,缺失就留空,前端会显示占位
                    notes: u.body.clone().unwrap_or_default(),
                    date: u.date.as_ref().map(|d| d.to_string()),
                    checked_now: true,
                    error: None,
                }
            }
        }
        Ok(None) => UpdateState {
            checked_now: true,
            ..UpdateState::idle(&current)
        },
        // 网络不通、限流、清单格式不对都走这里。自动检查时前端不会显示它
        Err(e) => UpdateState {
            checked_now: true,
            error: Some(e.to_string()),
            ..UpdateState::idle(&current)
        },
    })
}

/// 下载并安装。Windows 上 install 那一步会直接退出进程,
/// 所以这里不会返回 "装好了" —— 前端别在它后面再弹确认框。
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater_builder().build().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "没有可用的更新".to_string())?;

    let mut received: u64 = 0;
    let mut total: u64 = 0;
    let h1 = app.clone();
    let h2 = app.clone();
    let _ = h1.emit(
        "update-progress",
        serde_json::json!({ "phase": "started", "received": 0, "total": 0 }),
    );
    update
        .download_and_install(
            move |chunk: usize, content_length: Option<u64>| {
                received += chunk as u64;
                if let Some(cl) = content_length {
                    total = cl;
                }
                let _ = h1.emit(
                    "update-progress",
                    serde_json::json!({ "phase": "downloading", "received": received, "total": total }),
                );
            },
            move || {
                let _ = h2.emit("update-progress", serde_json::json!({ "phase": "installing" }));
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit("update-progress", serde_json::json!({ "phase": "finished" }));
    Ok(())
}

/// 记住「跳过此版本」。
#[tauri::command]
fn skip_update_version(state: State<'_, AppState>, version: String) -> Result<(), String> {
    let store = lock_rw(&state.store);
    let mut cfg = lock_rw(&state.config);
    cfg.skipped_version = Some(version);
    cfg.save(&store)
}

/// 记住窗口位置(报告 O-6)。只在自由态下记 —— 吸附/紧凑条/胶囊的坐标是布局
/// 算出来的,记下来反而会在下次启动覆盖正确的默认位置。
/// 前端在拖动结束后调用一次,不做高频写盘。
#[tauri::command]
fn save_window_pos(state: State<'_, AppState>, x: i32, y: i32) -> Result<(), String> {
    let store = lock_rw(&state.store);
    let mut cfg = lock_rw(&state.config);
    cfg.win_x = Some(x);
    cfg.win_y = Some(y);
    cfg.save(&store)
}

/// 把请求的窗口坐标夹进"至少有一条边露在外面"的范围。
/// 多屏环境下用户拔掉副屏后,记下的坐标会落在屏幕外,窗口看起来像消失了。
/// 至少要露出 KEEP_X × KEEP_Y 的可视区,保证还能拖回来。
fn clamp_to_monitors(app: &tauri::AppHandle, x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
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

/// 当前版本号(前端底部显示用)。之前硬编码在 index.html 里,发版必改且会忘(报告 P2-15)。
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 取走启动期的非致命错误(配置损坏回落默认、快捷键注册失败)。
/// 取走即清空 —— 提示一次就够,别每次刷新都弹。
#[tauri::command]
fn take_startup_error(state: State<'_, AppState>) -> Option<String> {
    lock_rw(&state.startup_error).take()
}

// ─────────────── 每日签到(Trae / WorkBuddy 加油站)───────────────
//
// 设计见 `design-auto-checkin-2026-10-13.html`。三条红线:
//   ① 独立开关、默认关:关掉就一个请求都不发(与 hidden_channels 同口径);
//   ② 判因只看 code / HTTP / 域名,**永远不看 message 文案**;
//   ③ 签到的状态不参与余额渲染 —— 签到失败不可能让余额显示成 0。
//
// 请求走**强制直连**(`checkin::direct_client`,不跟随系统代理):Trae 侧 9074 的成因之一
// 是按出口 IP 排队限流,而系统代理/机场 IP 正是那类高发出口(设计稿裁决 4A)。

/// 界面要的每个渠道的签到状态。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckinState {
    id: String,
    name: String,
    enabled: bool,
    auto: bool,
    /// 今天的状态短码(claimed / already / retry:ready / retry:device / auth / inactive / shape / failed)
    outcome: String,
    credits: Option<f64>,
    attempts: i64,
    last_try_at: Option<i64>,
    next_try_at: Option<i64>,
    /// 现在能不能点「领积分」
    can_try: bool,
    /// 要去用户动手(凭据失效 / 结构变了)
    needs_user: bool,
    /// 拿不到凭据的原因(界面直接显示)
    note: Option<String>,
    /// 本月自动领到的合计(只算真领到的)
    month_credits: f64,
    /// 近 7 天流水
    log: Vec<tokenscope_core::store::CheckinRow>,
}

/// 该渠道现在能不能拿到签到凭据;拿不到就把原因说清楚(界面照抄)。
fn checkin_credential(app: &tauri::AppHandle, def: &checkin::CheckinDef) -> Result<checkin::Cred, String> {
    match def.auth {
        checkin::CheckinAuth::TraeApp => {
            let c = appauth::candidates(appauth::App::Trae)
                .into_iter()
                .next()
                .ok_or_else(|| "没检测到 Trae 登录信息 · 打开一次 Trae 即可".to_string())?;
            let device_id = c
                .device_id
                .ok_or_else(|| "Trae 的 storage.json 里没读到设备号(签到需要它)".to_string())?;
            Ok(checkin::Cred::Trae {
                token: c.token,
                device_id,
            })
        }
        checkin::CheckinAuth::WebSession => {
            let session = wb_session_of(app, def.id)
                .ok_or_else(|| format!("未登录 {} 账号 · 在渠道卡上登录一次", def.name))?;
            Ok(checkin::Cred::Web(session))
        }
    }
}

fn checkin_state_of(id: &str, cfg: &Config, store: &Store, note: Option<String>) -> CheckinState {
    let def = checkin::find(id);
    let c = cfg.checkin.get(id).cloned().unwrap_or_default();
    let today = checkin::beijing_day(now_ts());
    let now = now_ts();
    let done = c.done_for_day(&today);
    let waiting = c.next_try_at.map(|t| t > now).unwrap_or(false);
    let outcome = c.last_code.clone().unwrap_or_default();
    let needs_user = outcome.starts_with("auth") || outcome.starts_with("shape");
    let log = store.recent_checkins(id, now - 7 * 86_400).unwrap_or_default();
    let month_credits = store
        .checkin_month_credits(id, &today[..7])
        .unwrap_or(0.0);
    CheckinState {
        id: id.to_string(),
        name: def.map(|d| d.name.to_string()).unwrap_or_else(|| id.to_string()),
        enabled: c.enabled,
        auto: c.auto,
        outcome,
        credits: c.last_credits,
        attempts: c.attempts,
        last_try_at: c.last_try_at,
        next_try_at: c.next_try_at,
        can_try: c.enabled && !done && !waiting,
        needs_user,
        note,
        month_credits,
        log,
    }
}

/// 跑一次签到:查状态 → 可领就领 → 领完回查一次 → 触发一次取数做到账核对。
/// 手动点与自动节拍器共用这一条路。
async fn run_checkin(app: &tauri::AppHandle, id: &str, manual: bool) -> Result<checkin::Outcome, String> {
    let def = checkin::find(id).ok_or_else(|| format!("未知签到渠道: {id}"))?;
    let cred = checkin_credential(app, def)?;
    let client = checkin::direct_client();
    let today = checkin::beijing_day(now_ts());
    let now = now_ts();

    // 查状态
    let mut out = checkin::call(&client, def, &cred, false).await;
    let mut credits: Option<f64> = match &out {
        checkin::Outcome::Already(c) | checkin::Outcome::Claimed(c) => *c,
        _ => None,
    };

    // 可领 → 领;领完**回查一次**(服务端可能已被客户端自己点掉,那种情况也算成功)
    if matches!(&out, checkin::Outcome::Retry(m) if m == "ready") {
        let claimed = checkin::call(&client, def, &cred, true).await;
        credits = match &claimed {
            checkin::Outcome::Claimed(c) | checkin::Outcome::Already(c) => c.or(credits),
            _ => credits,
        };
        let back = checkin::call(&client, def, &cred, false).await;
        out = match &back {
            checkin::Outcome::Already(_) => checkin::Outcome::Claimed(credits),
            _ => claimed,
        };
    }

    // 到账核对:立刻取一轮(新包会出现在余额里),失败不影响签到结论
    if matches!(out, checkin::Outcome::Claimed(_)) {
        refresh_now(app).await;
    }

    // 落配置 + 落流水(凭据不进去)
    {
        let state = app.state::<AppState>();
        let mut cfg = lock_rw(&state.config).clone();
        let entry = cfg.checkin.entry(id.to_string()).or_default();
        let same_day = entry.last_day.as_deref() == Some(today.as_str());
        entry.attempts = if same_day { entry.attempts + 1 } else { 1 };
        entry.last_day = Some(today.clone());
        entry.last_code = Some(out.code());
        entry.last_try_at = Some(now);
        entry.last_credits = credits;
        entry.next_try_at = if out.retryable() && checkin::should_retry(&out, entry.attempts) {
            Some(now + checkin::next_delay(entry.attempts))
        } else {
            None
        };
        entry.last_message = match &out {
            checkin::Outcome::Inactive(m) | checkin::Outcome::Retry(m) | checkin::Outcome::BadShape(m)
            | checkin::Outcome::Failed(m) => Some(m.clone()),
            _ => None,
        };
        let store = lock_rw(&state.store);
        if let Err(e) = store.record_checkin(
            id,
            now,
            &today,
            &out.code(),
            credits,
            entry.attempts,
            entry.last_message.as_deref().unwrap_or(""),
        ) {
            eprintln!("{e}");
        }
        if let Err(e) = cfg.save(&store) {
            eprintln!("签到配置落库失败: {e}");
        }
        drop(store);
        *lock_rw(&state.config) = cfg;
    }
    let _ = manual;
    let _ = app.emit("checkin-updated", serde_json::json!({ "id": id }));
    Ok(out)
}

#[tauri::command]
async fn checkin_state(state: State<'_, AppState>) -> Result<Vec<CheckinState>, String> {
    let cfg = lock_rw(&state.config).clone();
    let list = {
        let store = lock_rw(&state.store);
        checkin::CHECKINS
            .iter()
            .map(|d| checkin_state_of(d.id, &cfg, &store, None))
            .collect::<Vec<_>>()
    };
    Ok(list)
}

/// 手动领一次(渠道卡上的「领积分」)。
#[tauri::command]
async fn checkin_now(app: tauri::AppHandle, id: String) -> Result<CheckinState, String> {
    let out = run_checkin(&app, &id, true).await?;
    let state = app.state::<AppState>();
    let cfg = lock_rw(&state.config).clone();
    let store = lock_rw(&state.store);
    let _ = out;
    Ok(checkin_state_of(&id, &cfg, &store, None))
}

/// 每日节拍器:每 5 分钟看一次"今天该不该签"。
/// **不塞进 poll_loop 的 60 秒里** —— 签到是写操作,节奏要跟取数分开(设计稿第 8 节)。
fn spawn_checkin_ticker(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            let (cfg, today, now) = {
                let state = app.state::<AppState>();
                let cfg = lock_rw(&state.config).clone();
                (cfg, checkin::beijing_day(now_ts()), now_ts())
            };
            for def in checkin::CHECKINS {
                let Some(c) = cfg.checkin.get(def.id) else { continue };
                if !c.enabled || !c.auto {
                    continue; // 关了就一个请求都不发
                }
                if c.done_for_day(&today) {
                    continue;
                }
                if c.next_try_at.map(|t| t > now).unwrap_or(false) {
                    continue; // 退避中
                }
                // 北京时间 00:23 + 该渠道抖动之后才允许(当天更晚也算 → 开机补跑)
                if !checkin::due_now(now, &today, def.id) {
                    continue;
                }
                if let Err(e) = run_checkin(&app, def.id, false).await {
                    // 拿不到凭据:记一句就走,别每 5 分钟刷屏
                    eprintln!("签到 {} 跳过: {e}", def.name);
                }
                // 两渠道之间串行 + 间隔(设计稿第 8 节)
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            }
        }
    });
}

// ─────────────── 4SAPI 可用模型 + 一键配置到客户端 ───────────────
//
// 设计与取证见 `design-4sapi-models.html`。三条纪律:
//   1. 模型清单是**元数据接口**,不产生 token 计费(与余额取数同一条原则);
//   2. 写进客户端一律「先备份 → 写 → 读回校验」,目标在跑就拒写(规矩在 core::inject 里);
//   3. 密钥只从凭据管理器**现读现写**,不回前端、不落盘、不进日志。

/// 来源芯片要的信息。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceInfo {
    id: String,
    name: String,
    has_price: bool,
    list_public: bool,
}

fn source_infos() -> Vec<SourceInfo> {
    models::SOURCES
        .iter()
        .map(|d| SourceInfo {
            id: d.id.to_string(),
            name: d.name.to_string(),
            has_price: d.pricing_path.is_some(),
            list_public: d.list_public,
        })
        .collect()
}

/// 模型页要的整份视图。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogView {
    /// 来源 id("4sapi" / "hapi" / "opencode-go")
    source: String,
    /// 来源的界面名
    source_name: String,
    /// 全部来源(界面上的芯片;没取到的来源也要列出来,标「—」)
    sources: Vec<SourceInfo>,
    /// 这个来源有没有价格(只有 4SAPI 有)。false → 界面不显示单价、也不按价排序
    has_price: bool,
    /// 列表是否公开(Go 是:没配密钥也能列出模型)
    list_public: bool,
    /// 价目表取数的域名
    host: String,
    /// 要写进客户端的接入地址(挂件实测可用的那个)
    base_url: String,
    version: String,
    fetched_at: i64,
    /// 可用集合的取回时间;0 = 从没取到过(界面据此把「可用」退成「未知」)
    usable_at: i64,
    has_key: bool,
    group: String,
    groups: Vec<models::Group>,
    total: usize,
    chat_count: usize,
    usable_count: usize,
    rows: Vec<models::RowView>,
    /// 刷新失败时的真实原因(界面显示上次快照并用它说明)
    error: Option<String>,
}

/// 客户端状态 + 写入台账。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct InjectOverview {
    clients: Vec<inject::ClientState>,
    log: Vec<inject::InjectRecord>,
}

/// 写进客户端的接入地址:用户指定 > 实测可用的域名 > 主域名。
/// 写进客户端的接入地址。
/// 4SAPI:用户指定 > 实测可用的域名 > 主域名(它有几个域名,得挑一个真通的);
/// 其它来源:用来源定义里的固定地址。
fn source_base(cfg: &Config, cache: Option<&models::ModelCache>, def: &models::SourceDef) -> String {
    if def.id != "4sapi" {
        return def.base_url.to_string();
    }
    if let Some(u) = cfg.model_base_url.as_deref() {
        let t = u.trim().trim_end_matches('/');
        if !t.is_empty() {
            return t.to_string();
        }
    }
    match cache {
        Some(c) => c.base_url(),
        None => def.base_url.to_string(),
    }
}

/// 该来源的密钥。Go 的列表公开,但写进客户端必须带密钥。
fn source_key(id: &str) -> Option<String> {
    secrets::get(id)
        .ok()
        .flatten()
        .filter(|k| !k.trim().is_empty())
}


/// 入口顺序:配置里指定的域名优先,其次上次成功的(可用集合 > 价目表),最后默认顺序。
fn host_order(cfg: &Config, cache: Option<&models::ModelCache>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |h: String| {
        if !h.is_empty() && !out.iter().any(|x| x == &h) {
            out.push(h);
        }
    };
    if let Some(base) = cfg.model_base_url.as_deref() {
        if let Some(h) = providers::url_host(base) {
            push(h);
        }
    }
    if let Some(c) = cache {
        if c.usable_at > 0 {
            push(c.usable_host.clone());
        }
        push(c.host.clone());
    }
    for h in providers::four_s_api_hosts() {
        push(h);
    }
    out
}

/// 一个模型写进客户端时的全部信息。上下文/多模态从价目表缓存里取;
/// 缓存里没有就留空 —— 不编造能力上限。
///
/// **这里是阻塞活**(凭据管理器 / 读缓存),调用方负责把它放进 `spawn_blocking` ——
/// 见 `inject_preview` 上面那段注释。
fn model_spec_with(
    store: &Arc<Mutex<Store>>,
    cfg: &Config,
    source: &str,
    model: &str,
) -> Result<inject::ModelSpec, String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("没有指定模型".into());
    }
    let def = models::find_source(source).ok_or_else(|| format!("未知模型来源: {source}"))?;
    let key = match source_key(def.id) {
        Some(k) => k,
        None => {
            return Err(format!(
                "还没配置 {} 密钥:先在「渠道」页签里填一次",
                def.name
            ))
        }
    };
    let cache = {
        let s = lock_rw(store);
        models::ModelCache::load(&s)
    };
    let row = cache
        .as_ref()
        .and_then(|c| c.catalog().ok())
        .and_then(|cat| cat.find(model).cloned());
    let context = row
        .as_ref()
        .and_then(|r| r.context.as_deref())
        .and_then(models::context_tokens);
    let multimodal = row.as_ref().map(|r| r.multimodal).unwrap_or(false);
    let base = source_base(cfg, cache.as_ref(), def);
    Ok(inject::ModelSpec::new(
        def.id,
        model,
        &base,
        &key,
        context,
        multimodal,
    ))
}

fn inject_opts(cfg: &Config, client: inject::Client) -> inject::Opts {
    inject::Opts {
        // 学到的口径才写选中项;没学到就只写条目(core::inject::learn_prefix)
        selection_prefix: cfg.selection_prefix.get(client.id()).cloned(),
        trae_auto_select: cfg.trae_auto_select,
    }
}

/// 取 4SAPI 的可用模型:公开价目表 + 这把 key 能调的集合。
/// 两者都是元数据,不产生 token 计费;缓存一天,失败时退回上次快照并如实报错。
#[tauri::command]
async fn get_model_catalog(
    state: State<'_, AppState>,
    source: Option<String>,
    force: bool,
) -> Result<CatalogView, String> {
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    let client = state.fetcher.http.clone();
    let now = now_ts();
    let want = source.unwrap_or_default();
    let def = models::find_source(if want.is_empty() { "4sapi" } else { &want })
        .ok_or_else(|| format!("未知模型来源: {want}"))?;

    let mut cache = {
        let s = lock_rw(&store);
        models::ModelCache::load(&s)
    };
    let mut error: Option<String> = None;

    // ── 只有 id 列表的来源(Hapi / OpenCode Go):列表给什么就是什么,没有价、没有标签 ──
    if def.pricing_path.is_none() {
        let key = source_key(def.id);
        let need = force
            || cache
                .as_ref()
                .and_then(|c| c.sources.get(def.id))
                .map(|sc| sc.stale(now))
                .unwrap_or(true);
        if need {
            match models::fetch_source_ids(&client, def, key.as_deref()).await {
                Ok((ids, host)) => {
                    let c = cache.get_or_insert_with(Default::default);
                    let sc = c.sources.entry(def.id.to_string()).or_default();
                    sc.ids = ids.clone();
                    sc.fetched_at = now;
                    sc.host = host;
                    // 对这个来源而言,"能用"就是列表本身(公开列表 = 它提供这些模型)
                    sc.usable = ids;
                    sc.usable_at = now;
                }
                Err(e) => error = Some(e),
            }
        }
        if let Some(c) = cache.as_ref() {
            let s = lock_rw(&store);
            if let Err(e) = c.save(&s) {
                eprintln!("模型缓存写入失败: {e}");
            }
        }
        let sc = cache.as_ref().and_then(|c| c.sources.get(def.id));
        let (ids, usable, usable_at) = match sc {
            Some(sc) => (sc.ids.clone(), sc.usable.clone(), sc.usable_at),
            None => (Vec::new(), Vec::new(), 0),
        };
        let rows = models::rows_from_ids(&ids, &usable, usable_at > 0);
        return Ok(CatalogView {
            source: def.id.to_string(),
            source_name: def.name.to_string(),
            sources: source_infos(),
            has_price: false,
            list_public: def.list_public,
            host: sc.map(|s| s.host.clone()).unwrap_or_default(),
            base_url: def.base_url.to_string(),
            version: String::new(),
            fetched_at: sc.map(|s| s.fetched_at).unwrap_or(0),
            usable_at,
            has_key: key.is_some(),
            group: String::new(),
            groups: Vec::new(),
            total: rows.len(),
            chat_count: rows.len(),
            usable_count: rows.iter().filter(|r| r.usable == Some(true)).count(),
            rows: models::row_views_of(rows, &[], ""),
            error,
        });
    }

    // ── 4SAPI:价目表 + 可用集合(它有四个域名,挑真通的)──
    let hosts = host_order(&cfg, cache.as_ref());

    let needs_catalog = force || cache.as_ref().map(|c| c.stale(now)).unwrap_or(true);
    if needs_catalog {
        match models::fetch_catalog(&client, &hosts).await {
            Ok(f) => {
                let c = cache.get_or_insert_with(Default::default);
                c.version = f.catalog.version.clone();
                c.fetched_at = now;
                c.pricing = f.raw.to_string();
                c.host = f.host;
            }
            // 失败不把价目表清空:显示上次快照 + 真实原因(与余额取数同一契约)
            Err(e) => error = Some(e),
        }
    }

    let key = secrets::get("4sapi")
        .ok()
        .flatten()
        .filter(|k| !k.trim().is_empty());
    let needs_usable = cache
        .as_ref()
        .map(|c| force || c.usable_at <= 0 || now - c.usable_at > models::CACHE_TTL_SEC)
        .unwrap_or(true);
    if let (Some(k), true) = (key.as_ref(), needs_usable) {
        match models::fetch_usable(&client, &hosts, k).await {
            Ok((ids, host)) => {
                if let Some(c) = cache.as_mut() {
                    c.usable = ids;
                    c.usable_at = now;
                    c.usable_host = host;
                }
            }
            Err(e) => {
                if error.is_none() {
                    error = Some(e);
                }
            }
        }
    }

    if let Some(c) = cache.as_ref() {
        let s = lock_rw(&store);
        if let Err(e) = c.save(&s) {
            eprintln!("模型缓存写入失败: {e}");
        }
    }

    let Some(cache) = cache else {
        return Ok(CatalogView {
            source: def.id.to_string(),
            source_name: def.name.to_string(),
            sources: source_infos(),
            has_price: true,
            list_public: def.list_public,
            host: String::new(),
            base_url: source_base(&cfg, None, def),
            version: String::new(),
            fetched_at: 0,
            usable_at: 0,
            has_key: key.is_some(),
            group: String::new(),
            groups: Vec::new(),
            total: 0,
            chat_count: 0,
            usable_count: 0,
            rows: Vec::new(),
            error: error.or_else(|| Some("还没有价目表缓存".into())),
        });
    };

    let mut catalog = cache.catalog()?;
    if cache.usable_known() {
        // 只有**成功取到过**才打「不可用」标记:一次网络失败不该让 160 个模型全变红
        catalog.mark_usable(&cache.usable);
    }
    let group = cfg
        .model_group
        .clone()
        .filter(|g| !g.trim().is_empty())
        .unwrap_or_else(|| catalog.default_group());
    let rows = models::row_views(&catalog, &group);
    Ok(CatalogView {
        source: def.id.to_string(),
        source_name: def.name.to_string(),
        sources: source_infos(),
        has_price: true,
        list_public: def.list_public,
        host: cache.host.clone(),
        base_url: source_base(&cfg, Some(&cache), def),
        version: cache.version.clone(),
        fetched_at: cache.fetched_at,
        usable_at: cache.usable_at,
        has_key: key.is_some(),
        group,
        groups: catalog.groups.clone(),
        total: catalog.rows.len(),
        chat_count: catalog.chat_count(),
        usable_count: catalog.rows.iter().filter(|r| r.usable == Some(true)).count(),
        rows,
        error,
    })
}

/// 写入预告(纯读,绝不写盘)。
///
/// `async` + `spawn_blocking` 不是风格问题:**非 async 的 Tauri 命令跑在主线程上**,
/// 而这条路上要起 `tasklist` 探进程、读客户端配置、读凭据管理器 —— 全是阻塞活。
/// 真机实测过一次卡住:整个挂件不再响应任何命令(WebView 的 IPC 一起排队),
/// 连 WorkBuddy 渠道的会话读取都跟着超时。项目里早有同一条教训(取数路径的
/// 文件 IO / keyring 都下沉 spawn_blocking),这里照同一套办。
#[tauri::command]
async fn inject_preview(
    state: State<'_, AppState>,
    client: String,
    source: Option<String>,
    model: String,
) -> Result<Vec<inject::Preview>, String> {
    let c = inject::Client::parse(&client).ok_or_else(|| format!("未知客户端: {client}"))?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        let src = source.clone().unwrap_or_else(|| "4sapi".to_string());
        let spec = model_spec_with(&store, &cfg, &src, &model)?;
        Ok(inject::preview(c, &spec, &inject_opts(&cfg, c)))
    })
    .await
    .map_err(|e| format!("写入预告失败: {e}"))?
}

/// 执行写入。规矩(备份/校验/拒写运行中的目标)全在 core::inject 里。
#[tauri::command]
async fn inject_apply(
    state: State<'_, AppState>,
    client: String,
    source: Option<String>,
    model: String,
) -> Result<Vec<inject::Report>, String> {
    let c = inject::Client::parse(&client).ok_or_else(|| format!("未知客户端: {client}"))?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        let src = source.clone().unwrap_or_else(|| "4sapi".to_string());
        let spec = model_spec_with(&store, &cfg, &src, &model)?;
        let reports = inject::apply(c, &spec, &inject_opts(&cfg, c))?;
        // 记账:只记「写到哪、备份在哪、写了几条」——密钥不在其中
        {
            let s = lock_rw(&store);
            for r in &reports {
                let _ = inject::push_log(
                    &s,
                    inject::InjectRecord {
                        stamp: r.stamp.clone(),
                        at: now_ts(),
                        client: r.client.clone(),
                        label: r.label.clone(),
                        model: spec.id.clone(),
                        backup: r.backup.clone(),
                        backup_path: r.backup_path.clone(),
                        added: r.added,
                        updated: r.updated,
                        removed: r.removed,
                        selected: r.selected,
                    },
                );
            }
        }
        Ok(reports)
    })
    .await
    .map_err(|e| format!("写入失败: {e}"))?
}

/// 取消写入:把这条模型从客户端里删掉(写入的逆操作)。
/// 规矩完全一样:目标没运行、先备份、只动我们写的条目、写完读回校验,不过就回滚。
#[tauri::command]
async fn inject_remove(
    state: State<'_, AppState>,
    client: String,
    source: Option<String>,
    model: String,
) -> Result<Vec<inject::Report>, String> {
    let c = inject::Client::parse(&client).ok_or_else(|| format!("未知客户端: {client}"))?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        // 取消写入也要一个 spec:按 base_url / 名字认出"这条是不是我们写的"
        let src = source.clone().unwrap_or_else(|| "4sapi".to_string());
        let spec = model_spec_with(&store, &cfg, &src, &model)?;
        let reports = inject::remove(c, &spec)?;
        {
            let s = lock_rw(&store);
            for r in &reports {
                let _ = inject::push_log(
                    &s,
                    inject::InjectRecord {
                        stamp: r.stamp.clone(),
                        at: now_ts(),
                        client: r.client.clone(),
                        label: r.label.clone(),
                        model: spec.id.clone(),
                        backup: r.backup.clone(),
                        backup_path: r.backup_path.clone(),
                        added: 0,
                        updated: 0,
                        removed: r.removed,
                        selected: false,
                    },
                );
            }
        }
        Ok(reports)
    })
    .await
    .map_err(|e| format!("取消写入失败: {e}"))?
}

/// 四客户端状态 + 已写入的模型 + 台账。顺手把「选中项前缀口径」学下来并持久化:
/// 那是从客户端自己的文件里读出来的证据,不猜(见 core::inject::learn_prefix)。
#[tauri::command]
async fn inject_state(state: State<'_, AppState>) -> Result<InjectOverview, String> {
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    let scanned = tokio::task::spawn_blocking(move || {
        let list = inject::state(&cfg.selection_prefix);
        let mut learned = cfg.selection_prefix.clone();
        let mut dirty = false;
        for st in &list {
            if let Some(p) = &st.learned_prefix {
                if learned.get(&st.client) != Some(p) {
                    learned.insert(st.client.clone(), p.clone());
                    dirty = true;
                }
            }
        }
        let log = {
            let s = lock_rw(&store);
            inject::load_log(&s)
        };
        (list, log, learned, dirty)
    })
    .await;
    let Ok((clients, log, learned, dirty)) = scanned else {
        return Ok(InjectOverview {
            clients: Vec::new(),
            log: Vec::new(),
        });
    };
    if dirty {
        // 学到的口径写回配置。两把锁分开拿、各拿各的(不嵌套,避免与 set_config 互等)。
        let updated = {
            let mut c = lock_rw(&state.config);
            c.selection_prefix = learned;
            c.clone()
        };
        let store2 = state.store.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let s = lock_rw(&store2);
            updated.save(&s)
        })
        .await;
    }
    Ok(InjectOverview { clients, log })
}

/// 回滚一次写入:把备份目录里的文件放回原处。目标在跑就拒回滚(同写入的规矩)。
#[tauri::command]
async fn inject_rollback(state: State<'_, AppState>, backup: String) -> Result<String, String> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        let path = std::path::PathBuf::from(backup.trim());
        if !path.is_dir() {
            return Err(format!("备份目录不存在: {}", path.display()));
        }
        let log = {
            let s = lock_rw(&store);
            inject::load_log(&s)
        };
        let abs = path.to_string_lossy().to_string();
        if let Some(c) = inject::log_owner(&log, &abs) {
            if inject::process_running(c.process()) {
                return Err(format!(
                    "{} 正在运行,先退出它再回滚(否则回滚会被它的内存态覆盖)",
                    c.label()
                ));
            }
        }
        inject::restore(&path)
    })
    .await
    .map_err(|e| format!("回滚失败: {e}"))?
}

/// 「复制三件套」。密钥由后端拼进字符串,**不回前端**。
/// 注意:写入剪贴板对其他程序(含剪贴板历史)可见 —— 界面必须写明这一点。
#[tauri::command]
async fn copy_model_bundle(
    state: State<'_, AppState>,
    source: Option<String>,
    model: String,
) -> Result<String, String> {
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        let src = source.clone().unwrap_or_else(|| "4sapi".to_string());
        let spec = model_spec_with(&store, &cfg, &src, &model)?;
        let text = format!(
            "接入地址:{}\n模型名称:{}\nAPI Key:{}",
            spec.base_url, spec.id, spec.key
        );
        copy_to_clipboard(&text)?;
        Ok(format!("已复制:{} + 模型名 + 密钥", spec.base_url))
    })
    .await
    .map_err(|e| format!("复制失败: {e}"))?
}

/// 用系统 `clip.exe` 写剪贴板(经 stdin,密钥不进命令行)。
/// 输入按 UTF-16LE:clip 按控制台代码页解释字节流,中文标签走 UTF-8 会被切坏。
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut cmd = std::process::Command::new("clip");
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
async fn open_path(app: tauri::AppHandle, path: String) -> Result<(), String> {
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
        app.opener()
            .open_path(expanded, None::<&str>)
            .map_err(|e| format!("打开失败: {e}"))
    })
    .await
    .map_err(|e| format!("打开失败: {e}"))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 必须是第一个插件:第二个实例不再往下走,直接把已开实例的窗口唤到前台。
        // 之前双开会出现两个托盘图标、两个轮询一起打账务接口,而且两套 SQLite
        // 连接写同一个库(报告 O-3)。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let was = w.is_visible().unwrap_or(false);
                let _ = w.show();
                let _ = w.set_focus();
                if !was {
                    let _ = app.emit("window-shown", ());
                }
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                // 全插件共用这一个回调:按下的那一刻切换主窗口显隐(与托盘左键同语义)
                .with_handler(|app, _shortcut, ev| {
                    use tauri_plugin_global_shortcut::ShortcutState;
                    if ev.state != ShortcutState::Pressed {
                        return;
                    }
                    if let Some(w) = app.get_webview_window("main") {
                        if w.is_visible().unwrap_or(false) {
                            let _ = w.hide();
                        } else {
                            let _ = w.show();
                            let _ = w.set_focus();
                            // 真正「隐藏 → 显示」的那一刻告知前端(重放入场动画)。
                            // 前端不能靠 focus 事件区分 alt-tab 与唤回(报告 B8)
                            let _ = app.emit("window-shown", ());
                        }
                    }
                })
                .build(),
        )
        // 公钥与端点都在 tauri.conf.json 的 plugins.updater 里,插件自己读配置
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("无法定位数据目录: {e}"))?;
            let store = Store::open(&dir.join("tokenscope.db"))?;
            // 配置坏了不能静默用默认值:那会把用户的隐藏渠道、快捷键、轮询间隔
            // 全部悄悄还原(报告 P2-14)。取默认值继续启动,但把原因留给前端提示。
            let (config, mut startup_error) = match Config::try_load(&store) {
                Ok(c) => (c, None),
                Err(e) => (Config::default(), Some(e)),
            };

            // 注册表与配置对齐:开机自启以配置为准,并刷新为当前 exe 路径,
            // 用户挪动 exe 后无需重新设置自启。构建目录里的开发版不写键,
            // 旧版残留的自启项要让用户看得见(光靠新版删会被旧版开机时写回来)。
            if let Some(msg) = autostart::sync(config.autostart) {
                startup_error = note(startup_error, msg);
            }

            // 置顶 + 上次位置是持久化设置:启动时按配置应用,折叠形态没有置顶按钮也生效
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_always_on_top(config.always_on_top);
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

            let _ = store.prune(now_ts() - 90 * 86400);

            app.manage(AppState {
                store: Arc::new(Mutex::new(store)),
                config: Mutex::new(config.clone()),
                fetcher: Arc::new(Fetcher::new()),
                hotkey: Mutex::new(None),
                startup_error: Mutex::new(None),
            });

            // 网页登录型渠道的会话来源:每轮取数时现从 webview 的 Cookie 罐里读。
            // 回调会在取数路径的 spawn_blocking 里被调用(见 set_web_credential_source
            // 的注释),里面那次 webview 派发是阻塞的 —— 只能在非主线程上调,符合。
            let session_app = app.handle().clone();
            app.state::<AppState>()
                .fetcher
                .set_web_credential_source(Box::new(move |id| wb_session_of(&session_app, id)));

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

            let refresh = MenuItem::with_id(app, "refresh", "立即刷新", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "显示面板", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &refresh, &sep, &quit])?;

            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Grandettoken")
                .menu(&menu)
                // 左键只切换显示/隐藏(下面的事件处理),菜单留给右键。
                // 不设这个开关时 Tauri 在 Windows 上左键也会弹菜单。
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let was = w.is_visible().unwrap_or(false);
                            let _ = w.show();
                            let _ = w.set_focus();
                            if !was {
                                let _ = app.emit("window-shown", ());
                            }
                        }
                    }
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
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                                let _ = app.emit("window-shown", ());
                            }
                        }
                    }
                })
                .build(app)?;

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { poll_loop(handle).await });
            // 签到节拍器:独立于取数轮询(签到是写操作,节奏要分开)。
            // **必须在原有 setup 里起** —— Tauri 的 Builder::setup 只有一个槽位,
            // 再加一个会把 manage(AppState) 那套顶掉,应用会在第一条命令就 panic。
            spawn_checkin_ticker(app.handle().clone());

            // 窗口配置为 visible:false,这里统一控制显隐:
            // 正常启动立即显示;开机自启带 --hidden,直接进托盘等用户点开
            if !std::env::args().any(|a| a == "--hidden") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }

            Ok(())
        })
        // WorkBuddy 登录窗被用户自己关掉(没抓到会话)时,要让界面把「登录中」收回去,
        // 否则会一直卡在等令牌的样子。抓到会话那一次是我们主动关窗的,由 WB_LOGIN_DONE
        // 标位区分,不该再报一次"已取消"。观察者也靠 WB_LOGIN_CLOSED 收工 ——
        // 它因此不用去问一个正在销毁的窗口(那种调用会 panic,见 on_main_with_timeout)。
        .on_window_event(|window, event| {
            if window.label() != WB_LOGIN_LABEL || !matches!(event, tauri::WindowEvent::Destroyed) {
                return;
            }
            WB_LOGIN_CLOSED.store(true, std::sync::atomic::Ordering::Relaxed);
            if WB_LOGIN_DONE.swap(false, std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let _ = window.emit(
                "workbuddy-login",
                serde_json::json!({ "ok": false, "cancelled": true }),
            );
        })
        .invoke_handler(tauri::generate_handler![
            get_channels,
            get_config,
            set_config,
            set_key,
            delete_key,
            workbuddy_login,
            get_series,
            set_autostart,
            set_pin,
            set_tray_icon,
            key_hint,
            window_cmd,
            check_update,
            install_update,
            skip_update_version,
            save_window_pos,
            app_version,
            take_startup_error,
            get_model_catalog,
            inject_preview,
            inject_apply,
            inject_remove,
            inject_state,
            inject_rollback,
            copy_model_bundle,
            open_path,
            checkin_state,
            checkin_now,
        ])
        .run(tauri::generate_context!())
        .expect("TokenScope 启动失败");
}
