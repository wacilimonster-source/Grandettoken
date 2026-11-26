use axum::http::{header, HeaderMap};
use sha2::{Digest, Sha256};

use super::{lock, Gateway, Settings, SECRET_KEY};
use crate::vault::transaction::CredentialStore;

type TokenDigest = [u8; 32];

fn digest(token: &str) -> TokenDigest {
    Sha256::digest(token.as_bytes()).into()
}

impl Gateway {
    pub fn is_authorized(&self, headers: &HeaderMap, settings: &Settings) -> bool {
        check(headers, settings, || {
            let mut cached = lock(&self.access_token_sha256);
            if cached.is_none() {
                *cached = read_digest(self.access.as_ref());
            }
            *cached
        })
    }

    pub fn invalidate_access_token(&self) {
        *lock(&self.access_token_sha256) = None;
    }

    /// Serialize credential reads and rotation with digest updates so a late
    /// read cannot put the previous secret back into the cache.
    pub async fn access_secret(&self, rotate: bool) -> Result<String, String> {
        let gateway = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut cached = lock(&gateway.access_token_sha256);
            let existing = if rotate {
                None
            } else {
                gateway.access.get(SECRET_KEY)?
            };
            let token = match existing.filter(|s| !s.is_empty()) {
                Some(token) => token,
                None => {
                    let mut bytes = [0u8; 32];
                    getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
                    let token = format!(
                        "gw_{}",
                        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
                    );
                    gateway.access.set(SECRET_KEY, &token)?;
                    token
                }
            };
            *cached = Some(digest(&token));
            Ok(token)
        })
        .await
        .map_err(|e| format!("网关访问密钥准备失败: {e}"))?
    }
}

pub fn authorized_with(
    headers: &HeaderMap,
    settings: &Settings,
    access: &dyn CredentialStore,
) -> bool {
    check(headers, settings, || read_digest(access))
}

fn read_digest(access: &dyn CredentialStore) -> Option<TokenDigest> {
    access
        .get(SECRET_KEY)
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .map(|s| digest(&s))
}

fn check(
    headers: &HeaderMap,
    settings: &Settings,
    expected: impl FnOnce() -> Option<TokenDigest>,
) -> bool {
    if headers.contains_key(header::ORIGIN) {
        return false;
    }
    if !settings.auth && !settings.lan {
        let Some(h) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
            return false;
        };
        let host = if let Some(rest) = h.strip_prefix('[') {
            rest.split(']').next().unwrap_or("")
        } else {
            h.rsplit_once(':').map(|(a, _)| a).unwrap_or(h)
        };
        return matches!(host, "127.0.0.1" | "localhost" | "::1");
    }
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            let (scheme, token) = s.trim().split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
        })
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
        })
        .unwrap_or("");
    if provided.is_empty() {
        return false;
    }
    expected().is_some_and(|expected| {
        digest(provided)
            .into_iter()
            .zip(expected)
            .fold(0, |diff, (x, y)| diff | (x ^ y))
            == 0
    })
}
