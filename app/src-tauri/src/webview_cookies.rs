use std::collections::HashSet;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, WebviewWindow};

const TIMEOUT: Duration = Duration::from_secs(3);
const TIMED_OUT: &str = "读取或清除网页会话超时";

pub struct Cookie {
    pub name: String,
    pub value: String,
    // WebView2 用前导点区分域 Cookie 和仅当前主机的 Cookie。删除时要用原始值。
    pub domain: String,
    pub path: String,
    #[cfg(not(windows))]
    original: tauri::webview::Cookie<'static>,
}

impl Cookie {
    fn key(&self) -> (String, String, String) {
        (self.name.clone(), self.domain.clone(), self.path.clone())
    }
}

type Cookies = Vec<Cookie>;
type Reply<T> = Sender<Result<T, String>>;

// 调用方在工作线程上等待。主线程只登记操作，不能在这里等待 WebView2 的回调：
// Wry 的同步 Cookie API 会运行嵌套 Windows 消息循环，让导航回调重入 Tauri 的锁。
fn request<T: Send + 'static>(
    app: &AppHandle,
    deadline: Instant,
    begin: impl FnOnce(WebviewWindow, Reply<T>) -> Result<(), String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = mpsc::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let result = ensure_time(deadline).and_then(|()| {
            let window = handle.get_webview_window("main").ok_or("主窗口已关闭")?;
            begin(window, tx.clone())
        });
        if let Err(error) = result {
            let _ = tx.send(Err(error));
        }
    })
    .map_err(|error| error.to_string())?;
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => TIMED_OUT.to_string(),
            mpsc::RecvTimeoutError::Disconnected => "网页会话操作未返回结果".to_string(),
        })?
}

fn ensure_time(deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        return Err(TIMED_OUT.into());
    }
    Ok(())
}

pub fn read(app: &AppHandle, url: String) -> Result<Cookies, String> {
    read_before(app, url, Instant::now() + TIMEOUT)
}

fn read_before(app: &AppHandle, url: String, deadline: Instant) -> Result<Cookies, String> {
    ensure_time(deadline)?;
    request(app, deadline, move |window, tx| {
        platform::read(window, url, tx)
    })
}

// 先完整读取所有指定 URI，再按 (name, domain, path) 去重并清理。
// 任一读取失败都让调用方看到错误，避免把无法确认的旧会话当成已清空。
pub fn clear(
    app: &AppHandle,
    urls: impl IntoIterator<Item = String>,
    select: impl Fn(&Cookie) -> bool,
    require_match: bool,
) -> Result<usize, String> {
    let deadline = Instant::now() + TIMEOUT;
    let urls: Vec<_> = urls.into_iter().collect();
    let mut seen = HashSet::new();
    let mut selected = Vec::new();
    for url in &urls {
        for cookie in read_before(app, url.clone(), deadline)? {
            if seen.insert(cookie.key()) && select(&cookie) {
                selected.push(cookie);
            }
        }
    }
    if selected.is_empty() {
        if require_match && !seen.is_empty() {
            return Err(format!(
                "Cookie 罐里有 {} 个条目，但没有匹配的会话",
                seen.len()
            ));
        }
        return Ok(0);
    }

    let deleted: HashSet<_> = selected.iter().map(Cookie::key).collect();
    request(app, deadline, move |window, tx| {
        platform::delete(window, selected, deadline, tx)
    })?;
    // 删除调用返回时浏览器进程可能还没更新 Cookie 罐。确认后再允许登录窗开始新会话。
    for url in urls {
        while read_before(app, url.clone(), deadline)?
            .iter()
            .any(|cookie| deleted.contains(&cookie.key()))
        {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    Ok(deleted.len())
}

#[cfg(windows)]
mod platform {
    use super::*;
    use webview2_com::{
        take_pwstr, GetCookiesCompletedHandler,
        Microsoft::Web::WebView2::Win32::{
            ICoreWebView2CookieList, ICoreWebView2CookieManager, ICoreWebView2_2,
        },
    };
    use windows::core::{Interface, HSTRING, PWSTR};

    fn manager(
        view: tauri::webview::PlatformWebview,
    ) -> windows::core::Result<ICoreWebView2CookieManager> {
        // WebView2 COM 对象只在创建它们的界面线程使用。
        unsafe {
            view.controller()
                .CoreWebView2()?
                .cast::<ICoreWebView2_2>()?
                .CookieManager()
        }
    }

    fn collect(list: Option<ICoreWebView2CookieList>) -> windows::core::Result<Cookies> {
        let Some(list) = list else {
            return Ok(Vec::new());
        };
        let mut count = 0;
        // WebView2 分配的字符串由 take_pwstr 复制并释放。
        unsafe {
            list.Count(&mut count)?;
            (0..count)
                .map(|index| {
                    let cookie = list.GetValueAtIndex(index)?;
                    let read_field = |read: &dyn Fn(*mut PWSTR) -> windows::core::Result<()>| {
                        let mut value = PWSTR::null();
                        read(&mut value)?;
                        Ok::<_, windows::core::Error>(take_pwstr(value))
                    };
                    Ok(Cookie {
                        name: read_field(&|out| cookie.Name(out))?,
                        value: read_field(&|out| cookie.Value(out))?,
                        domain: read_field(&|out| cookie.Domain(out))?,
                        path: read_field(&|out| cookie.Path(out))?,
                    })
                })
                .collect()
        }
    }

    pub(super) fn read(
        window: WebviewWindow,
        url: String,
        tx: Reply<Cookies>,
    ) -> Result<(), String> {
        window
            .with_webview(move |view| {
                let reply = tx.clone();
                let completed =
                    GetCookiesCompletedHandler::create(Box::new(move |status, list| {
                        let result = status
                            .and_then(|()| collect(list))
                            .map_err(|error| error.to_string());
                        let _ = reply.send(result);
                        Ok(())
                    }));
                let result = manager(view).and_then(|manager| {
                    // GetCookies 只启动请求。回调会在正常消息循环中返回。
                    unsafe { manager.GetCookies(&HSTRING::from(url), &completed) }
                });
                if let Err(error) = result {
                    let _ = tx.send(Err(error.to_string()));
                }
            })
            .map_err(|error| error.to_string())
    }

    pub(super) fn delete(
        window: WebviewWindow,
        cookies: Cookies,
        deadline: Instant,
        tx: Reply<()>,
    ) -> Result<(), String> {
        window
            .with_webview(move |view| {
                let result = (|| {
                    let manager = manager(view).map_err(|error| error.to_string())?;
                    for cookie in cookies {
                        ensure_time(deadline)?;
                        // 精确限定名称、域名和路径，避免影响同名的其它会话。
                        unsafe {
                            manager.DeleteCookiesWithDomainAndPath(
                                &HSTRING::from(cookie.name),
                                &HSTRING::from(cookie.domain),
                                &HSTRING::from(cookie.path),
                            )
                        }
                        .map_err(|error| error.to_string())?;
                    }
                    Ok(())
                })();
                let _ = tx.send(result);
            })
            .map_err(|error| error.to_string())
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub(super) fn read(
        window: WebviewWindow,
        url: String,
        tx: Reply<Cookies>,
    ) -> Result<(), String> {
        let url = url
            .parse::<tauri::Url>()
            .map_err(|error| error.to_string())?;
        let result = window.cookies_for_url(url).map(|jar| {
            jar.into_iter()
                .map(|original| Cookie {
                    name: original.name().to_owned(),
                    value: original.value().to_owned(),
                    domain: original.domain().unwrap_or("").to_owned(),
                    path: original.path().unwrap_or("").to_owned(),
                    original,
                })
                .collect()
        });
        let _ = tx.send(result.map_err(|error| error.to_string()));
        Ok(())
    }

    pub(super) fn delete(
        window: WebviewWindow,
        cookies: Cookies,
        deadline: Instant,
        tx: Reply<()>,
    ) -> Result<(), String> {
        for cookie in cookies {
            ensure_time(deadline)?;
            window
                .delete_cookie(cookie.original)
                .map_err(|error| error.to_string())?;
        }
        let _ = tx.send(Ok(()));
        Ok(())
    }
}
