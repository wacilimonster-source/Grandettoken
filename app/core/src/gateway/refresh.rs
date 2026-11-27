//! 网络等待不持配置锁；只有当前配置、凭据版本和最新刷新票据才能提交派生结果。
use super::{http::{fetch_configured_models, now_unix, ModelFetchResult}, lock, Gateway, Published, CONFIG_KEY};
use crate::gateway_proto::{error::redact_secrets, GatewaySourceKind};

struct Ticket {
    channel: Published,
    revision: u64,
    sequence: u64,
    vault_updated_at: Option<i64>,
}

fn capture(g: &Gateway, id: &str) -> Result<Option<Ticket>, String> {
    let mut inner = lock(&g.inner);
    let Some(channel) = inner.settings.channels.iter().find(|c| c.channel_id == id)
        .or_else(|| inner.settings.channels.iter().find(|c| c.vault_id == id)).cloned()
        else { return Ok(None); };
    let vault_updated_at = if channel.no_upstream_key { None } else {
        Some(lock(&g.store).vault_get(&channel.vault_id)?
            .ok_or("关联的密钥箱条目不存在，请重新关联密钥")?.updated_at)
    };
    let sequence = inner.refresh_sequences.entry(channel.channel_id.clone()).or_default();
    *sequence = sequence.checked_add(1).ok_or("刷新序号已耗尽，请重启应用")?;
    Ok(Some(Ticket { channel, sequence: *sequence, revision: inner.config_revision, vault_updated_at }))
}

async fn read_key(g: &Gateway, channel: &Published) -> Result<Option<String>, String> {
    if channel.no_upstream_key {
        return if channel.kind() == GatewaySourceKind::OpenAiCompat { Ok(None) }
            else { Err("只有 OpenAI 兼容端点允许免上游密钥模式".into()) };
    }
    let id = channel.vault_id.trim().to_string();
    if id.is_empty() { return Err("请选择关联的密钥箱条目".into()); }
    let credentials = g.credentials.clone();
    tokio::task::spawn_blocking(move || {
        let key = credentials.get(&id).map_err(|_| "读取密钥箱凭据失败，请重试".to_string())?
            .ok_or("所选密钥箱条目缺少有效密钥")?;
        if key.trim().is_empty() { return Err("所选密钥箱条目的密钥为空".into()); }
        Ok(Some(key))
    }).await.map_err(|_| "读取密钥箱凭据任务失败，请重试".to_string())?
}

fn commit(g: &Gateway, ticket: Ticket, result: Result<ModelFetchResult, String>) -> Result<bool, String> {
    let mut inner = lock(&g.inner);
    if inner.config_revision != ticket.revision
        || inner.refresh_sequences.get(&ticket.channel.channel_id) != Some(&ticket.sequence) {
        return Ok(false);
    }
    let mut next = inner.settings.clone();
    let Some(slot) = next.channels.iter_mut().find(|c| c.channel_id == ticket.channel.channel_id)
        else { return Ok(false); };
    // inner → store 与配置提交一致；凭据 RPC 必须在这两个锁之外完成。
    let store = lock(&g.store);
    if !slot.no_upstream_key && store.vault_get(&slot.vault_id)?.map(|v| v.updated_at) != ticket.vault_updated_at {
        return Ok(false);
    }
    match result {
        Ok(res) => {
            slot.models = res.fetched;
            slot.last_status = "ok".into();
            slot.last_error = res.probe_note;
        }
        Err(error) => {
            slot.last_status = "error".into();
            slot.last_error = Some(error);
        }
    }
    let now = now_unix();
    slot.last_checked_at = Some(now);
    if let Some(updated_at) = ticket.vault_updated_at { slot.updated_at = updated_at; }
    else if slot.no_upstream_key { slot.updated_at = now; }
    // allowed_models 是用户意图：暂时消失的模型重新出现时仍应沿用原选择。
    let encoded = serde_json::to_string(&next).map_err(|e| e.to_string())?;
    store.set_setting(CONFIG_KEY, &encoded)?;
    inner.settings = next;
    Ok(true)
}

impl Gateway {
    /// true 表示结果已保存（含探测错误）；false 表示目标消失或票据失效，不自动重试。
    pub async fn refresh_channel(&self, id: &str) -> Result<bool, String> {
        let g = self.clone();
        let id = id.to_string();
        let Some(ticket) = tokio::task::spawn_blocking(move || capture(&g, &id)).await
            .map_err(|_| "读取网关刷新状态失败，请重试".to_string())?? else { return Ok(false); };
        let key = read_key(self, &ticket.channel).await;
        let result = match &key {
            Ok(key) => match self.clients.client_async(ticket.channel.system_proxy).await {
                Ok(client) => fetch_configured_models(&client, &ticket.channel, key.as_deref()).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error.clone()),
        }.map_err(|error| {
            let error = if let Ok(Some(key)) = &key { error.replace(key.as_str(), "***") } else { error };
            redact_secrets(&error)
        });
        // 锁外复读补充时间戳检查；同秒 ABA 由凭据写入前/后的 config_revision 失效覆盖。
        if read_key(self, &ticket.channel).await != key { return Ok(false); }
        let g = self.clone();
        tokio::task::spawn_blocking(move || commit(&g, ticket, result)).await
            .map_err(|_| "保存网关刷新结果失败，请重试".to_string())?
    }

    /// 有界、顺序刷新；失败继续处理其他渠道并返回汇总，由宿主展示同步状态。
    pub async fn sync_vault(&self, vault_id: &str) -> Result<(), String> {
        let ids: Vec<String> = lock(&self.inner).settings.channels.iter()
            .filter(|c| !c.no_upstream_key && c.vault_id == vault_id)
            .map(|c| c.channel_id.clone()).collect();
        let mut errors = Vec::new();
        for id in ids {
            match self.refresh_channel(&id).await {
                Ok(true) => {},
                Ok(false) => errors.push(format!("{id}：配置或凭据已变化，旧刷新已丢弃，请重新刷新")),
                Err(error) => errors.push(format!("{id}：{}", redact_secrets(&error))),
            }
        }
        if errors.is_empty() { Ok(()) }
        else { Err(format!("网关自动同步未完成：{}", errors.join("；"))) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{store::{Store, VaultItem}, vault::transaction::CredentialStore};
    use axum::{http::HeaderMap, routing::get, Router};
    use serde_json::{json, Value};
    use std::{sync::{Arc, Mutex}, time::Duration};
    use tokio::sync::{mpsc, oneshot};

    struct Memory(Mutex<Option<String>>);
    impl CredentialStore for Memory {
        fn get(&self, _: &str) -> Result<Option<String>, String> { Ok(lock(&self.0).clone()) }
        fn set(&self, _: &str, value: &str) -> Result<(), String> { *lock(&self.0) = Some(value.into()); Ok(()) }
        fn delete(&self, _: &str) -> Result<(), String> { *lock(&self.0) = None; Ok(()) }
    }
    fn fixture(store: Store, url: &str) -> (Gateway, Arc<Memory>) {
        let credentials = Arc::new(Memory(Mutex::new(Some("synthetic-key-A".into()))));
        let g = Gateway::with_credentials(Arc::new(Mutex::new(store)), credentials.clone(), credentials.clone()).unwrap();
        lock(&g.store).vault_upsert(&VaultItem {
            id: "v".into(), platform: "openai".into(), label: "fixture".into(), hint: "***".into(),
            note: String::new(), api_endpoint: url.into(), website: String::new(), created_at: 10, updated_at: 10,
        }).unwrap();
        let channel: Published = serde_json::from_value(json!({
            "channelId":"ch", "vaultId":"v", "source":"openai_compat", "template":"custom",
            "baseUrl":url, "alias":"original", "host":"127.0.0.1", "models":["old"],
            "manualModels":["manual"], "allowedModels":["old","new","manual"], "systemProxy":false,
            "updatedAt":1, "lastStatus":"ok", "lastCheckedAt":1
        })).unwrap();
        lock(&g.inner).settings.channels.push(channel);
        g.save(&lock(&g.inner).settings).unwrap();
        (g, credentials)
    }
    type Request = (HeaderMap, oneshot::Sender<Vec<String>>);
    struct Upstream { url: String, requests: mpsc::UnboundedReceiver<Request>, task: tokio::task::JoinHandle<()> }
    impl Drop for Upstream { fn drop(&mut self) { self.task.abort(); } }
    impl Upstream {
        async fn start() -> Self {
            let (tx, requests) = mpsc::unbounded_channel::<Request>();
            let router = Router::new().route("/v1/models", get(move |headers: HeaderMap| {
                let tx = tx.clone();
                async move {
                    let (done, wait) = oneshot::channel();
                    tx.send((headers, done)).unwrap();
                    let models = wait.await.unwrap();
                    axum::Json(json!({"data": models.into_iter().map(|id| json!({"id":id})).collect::<Vec<Value>>()}))
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/v1", listener.local_addr().unwrap());
            let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
            Self { url, requests, task }
        }
        async fn request(&mut self) -> Request {
            tokio::time::timeout(Duration::from_secs(5), self.requests.recv()).await.unwrap().unwrap()
        }
    }
    fn refresh(g: &Gateway) -> tokio::task::JoinHandle<Result<bool, String>> {
        let g = g.clone(); tokio::spawn(async move { g.refresh_channel("ch").await })
    }
    async fn finish(task: tokio::task::JoinHandle<Result<bool, String>>) -> Result<bool, String> {
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap()
    }
    fn snapshot(g: &Gateway) -> Value { serde_json::to_value(&lock(&g.inner).settings).unwrap() }

    #[tokio::test]
    async fn real_http_refresh_preserves_choices_and_only_merges_probe_fields() {
        let mut up = Upstream::start().await;
        let (g, _) = fixture(Store::open_memory().unwrap(), &up.url);
        let before = snapshot(&g);
        let first = refresh(&g);
        let (headers, done) = up.request().await;
        assert_eq!(headers["authorization"], "Bearer synthetic-key-A");
        done.send(vec!["new".into()]).unwrap();
        assert!(finish(first).await.unwrap());
        let after = snapshot(&g);
        let channel = &after["channels"][0];
        let mut owned_only = before;
        for field in ["models", "lastStatus", "lastError", "lastCheckedAt", "updatedAt"] {
            owned_only["channels"][0][field] = channel[field].clone();
        }
        assert_eq!(after, owned_only);
        assert_eq!(channel["models"], json!(["new"]));
        assert_eq!(channel["allowedModels"], json!(["old","new","manual"]));
        assert_eq!(channel["manualModels"], json!(["manual"]));
        assert_eq!(channel["alias"], "original");
        assert_eq!(channel["updatedAt"], 10);
        assert_eq!(channel["lastStatus"], "ok");
        assert!(channel["lastCheckedAt"].as_i64().unwrap() > 1);
        let second = refresh(&g);
        up.request().await.1.send(vec!["old".into(), "new".into()]).unwrap();
        assert!(finish(second).await.unwrap());
        assert!(super::super::effective_models(&lock(&g.inner).settings.channels[0]).contains(&"old".into()));
    }

    #[tokio::test]
    async fn configuration_credential_changes_and_same_id_republish_discard_old_http_results() {
        for change in ["config", "republish", "unpublish", "timestamp", "same-second-key", "aba", "delete"] {
            let mut up = Upstream::start().await;
            let (g, credentials) = fixture(Store::open_memory().unwrap(), &up.url);
            let old = refresh(&g);
            let (_, done) = up.request().await;
            match change {
                "config" | "republish" => {
                    let mut inner = lock(&g.inner);
                    let mut channel = inner.settings.channels[0].clone();
                    channel.alias = "edited".into(); channel.system_proxy = true;
                    channel.base_url = "http://127.0.0.1:1/v1".into(); channel.vault_id = "B".into();
                    channel.allowed_models = Some(vec!["manual".into()]);
                    if change == "republish" { inner.settings.channels.clear(); }
                    inner.settings.channels = vec![channel]; inner.config_revision += 1;
                }
                "unpublish" => {
                    let mut inner = lock(&g.inner);
                    inner.settings.channels.clear(); inner.config_revision += 1;
                }
                "timestamp" => {
                    let store = lock(&g.store); let mut v = store.vault_get("v").unwrap().unwrap();
                    v.updated_at += 1; store.vault_upsert(&v).unwrap();
                }
                "same-second-key" => { credentials.set("v", "synthetic-key-B").unwrap(); }
                "aba" => {
                    lock(&g.inner).config_revision += 1;
                    credentials.set("v", "synthetic-key-B").unwrap();
                    credentials.set("v", "synthetic-key-A").unwrap();
                    lock(&g.inner).config_revision += 1;
                }
                "delete" => { lock(&g.store).vault_delete("v").unwrap(); }
                _ => unreachable!(),
            }
            let expected = snapshot(&g);
            done.send(vec!["stale".into()]).unwrap();
            assert!(!finish(old).await.unwrap(), "{change}");
            assert_eq!(snapshot(&g), expected, "{change}");
        }
    }

    #[tokio::test]
    async fn newer_channel_refresh_wins_when_http_completes_in_reverse_order() {
        let mut up = Upstream::start().await;
        let (g, _) = fixture(Store::open_memory().unwrap(), &up.url);
        let old = refresh(&g); let (_, first) = up.request().await;
        let new = refresh(&g); let (_, second) = up.request().await;
        second.send(vec!["new".into()]).unwrap(); assert!(finish(new).await.unwrap());
        let expected = snapshot(&g);
        first.send(vec!["stale".into()]).unwrap(); assert!(!finish(old).await.unwrap());
        assert_eq!(snapshot(&g), expected);
    }

    #[tokio::test]
    async fn sync_save_failure_is_returned_without_memory_changes_and_retry_recovers() {
        let mut up = Upstream::start().await;
        let path = std::env::temp_dir().join(format!("gateway-refresh-{}.db", crate::vault::new_id()));
        {
            let (g, _) = fixture(Store::open(&path).unwrap(), &up.url);
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch("CREATE TRIGGER reject_gateway BEFORE INSERT ON settings WHEN NEW.key='local_gateway_v1' BEGIN SELECT RAISE(FAIL,'synthetic save failure'); END;").unwrap();
            let expected = snapshot(&g);
            let disk = lock(&g.store).get_setting(CONFIG_KEY).unwrap();
            for blocked in [true, false] {
                let copy = g.clone(); let task = tokio::spawn(async move { copy.sync_vault("v").await });
                up.request().await.1.send(vec!["new".into()]).unwrap();
                let result = tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
                if blocked {
                    assert!(result.unwrap_err().contains("synthetic save failure"));
                    assert_eq!(snapshot(&g), expected);
                    assert_eq!(lock(&g.store).get_setting(CONFIG_KEY).unwrap(), disk);
                    db.execute_batch("DROP TRIGGER reject_gateway;").unwrap();
                } else { assert!(result.is_ok()); assert_eq!(snapshot(&g)["channels"][0]["models"], json!(["new"])); }
            }
        }
        std::fs::remove_file(path).unwrap();
    }
}
