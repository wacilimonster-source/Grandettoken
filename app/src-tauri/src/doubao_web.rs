//! 豆包(doubao.com)网页登录流:独立登录窗 + 会话观察者,与 WorkBuddy 的
//! wb-login 同一套纪律(判"能不能收工"靠**探测**——真打一次账务接口,不靠
//! "cookie 罐里有没有东西")。会话不落盘:留在 WebView2 的 profile 里,
//! 取数时现读(与 WorkBuddy / Qoder 网页会话同一条裁决),平台轮换 Cookie 也能跟上。
//!
//! 为什么拆成独立模块而不是往 lib.rs 里塞:lib.rs 在规模棘轮上,登录流是
//! 完整的一块功能,拆出去才是 AGENTS 2.3 的正路。lib.rs 只留命令壳与三处接线。
//!
//! 与 WorkBuddy 的两处不同:
//! 1. 豆包是 SPA,登录发生在站点弹窗里,没有"登录完成落地页"—— 观察者
//!    **只靠探测**收工,landed 标位不存在;
//! 2. 凭据是全量 Cookie(字节系认证分散在 sessionid/sid_tt/msToken 等),
//!    组装规则在 `core::weblogin::doubao_cookie_credential`。

use tauri::{Emitter, Manager};
use tokenscope_core::weblogin::DOUBAO_HOST;

use crate::AppState;

pub const LOGIN_LABEL: &str = "doubao-login";

static LOGIN_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LOGIN_CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 打开豆包登录窗。已经开着就把它拉到前台(不重复开第二个)。
///
/// **必须是 async**,别改成同步的:同步命令在 WebView2 的 COM 回调里就地执行,
/// 里面再建 webview 会把整个 IPC 冻死(WorkBuddy 那条 2026-10-08 实测的坑,同因同果)。
pub async fn login(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(LOGIN_LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    LOGIN_DONE.store(false, std::sync::atomic::Ordering::Relaxed);
    LOGIN_CLOSED.store(false, std::sync::atomic::Ordering::Relaxed);
    // 豆包是 SPA:登录在站点自己的弹窗里完成,直接进首页就是对的入口
    let url: tauri::Url = format!("https://{DOUBAO_HOST}/")
        .parse()
        .map_err(|e| format!("登录地址有问题: {e}"))?;
    // 不注入任何脚本、不拦任何导航:登录页是第三方的,挂件只负责观察与探测
    tauri::WebviewWindowBuilder::new(&app, LOGIN_LABEL, tauri::WebviewUrl::External(url))
        .title("登录豆包账号（额度读取用）")
        // 登录与取数必须保持同一套身份:网页 Cookie 配桌面端 UA 是 710022004
        // 的典型触发条件。这里用的 UA 与 core::doubao 取数时发送的完全一致。
        .user_agent(tokenscope_core::doubao::app_ua())
        .inner_size(560.0, 780.0)
        .min_inner_size(420.0, 560.0)
        .resizable(true)
        // 面板是置顶的,登录窗不置顶就会被压在下面,用户看不见
        .always_on_top(true)
        .build()
        .map_err(|e| format!("登录窗口打不开: {e}"))?;
    spawn_watcher(app.clone());
    Ok(())
}

/// 登录窗被销毁(用户自己关掉):观察者靠它收工,不再去问一个正在销毁的窗口
/// 挂件主动关窗时先置 DONE,
/// 这里靠 swap 区分,不再发"已取消"。
pub fn on_login_window_destroyed(window: &tauri::Window) {
    LOGIN_CLOSED.store(true, std::sync::atomic::Ordering::Relaxed);
    if LOGIN_DONE.swap(false, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let _ = window.emit("doubao-login", serde_json::json!({ "ok": false, "cancelled": true }));
}

/// 从 webview 的 Cookie 罐里取豆包会话(全量组装,规则在 core::weblogin)。
///
/// **只读主窗口**:它从启动活到退出,登录窗随时可能被用户关掉;Cookie 按
/// profile 存,主窗口与登录窗读到的是同一份(WorkBuddy 2026-10-08 实测同款结论)。
pub fn session_of(app: &tauri::AppHandle) -> Option<String> {
    let jar = crate::webview_cookies::read(app, format!("https://{DOUBAO_HOST}/")).ok()?;
    tokenscope_core::weblogin::doubao_cookie_credential(jar.iter().map(|x| (x.name.as_str(), x.value.as_str())))
}

/// 会话可用吗:真打一次任务面板接口(走 fetcher.probe,与取数同一条链)。
/// 不验证就报成功,一份被平台拒掉的会话会变成"状态显示已登录、却一直取不到数"。
async fn session_works(app: &tauri::AppHandle, session: &str) -> bool {
    let fetcher = app.state::<AppState>().fetcher.clone();
    match tokenscope_core::providers::find("doubao") {
        Some(def) => fetcher.probe(def, session).await,
        None => false,
    }
}

fn spawn_watcher(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tried = String::new();
        let mut probes = 0u32;
        let mut ticks = 0u32;
        let mut warned = false;
        loop {
            if LOGIN_CLOSED.load(std::sync::atomic::Ordering::Relaxed) {
                return; // 用户自己把窗口关了
            }
            // 只在工作线程等待 WebView 的异步回调。
            let probe_app = app.clone();
            let session = tokio::task::spawn_blocking(move || session_of(&probe_app))
                .await
                .ok()
                .flatten();
            if let Some(session) = session {
                let fresh = session != tried;
                if fresh {
                    tried = session.clone();
                    probes = 0;
                    warned = false;
                }
                // 新会话立刻验;同一份会话每 ~6 秒补验一次,最多 10 次,
                // 不对着平台一直打(与 WorkBuddy 观察者同一口径)
                if fresh || (probes < 10 && ticks % 5 == 0) {
                    probes += 1;
                    if session_works(&app, &session).await {
                        finish_login(app).await;
                        return;
                    }
                    // 同一份会话验了 10 次都过不去:如实说话(只说一次),
                    // 让用户知道"窗口可以关了,这份会话没用",别干等
                    if fresh && probes >= 10 && !warned {
                        warned = true;
                        let _ = app.emit(
                            "doubao-login",
                            serde_json::json!({
                                "ok": false,
                                "reason": "拿到豆包会话了,但查额度被平台拒绝 —— 确认登录的是个人账号(飞书企业号不支持本活动)后,在渠道卡上点「重新登录」再试一次。"
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

/// 收尾:关窗 → 通知界面 → 立刻刷一轮。会话本身不用搬走 —— 它就在浏览器
/// profile 里,取数时现读(文件顶部第 4 条纪律)。
async fn finish_login(app: tauri::AppHandle) {
    // 先置位再关窗:Destroyed 监听靠这个标志区分"登录成功"与"用户自己关了"
    LOGIN_DONE.store(true, std::sync::atomic::Ordering::Relaxed);
    // 复登放行(与 wb_finish_login 同一条理由):复登成功 = 当天"凭据失效"的
    // 签到该放出补领了 —— done_for_day 把 auth 算成"今天完结",节拍器从此照章
    // 跳过,手动「领积分」也被 can_try 拦住;只清 auth/no_cred 之后的场次。
    {
        let st = app.state::<AppState>();
        let needs_reset = crate::lock_rw(&st.config)
            .checkin
            .get("doubao")
            .and_then(|entry| entry.last_code.as_deref())
            .is_some_and(|c| c.starts_with("auth") || c.starts_with("no_cred"));
        if needs_reset {
            let today = crate::cycle_day_of("doubao", crate::now_ts());
            if let Err(e) = crate::save_config_off_main(&st, |live| {
                if let Some(entry) = live.checkin.get_mut("doubao") {
                    if entry
                        .last_code
                        .as_deref()
                        .is_some_and(|c| c.starts_with("auth") || c.starts_with("no_cred"))
                    {
                        entry.last_day = Some(today);
                        entry.last_code = None;
                        entry.attempts = 0;
                        entry.next_try_at = None;
                    }
                }
            })
            .await
            {
                eprintln!("豆包复登后重置签到状态失败: {e}");
            }
        }
    }
    let handle = app.clone();
    crate::on_main(&app, move || {
        if let Some(w) = handle.get_webview_window(LOGIN_LABEL) {
            let _ = w.close();
        }
    });
    let _ = app.emit("doubao-login", serde_json::json!({"ok": true}));
    app.state::<AppState>().fetcher.invalidate();
    crate::refresh_now(&app).await;
}

/// 清豆包的登录会话(「删除密钥」时用):删掉 main 窗 Cookie 罐里所有
/// doubao.com 的 Cookie。找不到会话不算失败(目标状态本来就成立)。
pub fn clear_session(app: &tauri::AppHandle) -> Result<(), String> {
    crate::webview_cookies::clear(app, [format!("https://{DOUBAO_HOST}/")], |_| true, false)
        .map(|_| ())
}
