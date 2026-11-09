//! HTTP clients for per-group gateway proxy routing.
//!
//! reqwest snapshots system proxy settings when the client is built. Reuse its
//! connection pool until those settings change, then build a fresh client.

use std::{ffi::OsString, sync::Mutex, time::Duration};

#[derive(PartialEq, Eq)]
struct ProxySnapshot {
    env: Vec<Option<OsString>>,
    #[cfg(windows)]
    wininet: Option<(Option<u32>, Option<String>, Option<String>)>,
}

impl ProxySnapshot {
    fn current() -> Self {
        let env = [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
            "REQUEST_METHOD",
        ]
        .iter()
        .map(std::env::var_os)
        .collect();
        #[cfg(windows)]
        let wininet = {
            use winreg::{enums::HKEY_CURRENT_USER, RegKey};
            RegKey::predef(HKEY_CURRENT_USER)
                .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
                .ok()
                .map(|key| {
                    (
                        key.get_value("ProxyEnable").ok(),
                        key.get_value("ProxyServer").ok(),
                        key.get_value("ProxyOverride").ok(),
                    )
                })
        };
        Self {
            env,
            #[cfg(windows)]
            wininet,
        }
    }
}

struct ProxyClientCache {
    snapshot: ProxySnapshot,
    client: reqwest::Client,
}

impl ProxyClientCache {
    fn current_client(
        &mut self,
        snapshot: ProxySnapshot,
        build: impl FnOnce() -> Result<reqwest::Client, String>,
    ) -> Result<reqwest::Client, String> {
        if self.snapshot != snapshot {
            // Leave the old snapshot in place on failure so the next call retries.
            self.client = build()?;
            self.snapshot = snapshot;
        }
        Ok(self.client.clone())
    }
}

fn build_client(system_proxy: bool) -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        // 读空闲超时(报告 B7):上游握手完成后若不吐响应头、或 SSE 流中途断流,
        // 没有这一项连接会**永久悬挂**,网关任务堆积只能重启恢复。60s 对流式足够宽
        // —— 正常打字也会持续吐 chunk,真静默 60 秒就是挂了。
        .read_timeout(Duration::from_secs(60));
    let builder = if system_proxy {
        builder
    } else {
        builder.no_proxy()
    };
    builder
        .build()
        .map_err(|_| "无法初始化代理连接".to_string())
}

pub struct ProxyClients {
    system: Mutex<ProxyClientCache>,
    direct: reqwest::Client,
}

impl ProxyClients {
    pub fn new() -> Result<Self, String> {
        let snapshot = ProxySnapshot::current();
        Ok(Self {
            system: Mutex::new(ProxyClientCache {
                snapshot,
                client: build_client(true)?,
            }),
            direct: build_client(false)?,
        })
    }

    /// System proxy selection applies only to the upstream connection. The
    /// gateway's listener and the caller's network settings are independent.
    pub fn client(&self, system_proxy: bool) -> Result<reqwest::Client, String> {
        if !system_proxy {
            return Ok(self.direct.clone());
        }
        self.system
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current_client(ProxySnapshot::current(), || build_client(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{SocketAddr, TcpListener},
        thread,
        time::Instant,
    };

    fn snapshot(name: &str) -> ProxySnapshot {
        ProxySnapshot {
            env: vec![Some(name.into())],
            #[cfg(windows)]
            wininet: None,
        }
    }

    fn server() -> (SocketAddr, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("test server did not receive a request: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (addr, worker)
    }

    fn proxy_client(addr: SocketAddr) -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::http(format!("http://{addr}")).unwrap())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap()
    }

    #[test]
    fn proxy_changes_refresh_the_client_and_direct_groups_bypass_it() {
        let (first_addr, first) = server();
        let (second_addr, second) = server();
        let (direct_addr, direct) = server();
        let mut cache = ProxyClientCache {
            snapshot: snapshot("first"),
            client: proxy_client(first_addr),
        };
        let first_client = cache
            .current_client(snapshot("first"), || panic!("unchanged proxy rebuilt"))
            .unwrap();
        let second_client = cache
            .current_client(snapshot("second"), || Ok(proxy_client(second_addr)))
            .unwrap();
        // Install a fake system proxy so a mistaken proxy choice cannot pass
        // just because the real system bypasses localhost.
        let clients = ProxyClients {
            system: Mutex::new(ProxyClientCache {
                snapshot: ProxySnapshot::current(),
                client: proxy_client(first_addr),
            }),
            direct: build_client(false).unwrap(),
        };
        let direct_client = clients.client(false).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(first_client
                .get("http://gateway-test.invalid/v1/models")
                .send()
                .await
                .unwrap()
                .status()
                .is_success());
            assert!(second_client
                .post("http://gateway-test.invalid/v1/chat/completions")
                .body("{}")
                .send()
                .await
                .unwrap()
                .status()
                .is_success());
            assert!(direct_client
                .get(format!("http://{direct_addr}/direct"))
                .send()
                .await
                .unwrap()
                .status()
                .is_success());
        });
        assert!(first
            .join()
            .unwrap()
            .starts_with("GET http://gateway-test.invalid/v1/models "));
        assert!(second
            .join()
            .unwrap()
            .starts_with("POST http://gateway-test.invalid/v1/chat/completions "));
        assert!(direct.join().unwrap().starts_with("GET /direct "));
    }
}
