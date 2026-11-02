//! 发布与读取同步组。
//!
//! 发布侧的关键约束：
//! - 每次只 PATCH 自己的来源文件；不整份回写 files，不回发合并结果。
//! - 先把 seq 与本次密文持久化，再发送；超时重试复用原密文。
//! - 读取侧按来源维护 seq 水位；更小的 seq 忽略，同 seq 同密文幂等，
//!   同 seq 不同密文报冲突。
//!
//! 为了让 Tauri 侧**不把 `MutexGuard<Store>` 跨 await 持有**，网络与本地状态
//! 拆成三段：`prepare_publish`（同步）→ `send_prepared`（异步）→
//! `commit_publish`（同步）。读取同理：`fetch_sources`（异步）→
//! `apply_watermarks`（同步）→ `merge_snapshots`。

use serde::{Deserialize, Serialize};

use crate::store::Store;

use super::code::{self, SyncCode};
use super::crypto;
use super::error::{SyncError, CONFLICT, ENVELOPE_INVALID, STORE};
use super::gist::{GetOutcome, GistClient};
use super::merge::{merge_snapshots, MergedSnapshot};
use super::model::SyncSnapshot;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PublishState {
    pub source: String,
    pub seq: u64,
    pub last_at: Option<i64>,
    pub pending: Option<PendingPublish>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPublish {
    pub seq: u64,
    pub envelope: String,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Watermark {
    pub seq: u64,
    pub hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqDecision {
    Accept,
    SkipOld,
    Duplicate,
}

#[derive(Debug, Clone)]
pub struct PreparedPublish {
    pub source: String,
    pub pending: PendingPublish,
    pub reused: bool,
}

#[derive(Debug, Clone)]
pub struct DecryptedSource {
    pub source: String,
    pub seq: u64,
    pub hash: String,
    pub snapshot: SyncSnapshot,
}

#[derive(Debug, Clone)]
pub enum FetchedSources {
    NotModified {
        etag: Option<String>,
    },
    Loaded {
        sources: Vec<DecryptedSource>,
        failed_sources: Vec<String>,
        etag: Option<String>,
        files: usize,
        owner_id: Option<i64>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishOutcome {
    pub seq: u64,
    pub at: i64,
    pub bytes: usize,
    pub reused_pending: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ReadOutcome {
    NotModified {
        etag: Option<String>,
    },
    Loaded {
        merged: MergedSnapshot,
        etag: Option<String>,
        files: usize,
        skipped_old: usize,
        failed_sources: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRelayOutcome {
    pub gist_id: String,
    pub code_text: String,
    pub seq: u64,
    pub bytes: usize,
    /// 供调用方在本地初始化发布状态；不展示给手机。
    #[serde(skip_serializing)]
    pub source: String,
    #[serde(skip_serializing)]
    pub hash: String,
}

pub struct PublishInput<'a> {
    pub client: &'a GistClient,
    pub gist_id: &'a str,
    pub master: &'a [u8; crypto::KEY_LEN],
    pub relay_id: &'a [u8; crypto::RELAY_ID_LEN],
    pub key_epoch: u8,
    pub store: &'a Store,
    pub snapshot: SyncSnapshot,
    pub now: i64,
}

pub struct CreateRelayInput<'a> {
    pub client: &'a GistClient,
    pub master: &'a [u8; crypto::KEY_LEN],
    pub snapshot: SyncSnapshot,
    pub now: i64,
    pub description: &'a str,
}

pub struct ReadInput<'a> {
    pub client: &'a GistClient,
    pub gist_id: &'a str,
    pub master: &'a [u8; crypto::KEY_LEN],
    pub relay_id: &'a [u8; crypto::RELAY_ID_LEN],
    pub key_epoch: u8,
    pub store: &'a Store,
    pub etag: Option<&'a str>,
}

// ── 发布三步 ──────────────────────────────────────────────────────────────

/// 同步：读取/创建 pending、生成密文、先落盘。调用方可以只在这一小段持有 Store 锁。
pub fn prepare_publish(
    master: &[u8; crypto::KEY_LEN],
    relay_id: &[u8; crypto::RELAY_ID_LEN],
    key_epoch: u8,
    store: &Store,
    snapshot: SyncSnapshot,
    now: i64,
) -> Result<PreparedPublish, SyncError> {
    let source = crypto::normalize_source(&snapshot.source)
        .ok_or_else(|| SyncError::new(super::error::SOURCE_INVALID, "写入者 ID 格式不正确"))?;
    let mut state = load_state(store, &source)?;

    if let Some(pending) = state.pending.clone() {
        return Ok(PreparedPublish {
            source,
            pending,
            reused: true,
        });
    }

    let seq = state.seq.saturating_add(1);
    let mut snap = snapshot;
    snap.source = source.clone();
    snap.seq = seq;
    snap.at = now;
    let plaintext = snap.to_bytes()?;
    let envelope = crypto::seal(master, relay_id, &source, key_epoch, &plaintext)?;
    let envelope =
        String::from_utf8(envelope).map_err(|_| SyncError::new(ENVELOPE_INVALID, "密文外壳不是 UTF-8"))?;
    let hash = crypto::hex_encode(&crypto::sha256(envelope.as_bytes()));
    let pending = PendingPublish {
        seq,
        envelope,
        hash,
    };
    state.pending = Some(pending.clone());
    save_state(store, &state)?;
    Ok(PreparedPublish {
        source,
        pending,
        reused: false,
    })
}

/// 异步：只发送已经落盘的那份密文。
pub async fn send_prepared(
    client: &GistClient,
    gist_id: &str,
    prepared: &PreparedPublish,
) -> Result<(), SyncError> {
    let file_name = code::file_name_for_source(&prepared.source)?;
    client
        .patch_file(gist_id, &file_name, &prepared.pending.envelope)
        .await
}

/// 同步：成功后提交 seq/last_at 并清 pending。
pub fn commit_publish(
    store: &Store,
    prepared: &PreparedPublish,
    now: i64,
) -> Result<PublishOutcome, SyncError> {
    let mut state = load_state(store, &prepared.source)?;
    state.seq = prepared.pending.seq;
    state.last_at = Some(now);
    state.pending = None;
    save_state(store, &state)?;
    let _ = save_watermark(
        store,
        &prepared.source,
        &Watermark {
            seq: prepared.pending.seq,
            hash: prepared.pending.hash.clone(),
        },
    );
    Ok(PublishOutcome {
        seq: prepared.pending.seq,
        at: now,
        bytes: prepared.pending.envelope.len(),
        reused_pending: prepared.reused,
    })
}

/// 组合入口，给测试与不关心锁拆分的调用方用。
pub async fn publish_snapshot(input: PublishInput<'_>) -> Result<PublishOutcome, SyncError> {
    let prepared = prepare_publish(
        input.master,
        input.relay_id,
        input.key_epoch,
        input.store,
        input.snapshot,
        input.now,
    )?;
    send_prepared(input.client, input.gist_id, &prepared).await?;
    commit_publish(input.store, &prepared, input.now)
}

// ── 建组 ──────────────────────────────────────────────────────────────────

/// 新建同步组：先建只含占位文件的 secret gist，拿到 ID 后再加密并 PATCH 真正的
/// 来源文件。占位文件随后尽力删除；删不掉也不影响读取（手机只认来源文件名）。
pub async fn create_relay(input: CreateRelayInput<'_>) -> Result<CreateRelayOutcome, SyncError> {
    let source = crypto::normalize_source(&input.snapshot.source)
        .ok_or_else(|| SyncError::new(super::error::SOURCE_INVALID, "写入者 ID 格式不正确"))?;
    let (gist_id, _) = input
        .client
        .create_gist("gt_init.txt", "grandettoken-sync-v1", input.description)
        .await?;
    let relay_id = code::parse_gist_id_hex(&gist_id)?;

    let mut snap = input.snapshot;
    snap.source = source.clone();
    snap.seq = 1;
    snap.at = input.now;
    let plaintext = match snap.to_bytes() {
        Ok(v) => v,
        Err(e) => {
            let _ = input.client.delete_gist(&gist_id).await;
            return Err(e);
        }
    };
    let envelope = match crypto::seal(input.master, &relay_id, &source, 0, &plaintext) {
        Ok(v) => v,
        Err(e) => {
            let _ = input.client.delete_gist(&gist_id).await;
            return Err(e);
        }
    };
    let envelope = String::from_utf8(envelope)
        .map_err(|_| SyncError::new(ENVELOPE_INVALID, "密文外壳不是 UTF-8"))?;
    let file_name = code::file_name_for_source(&source)?;

    if let Err(e) = input
        .client
        .patch_file(&gist_id, &file_name, &envelope)
        .await
    {
        let _ = input.client.delete_gist(&gist_id).await;
        return Err(e);
    }
    let _ = input.client.delete_file(&gist_id, "gt_init.txt").await;

    let hash = crypto::hex_encode(&crypto::sha256(envelope.as_bytes()));
    Ok(CreateRelayOutcome {
        gist_id,
        code_text: SyncCode::new(relay_id, *input.master).encode(),
        seq: 1,
        bytes: envelope.len(),
        source,
        hash,
    })
}

/// 建组成功后初始化本地发布状态（同步、短锁）。
pub fn init_relay_state(
    store: &Store,
    outcome: &CreateRelayOutcome,
    now: i64,
) -> Result<(), SyncError> {
    save_state(
        store,
        &PublishState {
            source: outcome.source.clone(),
            seq: outcome.seq,
            last_at: Some(now),
            pending: None,
        },
    )?;
    save_watermark(
        store,
        &outcome.source,
        &Watermark {
            seq: outcome.seq,
            hash: outcome.hash.clone(),
        },
    )
}

// ── 读取三步 ──────────────────────────────────────────────────────────────

/// 异步：拉取、解密、解析、校验；不碰本地水位。
pub async fn fetch_sources(
    client: &GistClient,
    gist_id: &str,
    master: &[u8; crypto::KEY_LEN],
    relay_id: &[u8; crypto::RELAY_ID_LEN],
    key_epoch: u8,
    etag: Option<&str>,
) -> Result<FetchedSources, SyncError> {
    let outcome = client.get(gist_id, etag).await?;
    let load = match outcome {
        GetOutcome::NotModified { etag } => {
            return Ok(FetchedSources::NotModified { etag });
        }
        GetOutcome::Loaded(load) => load,
    };

    let mut sources = Vec::new();
    let mut failed_sources = Vec::new();
    for file in &load.files {
        let Some(source) = code::source_from_file_name(&file.name) else {
            continue;
        };
        let hash = crypto::hex_encode(&crypto::sha256(file.content.as_bytes()));
        let plaintext = match crypto::open(
            master,
            relay_id,
            &source,
            key_epoch,
            file.content.as_bytes(),
        ) {
            Ok(v) => v,
            Err(e) => {
                failed_sources.push(format!("{source}:{}", e.code));
                continue;
            }
        };
        let snap: SyncSnapshot = match serde_json::from_slice(&plaintext) {
            Ok(v) => v,
            Err(_) => {
                failed_sources.push(format!("{source}:{}", super::error::PAYLOAD_INVALID));
                continue;
            }
        };
        if let Err(e) = snap.validate() {
            failed_sources.push(format!("{source}:{}", e.code));
            continue;
        }
        if snap.source != source {
            failed_sources.push(format!("{source}:{}", super::error::PAYLOAD_INVALID));
            continue;
        }
        sources.push(DecryptedSource {
            source,
            seq: snap.seq,
            hash,
            snapshot: snap,
        });
    }

    Ok(FetchedSources::Loaded {
        sources,
        failed_sources,
        etag: load.etag,
        files: load.files.len(),
        owner_id: load.owner_id,
    })
}

/// 同步：按来源水位过滤，接受则立即落水位。调用方只在这一小段持有 Store 锁。
pub fn apply_watermarks(
    store: &Store,
    sources: Vec<DecryptedSource>,
) -> Result<(Vec<SyncSnapshot>, Vec<String>, usize), SyncError> {
    let mut accepted = Vec::new();
    let mut failed = Vec::new();
    let mut skipped = 0usize;
    for s in sources {
        let watermark = load_watermark(store, &s.source)?;
        match decide_seq(watermark.as_ref(), s.seq, &s.hash) {
            Ok(SeqDecision::Accept) => {
                save_watermark(
                    store,
                    &s.source,
                    &Watermark {
                        seq: s.seq,
                        hash: s.hash.clone(),
                    },
                )?;
                accepted.push(s.snapshot);
            }
            Ok(SeqDecision::SkipOld) | Ok(SeqDecision::Duplicate) => skipped += 1,
            Err(e) => failed.push(format!("{}:{}", s.source, e.code)),
        }
    }
    Ok((accepted, failed, skipped))
}

/// 组合入口，给测试与不关心锁拆分的调用方用。
pub async fn read_group(input: ReadInput<'_>) -> Result<ReadOutcome, SyncError> {
    let fetched = fetch_sources(
        input.client,
        input.gist_id,
        input.master,
        input.relay_id,
        input.key_epoch,
        input.etag,
    )
    .await?;
    let (sources, failed_sources, etag, files) = match fetched {
        FetchedSources::NotModified { etag } => {
            return Ok(ReadOutcome::NotModified { etag });
        }
        FetchedSources::Loaded {
            sources,
            failed_sources,
            etag,
            files,
            owner_id: _,
        } => (sources, failed_sources, etag, files),
    };
    let (accepted, mut watermark_failed, skipped_old) =
        apply_watermarks(input.store, sources)?;
    watermark_failed.extend(failed_sources);
    let merged = merge_snapshots(&accepted, watermark_failed.clone())?;
    Ok(ReadOutcome::Loaded {
        merged,
        etag,
        files,
        skipped_old,
        failed_sources: watermark_failed,
    })
}

// ── 水位与状态 ────────────────────────────────────────────────────────────

pub fn decide_seq(
    prev: Option<&Watermark>,
    seq: u64,
    hash: &str,
) -> Result<SeqDecision, SyncError> {
    let Some(prev) = prev else {
        return Ok(SeqDecision::Accept);
    };
    if seq < prev.seq {
        return Ok(SeqDecision::SkipOld);
    }
    if seq == prev.seq {
        if hash == prev.hash {
            return Ok(SeqDecision::Duplicate);
        }
        return Err(SyncError::new(
            CONFLICT,
            format!("seq={seq} 的密文与已见水位不一致"),
        ));
    }
    Ok(SeqDecision::Accept)
}

fn state_key(source: &str) -> String {
    format!("sync:state:{source}")
}

fn watermark_key(source: &str) -> String {
    format!("sync:wm:{source}")
}

pub fn load_state(store: &Store, source: &str) -> Result<PublishState, SyncError> {
    let raw = store
        .get_setting(&state_key(source))
        .map_err(|e| SyncError::new(STORE, format!("读取同步状态失败: {e}")))?;
    match raw {
        None => Ok(PublishState {
            source: source.to_string(),
            ..Default::default()
        }),
        Some(s) if s.trim().is_empty() => Ok(PublishState {
            source: source.to_string(),
            ..Default::default()
        }),
        Some(s) => {
            let mut state: PublishState = serde_json::from_str(&s)
                .map_err(|e| SyncError::new(STORE, format!("同步状态解析失败: {e}")))?;
            if state.source != source {
                state = PublishState {
                    source: source.to_string(),
                    ..Default::default()
                };
            }
            Ok(state)
        }
    }
}

pub fn save_state(store: &Store, state: &PublishState) -> Result<(), SyncError> {
    let json = serde_json::to_string(state)
        .map_err(|e| SyncError::new(STORE, format!("同步状态序列化失败: {e}")))?;
    store
        .set_setting(&state_key(&state.source), &json)
        .map_err(|e| SyncError::new(STORE, format!("写入同步状态失败: {e}")))
}

pub fn load_watermark(store: &Store, source: &str) -> Result<Option<Watermark>, SyncError> {
    let raw = store
        .get_setting(&watermark_key(source))
        .map_err(|e| SyncError::new(STORE, format!("读取序号水位失败: {e}")))?;
    match raw {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => Ok(serde_json::from_str(&s).ok()),
    }
}

pub fn save_watermark(store: &Store, source: &str, wm: &Watermark) -> Result<(), SyncError> {
    let json = serde_json::to_string(wm)
        .map_err(|e| SyncError::new(STORE, format!("水位序列化失败: {e}")))?;
    store
        .set_setting(&watermark_key(source), &json)
        .map_err(|e| SyncError::new(STORE, format!("写入水位失败: {e}")))
}

pub fn clear_state(store: &Store, source: &str) -> Result<(), SyncError> {
    store
        .delete_setting(&state_key(source))
        .map_err(|e| SyncError::new(STORE, format!("清理同步状态失败: {e}")))?;
    store
        .delete_setting(&watermark_key(source))
        .map_err(|e| SyncError::new(STORE, format!("清理水位失败: {e}")))?;
    Ok(())
}

/// 恢复旧组时，只认本机记录与远端密文一致的发布来源；不推测其他电脑的来源。
pub fn local_recovery_source(
    store: &Store,
    sources: &[DecryptedSource],
) -> Result<(Option<String>, Option<i64>), SyncError> {
    let mut matches = Vec::new();
    for remote in sources {
        let local = load_state(store, &remote.source)?;
        let watermark = load_watermark(store, &remote.source)?;
        let committed = local.seq == remote.seq && watermark.as_ref().is_some_and(|w| {
            w.seq == remote.seq && w.hash == remote.hash
        });
        let pending = local.pending.as_ref().is_some_and(|p| {
            p.seq == remote.seq && p.hash == remote.hash
        });
        if (local.seq > 0 && committed) || pending {
            matches.push((remote.source.clone(), local.last_at));
        }
    }
    Ok(if matches.len() == 1 {
        let (source, last) = matches.remove(0);
        (Some(source), last)
    } else {
        (None, None)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_decisions() {
        let wm = Watermark {
            seq: 5,
            hash: "abc".into(),
        };
        assert_eq!(decide_seq(None, 1, "x").unwrap(), SeqDecision::Accept);
        assert_eq!(decide_seq(Some(&wm), 4, "x").unwrap(), SeqDecision::SkipOld);
        assert_eq!(
            decide_seq(Some(&wm), 5, "abc").unwrap(),
            SeqDecision::Duplicate
        );
        let err = decide_seq(Some(&wm), 5, "different").unwrap_err();
        assert_eq!(err.code, CONFLICT);
        assert_eq!(decide_seq(Some(&wm), 6, "x").unwrap(), SeqDecision::Accept);
    }

    #[test]
    fn recovery_only_reuses_an_unambiguous_matching_local_writer() {
        let store = Store::open_memory().unwrap();
        let source = "s_00112233445566778899aabbccddeeff";
        let remote = DecryptedSource {
            source: source.into(), seq: 3, hash: "same".into(),
            snapshot: SyncSnapshot { v: 1, source: source.into(), seq: 3, at: 100,
                app: "test".into(), channels: vec![], checkin: None, totals: Default::default() },
        };
        assert_eq!(local_recovery_source(&store, &[remote.clone()]).unwrap().0, None);
        save_state(&store, &PublishState { source: source.into(), seq: 3, last_at: Some(100), pending: None }).unwrap();
        save_watermark(&store, source, &Watermark { seq: 3, hash: "different".into() }).unwrap();
        assert_eq!(local_recovery_source(&store, &[remote.clone()]).unwrap().0, None);
        save_watermark(&store, source, &Watermark { seq: 3, hash: "same".into() }).unwrap();
        assert_eq!(local_recovery_source(&store, &[remote.clone()]).unwrap(), (Some(source.into()), Some(100)));
        // Multiple apparently local sources are ambiguous: leave publishing paused.
        assert_eq!(local_recovery_source(&store, &[remote.clone(), remote.clone()]).unwrap().0, None);
        // A network request may have succeeded before the local commit was recorded.
        save_state(&store, &PublishState { source: source.into(), seq: 2, last_at: Some(90),
            pending: Some(PendingPublish { seq: 3, envelope: "ciphertext".into(), hash: "same".into() }) }).unwrap();
        assert_eq!(local_recovery_source(&store, &[remote]).unwrap(), (Some(source.into()), Some(90)));
    }
}
