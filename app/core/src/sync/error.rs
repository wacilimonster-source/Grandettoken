//! 同步协议的稳定错误码。
//!
//! 界面、日志和 Android 端共用这些码；消息只做人读补充，不参与判断。
//! 任何上游响应体、URL、账号、密钥都不得进入 `message`。

use std::fmt;

pub const INVALID_CODE: &str = "SYNC_CODE_INVALID";
pub const ENVELOPE_INVALID: &str = "SYNC_ENVELOPE_INVALID";
pub const DECRYPT_FAILED: &str = "SYNC_DECRYPT_FAILED";
pub const PAYLOAD_INVALID: &str = "SYNC_PAYLOAD_INVALID";
pub const UNSUPPORTED_VERSION: &str = "SYNC_UNSUPPORTED_VERSION";
pub const TRUNCATED: &str = "SYNC_TRUNCATED";
pub const TOO_LARGE: &str = "SYNC_TOO_LARGE";
pub const RATE_LIMITED: &str = "SYNC_RATE_LIMITED";
pub const AUTH: &str = "SYNC_AUTH";
pub const FORBIDDEN: &str = "SYNC_FORBIDDEN";
pub const NOT_FOUND: &str = "SYNC_NOT_FOUND";
pub const CONFLICT: &str = "SYNC_CONFLICT";
pub const NETWORK: &str = "SYNC_NETWORK";
pub const UPSTREAM: &str = "SYNC_UPSTREAM";
pub const NOT_CONFIGURED: &str = "SYNC_NOT_CONFIGURED";
pub const NO_KEY: &str = "SYNC_NO_KEY";
pub const GIST_ID_INVALID: &str = "SYNC_GIST_ID_INVALID";
pub const SOURCE_INVALID: &str = "SYNC_SOURCE_INVALID";
pub const LIMIT: &str = "SYNC_LIMIT";
pub const STORE: &str = "SYNC_STORE";
pub const QR: &str = "SYNC_QR";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SyncError {
    pub code: &'static str,
    pub message: String,
    /// 服务端要求的等待秒数（429 / Retry-After）；没有就是 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_sec: Option<u64>,
    /// GitHub 主限额重置的 unix 秒；没有就是 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<i64>,
}

impl SyncError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after_sec: None,
            reset_at: None,
        }
    }

    pub fn with_retry(mut self, retry_after_sec: Option<u64>, reset_at: Option<i64>) -> Self {
        self.retry_after_sec = retry_after_sec;
        self.reset_at = reset_at;
        self
    }

    pub fn code_only(code: &'static str) -> Self {
        Self::new(code, code)
    }
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SyncError {}

impl From<serde_json::Error> for SyncError {
    fn from(e: serde_json::Error) -> Self {
        Self::new(PAYLOAD_INVALID, format!("JSON 解析失败: {e}"))
    }
}
