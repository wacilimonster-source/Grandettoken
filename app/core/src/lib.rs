//! TokenScope 纯逻辑层。
//!
//! 刻意不依赖 Tauri / WebView2:这样 `cargo test` 只需链接 serde、rusqlite 等
//! 基础库,测试秒级完成,也不会因为 WebView2 运行时缺失而启动失败。

// Trae 的自定义模型条目是一个 ~40 字段的 json! 字面量,默认递归上限(128)不够 ——
// serde_json 的 json_internal! 每个键都要展开一层。抬高上限,不改宏写法。
#![recursion_limit = "256"]

pub mod appauth;
pub mod checkin;
pub mod config;
pub mod fetch;
pub mod fx;
pub mod inject;
pub mod models;
pub mod providers;
pub mod secrets;
pub mod store;
pub mod vault;
pub mod weblogin;
