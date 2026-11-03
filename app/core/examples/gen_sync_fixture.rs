//! 生成 `contract/fixtures/sync-v1.json` 的 golden fixture。
//!
//! 运行：
//!   cargo run --example gen_sync_fixture > ../../contract/fixtures/sync-v1.json
//!
//! 这个例子只用于生成固定向量；生产加密必须走 `crypto::seal` 的系统随机 nonce。

use tokenscope_core::sync::code::SyncCode;
use tokenscope_core::sync::crypto::{
    aad_bytes, derive_source_key, hex_decode, hex_encode, seal_with_nonce,
};
use tokenscope_core::sync::model::{
    ChannelKind, ChannelSnapshot, CheckinItem, CheckinSnapshot, CheckinStatus, ExpiringSnapshot,
    HistoryPoint, OtherMoney, SyncSnapshot, TotalsSnapshot, WindowSnapshot,
};

fn hex<const N: usize>(s: &str) -> [u8; N] {
    let v = hex_decode(s).expect("fixture hex");
    let mut out = [0u8; N];
    out.copy_from_slice(&v);
    out
}

fn main() {
    let master_hex = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    let relay_hex = "00112233445566778899aabbccddeeff";
    let source = "s_0123456789abcdef0123456789abcdef";
    let nonce_hex = "000102030405060708090a0b";
    let master: [u8; 32] = hex(master_hex);
    let relay: [u8; 16] = hex(relay_hex);
    let nonce: [u8; 12] = hex(nonce_hex);

    let snapshot = SyncSnapshot {
        v: 1,
        source: source.into(),
        seq: 7,
        at: 1_793_500_931,
        app: "0.3.2".into(),
        channels: vec![
            ChannelSnapshot {
                platform_id: "deepseek".into(),
                id: "deepseek".into(),
                name: "DeepSeek".into(),
                kind: ChannelKind::Money,
                unit: "CNY".into(),
                remaining: Some("88".into()),
                total: None,
                percent: None,
                windows: vec![],
                expiring: vec![],
                day: Some("1.2".into()),
                week: Some("8.4".into()),
                month: Some("23.6".into()),
                estimated: true,
                stale: false,
                error_code: None,
                updated_at: 1_793_500_850,
                history: vec![
                    HistoryPoint {
                        period_start: 1_793_414_400,
                        period_end: 1_793_500_800,
                        time_zone: "+08:00".into(),
                        unit: "CNY".into(),
                        balance: None,
                        spent: Some("1.2".into()),
                        estimated: true,
                        complete: true,
                        revised_at: 1_793_500_850,
                    },
                    HistoryPoint {
                        period_start: 1_793_500_800,
                        period_end: 1_793_587_200,
                        time_zone: "+08:00".into(),
                        unit: "CNY".into(),
                        balance: None,
                        spent: None,
                        estimated: true,
                        complete: false,
                        revised_at: 1_793_500_850,
                    },
                ],
            },
            ChannelSnapshot {
                platform_id: "opencode-go".into(),
                id: "opencode-go".into(),
                name: "OpenCode Go".into(),
                kind: ChannelKind::Quota,
                unit: "%".into(),
                remaining: None,
                total: None,
                percent: Some("42".into()),
                windows: vec![WindowSnapshot {
                    key: "monthly".into(),
                    label: "周期".into(),
                    remain_percent: Some("42".into()),
                    reset_at: Some(1_793_504_800),
                    reset_text: Some("18:00".into()),
                    main: true,
                }],
                expiring: vec![],
                day: None,
                week: None,
                month: None,
                estimated: false,
                stale: false,
                error_code: None,
                updated_at: 1_793_500_850,
                history: vec![],
            },
            ChannelSnapshot {
                platform_id: "trae".into(),
                id: "trae".into(),
                name: "Trae".into(),
                kind: ChannelKind::Points,
                unit: "credits".into(),
                remaining: Some("1240".into()),
                total: None,
                percent: None,
                windows: vec![],
                expiring: vec![ExpiringSnapshot {
                    amount: "300".into(),
                    at: 1_793_990_400,
                }],
                day: None,
                week: None,
                month: None,
                estimated: false,
                stale: false,
                error_code: None,
                updated_at: 1_793_500_850,
                history: vec![],
            },
        ],
        checkin: Some(CheckinSnapshot {
            date: "2026-11-02".into(),
            time_zone: "Asia/Shanghai".into(),
            items: vec![
                CheckinItem {
                    item_id: "deepseek:main".into(),
                    platform_id: "deepseek".into(),
                    status: CheckinStatus::Done,
                    observed_at: 1_793_500_800,
                },
                CheckinItem {
                    item_id: "trae:main".into(),
                    platform_id: "trae".into(),
                    status: CheckinStatus::Pending,
                    observed_at: 1_793_500_800,
                },
            ],
            done: 1,
            total: 2,
            unsupported: 0,
            unknown: 0,
        }),
        totals: TotalsSnapshot {
            cny: Some("88".into()),
            others: vec![OtherMoney {
                unit: "USD".into(),
                amount: "10".into(),
            }],
            today: Some("1.2".into()),
        },
    };

    let plaintext = snapshot.to_bytes().expect("fixture snapshot valid");
    let envelope =
        seal_with_nonce(&master, &relay, source, 0, &plaintext, &nonce).expect("fixture seal");
    let envelope_json: serde_json::Value =
        serde_json::from_slice(&envelope).expect("fixture envelope json");
    let derived = derive_source_key(&master, &relay, source, 0).expect("fixture derivative");

    let out = serde_json::json!({
        "protocol": "grandettoken-sync-v1",
        "generatedAt": "2026-11-02",
        "masterKeyHex": master_hex,
        "relayIdHex": relay_hex,
        "gistId": relay_hex,
        "source": source,
        "keyEpoch": 0,
        "nonceHex": nonce_hex,
        "aadHex": hex_encode(&aad_bytes(source, 0)),
        "derivedKeyHex": hex_encode(&derived),
        "plaintextJson": String::from_utf8(plaintext).expect("fixture plaintext utf8"),
        "envelope": envelope_json,
        "syncCode": SyncCode::new(relay, master).encode(),
        "expect": {
            "wrongKeyCode": "SYNC_DECRYPT_FAILED",
            "wrongAadCode": "SYNC_DECRYPT_FAILED",
            "tamperCode": "SYNC_DECRYPT_FAILED",
            "truncatedCode": "SYNC_ENVELOPE_INVALID",
            "badCodeCode": "SYNC_CODE_INVALID",
            "tooLargeCode": "SYNC_TOO_LARGE"
        }
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&out).expect("fixture serialize")
    );
}
