//! WorkBuddy 桌面端的凭据与取数(2026-10-19,取证自 `dsh-connect-workbuddy` v4.8.1)。
//!
//! ## 为什么要有这个模块
//!
//! 挂件原来只走**网页会话**(webview 的 `session` / `session_2` 两个 Cookie)。那条路是
//! **约 7 天的绝对 TTL**:到期后 `auth_mode` 仍然是"已登录",但每次取数都 401 ——
//! 界面就会停在"已登录取数失败",而用户点「重新登录」也只是把同一个 7 天窗口重开一次。
//! 本机实测:最后一次成功快照是 2026-10-15 11:56,之后三天全空,与 7 天 TTL 吻合。
//!
//! 桌面端(WorkBuddy 5.6+)自己那份凭据比网页会话长得多,而且是**可刷新**的:
//! accessToken 有效期 28 天、refreshToken 30 天。所以取数改成"桌面端优先、网页会话兜底"。
//!
//! ## 桌面端凭据的形态(真机取证,2026-10-19)
//!
//! `%LOCALAPPDATA%\CodeBuddyExtension\Data\Public\auth\workbuddy-desktop.info`:
//!
//! ```json
//! { "auth": { "accessToken": { "$wbEncrypted": 1, "envelope": "<base64>" },
//!             "refreshToken": { … }, "domain": "www.codebuddy.cn",
//!             "expiresAt": 1794709711836, "refreshExpiresAt": 1794882511836 },
//!   "account": { "uin": 330100933717, … } }
//! ```
//!
//! `envelope` 解 base64 后是 `{suite, keyId, nonce, authTag, ciphertext}`(AES-256-GCM)。
//! 32 字节密钥**不在文件里**:要向装好的桌面端自己问 —— 以 `ELECTRON_RUN_AS_NODE=1`
//! 跑 `WorkBuddy.exe -e "…loggerGet()"`,它回一段 JSON,`atRestSecretKey` 是 base64;
//! 真正的 key = `sha256(atRestSecretKey 字符串)`,而信封里的 `keyId` 正是这个 key 的
//! sha256 前 16 位十六进制 —— 对不上就说明"这份凭据不是这把 key 写的",必须明说,不能硬解。
//!
//! AAD 是一段带域前缀的定长转录(`WB-AAD\0` + 版本字节 + 长度前缀字段),逐字节照抄
//! 上游实现;写错一位 GCM 就会认证失败 —— 这正是它防止"解出半截 token"的设计。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ───────────────────────── 常量(逐字对齐上游)─────────────────────────

/// AAD 转录的域前缀。
const AAD_DOMAIN: &[u8] = b"WB-AAD\0";
/// 字段级信封的格式标识。
const FORMAT_ID_FIELD: &str = "WBEV1";
/// 对称信封的方案名。
const SYMMETRIC_SCHEME: &str = "sym-v1";
/// 字段级信封的 framing code。
const FRAMING_CODE_FIELD: u8 = 2;

/// 桌面端可执行文件名(国内版 / 国际版)。
const EXE_NAMES: [&str; 2] = ["WorkBuddy.exe", "WorkBuddyAI.exe"];

/// 取 key 的超时。桌面端是 Electron,冷启动也要一两秒。
const KEY_TIMEOUT: Duration = Duration::from_secs(15);
/// key 的缓存时长(用户裁决:缓存,不要每次轮询都起进程)。
const KEY_TTL: Duration = Duration::from_secs(6 * 3600);

/// 国内版计费基址。**注意是 codebuddy.cn**(桌面端凭据的 domain 也是它)。
pub const CN_BILLING_BASE: &str = "https://www.codebuddy.cn";
/// 备用基址。
pub const CN_BILLING_BASE_ALT: &str = "https://www.workbuddy.cn";
/// 取数路径。**必须带 `/v2`** —— 少一段就是 404/401(2026-10-19 实测)。
pub const BILLING_PATH: &str = "/v2/billing/meter/get-user-resource";

// ───────────────────────── 错误(每档都要能单独说清)─────────────────────────

/// 桌面端凭据这条路走不通的原因。**逐档分开**:用户能做的动作完全不同。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopErr {
    /// 没找到桌面端可执行文件(没装 / 装在自定义目录)。
    AppNotFound,
    /// 找到了 exe,但问不出密钥(老版本没有那个原生绑定 / 进程起不来)。
    KeyUnavailable(String),
    /// 有凭据文件,但字段是加密的,且拿不到匹配的 key。
    KeyMismatch { want: String, got: String },
    /// 凭据文件读不了 / 结构不认识。
    BadCredential(String),
    /// accessToken 已过期(且没有 refresh 可用)。
    Expired,
}

impl DesktopErr {
    /// 给用户看的一句话。**必须说对方向**:能自己修的别让人去重装。
    pub fn message(&self) -> String {
        match self {
            DesktopErr::AppNotFound => {
                "没找到 WorkBuddy 桌面端 —— 装一次桌面端并登录,挂件就能直接读它的凭据\
                 (也可以用 WORKBUDDY_APP_EXECUTABLE 指定路径)"
                    .to_string()
            }
            DesktopErr::KeyUnavailable(e) => format!(
                "WorkBuddy 桌面端在,但问不出它的凭据密钥({e})—— 请确认桌面端版本是 5.6 以上,\
                 或先手动打开一次桌面端再重试"
            ),
            DesktopErr::KeyMismatch { want, got } => format!(
                "凭据是用另一把密钥加密的(信封 {want},本机 {got})—— 通常是换过账号或桌面端重装过,\
                 在桌面端重新登录一次即可"
            ),
            DesktopErr::BadCredential(e) => {
                format!("桌面端凭据文件读不出来({e})—— 在桌面端重新登录一次")
            }
            DesktopErr::Expired => {
                "桌面端凭据已过期(accessToken 与 refreshToken 都超期)—— 打开 WorkBuddy 桌面端一次,\
                 它会自动续期,然后回来刷新即可"
                    .to_string()
            }
        }
    }
}

// ───────────────────────── 凭据文件 ─────────────────────────

/// 桌面端 auth 文件里我们关心的部分。
#[derive(Debug, Clone, Default)]
pub struct DesktopCredential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub domain: String,
    pub expires_at_ms: i64,
    pub refresh_expires_at_ms: i64,
    pub uin: String,
    /// 这份凭据是从哪个文件读出来的(界面要显示"读的是哪份")。
    pub source: String,
}

impl DesktopCredential {
    pub fn expired(&self, now_ms: i64) -> bool {
        self.expires_at_ms > 0 && now_ms >= self.expires_at_ms
    }
    pub fn refresh_expired(&self, now_ms: i64) -> bool {
        self.refresh_expires_at_ms > 0 && now_ms >= self.refresh_expires_at_ms
    }
    /// 区域:凭据 domain 落在国际版域名上就是 global,其余算国内版。
    pub fn is_global(&self) -> bool {
        let d = self.domain.to_ascii_lowercase();
        d.contains("workbuddy.ai") || d.contains("codebuddy.ai")
    }
}

mod locate;
pub use locate::{auth_dirs, credential_files, drive_letters, find_app_exe};
use locate::app_roots;
#[cfg(all(test, windows))]
use locate::parse_display_icon;

// ───────────────────────── 信封解密 ─────────────────────────

/// 加密字段的包装。
#[derive(Debug, Deserialize)]
struct Wrapper {
    #[serde(rename = "$wbEncrypted")]
    marker: u8,
    envelope: String,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    suite: u32,
    #[serde(rename = "keyId")]
    key_id: String,
    nonce: String,
    #[serde(rename = "authTag")]
    auth_tag: String,
    ciphertext: String,
}

fn b64(s: &str) -> Result<Vec<u8>, DesktopErr> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| DesktopErr::BadCredential(format!("base64 解不开: {e}")))
}

fn u32be(n: u32) -> [u8; 4] {
    n.to_be_bytes()
}

fn len_prefixed(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(4 + b.len());
    out.extend_from_slice(&u32be(b.len() as u32));
    out.extend_from_slice(b);
    out
}

/// AAD 转录:域前缀 + 版本 + 格式/方案(长度前缀)+ suite + keyId + framing + 两字节 0。
/// 逐字节对齐上游 `fieldAad()`。
fn field_aad(key_id: &str, suite: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(AAD_DOMAIN);
    out.push(1);
    out.extend_from_slice(&len_prefixed(FORMAT_ID_FIELD));
    out.extend_from_slice(&len_prefixed(SYMMETRIC_SCHEME));
    out.extend_from_slice(&u32be(suite));
    out.extend_from_slice(&len_prefixed(key_id));
    out.push(FRAMING_CODE_FIELD);
    out.push(0);
    out.push(0);
    out
}

/// 32 字节 key 的指纹:sha256 前 16 位十六进制。
pub fn key_id_of(key: &[u8]) -> String {
    let d = Sha256::digest(key);
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// 用给定 key 开一个加密字段。keyId 不匹配 / 认证失败都要明确报错,不返回半截明文。
pub fn open_field(wrapper_json: &serde_json::Value, key: &[u8]) -> Result<String, DesktopErr> {
    let w: Wrapper = serde_json::from_value(wrapper_json.clone())
        .map_err(|e| DesktopErr::BadCredential(format!("字段不是加密信封: {e}")))?;
    if w.marker != 1 {
        return Err(DesktopErr::BadCredential("信封版本不是 1".into()));
    }
    let raw = b64(&w.envelope)?;
    let env: Envelope = serde_json::from_slice(&raw)
        .map_err(|e| DesktopErr::BadCredential(format!("信封 JSON 解不开: {e}")))?;
    let got = key_id_of(key);
    if env.key_id != got {
        return Err(DesktopErr::KeyMismatch { want: env.key_id, got });
    }
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| DesktopErr::BadCredential("key 长度不对(要 32 字节)".into()))?;
    let nonce_bytes = b64(&env.nonce)?;
    if nonce_bytes.len() != 12 {
        return Err(DesktopErr::BadCredential(format!(
            "nonce 长度 {} 不是 12",
            nonce_bytes.len()
        )));
    }
    let nonce = Nonce::from_slice(&nonce_bytes);
    let aad = field_aad(&env.key_id, env.suite);
    let ct = b64(&env.ciphertext)?;
    let tag = b64(&env.auth_tag)?;
    if tag.len() != 16 {
        return Err(DesktopErr::BadCredential(format!(
            "authTag 长度 {} 不是 16",
            tag.len()
        )));
    }
    // aes-gcm 的 Aead 接口把 tag 拼在密文尾部
    let mut buf = ct;
    buf.extend_from_slice(&tag);
    let plain = cipher
        .decrypt(nonce, Payload { msg: &buf, aad: &aad })
        .map_err(|_| {
            DesktopErr::BadCredential(
                "信封认证失败(AAD/密钥/密文有一处对不上)—— 不返回半截 token".into(),
            )
        })?;
    String::from_utf8(plain).map_err(|e| DesktopErr::BadCredential(format!("明文不是 UTF-8: {e}")))
}

/// 从 auth 文件 JSON 里取一个可能加密、也可能明文的字段。
fn field_text(doc: &serde_json::Value, path: &[&str], key: Option<&[u8]>) -> Result<Option<String>, DesktopErr> {
    let mut cur = doc;
    for k in path {
        match cur.get(*k) {
            Some(v) => cur = v,
            None => return Ok(None),
        }
    }
    if let Some(s) = cur.as_str() {
        return Ok(Some(s.to_string()));
    }
    if cur.get("$wbEncrypted").is_some() {
        let Some(key) = key else {
            return Err(DesktopErr::BadCredential("字段是加密的,但没有可用密钥".into()));
        };
        return open_field(cur, key).map(Some);
    }
    Ok(None)
}

fn num_ms(doc: &serde_json::Value, path: &[&str]) -> i64 {
    let mut cur = doc;
    for k in path {
        match cur.get(*k) {
            Some(v) => cur = v,
            None => return 0,
        }
    }
    cur.as_i64()
        .or_else(|| cur.as_f64().map(|f| f as i64))
        .unwrap_or(0)
}

/// 解析一份 auth 文件。`key = None` 时只能读明文字段(加密的就报 BadCredential)。
pub fn parse_auth_file(text: &str, source: &str, key: Option<&[u8]>) -> Result<DesktopCredential, DesktopErr> {
    let doc: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| DesktopErr::BadCredential(format!("JSON 解不开: {e}")))?;
    let access = field_text(&doc, &["auth", "accessToken"], key)?
        .ok_or_else(|| DesktopErr::BadCredential("没有 auth.accessToken".into()))?;
    let refresh = field_text(&doc, &["auth", "refreshToken"], key).unwrap_or(None);
    let domain = doc
        .get("auth")
        .and_then(|a| a.get("domain"))
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();
    let uin = doc
        .get("account")
        .and_then(|a| a.get("uin"))
        .map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => String::new(),
        })
        .unwrap_or_default();
    Ok(DesktopCredential {
        access_token: access,
        refresh_token: refresh.filter(|s| !s.is_empty()),
        domain,
        expires_at_ms: num_ms(&doc, &["auth", "expiresAt"]),
        refresh_expires_at_ms: num_ms(&doc, &["auth", "refreshExpiresAt"]),
        uin,
        source: source.to_string(),
    })
}

// ───────────────────────── 向桌面端要密钥 ─────────────────────────

/// 缓存:key 与其取回时间。
static KEY_CACHE: Mutex<Option<(Vec<u8>, Instant)>> = Mutex::new(None);

/// 问桌面端要 at-rest key 的原始 payload(那段 JSON 字符串)。
///
/// **必须带超时**:桌面端是 Electron,个别环境下会卡在启动阶段 —— 挂件是常驻轮询程序,
/// 不能因为一个子进程把整条取数链挂住(与 `tasklist` 那条 5 秒超时同一个理由)。
fn fetch_key_payload(exe: &Path) -> Result<String, DesktopErr> {
    use std::process::{Command, Stdio};
    const SCRIPT: &str = "try{process.stdout.write(process._linkedBinding('electron_browser_workbuddy_storage').loggerGet())}catch(e){process.exitCode=3;process.stderr.write(String(e&&e.message||e))}";
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new(exe);
        // 挂件是 GUI 程序:别给子进程开控制台窗口
        c.creation_flags(0x0800_0000);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = Command::new(exe);
    cmd.arg("-e")
        .arg(SCRIPT)
        .env("ELECTRON_RUN_AS_NODE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd
        .spawn()
        .map_err(|e| DesktopErr::KeyUnavailable(format!("起不来: {e}")))?;
    run_child_with_timeout(child, KEY_TIMEOUT)
}

/// 等子进程退出并读走它的 stdout，**总耗时不超过 `timeout`**。
///
/// 2026-11-10 真机:这段逻辑原来实测要 **104~133 秒**才返回,而 `KEY_TIMEOUT` 是 15 秒。
/// 两个独立的坑,缺一不可:
///
/// 1. **只杀 launcher 杀不掉整棵树**。Electron 会派生子进程,launcher 被 `kill()` 之后
///    那些派生进程仍然握着 stdout/stderr 的**写端**,管道等不到 EOF。所以超时必须杀进程树。
/// 2. **读端不能用 `thread::scope` + `join()`**。作用域结束会强制等所有线程收尾,
///    等于把 15 秒超时又变回"等进程自己死"。改成 detached 线程 + channel:
///    超时直接返回,读线程随管道关闭自行结束。
fn run_child_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<String, DesktopErr> {
    use std::io::Read;
    use std::sync::mpsc;

    let pid = child.id();
    let (tx, rx) = mpsc::channel::<(bool, String)>();
    let mut readers = 0usize;
    // 两个管道类型不同(ChildStdout / ChildStderr),分开写而不硬凑一个数组
    if let Some(mut r) = child.stdout.take() {
        readers += 1;
        let tx = tx.clone();
        // 读线程故意不 join:它可能在等一个迟迟不来的 EOF,不能让它挡着返回值
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = r.read_to_string(&mut buf);
            let _ = tx.send((true, buf));
        });
    }
    if let Some(mut r) = child.stderr.take() {
        readers += 1;
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = r.read_to_string(&mut buf);
            let _ = tx.send((false, buf));
        });
    }
    drop(tx);

    let deadline = Instant::now() + timeout;
    let mut why: Option<String> = None;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_process_tree(pid);
                    let _ = child.kill();
                    let _ = child.wait();
                    why = Some(format!("桌面端 {} 秒内没有应答", timeout.as_secs()));
                    break;
                }
                std::thread::sleep(Duration::from_millis(80));
            }
            Err(e) => {
                kill_process_tree(pid);
                let _ = child.kill();
                why = Some(format!("等待桌面端失败: {e}"));
                break;
            }
        }
    }
    if let Some(w) = why {
        return Err(DesktopErr::KeyUnavailable(w));
    }

    // 进程已退出 → 写端关闭 → 两个读线程立刻送出结果。
    // 仍然带剩余预算:极端情况下(写端被别的进程继承)也不会把超时拖长。
    let mut out = String::new();
    let mut err = String::new();
    let mut got = 0usize;
    while got < readers {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok((true, s)) => { out = s; got += 1; }
            Ok((false, s)) => { err = s; got += 1; }
            Err(_) => break,
        }
    }

    let stdout = out.trim().to_string();
    if stdout.is_empty() {
        let err = err.trim().to_string();
        return Err(DesktopErr::KeyUnavailable(if err.is_empty() {
            "桌面端没有输出".to_string()
        } else {
            err
        }));
    }
    Ok(stdout)
}

/// 杀掉整个进程树。`Child::kill()` 只作用于 launcher,Electron 派生出来的进程
/// 会继续持有管道写端(见 `run_child_with_timeout` 的第 1 条)。
#[cfg(windows)]
fn kill_process_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    let Some(system) = std::env::var_os("SystemRoot") else { return };
    let _ = Command::new(std::path::PathBuf::from(system).join("System32").join("taskkill.exe"))
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .creation_flags(0x0800_0000) // 别给挂件弹控制台窗口
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn kill_process_tree(_pid: u32) {}

/// 从 payload 里派生 32 字节 key。
/// **关键细节**:上游是对 `atRestSecretKey` 这个 base64 **字符串**做 sha256(不是解码后的字节)。
pub fn derive_key(payload_json: &str) -> Result<Vec<u8>, DesktopErr> {
    let v: serde_json::Value = serde_json::from_str(payload_json)
        .map_err(|e| DesktopErr::KeyUnavailable(format!("payload 不是 JSON: {e}")))?;
    let secret = v
        .get("atRestSecretKey")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| DesktopErr::KeyUnavailable("payload 里没有 atRestSecretKey".into()))?;
    Ok(Sha256::digest(secret.as_bytes()).to_vec())
}

/// 拿到 key:先看缓存,过期才起进程(用户裁决:缓存,不要每次轮询都起)。
pub fn at_rest_key() -> Result<Vec<u8>, DesktopErr> {
    if let Ok(g) = KEY_CACHE.lock() {
        if let Some((k, at)) = g.as_ref() {
            if at.elapsed() < KEY_TTL {
                crate::diaglog::write_keyed(
                    "wb",
                    "key-cache",
                    &format!("用缓存的 at-rest key({} 秒前取到)", at.elapsed().as_secs()),
                );
                return Ok(k.clone());
            }
        }
    }
    let started = Instant::now();
    let exe = match find_app_exe() {
        Some(e) => e,
        None => {
            crate::diaglog::write_keyed(
                "wb",
                "key-noexe",
                "拿不到 at-rest key:没找到桌面端 exe(见上一条 exe-miss 的探测链)",
            );
            return Err(DesktopErr::AppNotFound);
        }
    };
    match fetch_key_payload(&exe) {
        Ok(payload) => {
            crate::diaglog::write_keyed(
                "wb",
                "key-ok",
                &format!(
                    "拿到密钥响应:{} · payload {} 字节 · 耗时 {} ms",
                    exe.display(),
                    payload.len(),
                    started.elapsed().as_millis()
                ),
            );
            match derive_key(&payload) {
                Ok(key) => {
                    crate::diaglog::write_keyed(
                        "wb",
                        "key-derived",
                        &format!("派生密钥成功 · keyId={}", key_id_of(&key)),
                    );
                    if let Ok(mut g) = KEY_CACHE.lock() {
                        *g = Some((key.clone(), Instant::now()));
                    }
                    Ok(key)
                }
                Err(e) => {
                    crate::diaglog::write_keyed(
                        "wb",
                        "key-bad",
                        &format!("密钥响应里没有可用的 atRestSecretKey:{}", e.message()),
                    );
                    Err(e)
                }
            }
        }
        Err(e) => {
            crate::diaglog::write_keyed(
                "wb",
                "key-fail",
                &format!(
                    "问不出 at-rest key({} · 耗时 {} ms)",
                    e.message(),
                    started.elapsed().as_millis()
                ),
            );
            Err(e)
        }
    }
}

// ───────────────────────── 组装:读凭据 → 取数 ─────────────────────────

/// 读桌面端凭据(含解密),返回**所有**没过期可用的候选。
pub fn read_desktop_credentials(now_ms: i64) -> Result<Vec<DesktopCredential>, DesktopErr> {
    collect_desktop_credentials(now_ms, false)
}

/// 刚解析出来的一份凭据该不该进候选。
///
/// 单独成函数是为了能被单测钉住:这条判断以前直接埋在四处 match 里,四处都写"只收未过期",
/// 于是"过期候选 → 续期"整条链恒不可达,却没人发现(2026-10-19 第三轮体检)。
fn accept(c: &DesktopCredential, now_ms: i64, include_expired: bool) -> bool {
    include_expired || !c.expired(now_ms)
}

/// 扫候选目录并解密,产出凭据列表。`include_expired = true` 时**也把"accessToken 已过期
/// 但仍带 refreshToken"的候选带出来** —— 续期只能用这些。
///
/// 为什么要有这个参数:2026-10-19 第一版只在"候选里有过期凭据"时才续期,而这里把过期的一律
/// 过滤掉了 → 续期分支恒不可达,渠道到 accessToken 第 28 天照样断(界面还宣称会自动续)。
/// 现在把"要不要过期那份"交给调用方:`desktop_tokens` 用 true,展示/其它路径用 false。
///
/// 候选顺序(踩过两次才对):
///   1. **live 文件优先**(`workbuddy-desktop.info` / `-ai.info`),加密的也要拿 key 解开 ——
///      桌面端 5.6+ 就是加密的,这份才是"现在真正在用"的凭据;
///   2. 只有 live 读不出来时,才回落到历史备份(那些通常是明文,但 token 可能已被上游作废:
///      2026-10-19 真机上一份 6 月的备份 token 未过期却 401)。
///
/// 返回全部候选是为了让上层在 401 时能换下一份,而不是直接让用户重新登录。
pub(crate) fn collect_desktop_credentials(now_ms: i64, include_expired: bool) -> Result<Vec<DesktopCredential>, DesktopErr> {
    collect_from(&credential_files(), now_ms, include_expired)
}

/// 从给定的候选文件列表组装凭据。把"扫目录"与"读 + 判"拆开,是为了能用临时目录把
/// `include_expired` 这条判据端到端测掉 —— 它决定过期凭据能不能被送去续期。
fn collect_from(files: &[PathBuf], now_ms: i64, include_expired: bool) -> Result<Vec<DesktopCredential>, DesktopErr> {
    if files.is_empty() {
        // 把"扫了哪些目录、每个目录在不在"一起记下来 —— 数据目录被挪到别的盘时,
        // 这是唯一能看出"我们压根没往那儿看"的证据。
        let dirs: Vec<String> = auth_dirs()
            .iter()
            .map(|d| format!("{}{}", d.display(), if d.is_dir() { "(有)" } else { "(无)" }))
            .collect();
        crate::diaglog::write_keyed(
            "wb",
            "cred-empty",
            &format!("auth 目录里没有 workbuddy-desktop*.info · 扫过的目录:{}", dirs.join(" | ")),
        );
        return Err(DesktopErr::BadCredential(
            "auth 目录里没有 workbuddy-desktop*.info".into(),
        ));
    }
    crate::diaglog::write_keyed(
        "wb",
        "cred-files",
        &format!(
            "找到 {} 份候选凭据:{}",
            files.len(),
            files
                .iter()
                .map(|f| format!("{}{}", f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), if is_live_name(f) { "(live)" } else { "(备份)" }))
                .collect::<Vec<_>>()
                .join(" | ")
        ),
    );
    let mut out: Vec<DesktopCredential> = Vec::new();
    let mut first_err: Option<DesktopErr> = None;

    // ① live 文件:可能是加密的,必要时拿 key
    let mut live_err: Option<DesktopErr> = None;
    let mut need_key = false;
    for f in files {
        if !is_live_name(f) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        let src = f.to_string_lossy().to_string();
        match parse_auth_file(&text, &src, None) {
            Ok(c) if accept(&c, now_ms, include_expired) => {
                crate::diaglog::write_keyed(
                    "wb",
                    "cred-live-plain",
                    &format!(
                        "live 凭据是明文,直接可用 · {} · {} 字节 · 区域={} · access {} · 到期 {}",
                        src,
                        text.len(),
                        if c.is_global() { "global" } else { "cn" },
                        crate::diaglog::secret_brief(&c.access_token),
                        fmt_ms(c.expires_at_ms)
                    ),
                );
                out.push(c)
            }
            Ok(c) => {
                crate::diaglog::write_keyed(
                    "wb",
                    "cred-live-expired",
                    &format!(
                        "live 凭据已过期 · {} · access {} · 到期 {} · refresh {}",
                        src,
                        crate::diaglog::secret_brief(&c.access_token),
                        fmt_ms(c.expires_at_ms),
                        c.refresh_token.as_deref().map(crate::diaglog::secret_brief).unwrap_or_else(|| "无".into())
                    ),
                );
                live_err.get_or_insert(DesktopErr::Expired);
            }
            Err(DesktopErr::BadCredential(_)) => need_key = true,
            Err(e) => {
                live_err.get_or_insert(e);
            }
        }
    }
    if need_key {
        crate::diaglog::write_keyed("wb", "cred-needkey", &format!("live 凭据是加密信封,需要 at-rest key({} 份 live 文件)", files.iter().filter(|f| is_live_name(f)).count()));
        match at_rest_key() {
            Ok(key) => {
                for f in files {
                    if !is_live_name(f) {
                        continue;
                    }
                    let Ok(text) = std::fs::read_to_string(f) else { continue };
                    let src = f.to_string_lossy().to_string();
                    match parse_auth_file(&text, &src, Some(&key)) {
                        Ok(c) if accept(&c, now_ms, include_expired) => {
                            crate::diaglog::write_keyed(
                                "wb",
                                "cred-live-open",
                                &format!(
                                    "解开 live 凭据 · {} · {} 字节 · 区域={} · access {} · 到期 {}",
                                    src,
                                    text.len(),
                                    if c.is_global() { "global" } else { "cn" },
                                    crate::diaglog::secret_brief(&c.access_token),
                                    fmt_ms(c.expires_at_ms)
                                ),
                            );
                            out.push(c)
                        }
                        Ok(c) => {
                            crate::diaglog::write_keyed(
                                "wb",
                                "cred-live-expired",
                                &format!(
                                    "live 凭据已过期 · {} · access {} · 到期 {} · refresh {}",
                                    src,
                                    crate::diaglog::secret_brief(&c.access_token),
                                    fmt_ms(c.expires_at_ms),
                                    c.refresh_token.as_deref().map(crate::diaglog::secret_brief).unwrap_or_else(|| "无".into())
                                ),
                            );
                            live_err.get_or_insert(DesktopErr::Expired);
                        }
                        Err(e) => {
                            crate::diaglog::write_keyed(
                                "wb",
                                "cred-live-bad",
                                &format!("解不开 live 凭据 · {src}:{}", e.message()),
                            );
                            live_err.get_or_insert(e);
                        }
                    }
                }
            }
            Err(e) => {
                crate::diaglog::write_keyed(
                    "wb",
                    "cred-nokey",
                    &format!("拿不到 at-rest key,没法解加密凭据:{}", e.message()),
                );
                live_err.get_or_insert(e);
            }
        }
    }

    // ② live 一份都没解出来,才回落到历史备份(明文直接读;加密的同样要 key)
    if out.is_empty() {
        let mut backup_need_key = false;
        for f in files {
            if is_live_name(f) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(f) else { continue };
            let src = f.to_string_lossy().to_string();
            match parse_auth_file(&text, &src, None) {
                Ok(c) if accept(&c, now_ms, include_expired) => {
                    crate::diaglog::write_keyed(
                        "wb",
                        "cred-backup",
                        &format!("回落到历史备份(明文)· {} · access {} · 到期 {}", src, crate::diaglog::secret_brief(&c.access_token), fmt_ms(c.expires_at_ms)),
                    );
                    out.push(c)
                }
                Ok(_) => {
                    first_err.get_or_insert(DesktopErr::Expired);
                }
                Err(DesktopErr::BadCredential(_)) => backup_need_key = true,
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        if backup_need_key {
            if let Ok(key) = at_rest_key() {
                for f in files {
                    if is_live_name(f) {
                        continue;
                    }
                    let Ok(text) = std::fs::read_to_string(f) else { continue };
                    let src = f.to_string_lossy().to_string();
                    if let Ok(c) = parse_auth_file(&text, &src, Some(&key)) {
                        if accept(&c, now_ms, include_expired) {
                            crate::diaglog::write_keyed(
                                "wb",
                                "cred-backup-open",
                                &format!("回落到历史备份(已解密)· {} · access {} · 到期 {}", src, crate::diaglog::secret_brief(&c.access_token), fmt_ms(c.expires_at_ms)),
                            );
                            out.push(c);
                        }
                    }
                }
            }
        }
    }

    if out.is_empty() {
        let e = live_err.or(first_err).unwrap_or(DesktopErr::Expired);
        crate::diaglog::write_keyed(
            "wb",
            "cred-none",
            &format!("一份可用凭据都没有(最终原因:{} · include_expired={include_expired})", e.message()),
        );
        return Err(e);
    }
    // 候选排序:**国内版优先**(挂件只展示国内版;国际版是同栈的另一套账号),
    // 同区域内 live 优先于历史备份。注意不能拿 source 判 live ——
    // `workbuddy-desktop-ai.info` 里也含 "workbuddy-desktop.info" 这个子串。
    out.sort_by_key(|c| {
        let live = is_live_name(Path::new(&c.source));
        (c.is_global(), !live)
    });
    Ok(out)
}

/// 体检报告用:把桌面端凭据这条链**当前的事实**拼成几行文本。
///
/// 为什么单独提供:日志是"发生过什么"的流水,而用户点「运行体检」想要的是
/// "现在这条链走到哪一步"。两样都要,不能互相替代 —— 用户报问题时往往只有截图,
/// 而这一段的每一行都能直接指认"是没装 / 没找到 / 拿不到密钥 / 凭据过期 / 被平台拒"。
///
/// **只描述形态与长度,不含任何令牌**。
pub fn diag_summary(now_ms: i64) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();

    // ① exe
    let exe = find_app_exe();
    lines.push(match &exe {
        Some(p) => format!("桌面端 exe · {}", p.display()),
        None => {
            let dirs = auth_dirs();
            format!(
                "桌面端 exe · 没找到(主程序名 {:?};盘符 {:?};{} 个候选目录)",
                EXE_NAMES,
                drive_letters(),
                app_roots().len()
            )
            .replace("· 没找到", &format!("· 没找到(顺带:auth 候选 {} 个)", dirs.len()))
        }
    });
    // 环境变量覆盖:设了但没用上,是最容易解释"为什么还是失败"的一条
    if let Ok(v) = std::env::var("WORKBUDDY_APP_EXECUTABLE") {
        lines.push(format!("桌面端 exe · WORKBUDDY_APP_EXECUTABLE={v}"));
    }

    // ② auth 目录(只列前几个:全列会把报告刷屏)
    let dirs = auth_dirs();
    let shown: Vec<String> = dirs
        .iter()
        .take(6)
        .map(|d| format!("{}{}", d.display(), if d.is_dir() { " ✓" } else { " ✗" }))
        .collect();
    lines.push(format!(
        "凭据目录 · {} 个候选:{}",
        dirs.len(),
        shown.join(" | ")
    ));

    // ③ 凭据文件 + 解析结果
    let files = credential_files();
    if files.is_empty() {
        lines.push("凭据文件 · 一个都没有(没装桌面端,或数据目录不在默认位置)".to_string());
    } else {
        for f in files.iter().take(4) {
            let kind = if is_live_name(f) { "live" } else { "备份" };
            match std::fs::read_to_string(f) {
                Ok(text) => {
                    // 先试明文:能读通就不必去起桌面端进程(体检要快、要无副作用)
                    match parse_auth_file(&text, &f.to_string_lossy(), None) {
                        Ok(c) => lines.push(format!(
                            "凭据文件 · {} [{kind} 明文] 区域={} access {} 到期 {}",
                            f.display(),
                            if c.is_global() { "global" } else { "cn" },
                            crate::diaglog::secret_brief(&c.access_token),
                            fmt_ms(c.expires_at_ms)
                        )),
                        Err(DesktopErr::BadCredential(_)) => lines.push(format!(
                            "凭据文件 · {} [{kind} 加密] {} 字节 · 需要桌面端密钥",
                            f.display(),
                            text.len()
                        )),
                        Err(e) => lines.push(format!("凭据文件 · {} [{kind}] 解析失败:{}", f.display(), e.message())),
                    }
                }
                Err(e) => lines.push(format!("凭据文件 · {} [{kind}] 读不了:{e}", f.display())),
            }
        }
    }

    // ④ 密钥缓存状态(不必真去起进程;缓存里有没有直接决定"这一轮走不走桌面端")
    let cached = KEY_CACHE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|(k, at)| (key_id_of(k), at.elapsed().as_secs())));
    lines.push(match cached {
        Some((id, age)) => format!("桌面端密钥 · 已缓存 keyId={id}({age} 秒前取到)"),
        None => "桌面端密钥 · 还没取过(首次取数时才起桌面端进程)".to_string(),
    });

    // ⑤ 凭据编排(不落库、不发请求,只看"能凑出几个候选令牌")
    let creds = collect_desktop_credentials(now_ms, true);
    match creds {
        Ok(c) => lines.push(format!(
            "可用令牌 · {} 个[{}]",
            c.len(),
            c.iter()
                .map(|x| format!("{}({})", crate::diaglog::secret_brief(&x.access_token), if x.is_global() { "global" } else { "cn" }))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Err(e) => lines.push(format!("可用令牌 · 0 个 → {}", e.message())),
    }
    if let Some(p) = crate::diaglog::log_path() {
        lines.push(format!("排查日志 · {}", p.display()));
    }
    lines
}

/// 把毫秒时间戳写成 `YYYY-MM-DD HH:MM:SS(本地)`,0 写成「未记录」。
/// 日志里给人看的,不是给机器解析的 —— 排查时最常问的是"这份凭据什么时候到期"。
fn fmt_ms(ms: i64) -> String {
    if ms <= 0 {
        return "未记录".into();
    }
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| format!("{ms}(解析不了)"))
}

/// 上次续期成果在凭据管理器里的形状。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRefresh {
    pub global: bool,
    pub access: String,
    /// 上游轮换过的 refreshToken(留着,桌面端文件被删时还能用)
    pub refresh: Option<String>,
    /// accessToken 的过期时刻(epoch 毫秒)。0 = 老数据没记过,按"还能用"处理。
    pub expires_at_ms: i64,
}

/// [`plan_tokens`] 的产出。
#[derive(Debug, Default)]
pub struct TokenPlan {
    /// 取数按这个顺序试(最可能活着的排最前)
    pub tokens: Vec<String>,
    /// 续期成功、需要落库的(区域, 结果)
    pub to_store: Vec<(bool, Refreshed)>,
    /// 续期失败的原因(如实记日志,不参与取数)
    pub refresh_fails: Vec<String>,
}

/// 上游没给 `expiresIn` 时按多久就再续一次。故意给得很短:
/// 没记下来的过期时刻不能拿来当"这令牌还能挂 28 天"的依据。
const REFRESH_FALLBACK_TTL_SEC: i64 = 24 * 3600;

/// **纯函数**:把"上次续期成果 + 桌面端候选"排成取数用的顺序,并对已过期但仍可续的
/// 候选发起续期。不碰文件、不碰凭据管理器、不发请求(续期动作由参数注入)。
///
/// 为什么要单独拆出来:这段判据以前写在 `desktop_tokens` 里,和真文件、真 HTTP 搅在
/// 一起,结果"过期候选被上游过滤掉 → 续期恒不可达"的断链一路带到发版都没被用例碰到
/// (2026-10-19 第三轮体检抓到)。拆成纯函数后它能直接被单测钉住。
pub fn plan_tokens(
    now_ms: i64,
    stored: &[StoredRefresh],
    creds: &[DesktopCredential],
    refresh: &dyn Fn(&DesktopCredential) -> Result<Refreshed, String>,
) -> TokenPlan {
    let mut plan = TokenPlan::default();
    // ① 过期但仍带可用 refreshToken 的:先续。续来的最新,排最前。
    for c in creds {
        if !c.expired(now_ms) {
            continue;
        }
        let has_rt = c
            .refresh_token
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
        // refreshToken 自己也超期 → 只能让人在桌面端登一次,机器替不了
        if !has_rt || c.refresh_expired(now_ms) {
            continue;
        }
        match refresh(c) {
            Ok(new) => {
                plan.to_store.push((c.is_global(), new.clone()));
                push_unique(&mut plan.tokens, &new.access_token);
            }
            Err(e) => plan.refresh_fails.push(format!("{}: {e}", c.source)),
        }
    }
    // ② 上次续期的成果:没过期才顶用(没记过期时刻的老数据按能用处理)
    for s in stored {
        if s.expires_at_ms == 0 || now_ms < s.expires_at_ms {
            push_unique(&mut plan.tokens, &s.access);
        }
    }
    // ③ 桌面端文件里现在还能用的(顺序已由 collect 排好:国内版优先、live 优先)
    for c in creds {
        if !c.expired(now_ms) {
            push_unique(&mut plan.tokens, &c.access_token);
        }
    }
    plan
}

fn push_unique(v: &mut Vec<String>, t: &str) {
    let t = t.trim();
    if !t.is_empty() && !v.iter().any(|x| x == t) {
        v.push(t.to_string());
    }
}

/// 读桌面端候选(顺序:刚续来的 → 上次续期成果 → 桌面端 live → 历史备份)。
///
/// 落库与冷却这些带副作用的都在这一层,判据本身在 [`plan_tokens`](可单测)。
///
/// `region`:`None` = 两个区都要(签到侧"逐份试"的旧口径),
/// `Some(true)` = 只要国际版,`Some(false)` = 只要国内版。
///
/// **为什么要分区**:国际版 token 打国内域名一定 401,反之亦然(2026-10-19 真机:
/// 两份凭据同时在线)。两个渠道各取各的区,才不会每轮先失败一次 ——
/// 否则日志里天天是 `cred-switch`,真正的凭据失效反而被噪音埋掉。
pub fn desktop_tokens_region(now_ms: i64, region: Option<bool>) -> Vec<String> {
    // include_expired = true:过期那份正是续期要的对象(见 plan_tokens 的来由)
    let creds = match collect_desktop_credentials(now_ms, true) {
        Ok(c) => c,
        Err(e) => {
            // 以前这里 unwrap_or_default 一吞,日志里就只剩"取数失败"。
            // 现在把"凭据这一层为什么空"如实说出来。
            crate::diaglog::write_keyed(
                "wb",
                "tokens-empty",
                &format!("桌面端凭据为空:{}", e.message()),
            );
            Vec::new()
        }
    };
    desktop_tokens_from(now_ms, region, creds)
}

pub(crate) fn desktop_tokens_from(now_ms: i64, region: Option<bool>, mut creds: Vec<DesktopCredential>) -> Vec<String> {
    if let Some(global) = region {
        creds.retain(|c| c.is_global() == global);
    }
    let mut stored: Vec<StoredRefresh> = Vec::new();
    for global in [false, true] {
        if region.map(|want| want != global).unwrap_or(false) {
            continue;
        }
        if let Some(s) = load_refreshed(global) {
            stored.push(s);
        }
    }
    let plan = plan_tokens(now_ms, &stored, &creds, &refresh_with_cooldown);
    for (global, new) in &plan.to_store {
        let ttl = new.expires_in_sec.unwrap_or(REFRESH_FALLBACK_TTL_SEC).max(60);
        let exp = now_ms + ttl * 1000;
        if let Err(e) = store_refreshed(*global, &new.access_token, new.refresh_token.as_deref(), exp) {
            crate::diaglog::write_keyed(
                "wb",
                "refresh-store",
                &format!("续期结果落库失败: {e}"),
            );
        }
    }
    for m in &plan.refresh_fails {
        crate::diaglog::write_keyed("wb", "refresh-fail", &format!("续期没成: {m}"));
    }
    crate::diaglog::write_keyed(
        "wb",
        "tokens-plan",
        &format!(
            "凭据编排({}):桌面端候选 {} 份 · 上次续期成果 {} 份 · 续期成功 {} 份 · 续期失败 {} 条 · 最终可用令牌 {} 个[{}]",
            match region {
                Some(true) => "国际版",
                Some(false) => "国内版",
                None => "全部",
            },
            creds.len(),
            stored.len(),
            plan.to_store.len(),
            plan.refresh_fails.len(),
            plan.tokens.len(),
            plan.tokens
                .iter()
                .map(|t| crate::diaglog::secret_brief(t))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    plan.tokens
}

/// 两个区的令牌合并(签到侧"逐份试"的旧口径)。取数侧**不要**用它 ——
/// 每个渠道只该看自己那个区,见 [`desktop_tokens_region`]。
pub fn desktop_tokens(now_ms: i64) -> Vec<String> {
    desktop_tokens_region(now_ms, None)
}

/// 这台机器当前登着的某个区域的账户号集合(auth 文件里的 `account.uin`,
/// 续期/换令牌都不变,换账号才变)。
///
/// 用途:`fetch` 侧给 WorkBuddy AI 的"积分到账基线"按账户分段 —— 换账号后,
/// 旧账户的最后余额不该被当成新账户的基线,那会凭空记出一笔大额"到账"。
/// 只读凭据文件、不触发续期;读不到(没装/没登录)返回空,调用方退回不分段的老口径。
/// 顺序与去重由 `fetch::accounts_fp` 负责,这里只如实收集。
pub fn region_account_uins(now_ms: i64, region: bool) -> Vec<String> {
    collect_desktop_credentials(now_ms, true)
        .map(|cs| {
            cs.into_iter()
                .filter(|c| c.is_global() == region)
                .map(|c| c.uin)
                .filter(|u| !u.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// 续期失败的冷却:挂件每 60 秒一轮,不冷却就会拿同一份失效的 refreshToken 每天打 1440 次。
const REFRESH_COOLDOWN: Duration = Duration::from_secs(30 * 60);
static REFRESH_FAIL_AT: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

fn refresh_with_cooldown(cred: &DesktopCredential) -> Result<Refreshed, String> {
    if cfg!(test) {
        // 单测不真发请求(要验判据请用 plan_tokens 注入假实现)
        return Err("单测里不发续期请求".into());
    }
    let slot = refresh_slot(cred.is_global()).to_string();
    let lk = || REFRESH_FAIL_AT.lock().unwrap_or_else(|p| p.into_inner());
    let cooling = lk()
        .as_ref()
        .and_then(|m| m.get(&slot))
        .map(|t| t.elapsed() < REFRESH_COOLDOWN)
        .unwrap_or(false);
    if cooling {
        return Err(format!(
            "距上次续期失败不到 {} 分钟,先不重试",
            REFRESH_COOLDOWN.as_secs() / 60
        ));
    }
    match try_refresh(cred) {
        Ok(r) => {
            if let Some(m) = lk().as_mut() {
                m.remove(&slot);
            }
            Ok(r)
        }
        Err(e) => {
            lk().get_or_insert_with(HashMap::new).insert(slot, Instant::now());
            Err(e)
        }
    }
}

/// 续期一次(同步包装,给 `desktop_tokens` 这种同步入口用)。
/// 失败返回 Err —— 续不上不该静默丢弃:上面的调用点会继续用没过期的旧凭据。
fn try_refresh(cred: &DesktopCredential) -> Result<Refreshed, String> {
    let http = crate::http_pool::shared_default()?;
    crate::http_pool::refresh_runtime()?.block_on(refresh(http, cred))
}
fn is_live_name(p: &Path) -> bool {
    p.file_name()
        .map(|n| {
            let n = n.to_string_lossy().to_lowercase();
            n == "workbuddy-desktop.info" || n == "workbuddy-desktop-ai.info"
        })
        .unwrap_or(false)
}

/// 只要一份(界面/日志用):取第一个候选。
pub fn read_desktop_credential(now_ms: i64) -> Result<DesktopCredential, DesktopErr> {
    read_desktop_credentials(now_ms)?
        .into_iter()
        .next()
        .ok_or(DesktopErr::Expired)
}

// ───────────────────────── 续期后的落库 ─────────────────────────

/// 续期结果存哪:Windows 凭据管理器(与其它渠道密钥同一处),key 按区域分开。
/// 不写回桌面端自己的文件 —— 那是它的东西,我们只读(与插件的"只读桌面端 auth 文件"同口径)。
pub fn refresh_slot(region_global: bool) -> &'static str {
    if region_global {
        "workbuddy-refresh-global"
    } else {
        "workbuddy-refresh-cn"
    }
}

/// 读上次续期存下的成果(含过期时刻)。
pub fn load_refreshed(region_global: bool) -> Option<StoredRefresh> {
    let raw = crate::secrets::get(refresh_slot(region_global)).ok().flatten()?;
    // 存的是三行:`access` / `refresh`(可空) / `过期毫秒`
    let mut it = raw.split('\n');
    let access = it.next()?.trim().to_string();
    if access.is_empty() {
        return None;
    }
    let refresh = it.next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    // 老格式(两行)没有第三行 → 0,判据里按"还能用"处理,不会把用户挡在门外
    let expires_at_ms = it.next().and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(0);
    Some(StoredRefresh {
        global: region_global,
        access,
        refresh,
        expires_at_ms,
    })
}

/// 存续期结果(空 refresh 表示上游没轮换,沿用旧的)。
/// 必须连过期时刻一起存:只存令牌本身的话,这份令牌过期后仍会顶在候选最前面,
/// 每轮白打一次 401。
pub fn store_refreshed(
    region_global: bool,
    access: &str,
    refresh: Option<&str>,
    expires_at_ms: i64,
) -> Result<(), String> {
    let payload = format!(
        "{}\n{}\n{}",
        access.trim(),
        refresh.unwrap_or("").trim(),
        expires_at_ms
    );
    crate::secrets::set(refresh_slot(region_global), &payload)
}

/// 取数用的凭据:桌面端 accessToken(Bearer)或网页会话 Cookie。
#[derive(Debug, Clone)]
pub enum FetchAuth {
    Bearer(String),
    Cookie(String),
}

impl FetchAuth {
    /// 拼请求头里的鉴权部分。
    pub fn header_value(&self) -> String {
        match self {
            FetchAuth::Bearer(t) => format!("Bearer {t}"),
            FetchAuth::Cookie(c) => format!("Cookie: {c}"),
        }
    }
}

/// 按凭据的 domain 选基址:**国内版打 codebuddy.cn,国际版打 workbuddy.ai**。
/// 用国际版 token 打国内域名一定 401(2026-10-19 真机踩过:两份凭据同时在线)。
pub fn bases_for(cred: &DesktopCredential) -> [&'static str; 2] {
    if cred.is_global() {
        // 国际版:主域 workbuddy.ai;codebuddy.ai 是同栈备用品牌域
        ["https://www.workbuddy.ai", "https://www.codebuddy.ai"]
    } else {
        [CN_BILLING_BASE, CN_BILLING_BASE_ALT]
    }
}

/// 按顺序试该凭据所属区域的主基址 → 备用基址。返回第一个 2xx 的响应体。
pub async fn fetch_billing(
    http: &reqwest::Client,
    auth: &FetchAuth,
    body: &serde_json::Value,
    bases: [&'static str; 2],
) -> Result<(String, String), String> {
    let mut last: Option<String> = None;
    for base in bases {
        let url = format!("{base}{BILLING_PATH}");
        let mut req = http.post(&url).json(body);
        req = match auth {
            FetchAuth::Bearer(t) => req.bearer_auth(t),
            FetchAuth::Cookie(c) => req.header("Cookie", c),
        };
        // 桌面端形态的请求头(与插件同口径);网页会话那条靠 X-Client-Platform 过关
        req = req
            .header("Accept", "application/json")
            .header("X-Client-Platform", "web")
            .header("Origin", base)
            .header("Referer", format!("{base}/profile/plans-usage"));
        match req.send().await {
            Ok(r) => {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                // 业务 code 也记:HTTP 200 + code!=0(例如 10085 缺 X-Client-Platform)
                // 与 HTTP 401 是两个完全不同的根因,只看状态码会判错。
                let code = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("code").and_then(|c| c.as_i64()));
                let kind = match auth {
                    FetchAuth::Bearer(_) => "桌面端 Bearer",
                    FetchAuth::Cookie(_) => "网页会话 Cookie",
                };
                if status.is_success() {
                    crate::diaglog::write_keyed(
                        "wb",
                        "billing-ok",
                        &format!("取数成功 · {url} · HTTP {} · code={code:?} · 凭据={kind}", status.as_u16()),
                    );
                    return Ok((url, text));
                }
                crate::diaglog::write_keyed(
                    "wb",
                    &format!("billing-{url}-{}", status.as_u16()),
                    &format!(
                        "取数被拒 · {url} · HTTP {} · code={code:?} · 凭据={kind} · 响应 {}",
                        status.as_u16(),
                        text.chars().take(120).collect::<String>()
                    ),
                );
                last = Some(format!(
                    "{url} → HTTP {} {}",
                    status.as_u16(),
                    text.chars().take(160).collect::<String>()
                ));
            }
            Err(e) => {
                crate::diaglog::write_keyed(
                    "wb",
                    &format!("billing-net-{url}"),
                    &format!("取数请求失败 · {url} · {e}"),
                );
                last = Some(format!("{url} → 请求失败: {e}"));
            }
        }
    }
    Err(last.unwrap_or_else(|| "没有可用的取数地址".into()))
}

/// 取数请求体(与插件逐字对齐)。`now` 由调用方给,便于测试。
pub fn billing_body(now: chrono::DateTime<chrono::Local>) -> serde_json::Value {
    let fmt = |d: chrono::DateTime<chrono::Local>| d.format("%Y-%m-%d %H:%M:%S").to_string();
    serde_json::json!({
        "PageNumber": 1,
        "PageSize": 100,
        "ProductCode": "p_tcaca",
        "Status": [0, 3],
        "PackageEndTimeRangeBegin": fmt(now),
        // 上游给的是 +3185136000ms(约 36.9 天),照抄
        "PackageEndTimeRangeEnd": fmt(now + chrono::Duration::milliseconds(3_185_136_000)),
    })
}

// ───────────────────────── 续期(refresh)─────────────────────────

/// 续期端点用的 chat 基址(与计费基址**不是同一个**):
/// 国内版 `copilot.tencent.com`,国际版与计费同域。
pub fn chat_base(cred: &DesktopCredential) -> &'static str {
    if cred.is_global() {
        "https://www.workbuddy.ai"
    } else {
        "https://copilot.tencent.com"
    }
}

/// 续期路径。
pub const REFRESH_PATH: &str = "/v2/plugin/auth/token/refresh";

/// 续期结果。
#[derive(Debug, Clone, Default)]
pub struct Refreshed {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in_sec: Option<i64>,
    pub domain: Option<String>,
}

/// 用 refreshToken 换一份新的 accessToken。
///
/// 契约(2026-10-19 取证自 dsh-connect-workbuddy v4.8.1 `refreshToken()`):
/// `POST {chatBase}/v2/plugin/auth/token/refresh`,凭据放在
/// **`X-Refresh-Token` 头**里(不是 body),另带 `X-Auth-Refresh-Source: workbuddy`;
/// 响应 `data.accessToken` 是新的令牌,`data.refreshToken` 可能轮换。
///
/// 为什么值得做:accessToken 28 天、refreshToken 30 天,续期成功就等于**永不过期**;
/// 而网页会话是 7 天绝对 TTL,到期只能人工重登 —— 这是本次改造的另一半。
pub async fn refresh(
    http: &reqwest::Client,
    cred: &DesktopCredential,
) -> Result<Refreshed, String> {
    let Some(rt) = cred.refresh_token.as_deref().filter(|s| !s.is_empty()) else {
        return Err("没有 refreshToken,无法续期".into());
    };
    let base = chat_base(cred);
    let url = format!("{base}{REFRESH_PATH}");
    let resp = http
        .post(&url)
        .header("X-Refresh-Token", rt)
        .header("X-Auth-Refresh-Source", "workbuddy")
        .header("Accept", "application/json, text/plain, */*")
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Origin", base)
        .header("Referer", format!("{base}/"))
        .header("User-Agent", DESKTOP_UA_FOR_REFRESH)
        .send()
        .await
        .map_err(|e| format!("续期请求失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("续期响应不是 JSON(HTTP {status}): {e}"))?;
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
    if !status.is_success() || code != 0 {
        let msg = v.get("msg").and_then(|m| m.as_str()).unwrap_or("续期被拒");
        return Err(format!("续期失败: {msg}(code {code}, HTTP {status})"));
    }
    let data = v.get("data").cloned().unwrap_or(serde_json::Value::Null);
    let access = data
        .get("accessToken")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "续期响应里没有 accessToken".to_string())?;
    Ok(Refreshed {
        access_token: access.to_string(),
        refresh_token: data
            .get("refreshToken")
            .and_then(|t| t.as_str())
            .filter(|t| !t.is_empty())
            .map(String::from),
        expires_in_sec: data.get("expiresIn").and_then(|e| e.as_i64()),
        domain: data
            .get("domain")
            .and_then(|d| d.as_str())
            .filter(|d| !d.is_empty())
            .map(String::from),
    })
}

/// 续期请求的 UA(与插件 `CLIENT_UA` 同口径)。
const DESKTOP_UA_FOR_REFRESH: &str = "CLI/2.63.2 CodeBuddy/2.63.2";

#[cfg(test)]
mod tests;

/// `app_roots()` 的**纯注入**版本:只返回给定根下的常见装法,不含环境变量与注册表。
/// 单测用它伪造"装在 D 盘的某个自定义目录",既不用碰真盘,也不会被本机真实安装干扰
/// (本机确实装着 WorkBuddy,混进来的话这条用例就测不到"候选构造"本身了)。
#[cfg(test)]
fn app_roots_for(drives: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for root in drives {
        for sub in [
            "Programs\\WorkBuddy",
            "WorkBuddy",
            "Program Files\\WorkBuddy",
            "Program Files (x86)\\WorkBuddy",
            "Apps\\WorkBuddy",
            "Applications\\WorkBuddy",
            "Software\\WorkBuddy",
        ] {
            // 临时根是普通目录,不是盘根:用 join 而不是拼 `X:\`
            out.push(root.join(sub.replace('\\', "/")));
        }
    }
    out
}
