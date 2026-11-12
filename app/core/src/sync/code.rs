//! 同步码与 Gist 文件命名。
//!
//! 文本格式：`GT1.<base64url(payload || crc32)>`,无填充。
//! payload 固定 51 字节：
//!   version(1) | relayKind(1) | keyEpoch(1) | relayId(16) | key(32)
//! CRC32 覆盖 payload,大端追加 4 字节。

use super::crypto::{
    base64url_decode, base64url_encode, hex_decode, hex_encode, KEY_LEN, RELAY_ID_LEN,
};
use super::error::{SyncError, GIST_ID_INVALID, INVALID_CODE, SOURCE_INVALID};

pub const MAGIC: &str = "GT1";
pub const CODE_VERSION: u8 = 1;
pub const PAYLOAD_LEN: usize = 1 + 1 + 1 + RELAY_ID_LEN + KEY_LEN; // 51
pub const CODED_LEN: usize = PAYLOAD_LEN + 4; // 55

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayKind {
    Gist = 1,
}

impl RelayKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Gist),
            _ => None,
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            Self::Gist => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncCode {
    pub version: u8,
    pub relay_kind: RelayKind,
    pub key_epoch: u8,
    pub relay_id: [u8; RELAY_ID_LEN],
    pub key: [u8; KEY_LEN],
}

impl SyncCode {
    pub fn new(relay_id: [u8; RELAY_ID_LEN], key: [u8; KEY_LEN]) -> Self {
        Self {
            version: CODE_VERSION,
            relay_kind: RelayKind::Gist,
            key_epoch: 0,
            relay_id,
            key,
        }
    }

    pub fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(CODED_LEN);
        payload.push(self.version);
        payload.push(self.relay_kind.as_u8());
        payload.push(self.key_epoch);
        payload.extend_from_slice(&self.relay_id);
        payload.extend_from_slice(&self.key);
        let crc = crc32(&payload);
        payload.extend_from_slice(&crc.to_be_bytes());
        format!("{}.{}", MAGIC, base64url_encode(&payload))
    }

    pub fn decode(text: &str) -> Result<Self, SyncError> {
        let text = text.trim();
        let prefix = format!("{}.", MAGIC);
        let body = text
            .strip_prefix(&prefix)
            .ok_or_else(|| SyncError::new(INVALID_CODE, "同步码缺少 GT1. 前缀"))?;
        let bytes = base64url_decode(body)
            .map_err(|_| SyncError::new(INVALID_CODE, "同步码不是合法 base64url"))?;
        if bytes.len() != CODED_LEN {
            return Err(SyncError::new(
                INVALID_CODE,
                format!("同步码长度不正确({})", bytes.len()),
            ));
        }
        let (payload, crc_bytes) = bytes.split_at(PAYLOAD_LEN);
        let got = u32::from_be_bytes([crc_bytes[0], crc_bytes[1], crc_bytes[2], crc_bytes[3]]);
        let want = crc32(payload);
        if got != want {
            return Err(SyncError::new(INVALID_CODE, "同步码校验失败"));
        }

        let version = payload[0];
        if version != CODE_VERSION {
            return Err(SyncError::new(
                super::error::UNSUPPORTED_VERSION,
                format!("不支持的同步码版本: {version}"),
            ));
        }
        let relay_kind = RelayKind::from_u8(payload[1])
            .ok_or_else(|| SyncError::new(INVALID_CODE, "未知的中继类型"))?;
        let key_epoch = payload[2];
        let mut relay_id = [0u8; RELAY_ID_LEN];
        relay_id.copy_from_slice(&payload[3..3 + RELAY_ID_LEN]);
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&payload[3 + RELAY_ID_LEN..PAYLOAD_LEN]);

        Ok(Self {
            version,
            relay_kind,
            key_epoch,
            relay_id,
            key,
        })
    }

    pub fn gist_id_hex(&self) -> String {
        hex_encode(&self.relay_id)
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

pub fn parse_gist_id_hex(id: &str) -> Result<[u8; RELAY_ID_LEN], SyncError> {
    let id = id.trim();
    if id.len() != 32 {
        return Err(SyncError::new(GIST_ID_INVALID, "Gist ID 必须是 32 位十六进制"));
    }
    let bytes = hex_decode(id)
        .ok_or_else(|| SyncError::new(GIST_ID_INVALID, "Gist ID 含非十六进制字符"))?;
    if bytes.len() != RELAY_ID_LEN {
        return Err(SyncError::new(GIST_ID_INVALID, "Gist ID 长度不正确"));
    }
    let mut out = [0u8; RELAY_ID_LEN];
    out.copy_from_slice(&bytes);
    Ok(out)
}

pub fn valid_gist_id(id: &str) -> bool {
    parse_gist_id_hex(id).is_ok()
}

pub fn file_name_for_source(source: &str) -> Result<String, SyncError> {
    let normalized = super::crypto::normalize_source(source)
        .ok_or_else(|| SyncError::new(SOURCE_INVALID, "写入者 ID 格式不正确"))?;
    Ok(format!("gt_{}.json", normalized))
}

/// 只接受 `gt_s_<32 hex>.json`。其他文件一律忽略。
pub fn source_from_file_name(name: &str) -> Option<String> {
    let body = name.strip_prefix("gt_")?.strip_suffix(".json")?;
    let normalized = super::crypto::normalize_source(body)?;
    Some(normalized)
}

pub fn is_source_file_name(name: &str) -> bool {
    source_from_file_name(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SyncCode {
        let mut relay_id = [0u8; 16];
        for (i, b) in relay_id.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut key = [0u8; 32];
        for (i, b) in key.iter_mut().enumerate() {
            *b = (0xa0 + i) as u8;
        }
        SyncCode::new(relay_id, key)
    }

    #[test]
    fn roundtrip() {
        let code = sample();
        let text = code.encode();
        assert!(text.starts_with("GT1."));
        assert_eq!(SyncCode::decode(&text).unwrap(), code);
    }

    #[test]
    fn crc_rejects_typo() {
        let code = sample();
        let text = code.encode();
        let mut chars: Vec<char> = text.chars().collect();
        let pos = chars.len() - 2;
        chars[pos] = if chars[pos] == 'a' { 'b' } else { 'a' };
        let broken: String = chars.into_iter().collect();
        let err = SyncCode::decode(&broken).unwrap_err();
        assert_eq!(err.code, INVALID_CODE);
    }

    #[test]
    fn rejects_missing_prefix_and_wrong_length() {
        assert_eq!(SyncCode::decode("").unwrap_err().code, INVALID_CODE);
        assert_eq!(SyncCode::decode("GT1.abc").unwrap_err().code, INVALID_CODE);
        assert_eq!(
            SyncCode::decode("XT1.abc").unwrap_err().code,
            INVALID_CODE
        );
    }

    #[test]
    fn gist_id_roundtrip() {
        let code = sample();
        let hex = code.gist_id_hex();
        assert_eq!(parse_gist_id_hex(&hex).unwrap(), code.relay_id);
        assert!(!valid_gist_id("not-a-gist"));
    }

    #[test]
    fn file_name_roundtrip() {
        let source = "s_0123456789abcdef0123456789abcdef";
        let name = file_name_for_source(source).unwrap();
        assert_eq!(name, "gt_s_0123456789abcdef0123456789abcdef.json");
        assert_eq!(source_from_file_name(&name).as_deref(), Some(source));
        assert!(is_source_file_name(&name));
        assert!(!is_source_file_name("gt_other.json"));
        assert!(!is_source_file_name("gt_s_0123.json"));
    }
}
