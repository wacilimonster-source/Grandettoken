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

/// 桌面端 auth 目录的候选(Windows 优先)。
///
/// **兼容"数据目录不在 C 盘"的各种装法**(2026-10-20 用户实证:WorkBuddy 装在 D 盘):
///   ① `LOCALAPPDATA` / `APPDATA` / `USERPROFILE` —— 正常机器(用户目录被迁移时,
///      这些环境变量本身就已经指向别的盘,跟着走即可);
///   ② `WORKBUDDY_AUTH_DIR` 显式覆盖 —— 用户把数据目录挪走 / 便携版时唯一的可靠来源;
///   ③ 从桌面端 exe 的位置**反推**:不少"绿色/自定义目录"装法会把 userData 放在
///      `<exe 所在目录>\CodeBuddyExtension\...` 或 `<exe 盘>:\CodeBuddyExtension\...`。
///      这条排在系统路径之后,读不到也无害,但能救回一类本来永远找不到的机器。
pub fn auth_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(v) = std::env::var_os("WORKBUDDY_AUTH_DIR") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            // 允许用户直接给 auth 目录,或给它的上一层(带了 CodeBuddyExtension 就照用)
            out.push(p.clone());
            if !p.ends_with("auth") {
                out.push(p.join("auth"));
            }
        }
    }
    for var in ["LOCALAPPDATA", "APPDATA"] {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(v);
            if !p.as_os_str().is_empty() {
                out.push(p.join("CodeBuddyExtension").join("Data").join("Public").join("auth"));
            }
        }
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let h = PathBuf::from(home);
        out.push(h.join("AppData").join("Local").join("CodeBuddyExtension").join("Data").join("Public").join("auth"));
    }
    // ③ 从 exe 位置反推(只在真的找到 exe 时才有内容;找不到就是空,不产生候选)
    if let Some(exe) = find_app_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("CodeBuddyExtension").join("Data").join("Public").join("auth"));
            // 盘根那一层:<盘>:\CodeBuddyExtension\...
            if let Some(root) = dir.ancestors().last() {
                out.push(root.join("CodeBuddyExtension").join("Data").join("Public").join("auth"));
            }
        }
    }
    // 正常机器上 LOCALAPPDATA 就等于 USERPROFILE\AppData\Local,同一条路径会被列两遍。
    // 不去重的话,后面会把同一个凭据文件读两遍、解两遍。
    let mut seen: Vec<String> = Vec::new();
    out.retain(|p| {
        let k = p.to_string_lossy().to_lowercase();
        if seen.iter().any(|s| s == &k) {
            false
        } else {
            seen.push(k);
            true
        }
    });
    out
}

/// 扫描 auth 目录里所有 `workbuddy-desktop*.info`。
///
/// 排序规则(踩过一次才定):**live 文件(`workbuddy-desktop.info`)永远第一**,其余按
/// 修改时间新→旧。桌面端会在同一目录留下带时间戳的历史备份,那些备份里的 token 可能
/// 早已被上游作废(2026-10-19 真机:一份 6 月的备份 token 未过期但接口 401),
/// 光按 mtime 排会把用户钉在作废的旧备份上。
pub fn credential_files() -> Vec<PathBuf> {
    let mut live: Vec<PathBuf> = Vec::new();
    let mut others: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for dir in auth_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !name.starts_with("workbuddy-desktop") || !name.ends_with(".info") {
                continue;
            }
            // live:workbuddy-desktop.info / workbuddy-desktop-ai.info(不带时间戳)
            if name == "workbuddy-desktop.info" || name == "workbuddy-desktop-ai.info" {
                live.push(p);
                continue;
            }
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            others.push((mtime, p));
        }
    }
    others.sort_by(|a, b| b.0.cmp(&a.0));
    live.extend(others.into_iter().map(|(_, p)| p));
    live
}

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

/// 清缓存(测试与"换了账号"时用)。
pub fn clear_key_cache() {
    if let Ok(mut g) = KEY_CACHE.lock() {
        *g = None;
    }
}

/// 列出本机存在的盘符(`A`..`Z`)。
///
/// 为什么不用 `GetLogicalDrives`:core 刻意不依赖 Win32 绑定(见 `lib.rs` 顶部),
/// 而 26 次 `is_dir` 是微秒级的事,还天然跳过光驱/未挂载盘。**只有盘根存在才算**,
/// 所以网络盘断线时不会卡在这里(那一次 `is_dir` 会失败并返回 false)。
pub fn drive_letters() -> Vec<char> {
    ('A'..='Z')
        .filter(|c| Path::new(&format!("{c}:\\")).is_dir())
        .collect()
}

/// 桌面端可能落地的目录。**每个盘符都看** —— 2026-10-20 用户的 WorkBuddy 装在 D 盘,
/// 而原来的实现只认 `%PROGRAMFILES%`(C 盘),于是整条凭据链断在第一步,
/// 界面还只能说"没装桌面端"(明明装着)。
fn app_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    // ① 注册表(最准):装到哪都写着,包括自定义目录与其它盘
    out.extend(registry_app_roots());
    // ② 环境变量所指的默认位置(用户目录被迁移到别的盘时会跟着走)
    for var in ["LOCALAPPDATA", "APPDATA", "PROGRAMFILES", "PROGRAMFILES(X86)"] {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(v);
            if p.as_os_str().is_empty() {
                continue;
            }
            out.push(p.join("Programs").join("WorkBuddy"));
            out.push(p.join("WorkBuddy"));
        }
    }
    // ③ 每个盘符下的常见装法。用户口径「装在 D 盘」通常就是这几种之一:
    //    D:\WorkBuddy、D:\Programs\WorkBuddy、D:\Program Files\WorkBuddy、D:\Apps\WorkBuddy
    for drive in drive_letters() {
        for sub in [
            "Programs\\WorkBuddy",
            "WorkBuddy",
            "Program Files\\WorkBuddy",
            "Program Files (x86)\\WorkBuddy",
            "Apps\\WorkBuddy",
            "Applications\\WorkBuddy",
            "Software\\WorkBuddy",
        ] {
            out.push(PathBuf::from(format!("{drive}:\\{sub}")));
        }
    }
    // 去重(注册表与环境变量经常指向同一个目录),保持顺序
    let mut seen: Vec<String> = Vec::new();
    out.retain(|p| {
        let k = p.to_string_lossy().to_lowercase();
        if seen.iter().any(|s| s == &k) {
            false
        } else {
            seen.push(k);
            true
        }
    });
    out
}

/// 从注册表找桌面端装在哪。这是**唯一不靠猜**的来源:官方安装器会把位置写进
/// App Paths 与卸载项,自定义安装目录 / 非 C 盘都会如实记在那儿。
///
/// 三处都看(顺序即优先级):
///   ① `HKCU\...\App Paths\WorkBuddy.exe` —— per-user 安装,值就是 exe 全路径;
///   ② `HKLM\SOFTWARE\...\App Paths\WorkBuddy.exe` —— 机器级安装;
///   ③ `HKLM/HKCU\...\Uninstall\*` 的 `InstallLocation` / `DisplayIcon` —— 兜底。
#[cfg(windows)]
fn registry_app_roots() -> Vec<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    let mut out: Vec<PathBuf> = Vec::new();
    let names = ["WorkBuddy.exe", "WorkBuddyAI.exe", "CodeBuddy.exe"];
    let app_paths = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths";

    let push_exe = |p: PathBuf, out: &mut Vec<PathBuf>| {
        if p.is_file() {
            if let Some(dir) = p.parent() {
                out.push(dir.to_path_buf());
            }
        }
    };
    let push_dir = |p: PathBuf, out: &mut Vec<PathBuf>| {
        if p.is_dir() {
            out.push(p);
        }
    };

    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        // ① / ② App Paths
        for sub in [app_paths.to_string(), format!(r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\App Paths")] {
            let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(&sub, KEY_READ) else {
                continue;
            };
            for name in names {
                let Ok(exe) = key.open_subkey_with_flags(name, KEY_READ) else {
                    continue;
                };
                if let Ok(path) = exe.get_value::<String, _>("") {
                    push_exe(PathBuf::from(path.trim().trim_matches('"')), &mut out);
                }
            }
        }
        // ③ 卸载项(找 InstallLocation / DisplayIcon)
        for sub in [
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        ] {
            let Ok(un) = RegKey::predef(hive).open_subkey_with_flags(sub, KEY_READ) else {
                continue;
            };
            for item in un.enum_keys().flatten() {
                let Ok(k) = un.open_subkey_with_flags(&item, KEY_READ) else {
                    continue;
                };
                let name = k
                    .get_value::<String, _>("DisplayName")
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !(name.contains("workbuddy") || name.contains("codebuddy")) {
                    continue;
                }
                if let Ok(loc) = k.get_value::<String, _>("InstallLocation") {
                    push_dir(PathBuf::from(loc.trim().trim_matches('"')), &mut out);
                }
                // DisplayIcon 形如 `D:\Apps\WorkBuddy\WorkBuddy.exe,0`
                if let Ok(icon) = k.get_value::<String, _>("DisplayIcon") {
                    if let Some(p) = parse_display_icon(&icon) {
                        push_exe(p, &mut out);
                    }
                }
            }
        }
    }
    out
}

/// 从卸载项的 `DisplayIcon` 里取出 exe 路径。
/// 真机形态:`C:\Program Files\WorkBuddy\WorkBuddy.exe,0`(逗号后是图标序号);
/// 也有只给 `.ico` 的(那种不是主程序,返回 None)。**纯函数**,便于钉住格式。
fn parse_display_icon(raw: &str) -> Option<PathBuf> {
    // 先按逗号切图标序号。注意路径里本身可能带逗号,所以只在"末尾像序号"时才切。
    let s = raw.trim().trim_matches('"').trim();
    let s = match s.rsplit_once(',') {
        Some((head, tail)) if tail.trim().parse::<i32>().is_ok() => head.trim().trim_matches('"'),
        _ => s,
    };
    // 只认 .exe:`.ico` / `.png` 是图标文件,不是桌面端本体
    let p = PathBuf::from(s);
    let is_exe = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("exe"))
        .unwrap_or(false);
    (is_exe && !s.is_empty()).then_some(p)
}

/// 非 Windows 没有注册表这一说。
#[cfg(not(windows))]
fn registry_app_roots() -> Vec<PathBuf> {
    Vec::new()
}

/// 找桌面端可执行文件。探测链与结果会**原样写进诊断日志** —— 用户报"取不到数"时,
/// 第一件事就是看这条链走到哪儿断了。
///
/// 顺序:
///   ① `WORKBUDDY_APP_EXECUTABLE`(显式指定,最高优先;用户自己改过目录时的逃生门)
///   ② 注册表(App Paths / 卸载项,任意盘任意目录)
///   ③ 环境变量所指的默认目录
///   ④ 每个盘符 × 常见目录名
pub fn find_app_exe() -> Option<PathBuf> {
    // ① 显式覆盖
    if let Some(v) = std::env::var_os("WORKBUDDY_APP_EXECUTABLE") {
        let p = PathBuf::from(v);
        // 允许给目录:给了目录就在里面找主程序名
        let hit = if p.is_file() {
            Some(p.clone())
        } else if p.is_dir() {
            EXE_NAMES.iter().map(|n| p.join(n)).find(|c| c.is_file())
        } else {
            None
        };
        if let Some(hit) = hit {
            crate::diaglog::write_keyed(
                "wb",
                "exe-env",
                &format!("桌面端 exe 来自 WORKBUDDY_APP_EXECUTABLE:{}", hit.display()),
            );
            return Some(hit);
        }
        crate::diaglog::write_keyed(
            "wb",
            "exe-env",
            &format!(
                "WORKBUDDY_APP_EXECUTABLE 已设置但不可用({})—— 继续按默认链探测",
                p.display()
            ),
        );
    }

    let roots = app_roots();
    for r in &roots {
        for name in EXE_NAMES {
            let p = r.join(name);
            if p.is_file() {
                crate::diaglog::write_keyed(
                    "wb",
                    "exe-hit",
                    &format!(
                        "找到桌面端:{} · 命中目录 {}",
                        p.display(),
                        r.display()
                    ),
                );
                return Some(p);
            }
        }
    }
    // 一个都没找到:**把整条链记下来**,否则只能远程让用户猜
    let shown: Vec<String> = roots.iter().take(40).map(|p| p.display().to_string()).collect();
    crate::diaglog::write_keyed(
        "wb",
        "exe-miss",
        &format!(
            "没找到桌面端 exe(主程序名 {:?} · 盘符 {:?} · 探了 {} 个目录){}",
            EXE_NAMES,
            drive_letters(),
            roots.len(),
            if shown.is_empty() {
                String::new()
            } else {
                format!(":{}", shown.join(" | "))
            }
        ),
    );
    None
}

/// 问桌面端要 at-rest key 的原始 payload(那段 JSON 字符串)。
///
/// **必须带超时**:桌面端是 Electron,个别环境下会卡在启动阶段 —— 挂件是常驻轮询程序,
/// 不能因为一个子进程把整条取数链挂住(与 `tasklist` 那条 5 秒超时同一个理由)。
fn fetch_key_payload(exe: &Path) -> Result<String, DesktopErr> {
    use std::io::Read;
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
    let mut child = cmd
        .spawn()
        .map_err(|e| DesktopErr::KeyUnavailable(format!("起不来: {e}")))?;
    // 输出要**趁子进程还活着的时候并发读走**。以前的写法是"轮询等它退出、退出后再读管道":
    // 一旦 loggerGet() 输出超过管道缓冲,子进程就永远卡在写上,我们则永远卡在等它退出,
    // 15 秒到点后误报成"桌面端没有应答"(本次改造没踩到,是因为当时那段 JSON 恰好没满)。
    let so = child.stdout.take();
    let se = child.stderr.take();
    let (out, err, timeout_reason) = std::thread::scope(|s| {
        let h_out = s.spawn(move || {
            let mut buf = String::new();
            if let Some(mut r) = so {
                let _ = r.read_to_string(&mut buf);
            }
            buf
        });
        let h_err = s.spawn(move || {
            let mut buf = String::new();
            if let Some(mut r) = se {
                let _ = r.read_to_string(&mut buf);
            }
            buf
        });
        // std 没有 wait_timeout,自己转;超时就杀掉,不留孤儿进程
        let deadline = Instant::now() + KEY_TIMEOUT;
        let mut why: Option<String> = None;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        why = Some(format!("桌面端 {} 秒内没有应答", KEY_TIMEOUT.as_secs()));
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(80));
                }
                Err(e) => {
                    let _ = child.kill();
                    why = Some(format!("等待桌面端失败: {e}"));
                    break;
                }
            }
        }
        // 子进程已退出或被杀 → 写端关闭,两个读线程读到 EOF 自然收尾
        (
            h_out.join().unwrap_or_default(),
            h_err.join().unwrap_or_default(),
            why,
        )
    });
    if let Some(w) = timeout_reason {
        return Err(DesktopErr::KeyUnavailable(w));
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
fn collect_desktop_credentials(now_ms: i64, include_expired: bool) -> Result<Vec<DesktopCredential>, DesktopErr> {
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

/// 读**全部**桌面端候选(顺序:刚续来的 → 上次续期成果 → 桌面端 live → 历史备份)。
///
/// 落库与冷却这些带副作用的都在这一层,判据本身在 [`plan_tokens`](可单测)。
pub fn desktop_tokens(now_ms: i64) -> Vec<String> {
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
    let mut stored: Vec<StoredRefresh> = Vec::new();
    for global in [false, true] {
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
            "凭据编排:桌面端候选 {} 份 · 上次续期成果 {} 份 · 续期成功 {} 份 · 续期失败 {} 条 · 最终可用令牌 {} 个[{}]",
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
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("起不了运行时: {e}"))?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(refresh(&http, cred))
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

/// 一次取数尝试的结果(便于上层把"哪条路失败了"如实报出来)。
#[derive(Debug, Clone)]
pub struct Attempt {
    pub base: String,
    pub status: u16,
    pub body: String,
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
mod tests {
    use super::*;
    use base64::Engine as _;

    /// keyId 口径:sha256 前 8 字节的十六进制 —— 真机信封里的 `9127dea1b44020a7` 就是这么来的。
    #[test]
    fn key_id_is_sha256_prefix() {
        let key = Sha256::digest(b"whatever");
        let id = key_id_of(&key);
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// AAD 转录必须逐字节稳定:改动一位,GCM 就会认证失败。
    #[test]
    fn aad_transcript_is_stable() {
        let aad = field_aad("9127dea1b44020a7", 1);
        // "WB-AAD\0" 是 7 字节(W B - A A D \0)—— 这里写错过一次,所以逐段断言
        assert_eq!(AAD_DOMAIN.len(), 7);
        assert!(aad.starts_with(b"WB-AAD\0"));
        assert_eq!(aad[7], 1, "版本字节紧跟域前缀");
        // 格式 id / 方案都是长度前缀
        assert_eq!(&aad[8..12], &5u32.to_be_bytes(), "WBEV1 长度");
        assert_eq!(&aad[12..17], b"WBEV1");
        assert_eq!(&aad[17..21], &6u32.to_be_bytes(), "sym-v1 长度");
        assert_eq!(&aad[21..27], b"sym-v1");
        assert_eq!(&aad[27..31], &1u32.to_be_bytes(), "suite");
        assert_eq!(&aad[31..35], &16u32.to_be_bytes(), "keyId 长度");
        assert_eq!(&aad[35..51], b"9127dea1b44020a7");
        assert_eq!(aad[51], 2, "framing code");
        assert_eq!(aad.len(), 54);
    }

    /// 明文凭据不需要 key;加密凭据没有 key 要明确报"加密但没密钥"。
    #[test]
    fn plaintext_and_encrypted_fields() {
        let plain = r#"{"auth":{"accessToken":"tok","domain":"www.codebuddy.cn","expiresAt":1794709711836},
                        "account":{"uin":330100933717}}"#;
        let c = parse_auth_file(plain, "x", None).unwrap();
        assert_eq!(c.access_token, "tok");
        assert_eq!(c.uin, "330100933717");
        assert!(!c.is_global());
        assert!(!c.expired(1_700_000_000_000));

        let enc = r#"{"auth":{"accessToken":{"$wbEncrypted":1,"envelope":"e30="}}}"#;
        let e = parse_auth_file(enc, "x", None).unwrap_err();
        assert!(matches!(e, DesktopErr::BadCredential(_)), "没有 key 时要说清:{e:?}");
    }

    /// keyId 对不上时必须报 KeyMismatch(而不是"解出来一堆乱码")。
    #[test]
    fn key_mismatch_is_explicit() {
        let env = serde_json::json!({
            "suite": 1, "keyId": "0000000000000000",
            "nonce": "AAAAAAAAAAAAAAAA", "authTag": "AAAAAAAAAAAAAAAAAAAAAA==",
            "ciphertext": "AAAA"
        });
        let wrapper = serde_json::json!({
            "$wbEncrypted": 1,
            "envelope": base64::engine::general_purpose::STANDARD.encode(env.to_string())
        });
        let key = Sha256::digest(b"some other key");
        let e = open_field(&wrapper, &key).unwrap_err();
        match e {
            DesktopErr::KeyMismatch { want, got } => {
                assert_eq!(want, "0000000000000000");
                assert_eq!(got, key_id_of(&key));
            }
            other => panic!("该报 KeyMismatch,实得 {other:?}"),
        }
    }

    /// 每档错误都要有一句能指导用户的文案(不能只说"失败")。
    #[test]
    fn every_error_has_actionable_text() {
        for e in [
            DesktopErr::AppNotFound,
            DesktopErr::KeyUnavailable("boom".into()),
            DesktopErr::KeyMismatch { want: "a".into(), got: "b".into() },
            DesktopErr::BadCredential("bad".into()),
            DesktopErr::Expired,
        ] {
            let m = e.message();
            assert!(m.len() > 10, "文案太短:{m}");
            assert!(
                m.contains("桌面端") || m.contains("重新登录") || m.contains("WORKBUDDY_APP_EXECUTABLE"),
                "文案要指对方向:{m}"
            );
        }
    }

    /// 请求体字段与上游逐字一致(尤其 ProductCode 与那 36.9 天的窗口)。
    #[test]
    fn billing_body_matches_upstream() {
        let now = chrono::Local::now();
        let b = billing_body(now);
        assert_eq!(b["PageNumber"], 1);
        assert_eq!(b["PageSize"], 100);
        assert_eq!(b["ProductCode"], "p_tcaca");
        assert_eq!(b["Status"], serde_json::json!([0, 3]));
        assert!(b["PackageEndTimeRangeBegin"].as_str().unwrap().contains('-'));
        assert!(b["PackageEndTimeRangeEnd"].as_str().unwrap().contains('-'));
    }

    /// 端点必须带 `/v2`:少一段就是 401/404(2026-10-19 实测)。
    #[test]
    fn endpoint_carries_v2() {
        assert!(BILLING_PATH.starts_with("/v2/"));
        assert!(CN_BILLING_BASE.contains("codebuddy.cn"));
    }

    /// 鉴权头:桌面端走 Bearer,网页会话走 Cookie,不能串。
    #[test]
    fn auth_headers_do_not_mix() {
        assert_eq!(FetchAuth::Bearer("t".into()).header_value(), "Bearer t");
        assert_eq!(FetchAuth::Cookie("session=a".into()).header_value(), "Cookie: session=a");
    }

    /// 续期契约:端点、头部字段名、以及"没有 refreshToken 就不发请求"。
    #[test]
    fn refresh_contract_matches_upstream() {
        assert_eq!(REFRESH_PATH, "/v2/plugin/auth/token/refresh");
        let cn = DesktopCredential {
            domain: "www.codebuddy.cn".into(),
            ..Default::default()
        };
        assert_eq!(chat_base(&cn), "https://copilot.tencent.com", "国内 chat 基址与计费不同域");
        let gl = DesktopCredential {
            domain: "www.workbuddy.ai".into(),
            ..Default::default()
        };
        assert_eq!(chat_base(&gl), "https://www.workbuddy.ai");
        // 没有 refreshToken → 明确拒绝,不发请求
        let http = reqwest::Client::new();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let e = rt.block_on(refresh(&http, &cn)).unwrap_err();
        assert!(e.contains("没有 refreshToken"), "{e}");
    }

    /// 续期结果的存取(凭据管理器按区域分开,互不覆盖)。
    #[test]
    fn refresh_slots_are_per_region() {
        assert_ne!(refresh_slot(true), refresh_slot(false));
        assert!(refresh_slot(false).contains("cn"));
        assert!(refresh_slot(true).contains("global"));
    }

    /// 区域基址:国内打 codebuddy.cn、国际打 workbuddy.ai —— 用错域名一定 401(真机踩过)。
    #[test]
    fn bases_follow_credential_region() {
        let cn = DesktopCredential { domain: "www.codebuddy.cn".into(), ..Default::default() };
        assert_eq!(bases_for(&cn)[0], "https://www.codebuddy.cn");
        let gl = DesktopCredential { domain: "www.workbuddy.ai".into(), ..Default::default() };
        assert_eq!(bases_for(&gl)[0], "https://www.workbuddy.ai");
        let cb = DesktopCredential { domain: "www.codebuddy.ai".into(), ..Default::default() };
        assert_eq!(bases_for(&cb)[0], "https://www.workbuddy.ai", "codebuddy.ai 属于国际栈");
    }

    /// live 文件名判定:带时间戳的备份不算 live。
    #[test]
    fn live_names_are_the_untimestamped_ones() {
        assert!(is_live_name(Path::new("/a/workbuddy-desktop.info")));
        assert!(is_live_name(Path::new("/a/workbuddy-desktop-ai.info")));
        assert!(!is_live_name(Path::new(
            "/a/workbuddy-desktop.2026-06-09T09-04-37-611Z.info"
        )));
    }

    /// 候选排序:国内版排在前面(挂件只展示国内版),同区域 live 先于备份。
    /// 用两把伪造的 key 验顺序,不依赖本机文件。
    #[test]
    fn domestic_and_live_come_first() {
        let mk = |src: &str, domain: &str| DesktopCredential {
            source: src.to_string(),
            domain: domain.to_string(),
            ..Default::default()
        };
        let mut v = vec![
            mk("/a/workbuddy-desktop-ai.info", "www.workbuddy.ai"),
            mk("/a/workbuddy-desktop.2026-06-09.info", "www.codebuddy.cn"),
            mk("/a/workbuddy-desktop.info", "www.codebuddy.cn"),
        ];
        v.sort_by_key(|c| {
            let live = is_live_name(Path::new(&c.source));
            (c.is_global(), !live)
        });
        assert_eq!(v[0].source, "/a/workbuddy-desktop.info", "国内 live 第一");
        assert_eq!(v[1].source, "/a/workbuddy-desktop.2026-06-09.info", "国内备份第二");
        assert_eq!(v[2].source, "/a/workbuddy-desktop-ai.info", "国际版最后");
    }

    // ───────── 第三轮体检(2026-10-19)抓到的续期断链:以下四条把它钉住 ─────────

    /// 端到端:临时目录里放一份"accessToken 已过期但仍带可用 refreshToken"的 live 凭据。
    /// 旧姿态(include_expired=false)根本看不见它 → 续期分支恒不可达;新姿态才续得上。
    #[test]
    fn expired_live_credential_reaches_the_refresh_step() {
        let now = 1_800_000_000_000i64;
        let dir = std::env::temp_dir().join(format!("ts-wb-expired-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        // 明文形态:单测不该去起桌面端进程拿 key
        let live = dir.join("workbuddy-desktop.info");
        std::fs::write(
            &live,
            format!(
                r#"{{"auth":{{"accessToken":"old-tok","refreshToken":"rt","domain":"www.codebuddy.cn","expiresAt":{},"refreshExpiresAt":{}}},"account":{{"uin":123}}}}"#,
                now - 1,
                now + 86_400_000
            ),
        )
        .expect("写伪造凭据");
        let files = vec![live.clone()];

        // ① 修复前的过滤:过期那份被丢掉 → 上层拿不到任何候选 → 不会去续
        let old = collect_from(&files, now, false).expect_err("过期的不该进候选");
        assert!(matches!(old, DesktopErr::Expired));

        // ② 修复后:同一份文件带进续期这一步
        let got = collect_from(&files, now, true).expect("include_expired 该收下过期那份");
        assert_eq!(got.len(), 1);
        assert!(got[0].expired(now));

        let tried = std::cell::Cell::new(0usize);
        let plan = plan_tokens(now, &[], &got, &|_: &DesktopCredential| -> Result<Refreshed, String> {
            tried.set(tried.get() + 1);
            Ok(Refreshed {
                access_token: "fresh-tok".into(),
                refresh_token: Some("rt2".into()),
                expires_in_sec: Some(3600),
                domain: None,
            })
        });
        assert_eq!(tried.get(), 1, "该发起一次续期");
        assert_eq!(plan.tokens[0], "fresh-tok", "续来的排最前");
        assert_eq!(plan.to_store.len(), 1, "续期结果该落库");
        assert_eq!(plan.to_store[0].1.refresh_token.as_deref(), Some("rt2"));
        assert!(plan.refresh_fails.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 不该续的三种情形:没过期、refreshToken 也超期、refreshToken 是空白。
    #[test]
    fn refresh_is_attempted_only_when_it_can_work() {
        let now = 1_800_000_000_000i64;
        let tried = std::cell::Cell::new(0usize);
        let fake = &|_: &DesktopCredential| -> Result<Refreshed, String> {
            tried.set(tried.get() + 1);
            Ok(Refreshed {
                access_token: "x".into(),
                ..Default::default()
            })
        };
        let mk = |rt: Option<&str>, exp: i64, rtx: i64| DesktopCredential {
            access_token: "old".into(),
            refresh_token: rt.map(String::from),
            expires_at_ms: exp,
            refresh_expires_at_ms: rtx,
            ..Default::default()
        };
        // 没过期 → 直接用,不起请求
        let p = plan_tokens(now, &[], &[mk(Some("rt"), now + 60_000, now + 86_400_000)], fake);
        assert_eq!(tried.get(), 0, "没过期就不该续");
        assert_eq!(p.tokens, vec!["old".to_string()]);
        // accessToken 与 refreshToken 都超期 → 只能人工登一次
        plan_tokens(now, &[], &[mk(Some("rt"), now - 1, now - 1)], fake);
        assert_eq!(tried.get(), 0, "refreshToken 也过期就不该白打");
        // 没有 refreshToken / 只有空白
        plan_tokens(now, &[], &[mk(None, now - 1, now + 1)], fake);
        plan_tokens(now, &[], &[mk(Some("   "), now - 1, now + 1)], fake);
        assert_eq!(tried.get(), 0, "空白 refreshToken 不算可用的续期凭据");
    }

    /// 上次续期的成果:必须按**过期时刻**决定用不用。
    /// 修复前落库只存令牌不存时刻 → 那份令牌作废后仍永远顶在候选第一位,每轮白吃一个 401。
    #[test]
    fn stored_refresh_is_dropped_once_expired() {
        let now = 1_800_000_000_000i64;
        let st = |exp: i64| StoredRefresh {
            global: false,
            access: format!("tok-{exp}"),
            refresh: None,
            expires_at_ms: exp,
        };
        let none = &|_: &DesktopCredential| -> Result<Refreshed, String> { Err("不该被调".into()) };
        let p = plan_tokens(now, &[st(0), st(now + 1000)], &[], none);
        assert!(p.tokens.contains(&format!("tok-{}", now + 1000)));
        assert!(p.tokens.contains(&"tok-0".to_string()), "老数据没记过期时刻,不该把人挡死");
        let p2 = plan_tokens(now, &[st(now - 1)], &[], none);
        assert!(p2.tokens.is_empty(), "已过期的上次续期成果不该再当首选");
    }

    /// 续期失败要如实带出来(界面/日志能说清"为什么还是没数"),但不能把其它候选挤掉。
    #[test]
    fn refresh_failure_is_reported_and_other_candidates_still_used() {
        let now = 1_800_000_000_000i64;
        let expired = DesktopCredential {
            access_token: "dead".into(),
            refresh_token: Some("rt".into()),
            expires_at_ms: now - 1,
            refresh_expires_at_ms: now + 1,
            source: "/a/workbuddy-desktop.info".into(),
            ..Default::default()
        };
        let live_backup = DesktopCredential {
            access_token: "still-ok".into(),
            expires_at_ms: now + 60_000,
            source: "/a/workbuddy-desktop.2026-06-09.info".into(),
            ..Default::default()
        };
        let plan = plan_tokens(
            now,
            &[],
            &[expired, live_backup],
            &|_: &DesktopCredential| -> Result<Refreshed, String> { Err("9074 排队".into()) },
        );
        assert_eq!(plan.tokens, vec!["still-ok".to_string()], "续期失败不该挤掉可用候选");
        assert_eq!(plan.refresh_fails.len(), 1);
        assert!(plan.refresh_fails[0].contains("9074"), "{:?}", plan.refresh_fails);
        assert!(plan.to_store.is_empty());
    }

    /// 端到端(真机):桌面端凭据 → 按区域选域名 → /v2 取数 → `extract_workbuddy` 算余额。
    /// 这是本次改动的最终判据:挂件真的能算出 WorkBuddy 余额。默认 ignore。
    #[test]
    #[ignore]
    fn wb_real_machine_end_to_end_balance() {
        let now_ms = chrono::Local::now().timestamp_millis();
        let creds = read_desktop_credentials(now_ms).expect("桌面端凭据");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        let mut ok: Option<(String, f64)> = None;
        for c in &creds {
            let body = billing_body(chrono::Local::now());
            let r = rt.block_on(fetch_billing(
                &http,
                &FetchAuth::Bearer(c.access_token.clone()),
                &body,
                bases_for(c),
            ));
            match r {
                Ok((url, text)) => {
                    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                    let parsed = crate::providers::extract_workbuddy(&v);
                    eprintln!(
                        "{} → {url} · valid={} remaining={:?} 包={} err={:?}",
                        c.source,
                        parsed.valid,
                        parsed.remaining,
                        parsed.expiring.len(),
                        parsed.error
                    );
                    if parsed.valid {
                        if let Some(rem) = parsed.remaining {
                            ok = Some((c.source.clone(), rem));
                            break;
                        }
                    }
                }
                Err(e) => eprintln!("{} → 取数失败:{e}", c.source),
            }
        }
        let (src, rem) = ok.expect("至少要有一份凭据能算出余额");
        eprintln!("WorkBuddy 余额 = {rem}(来自 {src})");
        assert!(rem >= 0.0);
    }

    /// 真机:走完 桌面端凭据 → 解密 → /v2 取数,断言能拿到积分。
    /// 默认 ignore(要本机装桌面端并登录):`cargo test --lib -- --ignored wb_real_machine`。
    #[test]
    #[ignore]
    fn wb_real_machine_reads_credits() {
        let now_ms = chrono::Local::now().timestamp_millis();
        let cred = match read_desktop_credential(now_ms) {
            Ok(c) => c,
            Err(e) => panic!("桌面端凭据读不出来:{}", e.message()),
        };
        eprintln!("凭据来源 = {}", cred.source);
        eprintln!("domain   = {}", cred.domain);
        eprintln!("uin      = {}", cred.uin);
        eprintln!(
            "accessToken 到 {}",
            chrono::DateTime::from_timestamp_millis(cred.expires_at_ms)
                .map(|d| d.with_timezone(&chrono::Local).to_rfc3339())
                .unwrap_or_else(|| "?".into()),
        );
        assert!(!cred.access_token.is_empty());

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        let body = billing_body(chrono::Local::now());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (url, text) = rt
            .block_on(fetch_billing(
                &http,
                &FetchAuth::Bearer(cred.access_token.clone()),
                &body,
                bases_for(&cred),
            ))
            .expect("取数应当成功");
        eprintln!("端点 = {url}");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v["code"],
            0,
            "业务 code 应为 0:{}",
            &text[..200.min(text.len())]
        );
        let accts = v["data"]["Response"]["Data"]["Accounts"]
            .as_array()
            .expect("要有 Accounts");
        eprintln!("Accounts 条数 = {}", accts.len());
        let total: f64 = accts
            .iter()
            .map(|a| {
                a.get("CycleCapacityRemain")
                    .or_else(|| a.get("CapacityRemain"))
                    .and_then(|x| x.as_f64())
                    .unwrap_or(0.0)
            })
            .sum();
        eprintln!("合计剩余 = {total}");
        // 解析层也要能认(与真实接口对齐)
        let r = crate::providers::extract_workbuddy(&v);
        eprintln!(
            "解析结果 = remaining {:?} / 到期条目 {} 个",
            r.remaining,
            r.expiring.len()
        );
        assert!(r.valid, "解析应当有效:{:?}", r.error);
        assert!(r.remaining.is_some(), "应当能算出余额");
    }

    // ───────── 装到别的盘(2026-10-20 用户实证:WorkBuddy 装在 D 盘)─────────

    /// 盘符枚举只列真实存在的盘,且一定是大写字母。
    #[test]
    fn drive_letters_are_real_drives() {
        let ds = drive_letters();
        assert!(!ds.is_empty(), "本机至少有一个盘");
        for d in &ds {
            assert!(d.is_ascii_uppercase(), "盘符要大写:{d}");
            assert!(Path::new(&format!("{d}:\\")).is_dir(), "列出来的盘符必须存在");
        }
        assert!(ds.contains(&'C'), "本机一定有 C 盘:{ds:?}");
    }

    /// **这条是用例的核心**:装到别的盘 / 自定义目录时,exe 也必须被找到。
    /// 用临时目录伪造一个 `D:\Programs\WorkBuddy` 式的布局,只验证"候选目录的构造"
    /// 与"命中逻辑",不依赖真机有没有装。
    #[test]
    fn exe_search_covers_non_default_install_dirs() {
        let base = std::env::temp_dir().join(format!("ts-wb-exe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // 伪造一个"用户自己挑的目录"下的安装
        let install = base.join("Programs").join("WorkBuddy");
        std::fs::create_dir_all(&install).unwrap();
        let exe = install.join("WorkBuddy.exe");
        std::fs::write(&exe, b"fake").unwrap();

        // 把用户盘符那一步替换成这个临时根,验证"逐盘 × 常见目录"确实会扫到它
        let roots = app_roots_for(&[base.clone()]);
        let hit = roots
            .iter()
            .flat_map(|r| EXE_NAMES.iter().map(move |n| r.join(n)))
            .find(|p| p.is_file());
        // 用存在性 + 文件名判,不比较绝对路径字符串:临时目录的大小写/短名在
        // Windows 上可能与候选拼出来的写法不一致,那不是被测语义。
        let hit = hit.unwrap_or_else(|| panic!("非默认目录里的 WorkBuddy.exe 必须被找到;候选根:{roots:?}"));
        assert_eq!(
            hit.file_name().unwrap().to_string_lossy(),
            "WorkBuddy.exe",
            "命中的是主程序名"
        );
        assert_eq!(
            hit.parent().map(|p| p.to_path_buf()),
            Some(install.clone()),
            "必须命中那个自定义安装目录:{hit:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 候选目录里必须**同时**含"环境变量默认位置"与"任意盘常见位置"两大类,
    /// 否则"装在 D 盘"这类机器还是会落空。
    #[test]
    fn app_roots_include_every_drive_and_standard_programs() {
        let roots = app_roots();
        let text: Vec<String> = roots.iter().map(|p| p.display().to_string().to_lowercase()).collect();
        // 每个存在的盘都要有候选(哪怕那个盘上其实没装,也要去看一眼)
        for d in drive_letters() {
            let prefix = format!("{}:\\", d.to_ascii_lowercase());
            assert!(
                text.iter().any(|p| p.starts_with(&prefix)),
                "{d} 盘没有任何候选:{}",
                text.join(" | ")
            );
        }
        // 常见的几种"用户自定义目录"命名都要覆盖
        for want in ["\\programs\\workbuddy", "\\workbuddy", "\\program files\\workbuddy", "\\apps\\workbuddy"] {
            assert!(
                text.iter().any(|p| p.ends_with(want)),
                "缺少候选后缀 {want}:{}",
                text.join(" | ")
            );
        }
        // 去重:同一个路径不许出现两遍(否则日志里看着像探了两遍)
        let mut uniq = text.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), text.len(), "候选目录里有重复");
    }

    /// exe 名字要认国内版与国际版两个(它们同栈但文件名不同)。
    #[test]
    fn exe_names_cover_both_editions() {
        assert!(EXE_NAMES.contains(&"WorkBuddy.exe"), "{EXE_NAMES:?}");
        assert!(EXE_NAMES.contains(&"WorkBuddyAI.exe"), "{EXE_NAMES:?}");
    }

    /// 注册表 `DisplayIcon` 的解析:真机形态是 `C:\Program Files\WorkBuddy\WorkBuddy.exe,0`
    /// (逗号后是图标序号)。**这条是"装到任意盘也能定位"的最后一道保险** ——
    /// 本机实测 WorkBuddy 5.8.3 的卸载项里 `InstallLocation` 是空的,
    /// 只剩 `DisplayIcon` 能给出路径,所以它必须解析得对。
    #[test]
    fn display_icon_parses_to_the_exe() {
        // 真机样例(本机 WorkBuddy 5.8.3)
        assert_eq!(
            parse_display_icon(r"C:\Program Files\WorkBuddy\WorkBuddy.exe,0"),
            Some(PathBuf::from(r"C:\Program Files\WorkBuddy\WorkBuddy.exe"))
        );
        // 装到别的盘 + 带引号
        assert_eq!(
            parse_display_icon(r#""D:\Apps\WorkBuddy\WorkBuddy.exe",0"#),
            Some(PathBuf::from(r"D:\Apps\WorkBuddy\WorkBuddy.exe"))
        );
        // 没有图标序号
        assert_eq!(
            parse_display_icon(r"E:\WorkBuddy\WorkBuddy.exe"),
            Some(PathBuf::from(r"E:\WorkBuddy\WorkBuddy.exe"))
        );
        // 图标文件不是主程序
        assert_eq!(parse_display_icon(r"C:\x\icon.ico"), None);
        assert_eq!(parse_display_icon(r"C:\x\icon.ico,0"), None);
        assert_eq!(parse_display_icon(""), None);
        // 路径本身带逗号时不能切错(尾部不是数字就当没有序号)
        assert_eq!(
            parse_display_icon(r"C:\a,b\WorkBuddy.exe,0"),
            Some(PathBuf::from(r"C:\a,b\WorkBuddy.exe"))
        );
    }

    /// `WORKBUDDY_AUTH_DIR` 是数据目录被挪走时的逃生门:给了就**必须**排在第一个,
    /// 而且要同时接受"给 auth 目录"和"给它的上一层"两种写法。
    #[test]
    fn auth_dir_env_override_comes_first() {
        // 环境变量是进程级的,用完就还原,别污染别的用例
        let prev = std::env::var_os("WORKBUDDY_AUTH_DIR");
        let probe = std::env::temp_dir().join(format!("ts-wb-auth-env-{}", std::process::id()));
        std::env::set_var("WORKBUDDY_AUTH_DIR", &probe);
        let dirs = auth_dirs();
        assert_eq!(dirs.first().map(|p| p.as_path()), Some(probe.as_path()), "{dirs:?}");
        // 给的是"上一层"时,还要自动补一个 auth 子目录(免得用户写错一层就失效)
        assert!(
            dirs.iter().any(|p| p.ends_with("auth") && p.starts_with(&probe)),
            "应当自动补 auth 子目录:{dirs:?}"
        );
        match prev {
            Some(v) => std::env::set_var("WORKBUDDY_AUTH_DIR", v),
            None => std::env::remove_var("WORKBUDDY_AUTH_DIR"),
        }
    }
}

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
