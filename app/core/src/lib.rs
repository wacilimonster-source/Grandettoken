//! TokenScope 纯逻辑层。
//!
//! 刻意不依赖 Tauri / WebView2:这样 `cargo test` 只需链接 serde、rusqlite 等
//! 基础库,测试秒级完成,也不会因为 WebView2 运行时缺失而启动失败。

// Trae 的自定义模型条目是一个 ~40 字段的 json! 字面量,默认递归上限(128)不够 ——
// serde_json 的 json_internal! 每个键都要展开一层。抬高上限,不改宏写法。
#![recursion_limit = "256"]

pub mod antigravity;
pub mod antigravity_accounts;
mod antigravity_console;
pub mod appauth;
pub mod backup_crypto;
pub mod checkin;
pub mod codex;
pub mod codex_reset;
pub mod config;
pub mod custom;
pub mod diaglog;
pub mod doubao;
pub mod doubao_client;
pub mod errcodes;
pub mod fetch;
pub mod fx;
pub mod gateway;
pub mod gateway_proto;
pub mod grants;
pub mod google_cloud;
pub mod http_pool;
pub mod inject;
pub(crate) mod lockx;
pub mod minimax;
pub mod models;
pub mod peak;
pub mod providers;
mod progress;
pub mod proxy;
pub mod qoder;
pub mod qoderweb;
pub mod recovery;
pub mod release_links;
pub mod secrets;
pub mod store;
pub use store::gateway as store_gateway;
pub mod sync;
pub mod tray_icon;
pub mod vault;
pub mod view;
pub mod weblogin;
pub mod workbuddy;
mod workbuddy_cycle;
pub mod yamledge;
