use super::*;

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
pub(crate) const WB_LOGIN_LABEL: &str = "wb-login";
/// 只有这条路走网页登录;手动粘贴的密钥仍存这个条目(Windows 凭据管理器)
pub(crate) const WB_CHANNEL_ID: &str = "workbuddy";
/// 登录页。redirect_uri 就是登录完成后回落的用量页 —— 也是判"登录完成"的路径之一。
pub(crate) const WB_LOGIN_URL: &str = "https://www.workbuddy.cn/login?platform=usercenter&state=0&redirect_uri=https%3A%2F%2Fwww.workbuddy.cn%2Fprofile%2Fplans-usage";
/// 读/删会话 Cookie 时用的地址(只要域名对得上,path 随便给一个能命中的)
// 会话 cookie 的取值域名清单以 weblogin::SITE_HOSTS 为唯一来源:以前这里另写一个常量,
// 于是"判定已登录"认两个域、"取会话"只查一个域,登录了却永远读不到(P2-16)。

/// 成功抓到会话后置位,用于区分"用户自己关掉了窗口"和"我们主动关窗" ——
/// 前者要让界面把「登录中」状态收回去,后者不该再报一次取消。
pub(crate) static WB_LOGIN_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 登录窗已经销毁(用户自己关的,或者我们收尾时关的)。观察者靠它收工 ——
/// 有了这个标志,就不必去问那个可能正在销毁的窗口。
pub(crate) static WB_LOGIN_CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 页面是否已经落到"登录完成"的路径上(判定规则在 core::weblogin)。由 on_navigation
/// 在主线程上置位 —— 那里拿到 URL 不需要任何跨线程派发,比事后去问窗口安全得多。
pub(crate) static WB_LANDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 把一段"要碰 webview"的代码投到主线程去跑,不等结果(关闭窗口这类不需要回信)。
pub(crate) fn on_main(app: &tauri::AppHandle, f: impl FnOnce() + Send + 'static) {
    let _ = app.run_on_main_thread(f);
}

/// 从 webview 的 Cookie 罐里取会话。
///
/// **只读主窗口**:它从启动活到退出,而登录窗随时可能被用户关掉(读一个正在销毁的
/// 窗口会走到上面那个 panic 上去)。Cookie 是按 profile 存的,主窗口读到的和登录窗
/// 读到的是同一份 —— 2026-10-08 实测(httpOnly 的 session / session_2 都读得到)。
pub(crate) fn wb_session_of(app: &tauri::AppHandle, id: &str) -> Option<String> {
    if id != WB_CHANNEL_ID {
        return None; // 只有 WorkBuddy 走这条路
    }
    // 两个工作台域名都能完成登录，必须使用同一份站点名单查找会话。
    tokenscope_core::weblogin::SITE_HOSTS.iter().find_map(|host| {
        let jar = webview_cookies::read(app, format!("https://{host}/")).ok()?;
        tokenscope_core::weblogin::cookie_credential(jar.iter().map(|x| (x.name.as_str(), x.value.as_str())))
    })
}

/// 清 WorkBuddy 的登录会话(「清除登录」用)。**返回结果不能丢**:以前调用方
/// `let _ =` 一吞,超时 / 删失败都变成"命令成功",界面说清掉了、下一轮取数又读到那个 Cookie。
/// 找不到会话不算失败(目标状态"没有会话"本来就成立),超时与删失败才报错。
pub(crate) fn wb_clear_session(app: &tauri::AppHandle) -> Result<(), String> {
    webview_cookies::clear(
        app,
        tokenscope_core::weblogin::SITE_HOSTS.iter().map(|host| format!("https://{host}/")),
        |cookie| tokenscope_core::weblogin::is_session_cookie(&cookie.name),
        false,
    )
    .map(|_| ())
}

/// 会话可用吗:用挂件自己的 HTTP 栈真打一次接口(见 Fetcher::probe)。
/// 不验证就报成功的话,一份被平台拒掉的会话会变成"状态显示已登录、却一直取不到数"。
pub(crate) async fn wb_session_works(app: &tauri::AppHandle, session: &str) -> bool {
    let fetcher = app.state::<AppState>().fetcher.clone();
    match providers::find(WB_CHANNEL_ID) {
        Some(def) => fetcher.probe(def, session).await,
        None => false,
    }
}

/// 收尾:关窗 → 通知界面 → 立刻刷一轮。会话本身不用搬走 —— 它就在浏览器 profile 里,
/// 取数时现读(见文件顶部第 4 条)。
pub(crate) async fn wb_finish_login(app: &tauri::AppHandle) {
    // 先置位再关窗:Destroyed 监听靠这个标志区分"登录成功"与"用户自己关了"
    WB_LOGIN_DONE.store(true, std::sync::atomic::Ordering::Relaxed);
    // 记下"这份会话从现在起是新鲜的":平台会话是绝对 TTL(实测 7 天,证据链在
    // core::weblogin::web_session_hint 的注释),取数侧按这个年龄出提示。
    // 键的定义只留 core 一份(WEB_SESSION_AT_KEY);写失败只是少一条提示,
    // 不值得让收工流程失败。就一次毫秒级的 SQLite 写,直接做。
    {
        let st = app.state::<AppState>();
        let store = lock_rw(&st.store);
        let _ = store.set_setting(
            tokenscope_core::weblogin::WEB_SESSION_AT_KEY,
            &now_ts().to_string(),
        );
    }
    // 复登成功 = 当天"凭据失效"的签到该放出补领了:done_for_day 把 auth 算成
    // "今天完结",节拍器从此照章跳过、手动「领积分」按钮也被 can_try 拦住 ——
    // 实锤过(2026-10-18:上午 auth 一次,中午复登成功,当天的分要静默到次日)。
    // 只清 auth 之后的场次,claimed/already/inactive 不动(那些是真签完了)。
    {
        let st = app.state::<AppState>();
        let needs_reset = lock_rw(&st.config)
            .checkin
            .get(WB_CHANNEL_ID)
            .and_then(|entry| entry.last_code.as_deref())
            .is_some_and(|c| c.starts_with("auth") || c.starts_with("no_cred"));
        if needs_reset {
            let today = cycle_day_of(WB_CHANNEL_ID, now_ts());
            // 过串行闸只合并本渠道 entry(报告 B4):以前整份克隆-落库-再把陈旧克隆
            // 写回内存,落库窗口里用户改的其它设置会被倒回去。
            if let Err(e) = save_config_off_main(&st, |live| {
                if let Some(entry) = live.checkin.get_mut(WB_CHANNEL_ID) {
                    // `auth` = 平台拒绝了那份凭据;`no_cred` = 本机压根没读到凭据。
                    // **两者都该被复登成功放行** —— 复登之后凭据就有了,当天的分不必等到次日。
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
                eprintln!("复登后重置签到状态失败: {e}");
            }
        }
    }
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

// ───────────── Qoder 网页登录(逐包到期专用,2026-10-30)─────────────
//
// 设计与排查:docs/product/product-qoder-credit-expiry-2026-10-30.md。逐包到期
// 明细只在 qoder.com 的网页会话后面,这套会话**只用于账务只读查询**:签到照旧走
// 桌面 Bearer,余额也照旧 —— 两条链路一概不碰(设计红线)。会话不落盘:留在
// WebView2 的 profile 里,取数时现读,平台轮换 Cookie 也能跟上(与 WorkBuddy
// 同一条裁决);判定规则(登录完成路径/Cookie 拼装)都在 core::weblogin,那边有单测。

pub(crate) const QODER_LOGIN_LABEL: &str = "qoder-login";
pub(crate) const QODER_CN_LOGIN_LABEL: &str = "qoder-cn-login";

/// 登录入口来自 core 的区域配置(`QODER_SITE.login_url` / `QODER_CN_SITE.login_url`),
/// 形状是"登录页 + 回调",**不要**直接给 /account/usage:那样首次导航就是"登录完成
/// 路径",随后才被 302 到登录页 —— landed 会在用户还没登录时就被置真,观察者的
/// "被拒绝"提示立刻误发(真机踩过,2026-10-30)。从登录页进:登录完平台自己跳回
/// /account/usage,那一跳才置 landed。
///
/// 两区各自一套标位:国际版登录窗还开着时再开国内版,不能让两个观察者互相把对方
/// 置成"已收工/已落地"。
pub(crate) static QODER_LOGIN_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub(crate) static QODER_LOGIN_CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
pub(crate) static QODER_LANDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub(crate) static QODER_CN_LOGIN_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub(crate) static QODER_CN_LOGIN_CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
pub(crate) static QODER_CN_LANDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 渠道 id → (完成标位, 关窗标位, 落地标位)。只认两个 Qoder 区。
pub(crate) fn qoder_login_flags(
    id: &str,
) -> (
    &'static std::sync::atomic::AtomicBool,
    &'static std::sync::atomic::AtomicBool,
    &'static std::sync::atomic::AtomicBool,
) {
    if id == "qoder-cn" {
        (&QODER_CN_LOGIN_DONE, &QODER_CN_LOGIN_CLOSED, &QODER_CN_LANDED)
    } else {
        (&QODER_LOGIN_DONE, &QODER_LOGIN_CLOSED, &QODER_LANDED)
    }
}

pub(crate) fn qoder_login_label(id: &str) -> &'static str {
    if id == "qoder-cn" {
        QODER_CN_LOGIN_LABEL
    } else {
        QODER_LOGIN_LABEL
    }
}

/// 窗口 label → 渠道 id(关窗事件只能拿到 label)。
pub(crate) fn qoder_label_id(label: &str) -> Option<&'static str> {
    match label {
        QODER_LOGIN_LABEL => Some("qoder"),
        QODER_CN_LOGIN_LABEL => Some("qoder-cn"),
        _ => None,
    }
}

/// Qoder 的网页会话:从主窗口的 Cookie 罐里把**该区站点域**的全部 Cookie 拼成
/// `cookie:` 凭据。名单是平台私有的,不按名字挑(core::weblogin::qoder_cookie_credential);
/// 拿到的东西能不能真取到数,由观察者的探测负责 —— 没登录的访客也可能有几枚营销 Cookie。
///
/// 国际版取 qoder.com,国内版取 qoder.com.cn:两套账号的 Cookie 罐必须按域分开,
/// 否则会把国际版会话发给国内账务接口(必然 401,还会误导用户)。
pub(crate) fn qoder_web_session_of(app: &tauri::AppHandle, id: &str) -> Option<String> {
    let site = tokenscope_core::weblogin::qoder_site(id)?;
    let jar = webview_cookies::read(app, format!("https://{}/", site.host)).ok()?;
    tokenscope_core::weblogin::qoder_cookie_credential(jar.iter().map(|x| (x.name.as_str(), x.value.as_str())))
}

/// 清掉某个 Qoder 区域的网页会话。**只清该区域站点 + 该区域登录页会用来免选账号的
/// SSO Cookie**,WorkBuddy/别的域一个都不碰 —— 与整份 profile 清空的粗暴做法分开。
///
/// 名单是平台私有的,所以按域清(`qoder.com` 及其子域);这是"换账号 / 换令牌后
/// 旧会话还挂在浏览器里"的唯一可靠退出方式(2026-11-09 真机)。
///
/// **不能用 `w.cookies()`**:2026-11-09 真机,它在这台机器的 WebView2 上没枚举到
/// qoder 的会话(qoder_session_cookie 创建时间仍是 11-03,点完「切换账号」原封不动),
/// 而 `cookies_for_url` 是 WorkBuddy 已经跑通的同一条路(带 URI 的枚举)。
/// 按 `/`、`/account/usage`、`/users/sign-in` 三个入口各查一遍:会话 Cookie 的
/// path 一般是 `/`,后两个兜底覆盖 path 更窄的版本;按 (name, domain, path) 去重。
///
/// **国际版还要清 Google SSO**(`.google.com` / `accounts.google.com`):2026-11-09
/// 真机,只清 qoder.com 后登录页拿现成的 Google 会话直接免选账号、2 秒内又建了一份
/// 旧账号会话并自动关窗 —— 用户根本没机会换号。清掉 Google 的 Cookie 只是迫使
/// 登录页重新选账号;这只动挂件自己的 WebView2 profile,不影响系统浏览器。
/// 国内版登录不走 Google,不扩这一刀。
///
/// 返回清除的条数;**一个都没匹配到但有 Cookie 就报错** ——
/// 以前 matched=0 被当成"本来就没有会话"静默放过,才会出现"日志说已清、实际没清"。
pub(crate) fn qoder_clear_session(app: &tauri::AppHandle, id: &str) -> Result<usize, String> {
    let site = tokenscope_core::weblogin::qoder_site(id)
        .ok_or_else(|| format!("未知的 Qoder 区域: {id}"))?;
    let host = site.host.to_ascii_lowercase();
    let suffix = format!(".{host}");
    let mut urls: Vec<String> = ["/", "/account/usage", "/users/sign-in"]
        .iter()
        .map(|p| format!("https://{host}{p}"))
        .collect();
    // Google 的 Cookie 不在 qoder.com 的罐里,要按 Google 自己的域再枚举一遍。
    if id == "qoder" {
        urls.push("https://google.com/".into());
        urls.push("https://accounts.google.com/".into());
    }
    webview_cookies::clear(app, urls, |cookie| {
        let domain = cookie.domain.trim_start_matches('.').to_ascii_lowercase();
        domain == host || domain.ends_with(&suffix)
            || (id == "qoder" && (domain == "google.com" || domain.ends_with(".google.com")))
    }, true)
}

/// 取数管线的会话分发。**故意不并进 wb_session_of**:签到凭据兜底(checkin_credentials
/// 的 WebSession 分支)引用的是那个函数 —— Qoder 的网页会话绝不能漏进签到链路(设计红线)。
pub(crate) fn web_session_of(app: &tauri::AppHandle, id: &str) -> Option<String> {
    match id {
        WB_CHANNEL_ID => wb_session_of(app, id),
        "qoder" | "qoder-cn" => qoder_web_session_of(app, id),
        // 豆包:登录窗捕获的会话现读(全量 Cookie 组装,规则在 core::weblogin)
        "doubao" => doubao_web::session_of(app),
        _ => None,
    }
}

/// 收尾:关窗 → 通知界面 → 立刻刷一轮(与 wb_finish_login 同构,去掉签到那一段:
/// Qoder 的签到走桌面 Bearer,网页会话与它无关)。
///
/// `note` 是"登录成功但有保留"的说明(目前只有一种:会话被平台接受、逐包接口的
/// 响应结构挂件还没认出来)。**这种情况仍按登录成功收尾** —— 会话本身是真的有效,
/// 把窗口留在那儿让用户对着它等,只会让人以为登录没成。
pub(crate) async fn qoderweb_finish_login(app: &tauri::AppHandle, id: &str, note: Option<String>) {
    let site = tokenscope_core::weblogin::qoder_site(id)
        .expect("调用方已限定 qoder | qoder-cn,站点配置必然存在");
    let (done, _, _) = qoder_login_flags(id);
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    // "这份网页会话从现在起是新鲜的":取数侧按这个年龄出提示(qoder_web_session_hint)。
    // 写**本区自己的键** —— 共用会让一边登录替另一边刷新年龄(串号的隐性形态)。
    {
        let st = app.state::<AppState>();
        let store = lock_rw(&st.store);
        let _ = store.set_setting(site.session_at_key, &now_ts().to_string());
    }
    let label = qoder_login_label(id);
    let handle = app.clone();
    on_main(app, move || {
        if let Some(w) = handle.get_webview_window(label) {
            let _ = w.close();
        }
    });
    let _ = app.emit(
        "qoderweb-login",
        serde_json::json!({"ok": true, "detail": note, "id": id}),
    );
    // 刚登录的是当前账号:旧逐包列表不能再作为"取数失败时沿用的上次成功"。
    // 余额快照不受影响(那是桌面凭据的账,与网页会话无关)。forget_qoder_web
    // 内部已经作废 5 秒复用窗口,这里不必再 invalidate 一次。
    app.state::<AppState>().fetcher.forget_qoder_web(id);
    refresh_now(app).await;
}

/// 打开 Qoder 网页登录窗(逐包到期用)。已开着就拉到前台。**必须 async**,理由同
/// workbuddy_login:同步命令在 WebMessageReceived 回调里再建 webview 会冻死(2026-10-08 实测)。
/// `id` 省略时按国际版(兼容旧调用)。
///
/// `switch = true` 是用户明确要**换账号**:先退出该区域当前的网页登录再开窗。
/// 不先退出的话,旧会话仍然有效,页面一落到 /account/usage 就会被观察者判成
/// "登录成功"并自动关窗 —— 用户根本没机会在窗口里换号(2026-11-09 真机)。
/// 余额凭据换过(`qoder_account_changed_at` 晚于网页会话时刻)时,普通登录
/// 也走同一条清理,否则旧账号的逐包明细会继续挂在新账号名下。
#[tauri::command]
pub(crate) async fn qoderweb_login(
    app: tauri::AppHandle,
    id: Option<String>,
    switch: Option<bool>,
) -> Result<(), String> {
    let id = id.as_deref().unwrap_or("qoder");
    let site = tokenscope_core::weblogin::qoder_site(id)
        .ok_or_else(|| format!("未知的 Qoder 区域: {id}"))?;
    let id = site.id; // &'static str,观察者/导航闭包都要 'static
    let label = qoder_login_label(id);
    if let Some(w) = app.get_webview_window(label) {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    let switch = switch.unwrap_or(false);
    let stale = {
        let st = app.state::<AppState>();
        let store = lock_rw(&st.store);
        let read_ts = |key: &str| {
            store
                .get_setting(key)
                .ok()
                .flatten()
                .and_then(|s| s.trim().parse::<i64>().ok())
        };
        tokenscope_core::weblogin::qoder_web_session_stale(
            read_ts(&format!("{}:{id}", tokenscope_core::fetch::QODER_CHANGED_KEY)),
            read_ts(site.session_at_key),
        )
    };
    if switch || stale {
        let handle = app.clone();
        let cleared = tokio::task::spawn_blocking(move || qoder_clear_session(&handle, id))
            .await
            .map_err(|error| error.to_string())??;
        {
            let st = app.state::<AppState>();
            let store = lock_rw(&st.store);
            let _ = store.delete_setting(site.session_at_key);
        }
        app.state::<AppState>().fetcher.forget_qoder_web(id);
        tokenscope_core::diaglog::write_keyed(
            id,
            "webauth",
            &format!(
                "{} → 已清网页登录 Cookie({cleared} 个)",
                if switch {
                    "用户切换网页账号"
                } else {
                    "余额凭据已更换"
                }
            ),
        );
    }
    let (done, closed, landed) = qoder_login_flags(id);
    done.store(false, std::sync::atomic::Ordering::Relaxed);
    landed.store(false, std::sync::atomic::Ordering::Relaxed);
    closed.store(false, std::sync::atomic::Ordering::Relaxed);
    let url: tauri::Url = site
        .login_url
        .parse()
        .map_err(|e| format!("登录地址有问题: {e}"))?;
    let title = if id == "qoder-cn" {
        "登录 Qoder 国内版网页账号（积分明细用）"
    } else {
        "登录 Qoder 网页账号（积分明细用）"
    };
    let mut builder =
        tauri::WebviewWindowBuilder::new(&app, label, tauri::WebviewUrl::External(url))
            .title(title)
            .inner_size(560.0, 780.0)
            .min_inner_size(420.0, 560.0)
            .resizable(true)
            .always_on_top(true);
    // 换号时还要清掉 Qoder 自己存在 localStorage 里的 token:2026-11-09 真机,
    // 只清 Cookie 不够 —— SPA 用旧 token 在 2 秒内重新建出会话并自动关窗。
    if switch || stale {
        builder = builder.initialization_script(QODER_SWITCH_CLEAR_SCRIPT);
    }
    builder
        .on_navigation(move |nav| {
            if tokenscope_core::weblogin::qoder_is_post_login_for(id, nav.as_str()) {
                landed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            true
        })
        .build()
        .map_err(|e| format!("登录窗口打不开: {e}"))?;
    spawn_qoder_login_watcher(app.clone(), id);
    Ok(())
}

/// 换号时在 Qoder 页面里执行的一次性清理(初始化脚本,先于页面 JS)。
///
/// Qoder 的网页登录把 token 存在 localStorage:只清 Cookie,SPA 会在 2 秒内
/// 用旧 token 重新建出会话,登录窗被观察者判成功并自动关掉 —— 用户根本没机会
/// 换号(2026-11-09 真机)。`window.name` 做一次性标记:OAuth 回调/二次导航
/// 不再清,否则会把刚登录的新 token 也清掉。只在 qoder.com / qoder.com.cn 的
/// 文档上清,about:blank 与 Google 页面跳过。
pub(crate) const QODER_SWITCH_CLEAR_SCRIPT: &str = r#"
(function () {
  try {
    var h = (location.hostname || '').toLowerCase();
    var isQoder = h === 'qoder.com' || /\.qoder\.com$/.test(h) ||
                  h === 'qoder.com.cn' || /\.qoder\.com\.cn$/.test(h);
    if (isQoder && window.name !== 'gt-qoder-switch') {
      try { localStorage.clear(); } catch (e) {}
      try { sessionStorage.clear(); } catch (e) {}
      window.name = 'gt-qoder-switch';
    }
  } catch (e) {}
})();
"#;

/// 与 spawn_login_watcher 同一条纪律:判"能不能收工"靠**探测**(真打一次账务接口),
/// 不靠"cookie 罐里有没有东西";两条状态由主线程侧钩子置位,绝不碰登录窗的 API。
pub(crate) fn spawn_qoder_login_watcher(app: tauri::AppHandle, id: &'static str) {
    let (_, closed, landed) = qoder_login_flags(id);
    tauri::async_runtime::spawn(async move {
        let mut tried = String::new();
        let mut probes = 0u32;
        let mut ticks = 0u32;
        loop {
            if closed.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let probe_app = app.clone();
            let session =
                tokio::task::spawn_blocking(move || qoder_web_session_of(&probe_app, id))
                    .await
                    .ok()
                    .flatten();
            if let Some(session) = session {
                let fresh = session != tried;
                if fresh {
                    tried = session.clone();
                    probes = 0;
                }
                // 新会话立刻验;同一份每 ~6 秒补验一次(用户可能刚点完登录),最多 10 次
                if fresh || (probes < 10 && ticks % 5 == 0) {
                    probes += 1;
                    let fetcher = app.state::<AppState>().fetcher.clone();
                    match fetcher.probe_qoder_web(id, &session).await {
                        tokenscope_core::fetch::QoderWebProbe::Packed => {
                            qoderweb_finish_login(&app, id, None).await;
                            return;
                        }
                        // 会话有效、只是结构没认出来:登录是成功的,别让用户去重登。
                        // 诊断里已经有结构草图(见 core::qoderweb::shape_sketch),
                        // 这条提示只需要说清"不是你的问题、也不是网络问题"。
                        tokenscope_core::fetch::QoderWebProbe::Unrecognized(_) => {
                            qoderweb_finish_login(
                                &app,
                                id,
                                Some(
                                    "已登录 Qoder 网页 · 但积分明细接口的返回结构还没识别(已记入诊断)。余额与签到不受影响"
                                        .to_string(),
                                ),
                            )
                            .await;
                            return;
                        }
                        // 被平台拒 / 网络失败:只有"确实落到登录完成页、且刚换了会话"才说话,
                        // 还停在登录页说明人家正要登录,别抢着报错。
                        tokenscope_core::fetch::QoderWebProbe::Rejected => {
                            tokenscope_core::diaglog::write_keyed(
                                id,
                                "webauth",
                                "登录轮询:网页会话被平台拒绝(401/403)",
                            );
                            if landed.load(std::sync::atomic::Ordering::Relaxed) && fresh {
                                let _ = app.emit(
                                    "qoderweb-login",
                                    serde_json::json!({
                                        "ok": false,
                                        "id": id,
                                        "reason": "登录页到了,但用这份会话查积分明细被平台拒绝。会自动再试几次;一直失败的话在渠道卡上点「登录网页版」重来。"
                                    }),
                                );
                            }
                        }
                        tokenscope_core::fetch::QoderWebProbe::Failed(detail) => {
                            tokenscope_core::diaglog::write_keyed(
                                id,
                                "webauth",
                                &format!("登录轮询:验证请求失败 · {detail}"),
                            );
                            if landed.load(std::sync::atomic::Ordering::Relaxed) && fresh {
                                let _ = app.emit(
                                    "qoderweb-login",
                                    serde_json::json!({
                                        "ok": false,
                                        "id": id,
                                        "reason": "登录页到了,但查积分明细的请求没打通(网络或接口地址变了)。会自动再试几次。"
                                    }),
                                );
                            }
                        }
                    }
                }
            }
            ticks += 1;
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        }
    });
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
/// 那类调用在窗口正在销毁时可能因阻塞派发失败而终止进程。
pub(crate) fn spawn_login_watcher(app: tauri::AppHandle) {
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
                                "reason": "登录是成功了,但用这份会话取数被平台拒绝 —— 在渠道卡上点「重新登录」即可。平台网页会话实测约 7 天过期,自动续不上;备选的访问密钥要标准版以上账号才能建(www.workbuddy.cn/profile/keys)。"
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
/// 打开豆包登录窗(壳命令,流程在 doubao_web::login)。async 的理由同 workbuddy_login。
#[tauri::command]
pub(crate) async fn doubao_login(app: tauri::AppHandle) -> Result<(), String> {
    doubao_web::login(app).await
}

#[tauri::command]
pub(crate) async fn workbuddy_login(app: tauri::AppHandle) -> Result<(), String> {
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

