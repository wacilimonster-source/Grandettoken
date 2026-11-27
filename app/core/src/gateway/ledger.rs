use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use tokio::sync::{mpsc, oneshot};

use super::{http::now_unix, lock, Gateway};
use crate::{
    store::Store,
    store_gateway::{GatewayUsageRow, GATEWAY_LEDGER_KEEP_DAYS},
};

const CAPACITY: usize = 256;
type Reply = oneshot::Sender<Result<(), String>>;

enum Message {
    Row(GatewayUsageRow),
    Flush(Reply),
    Clear(Reply),
}

#[derive(Default)]
pub(super) struct Ledger {
    sender: Mutex<Option<mpsc::Sender<Message>>>,
    writes: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}

impl Ledger {
    fn sender(&self, store: &Arc<Mutex<Store>>) -> mpsc::Sender<Message> {
        let mut sender = lock(&self.sender);
        if let Some(open) = sender.as_ref().filter(|sender| !sender.is_closed()) {
            return open.clone();
        }
        let (tx, rx) = mpsc::channel(CAPACITY);
        tokio::spawn(run(store.clone(), self.writes.clone(), self.dropped.clone(), rx));
        *sender = Some(tx.clone());
        tx
    }

    pub(super) fn push(&self, store: &Arc<Mutex<Store>>, row: GatewayUsageRow) {
        if tokio::runtime::Handle::try_current().is_err() {
            if let Err(error) = persist(&lock(store), &self.writes, &row) {
                crate::diaglog::write_keyed("gateway", "ledger", &error);
            }
        } else if self.sender(store).try_send(Message::Row(row)).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            crate::diaglog::write_keyed(
                "gateway",
                "ledger-queue",
                "网关用量台账队列暂不可用，已丢弃一条记录",
            );
        }
    }

    async fn barrier(
        sender: mpsc::Sender<Message>,
        message: impl FnOnce(Reply) -> Message,
    ) -> Result<(), String> {
        let (reply, done) = oneshot::channel();
        sender
            .send(message(reply))
            .await
            .map_err(|_| "网关用量台账队列已关闭".to_string())?;
        done.await
            .map_err(|_| "网关用量台账写入未完成".to_string())?
    }
}

fn persist(db: &Store, writes: &AtomicU64, row: &GatewayUsageRow) -> Result<(), String> {
    db.gateway_usage_insert(row)?;
    if writes.fetch_add(1, Ordering::Relaxed).wrapping_add(1) % 100 == 0 {
        db.gateway_usage_prune(now_unix() - GATEWAY_LEDGER_KEEP_DAYS * 86_400)?;
    }
    Ok(())
}

async fn run(
    store: Arc<Mutex<Store>>,
    writes: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    mut rx: mpsc::Receiver<Message>,
) {
    let mut pending = Vec::with_capacity(32);
    let mut last_error = None;
    while rx.recv_many(&mut pending, 32).await != 0 {
        let store = store.clone();
        let writes = writes.clone();
        let dropped = dropped.clone();
        let mut batch = std::mem::take(&mut pending);
        let mut error = last_error.take();
        match tokio::task::spawn_blocking(move || {
            let db = lock(&store);
            for message in batch.drain(..) {
                match message {
                    Message::Row(row) => {
                        if let Err(e) = persist(&db, &writes, &row) {
                            crate::diaglog::write_keyed("gateway", "ledger", &e);
                            error = Some(e);
                        }
                    }
                    Message::Flush(reply) => {
                        let result = error.take().map_or(Ok(()), Err);
                        if let Err(Err(e)) = reply.send(result) {
                            error = Some(e);
                        }
                    }
                    Message::Clear(reply) => {
                        let result = db.gateway_usage_clear().map(|_| {
                            writes.store(0, Ordering::Relaxed);
                            dropped.store(0, Ordering::Relaxed);
                            error = None;
                        });
                        if let Err(Err(e)) = reply.send(result) {
                            crate::diaglog::write_keyed("gateway", "ledger-clear", &e);
                        }
                    }
                }
            }
            (batch, error)
        })
        .await
        {
            Ok((batch, error)) => {
                pending = batch;
                last_error = error;
            }
            Err(e) => {
                let message = format!("网关用量台账写入任务异常: {e}");
                crate::diaglog::write_keyed("gateway", "ledger", &message);
                last_error = Some(message);
            }
        }
    }
}

impl Gateway {
    pub fn dropped_usage_records(&self) -> u64 {
        self.ledger.dropped.load(Ordering::Relaxed)
    }

    pub async fn flush_ledger(&self) -> Result<(), String> {
        let sender = lock(&self.ledger.sender).clone();
        match sender {
            Some(sender) => Ledger::barrier(sender, Message::Flush).await,
            None => Ok(()),
        }
    }

    pub async fn clear_ledger(&self) -> Result<(), String> {
        Ledger::barrier(self.ledger.sender(&self.store), Message::Clear).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn full_queue_reports_loss_until_clear() {
        let store = Arc::new(Mutex::new(Store::open_memory().unwrap()));
        let ledger = Ledger::default();
        let row = GatewayUsageRow {
            ts: now_unix(), channel_id: "c".into(), channel_alias: "fixture".into(),
            route_group: None, model: "fixture".into(), protocol: "openai_chat".into(),
            stream: false, status: 200, latency_ms: 1, first_byte_ms: None,
            prompt_tokens: None, cached_prompt_tokens: None, cache_creation_tokens: None,
            completion_tokens: None, reasoning_tokens: None, failover_count: 0, masked_count: 0,
            estimated_cost_cny: None, error_code: None, agent: None,
        };
        // Current-thread runtime sees no yield until CAPACITY + 1 records are offered.
        for _ in 0..=CAPACITY { ledger.push(&store, row.clone()); }
        Ledger::barrier(ledger.sender(&store), Message::Flush).await.unwrap();
        assert_eq!(ledger.dropped.load(Ordering::Relaxed), 1);
        assert_eq!(lock(&store).gateway_usage_summary(0, 0).unwrap().total_requests, CAPACITY as u64);
        Ledger::barrier(ledger.sender(&store), Message::Clear).await.unwrap();
        assert_eq!(ledger.dropped.load(Ordering::Relaxed), 0);
    }
}
