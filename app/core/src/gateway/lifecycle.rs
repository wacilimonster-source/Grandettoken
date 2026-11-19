use super::{http::build_router, lock, Gateway, SECRET_KEY};
use axum::serve::{Listener, ListenerExt};
use std::{io, net::{Ipv4Addr, SocketAddr}};
use tokio::{net::{TcpListener, TcpStream}, sync::oneshot};

struct ReleasingListener { socket: Option<TcpListener>, released: Option<oneshot::Sender<()>> }
impl Listener for ReleasingListener {
    type Io = TcpStream;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (TcpStream, SocketAddr) {
        Listener::accept(self.socket.as_mut().expect("listener owns socket")).await
    }
    fn local_addr(&self) -> io::Result<SocketAddr> { self.socket.as_ref().unwrap().local_addr() }
}
impl Drop for ReleasingListener {
    fn drop(&mut self) {
        // Axum releases its listener before draining existing connections.
        drop(self.socket.take());
        if let Some(tx) = self.released.take() { let _ = tx.send(()); }
    }
}

// A cancelled stop must leave the acknowledgement available to the next operation.
struct ReleaseWait<'a> { gateway: &'a Gateway, rx: Option<oneshot::Receiver<()>> }
impl Drop for ReleaseWait<'_> {
    fn drop(&mut self) {
        if self.rx.is_some() { lock(&self.gateway.inner).listener_released = self.rx.take(); }
    }
}
fn validate(port: u16, lan: bool, auth: bool) -> Result<(), String> {
    if port == 0 { return Err("端口必须在 1 到 65535 之间".into()); }
    if lan && !auth { return Err("局域网访问必须启用网关密钥".into()); }
    Ok(())
}
fn finish_run(gateway: &Gateway, run_id: u64, result: io::Result<()>) {
    let mut inner = lock(&gateway.inner);
    // Old streams can finish after restart; they must not clear the new run.
    if inner.run_id == run_id && inner.shutdown.take().is_some() {
        inner.startup_error = Some(result.err().map(|e| format!("网关监听异常停止: {e}"))
            .unwrap_or_else(|| "网关监听意外停止，请重新启动".into()));
    }
}
impl Gateway {
    pub async fn start(&self, port: u16, lan: bool, auth: bool) -> Result<(), String> {
        validate(port, lan, auth)?;
        self.prepare_access(auth).await?;
        let _guard = self.lifecycle.lock().await;
        if lock(&self.inner).shutdown.as_ref().is_some_and(|tx| !tx.is_closed()) {
            return Err("网关正在运行，请使用应用并重启或先停止网关".into());
        }
        self.stop_locked().await?;
        let result = self.start_locked(port, lan, auth, true).await;
        if let Err(ref error) = result { lock(&self.inner).startup_error = Some(error.clone()); }
        result
    }
    pub async fn stop(&self) -> Result<(), String> {
        let _guard = self.lifecycle.lock().await;
        self.stop_locked().await
    }
    pub async fn restart(&self, port: u16, lan: bool, auth: bool) -> Result<(), String> {
        validate(port, lan, auth)?;
        self.prepare_access(auth).await?;
        let _guard = self.lifecycle.lock().await;
        let (old, running) = {
            let inner = lock(&self.inner);
            (inner.settings.clone(), inner.shutdown.as_ref().is_some_and(|tx| !tx.is_closed()))
        };
        self.stop_locked().await?;
        if let Err(error) = self.start_locked(port, lan, auth, true).await {
            let error = if running {
                match self.start_locked(old.port, old.lan, old.auth, false).await {
                    Ok(()) => format!("{error}；已恢复原设置服务"),
                    Err(restore) => format!("{error}；原设置服务恢复失败: {restore}"),
                }
            } else { error };
            lock(&self.inner).startup_error = Some(error.clone());
            return Err(error);
        }
        Ok(())
    }
    async fn stop_locked(&self) -> Result<(), String> {
        let (tx, rx) = {
            let mut inner = lock(&self.inner);
            (inner.shutdown.take(), inner.listener_released.take())
        };
        if let Some(tx) = tx { let _ = tx.send(()); }
        let mut wait = ReleaseWait { gateway: self, rx };
        let result = match wait.rx.as_mut() {
            Some(rx) => rx.await.map_err(|_| "无法确认网关监听已释放".to_string()),
            None => Ok(()),
        };
        wait.rx = None;
        result
    }
    async fn prepare_access(&self, auth: bool) -> Result<(), String> {
        if !auth { return Ok(()); }
        let access = self.access.clone();
        // Credential RPC finishes before locking or stopping the currently running service.
        tokio::task::spawn_blocking(move || {
            if access.get(SECRET_KEY)?.map_or(true, |s| s.is_empty()) {
                let mut bytes = [0u8; 32];
                getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
                let token = format!("gw_{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
                access.set(SECRET_KEY, &token)?;
            }
            Ok(())
        }).await.map_err(|e| format!("网关访问密钥准备失败: {e}"))?
    }
    async fn start_locked(&self, port: u16, lan: bool, auth: bool, persist: bool) -> Result<(), String> {
        let addr = SocketAddr::from((if lan { Ipv4Addr::UNSPECIFIED } else { Ipv4Addr::LOCALHOST }, port));
        let socket = TcpListener::bind(addr).await.map_err(|e| format!("无法监听端口 {port}: {e}"))?;
        let (tx, rx) = oneshot::channel();
        let (released, listener_released) = oneshot::channel();
        let run_id = {
            let mut inner = lock(&self.inner);
            let mut next = inner.settings.clone();
            let changed = (next.port, next.lan, next.auth) != (port, lan, auth);
            next.port = port; next.lan = lan; next.auth = auth;
            if persist { self.save(&next)?; }
            if persist && changed { inner.config_revision = inner.config_revision.wrapping_add(1); }
            inner.settings = next;
            inner.shutdown = Some(tx);
            inner.listener_released = Some(listener_released);
            inner.startup_error = None;
            inner.run_id = inner.run_id.wrapping_add(1);
            inner.run_id
        };
        let gateway = self.clone();
        let listener = ReleasingListener { socket: Some(socket), released: Some(released) }.tap_io(|_| {});
        tokio::spawn(async move {
            let result = axum::serve(listener, build_router(gateway.clone()).into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(async { let _ = rx.await; }).await;
            finish_run(&gateway, run_id, result);
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{store::Store, vault::transaction::CredentialStore};
    use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicUsize, Ordering}};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[derive(Default)]
    struct MemoryAccess { value: Mutex<Option<String>>, fail: AtomicBool, fail_set: AtomicBool, reads: AtomicUsize }
    impl CredentialStore for MemoryAccess {
        fn get(&self, id: &str) -> Result<Option<String>, String> {
            assert_eq!(id, SECRET_KEY); self.reads.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) { Err("fixture access failure".into()) } else { Ok(lock(&self.value).clone()) }
        }
        fn set(&self, id: &str, value: &str) -> Result<(), String> {
            assert_eq!(id, SECRET_KEY);
            if self.fail_set.load(Ordering::SeqCst) { return Err("fixture access write failure".into()); }
            *lock(&self.value) = Some(value.into()); Ok(())
        }
        fn delete(&self, id: &str) -> Result<(), String> {
            assert_eq!(id, SECRET_KEY); *lock(&self.value) = None; Ok(())
        }
    }
    fn gateway(store: Store, fail: bool) -> Gateway {
        let access = Arc::new(MemoryAccess { fail: AtomicBool::new(fail), ..Default::default() });
        with_access(store, access)
    }
    fn with_access(store: Store, access: Arc<MemoryAccess>) -> Gateway {
        Gateway::with_credentials(Arc::new(Mutex::new(store)), access.clone(), access).unwrap()
    }
    fn free_port() -> u16 {
        std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port()
    }
    async fn healthy(port: u16) {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
        stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut status = [0u8; 12];
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.read_exact(&mut status)).await.unwrap().unwrap();
        assert_eq!(&status, b"HTTP/1.1 200");
    }
    #[tokio::test]
    async fn immediate_stop_rebind_and_concurrent_restarts() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let g = gateway(Store::open_memory().unwrap(), false); let port = free_port();
            g.start(port, false, true).await.unwrap();
            assert!(g.access.get(SECRET_KEY).unwrap().is_some());
            assert!(g.start(port, false, true).await.is_err());
            let (a, b) = tokio::join!(g.restart(port, false, true), g.restart(port, false, true));
            a.unwrap(); b.unwrap(); healthy(port).await;
            let run_id = lock(&g.inner).run_id;
            finish_run(&g, run_id.wrapping_sub(1), Err(io::Error::other("stale run")));
            assert_eq!(lock(&g.inner).run_id, run_id); assert!(lock(&g.inner).shutdown.is_some());
            assert!(lock(&g.inner).startup_error.is_none());
            let (a, b) = tokio::join!(g.stop(), g.start(port, false, true)); a.unwrap(); b.unwrap();
            healthy(port).await; g.stop().await.unwrap();
            let _rebound = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await.unwrap();
            assert!(lock(&g.inner).shutdown.is_none());
        }).await.expect("lifecycle did not release its lock or listener");
    }
    #[tokio::test]
    async fn failed_restart_restores_service_and_persisted_settings() {
        let path = std::env::temp_dir().join(format!("gateway-lifecycle-{}.db", crate::vault::new_id()));
        let g = gateway(Store::open(&path).unwrap(), false); let port = free_port();
        g.start(port, false, true).await.unwrap();
        let before = lock(&g.store).get_setting(super::super::CONFIG_KEY).unwrap();
        let revision = lock(&g.inner).config_revision;
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        assert!(g.restart(occupied.local_addr().unwrap().port(), false, true).await.unwrap_err().contains("已恢复"));
        healthy(port).await;
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TRIGGER refuse_settings BEFORE INSERT ON settings BEGIN SELECT RAISE(ABORT, 'fixture save failure'); END;").unwrap();
        assert!(g.restart(free_port(), false, true).await.unwrap_err().contains("已恢复"));
        healthy(port).await;
        assert_eq!(lock(&g.inner).settings.port, port);
        assert_eq!(lock(&g.inner).config_revision, revision);
        assert_eq!(lock(&g.store).get_setting(super::super::CONFIG_KEY).unwrap(), before);
        g.stop().await.unwrap(); drop(db); drop(g);
        for suffix in ["", "-wal", "-shm"] { let _ = std::fs::remove_file(format!("{}{suffix}", path.display())); }
    }
    #[tokio::test]
    async fn invalid_parameters_and_access_failure_do_not_publish_listener() {
        let g = gateway(Store::open_memory().unwrap(), true); let port = free_port();
        assert!(g.start(0, false, true).await.is_err());
        assert!(g.start(port, true, false).await.is_err());
        assert!(g.start(port, false, true).await.is_err());
        assert!(lock(&g.inner).shutdown.is_none());
        let _rebound = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await.unwrap();
    }
    #[tokio::test]
    async fn access_failures_leave_the_existing_run_uninterrupted() {
        let access = Arc::new(MemoryAccess::default());
        let g = with_access(Store::open_memory().unwrap(), access.clone()); let port = free_port();
        g.start(port, false, true).await.unwrap(); assert_eq!(access.reads.load(Ordering::SeqCst), 1);
        let (run, revision) = { let i = lock(&g.inner); (i.run_id, i.config_revision) };
        let saved = lock(&g.store).get_setting(super::super::CONFIG_KEY).unwrap();
        for fail_read in [true, false] {
            access.fail.store(fail_read, Ordering::SeqCst);
            access.fail_set.store(!fail_read, Ordering::SeqCst);
            if !fail_read { *lock(&access.value) = None; }
            let reads = access.reads.load(Ordering::SeqCst);
            assert!(g.restart(port, false, true).await.is_err()); healthy(port).await;
            assert_eq!(access.reads.load(Ordering::SeqCst), reads + 1);
            let i = lock(&g.inner); assert_eq!(i.run_id, run); assert_eq!(i.config_revision, revision);
            assert_eq!(i.settings.port, port); assert!(i.shutdown.is_some()); assert!(i.startup_error.is_none());
        }
        assert_eq!(lock(&g.store).get_setting(super::super::CONFIG_KEY).unwrap(), saved);
        access.fail_set.store(false, Ordering::SeqCst);
        let reads = access.reads.load(Ordering::SeqCst); g.restart(port, false, true).await.unwrap();
        assert_eq!(access.reads.load(Ordering::SeqCst), reads + 1); healthy(port).await; g.stop().await.unwrap();
    }
    #[tokio::test]
    async fn listener_releases_before_active_request_drains() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let socket = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let addr = socket.local_addr().unwrap();
            let (released, ack) = oneshot::channel(); let (stop, stopped) = oneshot::channel();
            let entered = Arc::new(tokio::sync::Notify::new()); let finish = Arc::new(tokio::sync::Notify::new());
            let (entry, drain) = (entered.clone(), finish.clone());
            let router = axum::Router::new().route("/wait", axum::routing::get(move || {
                let (entry, drain) = (entry.clone(), drain.clone());
                async move { entry.notify_one(); drain.notified().await; "done" }
            }));
            let listener = ReleasingListener { socket: Some(socket), released: Some(released) };
            let server = tokio::spawn(async move {
                axum::serve(listener, router).with_graceful_shutdown(async { let _ = stopped.await; }).await
            });
            let mut client = TcpStream::connect(addr).await.unwrap();
            client.write_all(b"GET /wait HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
            entered.notified().await; stop.send(()).unwrap(); ack.await.unwrap();
            let _rebound = TcpListener::bind(addr).await.unwrap(); assert!(!server.is_finished());
            finish.notify_one(); server.await.unwrap().unwrap();
        }).await.expect("listener release was blocked by an active request");
    }
    #[tokio::test]
    async fn cancelled_stop_retains_release_acknowledgement() {
        let g = gateway(Store::open_memory().unwrap(), false);
        let (tx, rx) = oneshot::channel(); let (released, ack) = oneshot::channel();
        { let mut inner = lock(&g.inner); inner.shutdown = Some(tx); inner.listener_released = Some(ack); }
        let copy = g.clone(); let task = tokio::spawn(async move { copy.stop().await });
        rx.await.unwrap(); task.abort(); assert!(task.await.unwrap_err().is_cancelled());
        assert!(lock(&g.inner).listener_released.is_some());
        released.send(()).unwrap(); g.stop().await.unwrap();
        assert!(lock(&g.inner).listener_released.is_none());
    }
}
