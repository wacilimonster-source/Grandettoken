//! 本机已登录应用凭据的只读复用(Trae / Codex)。
//!
//! 这两个平台的积分没有开放 API Key,只能拿客户端自己的登录态去查。所以这里
//! 直接读客户端已经写在磁盘上的凭据文件 —— **只读,不写回,不刷新**。
//!
//! 为什么不刷新:他们的刷新都会轮换 refreshToken(新 token 发下来、旧的作废),
//! 我们刷完客户端手里那份就成了废票,用户下次打开 IDE 会被登出。为了一个挂件
//! 把用户踢下线是不能接受的。token 过期就如实报"打开一次 Trae / Codex 即可",
//! 由用户在客户端里正常续期,我们只是搭个便车。
//!
//! WorkBuddy 曾经也走这条路,但桌面端 5.6.2(2026-10-01 实测)起把
//! `auth.accessToken` 改成了 `{"$wbEncrypted":1,"envelope":…}` 的 AES-GCM 信封,
//! 静态钥编译期内置、本机拿不到,所以这条路已经废了 —— 见 `providers::AuthKind::WebSession`。
//!
//! 凭据本身(accessToken)只在本进程内存里流转,不落盘、不进日志、不进数据库。

use base64::Engine;
use sha2::{Digest, Sha512};
use std::path::PathBuf;

/// 需要复用本机登录态的应用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum App {
    Trae,
    Codex,
}

impl App {
    /// 界面上的叫法。也用于"打开一次 X 即可"的提示文案。
    pub fn label(self) -> &'static str {
        match self {
            App::Trae => "Trae",
            App::Codex => "Codex",
        }
    }
}

/// 一份可用的凭据。`source` 只用于界面展示"读的是哪份文件/哪个安装"。
#[derive(Debug, Clone)]
pub struct Credential {
    pub token: String,
    pub source: String,
}

/// Trae 的凭据键名(各版本一致)。
const TRAE_STORAGE_KEY: &str = "iCubeAuthInfo://icube.cloudide";

/// Trae 的安装目录名,按优先级排列。SOLO CN 是带订阅的那套,优先用它。
const TRAE_DIRS: &[&str] = &["TRAE SOLO CN", "Trae CN", "TRAE SOLO", "Trae"];

/// 可用凭据列表(按可信度排序,首个通常就是答案)。
///
/// 返回多个是为了容错:Trae 可能装了多个版本,其中一份的 token 可能已经作废。
/// 取数失败时可以顺着往下试下一份。
pub fn candidates(app: App) -> Vec<Credential> {
    match app {
        App::Trae => trae_candidates(),
        App::Codex => codex_candidates(),
    }
}

// ───────────── Trae ─────────────

fn appdata() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

fn trae_candidates() -> Vec<Credential> {
    let Some(roaming) = appdata() else {
        return Vec::new();
    };
    TRAE_DIRS
        .iter()
        .filter_map(|dir| {
            let path = roaming
                .join(dir)
                .join("User")
                .join("globalStorage")
                .join("storage.json");
            let raw = std::fs::read_to_string(&path).ok()?;
            let token = trae_token_from_storage(&raw)?;
            Some(Credential {
                token,
                source: (*dir).to_string(),
            })
        })
        .collect()
}

/// 从 storage.json 里取 accessToken。老版本(部分 CN 版)直接存明文 JSON,
/// SOLO 版存的是 `tc` 加密容器的 base64。两种都要认。
fn trae_token_from_storage(storage_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(storage_json).ok()?;
    let raw = v.get(TRAE_STORAGE_KEY)?.as_str()?.trim();
    let json = if raw.starts_with('{') {
        raw.to_string() // 明文那种
    } else {
        decrypt_tc(raw)?
    };
    let auth: serde_json::Value = serde_json::from_str(&json).ok()?;
    let token = auth.get("token")?.as_str()?.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

/// Trae `tc` 容器:base64 → [6 字节头][32 字节随机密钥][密文]。
/// key/iv 由"随机密钥 + 固定盐"的 SHA-512 双重派生;明文前 64 字节是
/// SHA-512(其余明文)的校验和,真实内容从第 65 字节开始。
/// 算法与公开实现(trae-daily-checkin / Trae-AutoCheckin)逐行对齐,并有一份
/// OpenSSL 生成的交叉验证向量在下面单测里守着。
fn decrypt_tc(b64: &str) -> Option<String> {
    let buf = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    if buf.len() < 38 + 16 || &buf[0..2] != b"tc" {
        return None;
    }
    let rb = &buf[6..38];
    let enc = &buf[38..];
    if enc.len() % 16 != 0 {
        return None;
    }

    let h = Sha512::digest(rb);
    let mut salted = Vec::with_capacity(64 + 64);
    salted.extend_from_slice(&h);
    salted.extend_from_slice(&FIXED_SALT);
    let fh = Sha512::digest(&salted);

    let plain = aes128_cbc_decrypt(&fh[0..16], &fh[16..32], enc)?;
    if plain.len() <= 64 {
        return None;
    }
    // 尾部是 PKCS7 填充(也可能是 0),按公开实现的做法宽容处理:先去填充再去空白
    let body = strip_padding(&plain[64..]);
    String::from_utf8(body).ok()
}

/// 去掉尾部填充:PKCS7(整段同值)优先,退化成去掉尾部 0 与空白。
fn strip_padding(mut data: &[u8]) -> Vec<u8> {
    if let Some(&last) = data.last() {
        let n = last as usize;
        if (1..=16).contains(&n)
            && data.len() >= n
            && data[data.len() - n..].iter().all(|&b| b == last)
        {
            data = &data[..data.len() - n];
        }
    }
    let end = data
        .iter()
        .rposition(|&b| b != 0 && !b.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);
    data[..end].to_vec()
}

/// AES-128-CBC 解密。分组算法用 RustCrypto 的 `aes`,链接模式只有异或,
/// 自己接起来即可 —— 不值得为 10 行代码再引一个 crate。
fn aes128_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    use aes::cipher::{Block, BlockDecrypt, KeyInit};
    use aes::Aes128;

    if key.len() != 16 || iv.len() != 16 || data.is_empty() || data.len() % 16 != 0 {
        return None;
    }
    let cipher = Aes128::new(Block::<Aes128>::from_slice(key));
    let mut prev = Block::<Aes128>::clone_from_slice(iv);
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks_exact(16) {
        let mut block = Block::<Aes128>::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        for i in 0..16 {
            out.push(block[i] ^ prev[i]);
        }
        prev = Block::<Aes128>::clone_from_slice(chunk);
    }
    Some(out)
}

/// 两个 64 字节盐异或的结果(公开实现里叫 SALT_AES)。直接写死展开值没有意义,
/// 保留两个原始盐 + 运行时异或,便于和公开实现逐行比对。
const FIXED_SALT: [u8; 64] = {
    const SALT_A: [u8; 64] = [
        82, 9, 106, 213, 48, 54, 165, 56, 191, 64, 163, 158, 129, 243, 215, 251, 124, 227, 57, 130,
        155, 47, 255, 135, 52, 142, 67, 68, 196, 222, 233, 203, 84, 123, 148, 50, 166, 194, 35, 61,
        238, 76, 149, 11, 66, 250, 195, 78, 8, 46, 161, 102, 40, 217, 36, 178, 118, 91, 162, 73,
        109, 139, 209, 37,
    ];
    const SALT_B: [u8; 64] = [
        31, 221, 168, 51, 136, 7, 199, 49, 177, 18, 16, 89, 39, 128, 236, 95, 96, 81, 127, 169, 25,
        181, 74, 13, 45, 229, 122, 159, 147, 201, 156, 239, 160, 224, 59, 77, 174, 42, 245, 176,
        200, 235, 187, 60, 131, 83, 153, 97, 23, 43, 4, 126, 186, 119, 214, 38, 225, 105, 20, 99,
        85, 33, 12, 125,
    ];
    let mut out = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        out[i] = SALT_A[i] ^ SALT_B[i];
        i += 1;
    }
    out
};

// ───────────── Codex ─────────────

/// Codex CLI 的登录态文件(Windows 下在 %USERPROFILE%\.codex\auth.json)。
/// 明文 JSON,无解密 —— 比 Trae/WorkBuddy 都简单,唯一门槛是 auth_mode。
fn codex_candidates() -> Vec<Credential> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    let Some(home) = home else {
        return Vec::new();
    };
    let path = home.join(".codex").join("auth.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Some(token) = codex_token_from_storage(&raw) else {
        return Vec::new();
    };
    vec![Credential {
        token,
        source: "CLI 登录态".into(),
    }]
}

/// 只认 ChatGPT 订阅登录(auth_mode=chatgpt)。用平台 API Key 登 CLI 的机器
/// 没有 OAuth tokens,查不到订阅额度 —— 按「未检测到登录信息」处理(裁决③)。
/// 不在这里校验 exp:CLI 平时自动续期,真过期了 401 的文案会指路。
fn codex_token_from_storage(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    if v.get("auth_mode").and_then(|m| m.as_str()) != Some("chatgpt") {
        return None;
    }
    let token = v.get("tokens")?.get("access_token")?.as_str()?.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OpenSSL(node crypto)按同一套 KDF + AES-128-CBC 生成的向量。
    /// 它证明我们的实现和公开实现/浏览器端是同一套算法,而不是"自己和自己对得上"。
    #[test]
    fn tc_container_decrypts_openssl_vector() {
        // 固定随机密钥 = 0x01..0x20,明文 = {"token":"test-access-token",...}
        const B64: &str = "dGMFEAAAAQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyC+IPfThqGIJGciaP7auGWN7A5id7NgqGT8a1LaRzK4yMzXjBjfoA8gK5NJ6fyQFgXzqk3TUK3jZhTHQAbJt2aJi1cycYYAbj0hqsE6rsdGSGVImufKz0gh5t+/Oegd+1VrMGUO1ggBoMrdVskA6h9VPlABXhY6ap/ILwHFrS0hMupn3bqHUi/SSXbvY0lXzQchQHVO4AYbEVNxPJpsAmMX";
        let plain = decrypt_tc(B64).expect("应能解密");
        let v: serde_json::Value = serde_json::from_str(&plain).expect("明文应是 JSON");
        assert_eq!(v["token"], "test-access-token");
        assert_eq!(v["userId"], "123");
    }

    /// 头部不对 / 长度不对的内容必须返回 None,不能 panic 也不能瞎猜。
    #[test]
    fn tc_container_rejects_garbage() {
        assert!(decrypt_tc("not-base64!!").is_none());
        // 合法 base64,但没有 tc 头
        let junk = base64::engine::general_purpose::STANDARD.encode([0u8; 64]);
        assert!(decrypt_tc(&junk).is_none());
        // 头对但密文长度不是 16 的倍数
        let mut buf = vec![0x74, 0x63, 0x05, 0x10, 0x00, 0x00];
        buf.extend_from_slice(&[7u8; 32]);
        buf.extend_from_slice(&[9u8; 20]);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
        assert!(decrypt_tc(&b64).is_none());
    }

    /// 老版本 Trae 直接存明文 JSON,也要能读出来。
    #[test]
    fn plaintext_storage_value_is_read() {
        let raw = r#"{"iCubeAuthInfo://icube.cloudide":"{\"token\":\"abc123\",\"userId\":\"42\"}",
                      "other":"x"}"#;
        assert_eq!(trae_token_from_storage(raw).as_deref(), Some("abc123"));
        // 空 token 视为没有
        let empty = r#"{"iCubeAuthInfo://icube.cloudide":"{\"token\":\"\"}"}"#;
        assert!(trae_token_from_storage(empty).is_none());
        // 没有这个键
        assert!(trae_token_from_storage(r#"{"a":1}"#).is_none());
        // 不是 JSON
        assert!(trae_token_from_storage("<<<").is_none());
    }

    #[test]
    fn codex_reads_chatgpt_oauth_only() {
        let ok = r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,
            "tokens":{"access_token":"eyJhbC.x.y","refresh_token":"rt","account_id":"u-1"}}"#;
        assert_eq!(codex_token_from_storage(ok).as_deref(), Some("eyJhbC.x.y"));

        // 平台 Key 登录的 CLI:没有订阅额度可查,按未检测到处理(裁决③)
        let key = r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-x"}"#;
        assert!(codex_token_from_storage(key).is_none());

        // 空 token / 缺 tokens / 不是 JSON 一律 None,不 panic
        assert!(
            codex_token_from_storage(r#"{"auth_mode":"chatgpt","tokens":{"access_token":" "}}"#)
                .is_none()
        );
        assert!(codex_token_from_storage(r#"{"auth_mode":"chatgpt"}"#).is_none());
        assert!(codex_token_from_storage("{").is_none());
    }
}
