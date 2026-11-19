//! Windows 原生回归，使用独立 WebView2 profile 和保留域名，不加载业务页面。
//! `--sync-sentinel` 走旧的同步 Cookie API，必须被同一个时序断言检出。

#[cfg(windows)]
mod probe {
    use std::sync::mpsc;
    use std::time::Duration;
    use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_2;
    use windows::core::{Interface, HSTRING};

    const URL: &str = "https://cookie-fixture.test/private/";
    const OTHER_URL: &str = "https://other-fixture.test/";
    const WAIT: Duration = Duration::from_secs(5);

    mod cookies {
        include!("../src/webview_cookies.rs");
        use std::sync::mpsc::TryRecvError;

        pub(super) fn assert_deferred(app: &AppHandle, sentinel: bool) -> Result<(), String> {
            let handle = app.clone();
            let (phase_tx, phase_rx) = mpsc::channel();
            app.run_on_main_thread(move || {
                let result = (|| -> Result<_, String> {
                    let window = handle.get_webview_window("main").ok_or("主窗口已关闭")?;
                    let (tx, rx) = mpsc::channel();
                    if sentinel {
                        let url: tauri::Url = super::URL.parse().map_err(|_| "夹具 URL 无效")?;
                        let result = window
                            .cookies_for_url(url)
                            .map(|_| Vec::new())
                            .map_err(|error| error.to_string());
                        tx.send(result).map_err(|error| error.to_string())?;
                    } else {
                        platform::read(window, super::URL.into(), tx)?;
                    }
                    if !matches!(rx.try_recv(), Err(TryRecvError::Empty)) {
                        return Err("Cookie 请求在 UI 调用返回前已完成，仍在同步等待".into());
                    }
                    Ok(rx)
                })();
                let _ = phase_tx.send(result);
            })
            .map_err(|error| error.to_string())?;

            let rx = phase_rx
                .recv_timeout(super::WAIT)
                .map_err(|_| "登记 Cookie 请求后 UI 调用未返回")??;
            let result = rx
                .recv_timeout(super::WAIT)
                .map_err(|_| "Cookie 回调未完成")??;
            if result.len() != 3 {
                return Err("原生回调未返回三个合成 Cookie".into());
            }
            Ok(())
        }
    }

    fn verify(app: &AppHandle, sentinel: bool) -> Result<(), String> {
        cookies::assert_deferred(app, sentinel)?;
        let original = cookies::read(app, URL.into())?;
        if !original.iter().any(|cookie| {
            cookie.domain == ".cookie-fixture.test"
                && cookie.path == "/private"
                && cookie.value == "scoped"
        }) {
            return Err("子路径 Cookie 未按原值读取".into());
        }
        let removed = cookies::clear(
            app,
            [URL.into(), URL.into()],
            |cookie| cookie.domain == ".cookie-fixture.test" && cookie.path == "/private",
            true,
        )?;
        if removed != 1 {
            return Err("多入口清理没有按名称、域名和路径去重".into());
        }
        let remaining = cookies::read(app, URL.into())?;
        if remaining.len() != 2
            || !remaining
                .iter()
                .any(|cookie| cookie.path == "/" && cookie.value == "root")
            || !remaining
                .iter()
                .any(|cookie| cookie.path == "/private" && cookie.value == "host-scoped")
        {
            return Err("清理没有保留根路径和仅限当前主机的同名 Cookie".into());
        }
        let other = cookies::read(app, OTHER_URL.into())?;
        if other.len() != 1 || other[0].value != "other" {
            return Err("清理影响了其它域名的同名 Cookie".into());
        }
        if cookies::clear(app, [URL.into()], |_| false, true).is_ok() {
            return Err("没有匹配到会话却报告清理成功".into());
        }
        Ok(())
    }

    fn seed(window: &WebviewWindow) -> tauri::Result<()> {
        window.with_webview(|view| {
            // 直接写入合成条目，避免 Tauri 的 Cookie 类型把前导点去掉。
            let result = (|| unsafe {
                let manager = view
                    .controller()
                    .CoreWebView2()?
                    .cast::<ICoreWebView2_2>()?
                    .CookieManager()?;
                for (domain, path, value) in [
                    ("cookie-fixture.test", "/", "root"),
                    ("cookie-fixture.test", "/private", "host-scoped"),
                    (".cookie-fixture.test", "/private", "scoped"),
                    ("other-fixture.test", "/", "other"),
                ] {
                    let cookie = manager.CreateCookie(
                        &HSTRING::from("fixture"),
                        &HSTRING::from(value),
                        &HSTRING::from(domain),
                        &HSTRING::from(path),
                    )?;
                    cookie.SetIsHttpOnly(true)?;
                    manager.AddOrUpdateCookie(&cookie)?;
                }
                Ok::<_, windows::core::Error>(())
            })();
            result.expect("写入合成 Cookie 失败");
        })
    }

    pub fn run() -> tauri::Result<()> {
        let sentinel = std::env::args().any(|arg| arg == "--sync-sentinel");
        let mut context = tauri::generate_context!();
        context.config_mut().app.windows.clear();
        let profile =
            std::env::temp_dir().join(format!("grandettoken-cookie-test-{}", std::process::id()));
        std::fs::create_dir(&profile).expect("无法创建独立的测试 profile");

        let data_directory = profile.clone();
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let app = tauri::Builder::default()
            .setup(move |app| {
                let window = WebviewWindowBuilder::new(
                    app,
                    "main",
                    WebviewUrl::External("about:blank".parse()?),
                )
                .data_directory(data_directory)
                .visible(false)
                .build()?;
                seed(&window)?;
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let _ = outcome_tx.send(verify(&handle, sentinel));
                    handle.exit(0);
                });
                Ok(())
            })
            .build(context)?;
        app.run_return(|_, _| {});
        for attempt in 0..20 {
            match std::fs::remove_dir_all(&profile) {
                Ok(()) => break,
                Err(error) if attempt == 19 => eprintln!("测试 profile 清理失败: {error}"),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        match outcome_rx
            .try_recv()
            .unwrap_or_else(|_| Err("窗口退出前未收到验证结果".into()))
        {
            Ok(()) => println!("PASS: WebView2 异步回调、按域和路径清理、无匹配错误"),
            Err(error) => {
                eprintln!("FAIL: {error}");
                std::process::exit(1);
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> tauri::Result<()> {
    probe::run()
}

#[cfg(not(windows))]
fn main() {
    println!("SKIP: 此回归需要 Windows WebView2");
}
