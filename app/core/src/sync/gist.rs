//! GitHub Gist 传输层。
//!
//! 只做三件事：读全部来源文件、只 PATCH 自己的来源文件、创建/删除中继。
//! 所有响应都做截断与大小检查;错误只保留状态码与错误码,不把响应体带出去。

use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{HeaderMap, ACCEPT, IF_NONE_MATCH, USER_AGENT};
use reqwest::{Method, RequestBuilder, StatusCode};

use super::code::{is_source_file_name, valid_gist_id};
use super::error::{
    SyncError, AUTH, CONFLICT, FORBIDDEN, GIST_ID_INVALID, NETWORK, NOT_FOUND, PAYLOAD_INVALID,
    RATE_LIMITED, TOO_LARGE, TRUNCATED, UPSTREAM,
};
use super::model::MAX_SOURCE_FILE_BYTES;

pub const MAX_SOURCES: usize = 32;
pub const MAX_TOTAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const GIST_API: &str = "https://api.github.com/gists";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GistFile {
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GistLoad {
    pub id: String,
    pub files: Vec<GistFile>,
    pub etag: Option<String>,
    /// GitHub 账号 ID;加入已有组时用来拒绝不同账号。
    pub owner_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GetOutcome {
    NotModified { etag: Option<String> },
    Loaded(GistLoad),
}

pub struct GistClient {
    http: reqwest::Client,
    token: Option<String>,
}

impl GistClient {
    pub fn new(token: Option<String>) -> Result<Self, SyncError> {
        let http = crate::http_pool::shared_default()
            .map_err(|e| SyncError::new(NETWORK, format!("HTTP 客户端初始化失败: {e}")))?
            .clone();
        Ok(Self {
            http,
            token: token.filter(|t| !t.trim().is_empty()),
        })
    }

    fn request(&self, method: Method, url: &str) -> RequestBuilder {
        let req = self.http.request(method, url)
            .timeout(Duration::from_secs(20))
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header(USER_AGENT, "Grandettoken-Sync/1");
        match &self.token {
            Some(token) => req.bearer_auth(token),
            None => req,
        }
    }

    pub async fn get(&self, gist_id: &str, etag: Option<&str>) -> Result<GetOutcome, SyncError> {
        if !valid_gist_id(gist_id) {
            return Err(SyncError::new(GIST_ID_INVALID, "Gist ID 格式不正确"));
        }
        let url = format!("{GIST_API}/{gist_id}");
        let mut req = self.request(Method::GET, &url);
        if let Some(etag) = etag {
            req = req.header(IF_NONE_MATCH, etag);
        }

        let resp = req.send().await.map_err(map_reqwest)?;
        let meta = ResponseMeta::from_headers(resp.headers());
        let status = resp.status();

        if status == StatusCode::NOT_MODIFIED {
            return Ok(GetOutcome::NotModified {
                etag: meta.etag.or_else(|| etag.map(|s| s.to_string())),
            });
        }
        if !status.is_success() {
            return Err(status_error(status, &meta));
        }

        let parsed = parse_response(resp).await?;
        if parsed.truncated {
            return Err(SyncError::new(TRUNCATED, "Gist 响应被 GitHub 截断"));
        }

        let mut files = Vec::new();
        let mut total = 0usize;
        for (name, file) in parsed.files {
            if !is_source_file_name(&name) {
                continue;
            }
            if files.len() >= MAX_SOURCES {
                return Err(SyncError::new(
                    TOO_LARGE,
                    format!("来源文件超过 {MAX_SOURCES} 个"),
                ));
            }
            if file.truncated {
                return Err(SyncError::new(TRUNCATED, format!("文件 {name} 被截断")));
            }
            let Some(content) = file.content else {
                return Err(SyncError::new(
                    TRUNCATED,
                    format!("文件 {name} 没有内联内容"),
                ));
            };
            if file.size as usize > MAX_SOURCE_FILE_BYTES || content.len() > MAX_SOURCE_FILE_BYTES {
                return Err(SyncError::new(
                    TOO_LARGE,
                    format!("文件 {name} 超过单文件上限"),
                ));
            }
            total = total.saturating_add(content.len());
            if total > MAX_TOTAL_BYTES {
                return Err(SyncError::new(TOO_LARGE, "来源文件总大小超过上限"));
            }
            files.push(GistFile { name, content });
        }

        Ok(GetOutcome::Loaded(GistLoad {
            id: parsed.id,
            files,
            etag: meta.etag,
            owner_id: parsed.owner.map(|o| o.id),
        }))
    }

    /// 只更新一个文件。请求体里只出现这一个文件,绝不整份回写 files。
    pub async fn patch_file(
        &self,
        gist_id: &str,
        file_name: &str,
        content: &str,
    ) -> Result<(), SyncError> {
        if !valid_gist_id(gist_id) {
            return Err(SyncError::new(GIST_ID_INVALID, "Gist ID 格式不正确"));
        }
        if !is_source_file_name(file_name) {
            return Err(SyncError::new(
                PAYLOAD_INVALID,
                "文件名为空或不是来源文件格式",
            ));
        }
        if content.len() > MAX_SOURCE_FILE_BYTES {
            return Err(SyncError::new(TOO_LARGE, "单文件内容超过上限"));
        }
        let url = format!("{GIST_API}/{gist_id}");
        let mut files = HashMap::new();
        files.insert(file_name, serde_json::json!({ "content": content }));
        let body = serde_json::json!({ "files": files });
        let req = self.request(Method::PATCH, &url).json(&body);
        let resp = req.send().await.map_err(map_reqwest)?;
        let meta = ResponseMeta::from_headers(resp.headers());
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        Err(status_error(status, &meta))
    }

    /// 创建 secret gist。返回 (gist_id, etag)。
    ///
    /// `file_name` 允许是初始化占位文件(`gt_init.txt`)：创建 Gist 之前拿不到
    /// 服务端生成的 gist ID,而加密子密钥又绑定 gist ID,所以只能先用占位文件
    /// 建组,拿到 ID 后再 PATCH 真正的来源文件。
    pub async fn create_gist(
        &self,
        file_name: &str,
        content: &str,
        description: &str,
    ) -> Result<(String, Option<String>), SyncError> {
        if !valid_plain_file_name(file_name) {
            return Err(SyncError::new(PAYLOAD_INVALID, "文件名不合法"));
        }
        if content.len() > MAX_SOURCE_FILE_BYTES {
            return Err(SyncError::new(TOO_LARGE, "单文件内容超过上限"));
        }
        let mut files = HashMap::new();
        files.insert(file_name, serde_json::json!({ "content": content }));
        let body = serde_json::json!({
            "description": description,
            "public": false,
            "files": files,
        });
        let req = self.request(Method::POST, GIST_API).json(&body);
        let resp = req.send().await.map_err(map_reqwest)?;
        let meta = ResponseMeta::from_headers(resp.headers());
        let status = resp.status();
        if !status.is_success() {
            return Err(status_error(status, &meta));
        }
        let parsed = parse_response(resp).await?;
        if parsed.id.is_empty() || !valid_gist_id(&parsed.id) {
            return Err(SyncError::new(PAYLOAD_INVALID, "GitHub 返回的 Gist ID 不合法"));
        }
        Ok((parsed.id, meta.etag))
    }

    /// 删除一个文件(只用于建组后的占位文件)。传 null 是 GitHub 的删除语义。
    pub async fn delete_file(&self, gist_id: &str, file_name: &str) -> Result<(), SyncError> {
        if !valid_gist_id(gist_id) {
            return Err(SyncError::new(GIST_ID_INVALID, "Gist ID 格式不正确"));
        }
        if !valid_plain_file_name(file_name) {
            return Err(SyncError::new(PAYLOAD_INVALID, "文件名不合法"));
        }
        let url = format!("{GIST_API}/{gist_id}");
        let mut files = HashMap::new();
        files.insert(file_name, serde_json::Value::Null);
        let body = serde_json::json!({ "files": files });
        let req = self.request(Method::PATCH, &url).json(&body);
        let resp = req.send().await.map_err(map_reqwest)?;
        let meta = ResponseMeta::from_headers(resp.headers());
        let status = resp.status();
        if status.is_success() || status == StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(status_error(status, &meta))
    }

    /// 删除中继。404 视为已不存在(幂等)。
    pub async fn delete_gist(&self, gist_id: &str) -> Result<(), SyncError> {
        if !valid_gist_id(gist_id) {
            return Err(SyncError::new(GIST_ID_INVALID, "Gist ID 格式不正确"));
        }
        let url = format!("{GIST_API}/{gist_id}");
        let req = self.request(Method::DELETE, &url);
        let resp = req.send().await.map_err(map_reqwest)?;
        let meta = ResponseMeta::from_headers(resp.headers());
        let status = resp.status();
        if status.is_success() || status == StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(status_error(status, &meta))
    }
}

async fn parse_response(resp: reqwest::Response) -> Result<GistResponse, SyncError> {
    let body = crate::fetch::read_body_capped(resp, MAX_RESPONSE_BYTES).await
        .map_err(|(message, kind)| match kind {
            crate::fetch::FailKind::Server => SyncError::new(TOO_LARGE, "Gist 响应超过上限"),
            _ => SyncError::new(NETWORK, message),
        })?;
    serde_json::from_slice(&body).map_err(|_| SyncError::new(PAYLOAD_INVALID, "Gist 响应不是预期 JSON"))
}

#[derive(Default, Clone)]
struct ResponseMeta {
    etag: Option<String>,
    remaining: Option<u64>,
    reset_at: Option<i64>,
    retry_after: Option<u64>,
}

impl ResponseMeta {
    fn from_headers(h: &HeaderMap) -> Self {
        Self {
            etag: header_str(h, "etag"),
            remaining: header_u64(h, "x-ratelimit-remaining"),
            reset_at: header_i64(h, "x-ratelimit-reset"),
            retry_after: header_u64(h, "retry-after"),
        }
    }
}

fn valid_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 80
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(|c| c.is_control())
}

fn header_str(h: &HeaderMap, name: &str) -> Option<String> {    h.get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn header_u64(h: &HeaderMap, name: &str) -> Option<u64> {
    header_str(h, name).and_then(|s| s.parse().ok())
}

fn header_i64(h: &HeaderMap, name: &str) -> Option<i64> {
    header_str(h, name).and_then(|s| s.parse().ok())
}

fn status_error(status: StatusCode, meta: &ResponseMeta) -> SyncError {
    let code = status.as_u16();
    if status == StatusCode::UNAUTHORIZED {
        return SyncError::new(AUTH, "GitHub 令牌无效或已过期");
    }
    if status == StatusCode::FORBIDDEN {
        if meta.remaining == Some(0) || meta.retry_after.is_some() {
            return SyncError::new(RATE_LIMITED, "GitHub 限流")
                .with_retry(meta.retry_after, meta.reset_at);
        }
        return SyncError::new(FORBIDDEN, "GitHub 拒绝访问(权限不足)");
    }
    if status == StatusCode::NOT_FOUND {
        return SyncError::new(NOT_FOUND, "Gist 不存在或不可访问");
    }
    if status == StatusCode::CONFLICT {
        return SyncError::new(CONFLICT, "Gist 并发更新冲突");
    }
    if status == StatusCode::UNPROCESSABLE_ENTITY {
        return SyncError::new(PAYLOAD_INVALID, "GitHub 拒绝了请求体(422)");
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return SyncError::new(RATE_LIMITED, "GitHub 限流(429)")
            .with_retry(meta.retry_after, meta.reset_at);
    }
    if status.is_server_error() {
        return SyncError::new(UPSTREAM, format!("GitHub 服务端错误({code})"));
    }
    SyncError::new(UPSTREAM, format!("GitHub 返回 {code}"))
}

fn map_reqwest(e: reqwest::Error) -> SyncError {
    if e.is_timeout() {
        SyncError::new(NETWORK, "请求 GitHub 超时")
    } else if e.is_connect() {
        SyncError::new(NETWORK, "连不上 GitHub")
    } else {
        SyncError::new(NETWORK, "请求 GitHub 失败")
    }
}

#[derive(serde::Deserialize)]
struct GistResponse {
    #[serde(default)]
    id: String,
    #[serde(default)]
    truncated: bool,
    #[serde(default)]
    files: HashMap<String, GistFileResponse>,
    #[serde(default)]
    owner: Option<GistOwner>,
}

#[derive(serde::Deserialize)]
struct GistOwner {
    id: i64,
}

#[derive(serde::Deserialize)]
struct GistFileResponse {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    truncated: bool,
    #[serde(default)]
    size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_pool_keeps_authorization_scoped_to_each_request() {
        use reqwest::header::AUTHORIZATION;
        let first = GistClient::new(Some("first-fixture".into())).unwrap();
        let second = GistClient::new(Some("second-fixture".into())).unwrap();
        let public = GistClient::new(None).unwrap();
        let get = |client: &GistClient| client.request(Method::GET, GIST_API).build().unwrap();
        assert_eq!(get(&first).headers()[AUTHORIZATION], "Bearer first-fixture");
        assert_eq!(get(&second).headers()[AUTHORIZATION], "Bearer second-fixture");
        assert!(!get(&public).headers().contains_key(AUTHORIZATION));
        assert!(get(&first).headers()[AUTHORIZATION].is_sensitive());
    }

    #[tokio::test]
    async fn parser_limits_a_chunked_response_and_accepts_normal_json() {
        use axum::{body::{Body, Bytes}, routing::get, Router};
        use std::convert::Infallible;
        let router = Router::new()
            .route("/ok", get(|| async { r#"{"id":"fixture","files":{}}"# }))
            .route("/size", get(|| async {
                let block = Bytes::from(vec![b' '; 1024 * 1024]);
                let chunks = (0..=MAX_RESPONSE_BYTES.div_ceil(block.len())).map(move |_| Ok::<_, Infallible>(block.clone()));
                Body::from_stream(futures::stream::iter(chunks))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        let normal = http.get(format!("http://{addr}/ok")).send().await.unwrap();
        assert_eq!(parse_response(normal).await.unwrap().id, "fixture");
        let large = http.get(format!("http://{addr}/size")).send().await.unwrap();
        assert!(large.content_length().is_none());
        let error = parse_response(large).await.err().expect("response limit");
        assert_eq!(error.code, TOO_LARGE);
        task.abort();
    }

    #[test]
    fn status_mapping() {
        let meta = ResponseMeta {
            remaining: Some(0),
            reset_at: Some(123),
            retry_after: None,
            etag: None,
        };
        let e = status_error(StatusCode::FORBIDDEN, &meta);
        assert_eq!(e.code, RATE_LIMITED);
        assert_eq!(e.reset_at, Some(123));

        let meta = ResponseMeta::default();
        assert_eq!(
            status_error(StatusCode::UNAUTHORIZED, &meta).code,
            AUTH
        );
        assert_eq!(
            status_error(StatusCode::NOT_FOUND, &meta).code,
            NOT_FOUND
        );
        assert_eq!(
            status_error(StatusCode::UNPROCESSABLE_ENTITY, &meta).code,
            PAYLOAD_INVALID
        );
    }
}
