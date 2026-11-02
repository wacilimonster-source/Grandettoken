//! 同步协议的加密原语。
//!
//! 这里不引 `hkdf` / `hmac` 新依赖：项目走离线构建，`sha2` 已在锁文件里，
//! 用 RFC 2104 的 HMAC-SHA256 与 RFC 5869 的 HKDF-SHA256 手写实现，
//! 并用 RFC 5869 Test Case 1 的官方向量锁死。AES-256-GCM 用已有的 `aes-gcm`。

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

use super::error::{SyncError, DECRYPT_FAILED, ENVELOPE_INVALID};

pub const ENVELOPE_VERSION: u8 = 1;
pub const KEY_LEN: usize = 32;
pub const RELAY_ID_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;

/// HKDF 的固定 salt 输入。协议 v1 不允许改；改它等于换协议主版本。
const HKDF_SALT_INPUT: &[u8] = b"grandettoken-sync-v1/salt";
/// HKDF info 的固定前缀。
const HKDF_INFO_PREFIX: &str = "grandettoken-sync-v1/source-key";
/// AAD 的固定前缀。`v1` 是密文外壳版本，不是载荷版本。
const AAD_PREFIX: &str = "v1";

/// 密文外壳。`c` 是 ciphertext || 16 字节 GCM tag 的 base64url。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub v: u8,
    pub n: String,
    pub c: String,
}

pub fn random_bytes<const N: usize>() -> Result<[u8; N], SyncError> {
    let mut out = [0u8; N];
    getrandom::getrandom(&mut out)
        .map_err(|e| SyncError::new(super::error::NETWORK, format!("随机源不可用: {e}")))?;
    Ok(out)
}

/// 随机 128 位写入者 ID，文本形式 `s_<32 hex>`。
pub fn random_source() -> Result<String, SyncError> {
    let bytes: [u8; 16] = random_bytes()?;
    Ok(format!("s_{}", hex_encode(&bytes)))
}

pub fn random_master_key() -> Result<[u8; KEY_LEN], SyncError> {
    random_bytes()
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let out = Sha256::digest(data);
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&out);
    buf
}

/// RFC 2104 HMAC-SHA256。块长 64 字节。
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let h = sha256(key);
        k[..32].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let out = outer.finalize();
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&out);
    buf
}

/// RFC 5869 HKDF-SHA256。
pub fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8], out_len: usize) -> Vec<u8> {
    let prk = hmac_sha256(salt, ikm);
    let mut okm = Vec::with_capacity(out_len);
    let mut t: Vec<u8> = Vec::new();
    let mut counter: u8 = 1;
    while okm.len() < out_len {
        let mut msg = Vec::with_capacity(t.len() + info.len() + 1);
        msg.extend_from_slice(&t);
        msg.extend_from_slice(info);
        msg.push(counter);
        t = hmac_sha256(&prk, &msg).to_vec();
        okm.extend_from_slice(&t);
        if counter == u8::MAX {
            break;
        }
        counter += 1;
    }
    okm.truncate(out_len);
    okm
}

pub fn hkdf_salt() -> [u8; 32] {
    sha256(HKDF_SALT_INPUT)
}

/// 来源子密钥：master + relayId + source + keyEpoch。
pub fn derive_source_key(
    master: &[u8; KEY_LEN],
    relay_id: &[u8; RELAY_ID_LEN],
    source: &str,
    key_epoch: u8,
) -> Result<[u8; KEY_LEN], SyncError> {
    if !valid_source(source) {
        return Err(SyncError::new(
            super::error::SOURCE_INVALID,
            "写入者 ID 格式不正确",
        ));
    }
    let info = format!(
        "{}|{}|{}|{}",
        HKDF_INFO_PREFIX,
        hex_encode(relay_id),
        source,
        key_epoch
    );
    let okm = hkdf_sha256(master, &hkdf_salt(), info.as_bytes(), KEY_LEN);
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&okm);
    Ok(key)
}

/// AAD = `v1|<source>|<keyEpoch>` 的 UTF-8 字节。
pub fn aad_bytes(source: &str, key_epoch: u8) -> Vec<u8> {
    format!("{}|{}|{}", AAD_PREFIX, source, key_epoch).into_bytes()
}

pub fn seal(
    master: &[u8; KEY_LEN],
    relay_id: &[u8; RELAY_ID_LEN],
    source: &str,
    key_epoch: u8,
    plaintext: &[u8],
) -> Result<Vec<u8>, SyncError> {
    let nonce_bytes: [u8; NONCE_LEN] = random_bytes()?;
    seal_with_nonce(master, relay_id, source, key_epoch, plaintext, &nonce_bytes)
}

/// 固定 nonce 的加密入口，只给 golden fixtures / 跨语言测试用。
/// 生产路径必须使用 [`seal`] 的系统随机 nonce。
pub fn seal_with_nonce(
    master: &[u8; KEY_LEN],
    relay_id: &[u8; RELAY_ID_LEN],
    source: &str,
    key_epoch: u8,
    plaintext: &[u8],
    nonce_bytes: &[u8; NONCE_LEN],
) -> Result<Vec<u8>, SyncError> {
    let key = derive_source_key(master, relay_id, source, key_epoch)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| SyncError::new(ENVELOPE_INVALID, "AES 密钥长度不正确"))?;
    let nonce = Nonce::from_slice(nonce_bytes);
    let aad = aad_bytes(source, key_epoch);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| SyncError::new(ENVELOPE_INVALID, "加密失败"))?;

    let envelope = Envelope {
        v: ENVELOPE_VERSION,
        n: base64url_encode(nonce_bytes),
        c: base64url_encode(&ciphertext),
    };
    serde_json::to_vec(&envelope).map_err(|e| SyncError::new(ENVELOPE_INVALID, e.to_string()))
}

pub fn open(
    master: &[u8; KEY_LEN],
    relay_id: &[u8; RELAY_ID_LEN],
    source: &str,
    key_epoch: u8,
    envelope_bytes: &[u8],
) -> Result<Vec<u8>, SyncError> {
    let envelope: Envelope = serde_json::from_slice(envelope_bytes)
        .map_err(|_| SyncError::new(ENVELOPE_INVALID, "密文外壳不是合法 JSON"))?;
    if envelope.v != ENVELOPE_VERSION {
        return Err(SyncError::new(
            super::error::UNSUPPORTED_VERSION,
            format!("不支持的密文外壳版本: {}", envelope.v),
        ));
    }
    let nonce_bytes = base64url_decode(&envelope.n)
        .map_err(|_| SyncError::new(ENVELOPE_INVALID, "nonce 不是合法 base64url"))?;
    if nonce_bytes.len() != NONCE_LEN {
        return Err(SyncError::new(ENVELOPE_INVALID, "nonce 长度不是 12 字节"));
    }
    let ciphertext = base64url_decode(&envelope.c)
        .map_err(|_| SyncError::new(ENVELOPE_INVALID, "密文不是合法 base64url"))?;
    if ciphertext.len() < TAG_LEN {
        return Err(SyncError::new(ENVELOPE_INVALID, "密文短于 GCM tag"));
    }

    let key = derive_source_key(master, relay_id, source, key_epoch)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| SyncError::new(ENVELOPE_INVALID, "AES 密钥长度不正确"))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let aad = aad_bytes(source, key_epoch);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| SyncError::new(DECRYPT_FAILED, "密文校验失败（密钥、AAD 或内容被篡改）"))
}

pub fn valid_source(source: &str) -> bool {
    let bytes = source.as_bytes();
    if bytes.len() != 34 || !source.starts_with("s_") {
        return false;
    }
    bytes[2..].iter().all(|b| b.is_ascii_hexdigit())
}

pub fn normalize_source(source: &str) -> Option<String> {
    let s = source.trim().to_ascii_lowercase();
    if valid_source(&s) {
        Some(s)
    } else {
        None
    }
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_val(bytes[i])?;
        let lo = hex_val(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn base64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn base64url_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    URL_SAFE_NO_PAD.decode(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 5869 Test Case 1。
    #[test]
    fn hkdf_matches_rfc5869_case_1() {
        let ikm = vec![0x0b; 22];
        let salt: Vec<u8> = (0x00..=0x0c).collect();
        let info: Vec<u8> = (0xf0..=0xf9).collect();
        let okm = hkdf_sha256(&ikm, &salt, &info, 42);
        let expect_hex = "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865";
        assert_eq!(hex_encode(&okm), expect_hex);
    }

    #[test]
    fn base64url_is_unpadded() {
        let s = base64url_encode(b"\xfb\xff\x00abc");
        assert!(!s.contains('='));
        assert_eq!(base64url_decode(&s).unwrap(), b"\xfb\xff\x00abc");
    }

    #[test]
    fn seal_open_roundtrip() {
        let master = [7u8; 32];
        let relay = [9u8; 16];
        let plaintext = br#"{"v":1,"hello":"world"}"#;
        let sealed = seal(&master, &relay, "s_0123456789abcdef0123456789abcdef", 0, plaintext)
            .unwrap();
        let opened = open(
            &master,
            &relay,
            "s_0123456789abcdef0123456789abcdef",
            0,
            &sealed,
        )
        .unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let master = [7u8; 32];
        let relay = [9u8; 16];
        let sealed = seal(&master, &relay, "s_0123456789abcdef0123456789abcdef", 0, b"x")
            .unwrap();
        let mut other = master;
        other[0] ^= 1;
        let err = open(&other, &relay, "s_0123456789abcdef0123456789abcdef", 0, &sealed)
            .unwrap_err();
        assert_eq!(err.code, DECRYPT_FAILED);
    }

    #[test]
    fn aad_mismatch_fails() {
        let master = [7u8; 32];
        let relay = [9u8; 16];
        let sealed = seal(&master, &relay, "s_0123456789abcdef0123456789abcdef", 0, b"x")
            .unwrap();
        let err = open(
            &master,
            &relay,
            "s_0123456789abcdef0123456789abcdee",
            0,
            &sealed,
        )
        .unwrap_err();
        assert_eq!(err.code, DECRYPT_FAILED);
    }

    #[test]
    fn tamper_fails() {
        let master = [7u8; 32];
        let relay = [9u8; 16];
        let sealed = seal(&master, &relay, "s_0123456789abcdef0123456789abcdef", 0, b"x")
            .unwrap();
        let mut env: Envelope = serde_json::from_slice(&sealed).unwrap();
        let mut ct = base64url_decode(&env.c).unwrap();
        ct[0] ^= 1;
        env.c = base64url_encode(&ct);
        let tampered = serde_json::to_vec(&env).unwrap();
        let err = open(
            &master,
            &relay,
            "s_0123456789abcdef0123456789abcdef",
            0,
            &tampered,
        )
        .unwrap_err();
        assert_eq!(err.code, DECRYPT_FAILED);
    }

    #[test]
    fn source_validation() {
        assert!(valid_source("s_0123456789abcdef0123456789abcdef"));
        assert!(!valid_source("0123456789abcdef0123456789abcdef"));
        assert!(!valid_source("s_0123456789abcdef0123456789abcde"));
        assert!(!valid_source("s_0123456789abcdef0123456789abcdeg"));
    }
}
