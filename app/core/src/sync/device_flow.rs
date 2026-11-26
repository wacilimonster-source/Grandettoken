use reqwest::{
    header::{ACCEPT, USER_AGENT},
    Method, RequestBuilder,
};
use std::time::Duration;

const GITHUB_DEVICE_CODE: &str = "https://github.com/login/device/code";
const GITHUB_TOKEN: &str = "https://github.com/login/oauth/access_token";
const GITHUB_USER: &str = "https://api.github.com/user";
const GITHUB_VERIFY: &str = "https://github.com/login/device";

#[derive(Clone)]
pub struct DeviceSession {
    device_code: String,
    expires_at: i64,
    interval: u64,
}

impl DeviceSession {
    pub fn same_attempt(&self, other: &Self) -> bool {
        self.device_code == other.device_code
    }

    pub fn expired(&self, now: i64) -> bool {
        now >= self.expires_at
    }

    pub fn set_interval(&mut self, interval: u64) {
        self.interval = interval;
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthStart {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: i64,
    pub interval: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OAuthPoll {
    Pending { interval: u64 },
    Authorized { login: String, user_id: i64 },
    Error { code: String, message: String },
    NoSession,
}

#[derive(serde::Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: i64,
    interval: u64,
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    scope: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
}

#[derive(serde::Deserialize)]
pub struct GithubUserResponse {
    pub id: i64,
    pub login: String,
}

pub enum TokenDecision {
    Pending {
        interval: u64,
        slow_down: bool,
    },
    Error {
        code: String,
        message: String,
        clear: bool,
    },
    Ready(String),
}

fn github_request(method: Method, url: &str) -> Result<RequestBuilder, String> {
    let http = crate::http_pool::shared_default()
        .map_err(|e| format!("SYNC_NETWORK: HTTP 客户端初始化失败: {e}"))?;
    Ok(http
        .request(method, url)
        .timeout(Duration::from_secs(20))
        .header(USER_AGENT, "Grandettoken-Desktop/1"))
}

pub async fn github_user(token: &str) -> Result<GithubUserResponse, String> {
    let resp = github_request(Method::GET, GITHUB_USER)?
        .header(ACCEPT, "application/vnd.github+json")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "SYNC_NETWORK: 请求 GitHub 用户信息失败".to_string())?;
    if resp.status().as_u16() == 401 {
        return Err("SYNC_AUTH: GitHub 授权已失效,请重新授权".into());
    }
    if !resp.status().is_success() {
        return Err(format!(
            "SYNC_UPSTREAM: GitHub 用户信息返回 {}",
            resp.status()
        ));
    }
    let user: GithubUserResponse = resp
        .json()
        .await
        .map_err(|_| "SYNC_UPSTREAM: GitHub 用户信息不是预期 JSON".to_string())?;
    if user.id <= 0 || user.login.trim().is_empty() {
        return Err("SYNC_UPSTREAM: GitHub 用户信息不完整".into());
    }
    Ok(user)
}

fn start_session(
    body: DeviceCodeResponse,
    now: i64,
) -> Result<(DeviceSession, OAuthStart), String> {
    if body.device_code.is_empty() || body.user_code.is_empty() || body.interval == 0 {
        return Err("SYNC_UPSTREAM: GitHub 授权码响应不完整".into());
    }
    if !body.verification_uri.starts_with("https://github.com/") {
        return Err("SYNC_UPSTREAM: GitHub 返回了非官方授权地址".into());
    }
    let expires_in = body.expires_in.max(60);
    let interval = body.interval.max(5);
    Ok((
        DeviceSession {
            device_code: body.device_code,
            expires_at: now.saturating_add(expires_in),
            interval,
        },
        OAuthStart {
            user_code: body.user_code,
            verification_uri: GITHUB_VERIFY.into(),
            expires_in,
            interval,
        },
    ))
}

pub async fn start(client_id: &str) -> Result<(DeviceSession, OAuthStart), String> {
    let resp = github_request(Method::POST, GITHUB_DEVICE_CODE)?
        .header(ACCEPT, "application/json")
        .form(&[("client_id", client_id), ("scope", "gist")])
        .send()
        .await
        .map_err(|_| "SYNC_NETWORK: 无法申请 GitHub 授权码".to_string())?;
    if !resp.status().is_success() {
        return Err(format!(
            "SYNC_UPSTREAM: GitHub 授权码接口返回 {}",
            resp.status()
        ));
    }
    let body = resp
        .json()
        .await
        .map_err(|_| "SYNC_UPSTREAM: GitHub 授权码响应不是预期 JSON".to_string())?;
    start_session(body, crate::fetch::now_ts())
}

fn decide_token(session: &DeviceSession, token: TokenResponse) -> TokenDecision {
    let error = |code: &str, message: &str, clear| TokenDecision::Error {
        code: code.into(),
        message: message.into(),
        clear,
    };
    if let Some(err) = token.error.as_deref() {
        return match err {
            "authorization_pending" => TokenDecision::Pending {
                interval: session.interval,
                slow_down: false,
            },
            "slow_down" => TokenDecision::Pending {
                interval: token
                    .interval
                    .unwrap_or(session.interval.saturating_add(5))
                    .max(5),
                slow_down: true,
            },
            "access_denied" => error("SYNC_OAUTH_DENIED", "GitHub 授权被拒绝", true),
            "expired_token" => error("SYNC_OAUTH_EXPIRED", "授权码已过期,请重新获取", true),
            other => error(
                &format!("SYNC_OAUTH_{}", other.to_ascii_uppercase()),
                &token
                    .error_description
                    .unwrap_or_else(|| "GitHub 授权失败".into())
                    .chars()
                    .take(200)
                    .collect::<String>(),
                true,
            ),
        };
    }
    let Some(access) = token.access_token.filter(|t| !t.trim().is_empty()) else {
        return error("SYNC_UPSTREAM", "GitHub 没有返回 access token", false);
    };
    let scopes = token.scope.as_deref().unwrap_or("").split_whitespace();
    if !scopes.clone().any(|scope| scope == "gist") || scopes.clone().any(|scope| scope != "gist") {
        return error(
            "SYNC_SCOPE_INVALID",
            "GitHub 返回的授权范围不是仅 gist;请在 GitHub 撤销授权后重试",
            true,
        );
    }
    TokenDecision::Ready(access)
}

pub async fn poll(session: &DeviceSession, client_id: &str) -> Result<TokenDecision, String> {
    let resp = github_request(Method::POST, GITHUB_TOKEN)?
        .header(ACCEPT, "application/json")
        .form(&[
            ("client_id", client_id),
            ("device_code", &session.device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await
        .map_err(|_| "SYNC_NETWORK: 轮询 GitHub 授权失败".to_string())?;
    if !resp.status().is_success() {
        return Ok(TokenDecision::Error {
            code: "SYNC_UPSTREAM".into(),
            message: format!("GitHub 授权接口返回 {}", resp.status()),
            clear: false,
        });
    }
    let token = resp
        .json()
        .await
        .map_err(|_| "SYNC_UPSTREAM: GitHub 授权响应不是预期 JSON".to_string())?;
    Ok(decide_token(session, token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_and_poll_results_preserve_protocol_bounds() {
        let start_body = |uri| {
            serde_json::from_value(json!({
                "device_code": "fixture-device", "user_code": "F00-TEST",
                "verification_uri": uri, "expires_in": 20, "interval": 2,
            }))
            .unwrap()
        };
        assert!(start_session(start_body("https://example.invalid/"), 100).is_err());
        let (mut session, payload) = start_session(start_body(GITHUB_VERIFY), 100).unwrap();
        assert_eq!(payload.interval, 5);
        assert!(!session.expired(159));
        assert!(session.expired(160));
        let token = |v| serde_json::from_value(v).unwrap();
        assert!(matches!(
            decide_token(&session, token(json!({"error":"slow_down"}))),
            TokenDecision::Pending {
                interval: 10,
                slow_down: true
            }
        ));
        session.set_interval(10);
        assert!(matches!(
            decide_token(&session, token(json!({"error":"authorization_pending"}))),
            TokenDecision::Pending {
                interval: 10,
                slow_down: false
            }
        ));
        assert!(
            matches!(decide_token(&session, token(json!({"error":"expired_token"}))),
            TokenDecision::Error { code, clear: true, .. } if code == "SYNC_OAUTH_EXPIRED")
        );
        assert!(matches!(
            decide_token(
                &session,
                token(json!({"access_token":"fixture", "scope":"gist"}))
            ),
            TokenDecision::Ready(_)
        ));
        assert!(
            matches!(decide_token(&session, token(json!({"access_token":"fixture", "scope":"gist repo"}))),
            TokenDecision::Error { code, clear: true, .. } if code == "SYNC_SCOPE_INVALID")
        );
    }
}
