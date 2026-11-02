//! 同步协议 v1。
//!
//! 分层：
//! - [`crypto`]：HKDF + AES-256-GCM 与同步码载荷加密。
//! - [`code`]：`GT1.` 同步码与 Gist 文件命名。
//! - [`model`]：白名单载荷、十进制口径与校验。
//! - [`platform`]：跨来源合并键 platformId。
//! - [`builder`]：从本机 ChannelView + 历史/签到构建快照。
//! - [`merge`]：多来源合并（取最新整条记录，不求和）。
//! - [`gist`]：GitHub Gist 传输与大小/截断防护。
//! - [`publish`]：发布、序号水位、建组与读组。

pub mod builder;
pub mod code;
pub mod crypto;
pub mod error;
pub mod gist;
pub mod merge;
pub mod model;
pub mod platform;
pub mod publish;
pub mod profile;
pub mod qr;

pub use builder::{build_snapshot, BuildInput};
pub use code::{SyncCode, RelayKind};
pub use crypto::{Envelope, KEY_LEN, RELAY_ID_LEN};
pub use error::SyncError;
pub use gist::{GetOutcome, GistClient, GistFile, GistLoad};
pub use merge::{merge_snapshots, MergedSnapshot};
pub use model::{
    ChannelKind, ChannelSnapshot, CheckinItem, CheckinSnapshot, CheckinStatus, HistoryPoint,
    OtherMoney, SyncSnapshot, TotalsSnapshot,
};
pub use platform::platform_id_for;
pub use publish::{
    apply_watermarks, commit_publish, create_relay, fetch_sources, init_relay_state,
    prepare_publish, publish_snapshot, read_group, send_prepared, CreateRelayInput,
    CreateRelayOutcome, DecryptedSource, FetchedSources, PreparedPublish, PublishInput,
    PublishOutcome, PublishState, ReadInput, ReadOutcome, SeqDecision, Watermark,
};
