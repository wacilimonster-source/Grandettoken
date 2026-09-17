//! TokenScope 纯逻辑层。
//!
//! 刻意不依赖 Tauri / WebView2:这样 `cargo test` 只需链接 serde、rusqlite 等
//! 基础库,测试秒级完成,也不会因为 WebView2 运行时缺失而启动失败。

pub mod appauth;
pub mod config;
pub mod fetch;
pub mod providers;
pub mod secrets;
pub mod store;
