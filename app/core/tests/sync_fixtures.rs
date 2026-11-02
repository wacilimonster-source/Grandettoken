//! 跨语言 golden fixture 验证。
//!
//! `contract/fixtures/sync-v1.json` 由 `examples/gen_sync_fixture.rs` 生成；
//! Rust 与 Android（Kotlin）测试都读同一份文件，确保两端的 HKDF、AAD、
//! base64url、GCM tag 与同步码编码完全一致。

use serde_json::Value;
use std::path::PathBuf;
use tokenscope_core::sync::code::SyncCode;
use tokenscope_core::sync::crypto;
use tokenscope_core::sync::model::SyncSnapshot;

fn fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("contract")
        .join("fixtures")
        .join("sync-v1.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读取 {} 失败: {e}", path.display()));
    serde_json::from_str(&text).expect("fixture 不是合法 JSON")
}

fn hex<const N: usize>(v: &Value, key: &str) -> [u8; N] {
    let s = v[key].as_str().expect("hex 字段缺失");
    let bytes = crypto::hex_decode(s).expect("hex 解码失败");
    assert_eq!(bytes.len(), N, "{key} 长度不对");
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    out
}

#[test]
fn fixture_derived_key_and_aad_match() {
    let f = fixture();
    let master: [u8; 32] = hex(&f, "masterKeyHex");
    let relay: [u8; 16] = hex(&f, "relayIdHex");
    let source = f["source"].as_str().unwrap();
    let epoch = f["keyEpoch"].as_u64().unwrap() as u8;

    let derived = crypto::derive_source_key(&master, &relay, source, epoch).unwrap();
    assert_eq!(
        crypto::hex_encode(&derived),
        f["derivedKeyHex"].as_str().unwrap()
    );
    assert_eq!(
        crypto::hex_encode(&crypto::aad_bytes(source, epoch)),
        f["aadHex"].as_str().unwrap()
    );
}

#[test]
fn fixture_envelope_decrypts_to_plaintext() {
    let f = fixture();
    let master: [u8; 32] = hex(&f, "masterKeyHex");
    let relay: [u8; 16] = hex(&f, "relayIdHex");
    let source = f["source"].as_str().unwrap();
    let epoch = f["keyEpoch"].as_u64().unwrap() as u8;
    let envelope = serde_json::to_vec(&f["envelope"]).unwrap();
    let plaintext = crypto::open(&master, &relay, source, epoch, &envelope).unwrap();
    let expected = f["plaintextJson"].as_str().unwrap().as_bytes();
    assert_eq!(plaintext, expected);

    let snap: SyncSnapshot = serde_json::from_slice(&plaintext).unwrap();
    snap.validate().unwrap();
    assert_eq!(snap.source, source);
    assert_eq!(snap.seq, 7);
    assert_eq!(snap.channels.len(), 3);
}

#[test]
fn fixture_envelope_is_reproducible() {
    let f = fixture();
    let master: [u8; 32] = hex(&f, "masterKeyHex");
    let relay: [u8; 16] = hex(&f, "relayIdHex");
    let nonce: [u8; 12] = hex(&f, "nonceHex");
    let source = f["source"].as_str().unwrap();
    let plaintext = f["plaintextJson"].as_str().unwrap().as_bytes();
    let sealed = crypto::seal_with_nonce(&master, &relay, source, 0, plaintext, &nonce).unwrap();
    let got: Value = serde_json::from_slice(&sealed).unwrap();
    assert_eq!(got, f["envelope"]);
}

#[test]
fn fixture_sync_code_roundtrips() {
    let f = fixture();
    let master: [u8; 32] = hex(&f, "masterKeyHex");
    let relay: [u8; 16] = hex(&f, "relayIdHex");
    let code = SyncCode::decode(f["syncCode"].as_str().unwrap()).unwrap();
    assert_eq!(code.key, master);
    assert_eq!(code.relay_id, relay);
    assert_eq!(code.key_epoch, 0);
    assert_eq!(code.encode(), f["syncCode"].as_str().unwrap());
}

#[test]
fn fixture_rejects_wrong_key_aad_tamper_and_truncation() {
    let f = fixture();
    let mut master: [u8; 32] = hex(&f, "masterKeyHex");
    let relay: [u8; 16] = hex(&f, "relayIdHex");
    let source = f["source"].as_str().unwrap();
    let envelope = serde_json::to_vec(&f["envelope"]).unwrap();

    master[0] ^= 1;
    let e = crypto::open(&master, &relay, source, 0, &envelope).unwrap_err();
    assert_eq!(e.code, "SYNC_DECRYPT_FAILED");

    let master: [u8; 32] = hex(&f, "masterKeyHex");
    let e = crypto::open(
        &master,
        &relay,
        "s_0123456789abcdef0123456789abcdee",
        0,
        &envelope,
    )
    .unwrap_err();
    assert_eq!(e.code, "SYNC_DECRYPT_FAILED");

    let mut env: crypto::Envelope = serde_json::from_slice(&envelope).unwrap();
    let mut ct = crypto::base64url_decode(&env.c).unwrap();
    ct[0] ^= 1;
    env.c = crypto::base64url_encode(&ct);
    let tampered = serde_json::to_vec(&env).unwrap();
    let e = crypto::open(&master, &relay, source, 0, &tampered).unwrap_err();
    assert_eq!(e.code, "SYNC_DECRYPT_FAILED");

    let mut env: crypto::Envelope = serde_json::from_slice(&envelope).unwrap();
    env.c = crypto::base64url_encode(&[0u8; 4]);
    let truncated = serde_json::to_vec(&env).unwrap();
    let e = crypto::open(&master, &relay, source, 0, &truncated).unwrap_err();
    assert_eq!(e.code, "SYNC_ENVELOPE_INVALID");
}

#[test]
fn fixture_sync_code_rejects_typo() {
    let f = fixture();
    let text = f["syncCode"].as_str().unwrap();
    let mut chars: Vec<char> = text.chars().collect();
    let pos = 10; // 不要在最后一个 base64 字符上翻转：那几位可能只是填充位
    chars[pos] = if chars[pos] == 'A' { 'B' } else { 'A' };
    let broken: String = chars.into_iter().collect();
    let e = SyncCode::decode(&broken).unwrap_err();
    assert_eq!(e.code, "SYNC_CODE_INVALID");
}
