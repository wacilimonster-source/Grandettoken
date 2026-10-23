//! Qoder 桌面端凭据的只读复用(国际版 / 国内版)。
//!
//! ## 凭据长什么样(2026-10-20 真机取证)
//!
//! 落点:`%APPDATA%\com.qoder.app.stable\auth.v1.dat`(国际版)/
//!       `%APPDATA%\com.qodercn.app.stable\auth.v1.dat`(国内版)。
//!
//! 它是 **Electron safeStorage** 的标准产物,不是 WorkBuddy 那种"静态钥信封":
//!
//! ```text
//! auth.v1.dat = b"v10" + nonce(12) + AES-256-GCM 密文 + authTag(16)
//! 密钥        = DPAPI 解 Local State 的 os_crypt.encrypted_key(去掉 "DPAPI" 前缀)
//! 明文        = {"schemaVersion":1,"token":…,"refreshToken":…,
//!                "expiresAt":…,"refreshTokenExpiresAt":…,"user":{…}}
//! ```
//!
//! **与 WorkBuddy 的关键区别**:WorkBuddy 的信封用编译期内置的静态钥(本机拿不到),
//! 所以那条路断了;Qoder 用的是 **Windows DPAPI**,由当前登录账户保护 ——
//! 同一个用户在自己的机器上**本来就能解**,这正是 Electron safeStorage 的设计意图。
//! 所以我们不需要"破解"任何东西:调一次 `CryptUnprotectData` 就行。
//!
//! ## 为什么不刷新
//!
//! 与 `appauth` 同口径:refreshToken 一旦使用,上游通常会轮换(旧的作废),
//! 我们刷完客户端手里那份就成了废票,用户下次打开 IDE 会被登出。
//! token 过期就如实报"打开一次 Qoder 即可",由客户端自己续期 —— 我们只搭便车。
//!
//! ## 红线
//!
//! token 只在本进程内存里过一趟:**不落盘、不进日志、不进数据库**。
//! 日志里只留 [`crate::diaglog::secret_brief`] 的摘要。

use serde::Deserialize;
use std::path::PathBuf;

/// Qoder 国际版活动 API 用的本机设备身份。
///
/// 值只在进程内短期缓存,不序列化、不写日志。`Debug` 故意不派生,
/// 避免调用方无意把设备令牌打印出来。
#[derive(Clone)]
pub struct QoderMachineIdentity {
    pub machine_token: String,
    pub machine_type: String,
    pub machine_code: String,
    pub client_version: Option<String>,
    pub machine_os: Option<String>,
    pub machine_hostname: Option<String>,
    pub machine_id: Option<String>,
}

/// 从 Qoder 自带的 UMID helper 读取国际版请求需要的设备头。
///
/// Qoder 升级目录会保留多个版本;优先用当前安装根目录,再从版本目录里找最新 helper。
/// 失败时返回 `None`,调用方仍可带账号令牌查询活动并展示发现结果,但不伪造设备身份。
pub fn machine_identity(account_id: &str) -> Option<QoderMachineIdentity> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Option<QoderMachineIdentity>)>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = account_id.trim().to_string();
    if key.is_empty() || key.len() > 512 {
        return None;
    }
    {
        let entries = cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((loaded_at, identity)) = entries.get(&key) {
            let ttl = if identity.is_some() {
                Duration::from_secs(45 * 60)
            } else {
                Duration::from_secs(5 * 60)
            };
            if loaded_at.elapsed() < ttl {
                return identity.clone();
            }
        }
    }
    let identity = machine_identity_uncached(&key);
    cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key, (Instant::now(), identity.clone()));
    identity
}

#[cfg(windows)]
fn machine_identity_uncached(account_id: &str) -> Option<QoderMachineIdentity> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[derive(Deserialize)]
    struct RuntimeInfo {
        #[serde(rename = "machineToken")]
        machine_token: serde_json::Value,
        #[serde(rename = "machineType")]
        machine_type: serde_json::Value,
        #[serde(rename = "machineCode")]
        machine_code: serde_json::Value,
    }

    fn value_string(v: &serde_json::Value) -> Option<String> {
        v.as_str()
            .map(str::to_string)
            .or_else(|| v.as_i64().map(|n| n.to_string()))
            .filter(|s| !s.trim().is_empty())
    }

    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    let install = local.join("Programs").join("Qoder");
    let client_version = std::fs::read_dir(install.join(".qoder-versions"))
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if !p.is_dir() {
                return None;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let parts: Option<Vec<u64>> = name.split('.').map(|p| p.parse().ok()).collect();
            parts.map(|parts| (parts, name))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, version)| version);
    let machine_os = Some(format!(
        "{}_win32",
        match std::env::consts::ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            arch => arch,
        }
    ));
    let machine_hostname = std::env::var("COMPUTERNAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 96
                && s.is_ascii()
                && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
        });
    let machine_id = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .and_then(|p| {
            std::fs::read_to_string(p.join(Region::Global.data_dir()).join("auth.machine-id")).ok()
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s.len() <= 128 && s.is_ascii());
    let mut candidates = vec![install.join("resources").join("umid").join("runtime-info.exe")];
    if let Ok(entries) = std::fs::read_dir(install.join(".qoder-versions")) {
        let mut versions: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        versions.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        candidates.extend(
            versions
                .into_iter()
                .map(|p| p.join("resources").join("umid").join("runtime-info.exe")),
        );
    }

    for exe in candidates.into_iter().filter(|p| p.is_file()) {
        // Qoder 的 UMID helper(`runtime-info.exe`)是控制台程序。挂件是 GUI 进程,
        // 不指定 CREATE_NO_WINDOW 时 Windows 会为它配一个新控制台 —— 真机表现为
        // 隔一阵就闪一个黑框(缓存过期后每次探测都会再起一次)。与 tasklist / clip /
        // WorkBuddy 桌面端同一个处理。
        #[cfg(windows)]
        let mut cmd = {
            use std::os::windows::process::CommandExt;
            let mut c = Command::new(&exe);
            c.creation_flags(0x0800_0000);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = Command::new(&exe);
        let child_result = cmd
            .args(["prod", "--account-stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child_result else { continue };
        let Some(mut input) = child.stdin.take() else {
            let _ = child.kill();
            continue;
        };
        // Qoder 本身把活动账号 UID 传给 helper;stdin 以换行结尾与客户端契约一致。
        let mut account = serde_json::to_vec(&serde_json::json!({ "account": account_id })).ok()?;
        account.push(b'\n');
        if input.write_all(&account).is_err() {
            let _ = child.kill();
            continue;
        }
        drop(input);
        // 与 Qoder 自己一致,给 native helper 留 25 秒上限,不能让活动线程无限挂起。
        let started = std::time::Instant::now();
        let exited = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() < std::time::Duration::from_secs(25) => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let Some(status) = exited else { continue };
        let output = match child.wait_with_output() {
            Ok(out) if status.success() && out.status.success() => out,
            _ => continue,
        };
        let info: RuntimeInfo = match serde_json::from_slice(&output.stdout) {
            Ok(info) => info,
            Err(_) => continue,
        };
        let (Some(machine_token), Some(machine_type), Some(machine_code)) = (
            value_string(&info.machine_token),
            value_string(&info.machine_type),
            value_string(&info.machine_code),
        ) else {
            continue;
        };
        return Some(QoderMachineIdentity {
            machine_token,
            machine_type,
            machine_code,
            client_version,
            machine_os,
            machine_hostname,
            machine_id,
        });
    }
    None
}

/// 非 Windows 平台没有可读取的 Qoder 桌面 UMID helper。
#[cfg(not(windows))]
fn machine_identity_uncached(_account_id: &str) -> Option<QoderMachineIdentity> {
    None
}

/// 需要复用桌面端登录态的 Qoder 版本。
///
/// `Default` = 国际版:它是两个 region 里更常见的一个,也让 `QoderCredential::default()`
/// 有一个确定的值(而不是让 `Default` 派生失败)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Region {
    /// 国际版:`%APPDATA%\com.qoder.app.stable`
    #[default]
    Global,
    /// 国内版:`%APPDATA%\com.qodercn.app.stable`
    Cn,
}

impl Region {
    pub fn label(self) -> &'static str {
        match self {
            Region::Global => "Qoder",
            Region::Cn => "Qoder 国内版",
        }
    }
    /// Electron app 的数据目录名(`product.json` 的 `dataDirectoryName`)。
    ///
    /// 国际版实测 `com.qoder.app.stable`;国内版按 Electron 惯例是 `com.qodercn.app.stable`
    /// —— **国内版本机没有安装,这一条未经验证**(见设计稿 §1 的证据等级表)。
    pub fn data_dir(self) -> &'static str {
        match self {
            Region::Global => "com.qoder.app.stable",
            Region::Cn => "com.qodercn.app.stable",
        }
    }
}

/// 走不通的原因。**逐档分开**:用户能做的动作完全不同。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QoderErr {
    /// 没找到数据目录(没装桌面端 / 装的是别的版本)
    AppNotFound,
    /// 目录在,但没有 auth.v1.dat(没登录过)
    NotLoggedIn,
    /// 文件结构不认识(v10 头缺失 / 长度不对 / 明文不是 JSON)
    BadCredential(String),
    /// DPAPI 解密钥失败(通常是**换了 Windows 账户或换了机器** —— 密文是绑账户的)
    KeyUnavailable(String),
    /// 信封认证失败(密钥对不上:换过账户 / 文件被改过)
    DecryptFailed,
    /// token 已过期(且我们不替它续)
    Expired,
}

impl QoderErr {
    /// 给用户看的一句话。**必须说对方向**:能自己修的别让人去重装。
    pub fn message(&self) -> String {
        match self {
            QoderErr::AppNotFound => {
                "没找到 Qoder 桌面端 —— 装一次并登录,挂件就能直接读它的凭据".to_string()
            }
            QoderErr::NotLoggedIn => "Qoder 桌面端还没登录 —— 打开它登录一次即可".to_string(),
            QoderErr::BadCredential(e) => {
                format!("Qoder 凭据文件读不出来({e})—— 打开一次桌面端,或重新登录")
            }
            QoderErr::KeyUnavailable(e) => format!(
                "解不开 Qoder 凭据的密钥({e})—— 该密文绑定 Windows 账户,\
                 换过账户/换过机器就会这样;在本机重新登录一次 Qoder 即可"
            ),
            QoderErr::DecryptFailed => {
                "Qoder 凭据解不开(密钥对不上)—— 通常是换过账户或重装过,重新登录一次即可"
                    .to_string()
            }
            QoderErr::Expired => {
                "Qoder 凭据已过期 —— 打开一次 Qoder 桌面端(它会自动续期),再回来刷新".to_string()
            }
        }
    }
}

/// 解出来的一份凭据。
#[derive(Debug, Clone, Default)]
pub struct QoderCredential {
    pub token: String,
    /// Qoder 账号 UID,只用于把真实账号标识交给 Qoder 自带的 UMID helper。
    pub account_id: String,
    /// 上游给的过期时刻(RFC3339 字符串,如 `2026-11-10T06:19:29Z`)。
    /// 保留原串 —— 它带时区后缀,自己 parse 成时间戳只会引入时区 bug。
    pub expires_at: String,
    /// 账号名(界面显示"读的是哪个账号")。
    pub user_name: String,
    /// 这份凭据是从哪个 region 读出来的。
    pub region: Region,
}

/// `auth.v1.dat` 的明文结构(只声明我们关心的字段,其余忽略)。
#[derive(Debug, Deserialize)]
struct AuthFile {
    #[serde(default)]
    token: String,
    #[serde(default, rename = "expiresAt")]
    expires_at: String,
    #[serde(default)]
    user: Option<AuthUser>,
}

#[derive(Debug, Deserialize)]
struct AuthUser {
    #[serde(default)]
    id: serde_json::Value,
    #[serde(default)]
    uid: serde_json::Value,
    #[serde(default, rename = "userId")]
    user_id: serde_json::Value,
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
}

/// Electron safeStorage 在 Windows 上的密文头(ASCII `v10`)。
const V10: &[u8] = b"v10";
/// GCM nonce 长度。
const NONCE_LEN: usize = 12;
/// GCM auth tag 长度。
const TAG_LEN: usize = 16;
/// `os_crypt.encrypted_key` 解出来后的前缀(DPAPI 自己加的类型标记)。
const DPAPI_PREFIX: &[u8] = b"DPAPI";

/// 数据目录(按 region)。允许用环境变量覆盖 —— 便携版 / 数据目录被挪走的机器。
fn data_dir(region: Region) -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("QODER_DATA_DIR") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    let roaming = std::env::var_os("APPDATA").map(PathBuf::from)?;
    Some(roaming.join(region.data_dir()))
}

fn auth_path(region: Region) -> Option<PathBuf> {
    Some(data_dir(region)?.join("auth.v1.dat"))
}

/// 从 `Local State` 的 JSON 里取出 `os_crypt.encrypted_key`(base64)。
///
/// 手写解析而不是 `serde_json`:这个文件在应用运行时**可能被并发写**
/// (Chromium 会不断更新它),读到半截 JSON 时我们只想拿这一个字段,
/// 不想因为整体解析失败就整条路走不通。字符串扫描对这种"只要一个键"的场景更稳。
pub fn encrypted_key_b64(local_state: &str) -> Option<String> {
    let i = local_state.find("\"encrypted_key\"")?;
    let rest = &local_state[i..];
    let c = rest.find(':')?;
    let rest = &rest[c + 1..];
    let q1 = rest.find('"')?;
    let rest = &rest[q1 + 1..];
    let q2 = rest.find('"')?;
    let key = &rest[..q2];
    let key = key.trim();
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
}

/// 解 `os_crypt.encrypted_key`:base64 → 去 `DPAPI` 前缀 → DPAPI 解 → 32 字节 AES 密钥。
#[cfg(windows)]
pub fn unwrap_os_crypt_key(encrypted_key_b64: &str) -> Result<Vec<u8>, QoderErr> {
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encrypted_key_b64.trim())
        .map_err(|e| QoderErr::KeyUnavailable(format!("encrypted_key 不是 base64: {e}")))?;
    let blob = match raw.strip_prefix(DPAPI_PREFIX) {
        Some(rest) => rest,
        None => &raw[..], // 有的版本不带前缀,照解
    };
    let key = dpapi_unprotect(blob)?;
    if key.len() != 32 {
        return Err(QoderErr::KeyUnavailable(format!(
            "解出来的密钥是 {} 字节,期望 32",
            key.len()
        )));
    }
    Ok(key)
}

/// 非 Windows 上不存在 DPAPI —— 整条路走不通,如实报(不假装支持)。
#[cfg(not(windows))]
pub fn unwrap_os_crypt_key(_b64: &str) -> Result<Vec<u8>, QoderErr> {
    Err(QoderErr::KeyUnavailable("DPAPI 只在 Windows 上存在".into()))
}

/// 调一次 `CryptUnprotectData`。**这是全项目唯一直接调 Win32 的地方**,
/// 所以单独圈出来:失败一律归成 `KeyUnavailable`(绝大多数成因是"换了账户")。
#[cfg(windows)]
fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>, QoderErr> {
    // LocalFree 在 Foundation(不是 System::Memory)—— 0.61 起挪了位置。
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    if data.is_empty() {
        return Err(QoderErr::KeyUnavailable("密文是空的".into()));
    }
    let mut input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY:两个 blob 都是本栈上的有效结构;input 指向的切片在调用期间存活;
    // 成功时 output.pbData 由系统分配,下面立刻拷走并用 LocalFree 归还。
    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            std::ptr::null_mut(), // 不关心描述串(它会额外分配,还得再释放)
            std::ptr::null(),     // 无额外熵
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut output,
        )
    };
    if ok == 0 {
        // 不把 GetLastError 的原文拼进来:它对用户没有意义,而且这里最可能是
        // "密文是别的账户/机器加密的",文案里已经说清方向了。
        return Err(QoderErr::KeyUnavailable("DPAPI 拒绝解密(密文不属于当前账户)".into()));
    }
    // SAFETY:成功时 pbData 指向 cbData 字节的有效内存(系统分配)。
    let out = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    // SAFETY:归还系统分配的内存;失败只是漏一点内存,不影响正确性。
    unsafe {
        LocalFree(output.pbData as *mut core::ffi::c_void);
    }
    Ok(out)
}

/// 解一个 `v10` 信封(AES-256-GCM,无 AAD —— 与 WorkBuddy 的带 AAD 信封不同)。
///
/// **与 WorkBuddy 的差别就在这里**:WorkBuddy 的信封要按 `fieldAad()` 转录一段 AAD;
/// Electron safeStorage 不带 AAD,所以这里 `set_aad` 一次都不调。
pub fn decrypt_v10(key: &[u8], blob: &[u8]) -> Result<Vec<u8>, QoderErr> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};

    if blob.len() < V10.len() + NONCE_LEN + TAG_LEN {
        return Err(QoderErr::BadCredential(format!(
            "密文太短({} 字节)",
            blob.len()
        )));
    }
    if &blob[..V10.len()] != V10 {
        return Err(QoderErr::BadCredential(format!(
            "不是 v10 信封(头是 {:?})",
            String::from_utf8_lossy(&blob[..V10.len().min(blob.len())])
        )));
    }
    let nonce_bytes = &blob[V10.len()..V10.len() + NONCE_LEN];
    let ct = &blob[V10.len() + NONCE_LEN..blob.len() - TAG_LEN];
    let tag = &blob[blob.len() - TAG_LEN..];

    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| QoderErr::KeyUnavailable("密钥长度不是 32 字节".into()))?;
    // aes-gcm 的 Aead 接口把 tag 拼在密文尾部
    let mut buf = ct.to_vec();
    buf.extend_from_slice(tag);
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), Payload { msg: &buf, aad: &[] })
        .map_err(|_| QoderErr::DecryptFailed)
}

/// 从 `auth.v1.dat` 的明文里取 token(纯函数,可单测)。
pub fn parse_auth_file(plain: &str, region: Region) -> Result<QoderCredential, QoderErr> {
    let f: AuthFile = serde_json::from_str(plain)
        .map_err(|e| QoderErr::BadCredential(format!("明文不是预期的 JSON: {e}")))?;
    let token = f.token.trim().to_string();
    if token.is_empty() {
        return Err(QoderErr::NotLoggedIn);
    }
    let (user_name, account_id) = match &f.user {
        Some(u) => {
            let name = if !u.name.trim().is_empty() {
                u.name.trim().to_string()
            } else {
                u.email.trim().to_string()
            };
            let account_id = qoder_id_string(&u.id)
                .or_else(|| qoder_id_string(&u.uid))
                .or_else(|| qoder_id_string(&u.user_id))
                .unwrap_or_default();
            (name, account_id)
        }
        None => (String::new(), String::new()),
    };
    Ok(QoderCredential {
        token,
        account_id,
        expires_at: f.expires_at.trim().to_string(),
        user_name,
        region,
    })
}

fn qoder_id_string(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|n| n.to_string()))
        .filter(|id| !id.trim().is_empty())
}

/// 读一份凭据(含 DPAPI + AES-GCM 全链路)。
///
/// **只读不写**:不碰 Qoder 自己的文件,也不缓存到别处。
pub fn read(region: Region) -> Result<QoderCredential, QoderErr> {
    let path = auth_path(region).ok_or(QoderErr::AppNotFound)?;
    if !path.parent().map(|p| p.exists()).unwrap_or(false) {
        return Err(QoderErr::AppNotFound);
    }
    let blob = std::fs::read(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            QoderErr::NotLoggedIn
        } else {
            QoderErr::BadCredential(format!("读文件失败: {e}"))
        }
    })?;
    let key = at_rest_key(region)?;
    let plain = decrypt_v10(&key, &blob)?;
    let text = String::from_utf8(plain)
        .map_err(|e| QoderErr::BadCredential(format!("明文不是 UTF-8: {e}")))?;
    parse_auth_file(&text, region)
}

/// 取 AES 密钥(读 `Local State` → DPAPI 解)。
fn at_rest_key(region: Region) -> Result<Vec<u8>, QoderErr> {
    let dir = data_dir(region).ok_or(QoderErr::AppNotFound)?;
    let ls = dir.join("Local State");
    let text = std::fs::read_to_string(&ls).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            QoderErr::AppNotFound
        } else {
            QoderErr::KeyUnavailable(format!("读 Local State 失败: {e}"))
        }
    })?;
    let b64 = encrypted_key_b64(&text)
        .ok_or_else(|| QoderErr::KeyUnavailable("Local State 里没有 os_crypt.encrypted_key".into()))?;
    unwrap_os_crypt_key(&b64)
}

/// 这个 region 在不在(数据目录存在就算在)。界面据此决定要不要显示这个渠道的提示。
pub fn present(region: Region) -> bool {
    data_dir(region).map(|d| d.exists()).unwrap_or(false)
}

/// 凭据是否已过期。
///
/// `expires_at` 是上游给的 **RFC3339 字符串**(带 `Z`/偏移),按它 parse;
/// parse 不了就**当作没过期**(老数据/格式变了不该让渠道直接瞎掉,401 的文案会兜底)。
pub fn expired(cred: &QoderCredential, now_ts: i64) -> bool {
    match chrono::DateTime::parse_from_rfc3339(&cred.expires_at) {
        Ok(dt) => now_ts >= dt.timestamp(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `encrypted_key` 的扫描要能容忍"文件里有别的键、且我们只要这一个"。
    #[test]
    fn encrypted_key_is_scanned_out_of_local_state() {
        let ls = r#"{"os_crypt":{"audit_enabled":true,"encrypted_key":"RFBBUEkBAAAA0Iyd3wEV0RGMegDAT8KX6wEAAAC"}}"#;
        assert_eq!(
            encrypted_key_b64(ls).as_deref(),
            Some("RFBBUEkBAAAA0Iyd3wEV0RGMegDAT8KX6wEAAAC")
        );
        // 键在但值是空的 → None(不能拿空串去 base64 解)
        assert!(encrypted_key_b64(r#"{"encrypted_key":""}"#).is_none());
        // 没有这个键 / 不是 JSON(半截文件) → None,不 panic
        assert!(encrypted_key_b64(r#"{"other":1}"#).is_none());
        assert!(encrypted_key_b64("{\"encrypted_key\"").is_none());
        // 带空格的值要去空白
        assert_eq!(encrypted_key_b64(r#"{"encrypted_key":"  ABC  "}"#).as_deref(), Some("ABC"));
    }

    /// v10 信封:头不对 / 太短都要明确报错,不能 panic 也不能猜。
    #[test]
    fn v10_rejects_bad_shapes() {
        let key = [7u8; 32];
        assert!(matches!(decrypt_v10(&key, b"v11xxxxxxxxxxxxxxxxxxxxxx"), Err(QoderErr::BadCredential(_))));
        assert!(matches!(decrypt_v10(&key, b"v10short"), Err(QoderErr::BadCredential(_))));
        assert!(matches!(decrypt_v10(&key, &[]), Err(QoderErr::BadCredential(_))));
        // 长度够、头也对,但密钥不对 → DecryptFailed(与"文件坏了"分开)
        let mut blob = V10.to_vec();
        blob.extend_from_slice(&[0u8; NONCE_LEN]);
        blob.extend_from_slice(&[0u8; 8]);
        blob.extend_from_slice(&[0u8; TAG_LEN]);
        assert!(matches!(decrypt_v10(&[9u8; 32], &blob), Err(QoderErr::DecryptFailed)));
    }

    /// 密钥长度不是 32 → 明确报错(而不是拿它去解、然后报认证失败)。
    #[test]
    fn v10_requires_32_byte_key() {
        let mut blob = V10.to_vec();
        blob.extend_from_slice(&[0u8; NONCE_LEN]);
        blob.extend_from_slice(&[0u8; 8]);
        blob.extend_from_slice(&[0u8; TAG_LEN]);
        assert!(matches!(decrypt_v10(&[0u8; 16], &blob), Err(QoderErr::KeyUnavailable(_))));
    }

    /// 明文解析:token 必填;user 缺失/字段空都不能 panic。
    #[test]
    fn auth_file_parses_and_requires_token() {
        let ok = r#"{"schemaVersion":1,"token":"t-123","refreshToken":"r",
                     "expiresAt":"2026-11-10T06:19:29Z",
                     "user":{"id":"u1","name":"Alice","email":"a@b.c"}}"#;
        let c = parse_auth_file(ok, Region::Global).unwrap();
        assert_eq!(c.token, "t-123");
        assert_eq!(c.expires_at, "2026-11-10T06:19:29Z");
        assert_eq!(c.user_name, "Alice");

        // 没有 user → 仍然可用(只是没有账号名可显示)
        let no_user = r#"{"token":"t","expiresAt":"2026-11-10T06:19:29Z"}"#;
        let c = parse_auth_file(no_user, Region::Cn).unwrap();
        assert_eq!(c.user_name, "");
        assert_eq!(c.region, Region::Cn);

        // 空 token → 未登录(不是"结构坏了")
        assert!(matches!(parse_auth_file(r#"{"token":"  "}"#, Region::Global), Err(QoderErr::NotLoggedIn)));
        assert!(matches!(parse_auth_file(r#"{"token":"  "}"#, Region::Global), Err(QoderErr::NotLoggedIn)));
        // 不是 JSON → 结构坏了
        assert!(matches!(parse_auth_file("<<<", Region::Global), Err(QoderErr::BadCredential(_))));
    }

    /// 过期判定:能 parse 就按它比;**parse 不了当没过期**(别让格式变化把渠道判死)。
    #[test]
    fn expiry_uses_rfc3339_and_defaults_to_alive() {
        let mut c = QoderCredential {
            expires_at: "2026-11-10T06:19:29Z".into(),
            ..Default::default()
        };
        let before = chrono::DateTime::parse_from_rfc3339("2026-11-10T06:19:29Z")
            .unwrap()
            .timestamp();
        assert!(!expired(&c, before - 1));
        assert!(expired(&c, before));
        assert!(expired(&c, before + 1));
        // 空串 / 乱码 → 不判过期
        c.expires_at = String::new();
        assert!(!expired(&c, before + 999_999));
        c.expires_at = "not a date".into();
        assert!(!expired(&c, before + 999_999));
    }

    /// 两个 region 的数据目录名不能写反(写反就会去读另一个版本的文件)。
    #[test]
    fn region_data_dirs_are_distinct() {
        assert_eq!(Region::Global.data_dir(), "com.qoder.app.stable");
        assert_eq!(Region::Cn.data_dir(), "com.qodercn.app.stable");
        assert_ne!(Region::Global.data_dir(), Region::Cn.data_dir());
    }

    /// **真机全链路**:读 Local State → DPAPI 解密钥 → v10 解密 → 解析 token →
    /// 真打一次额度接口与签到接口。
    ///
    /// 默认 ignore(要本机装 Qoder 桌面端并登录):
    /// `cargo test --lib -- --ignored qoder_real_machine`。
    ///
    /// 这条用例是整条链的**唯一端到端证据**:任何一环写错(DPAPI 标志位、
    /// GCM 的 AAD 该不该带、字段路径)都只会在真机上暴露。
    #[test]
    #[ignore]
    fn qoder_real_machine_reads_credits() {
        let now = chrono::Utc::now().timestamp();
        let cred = match read(Region::Global) {
            Ok(c) => c,
            Err(e) => panic!("Qoder 凭据读不出来:{}", e.message()),
        };
        eprintln!("region     = {:?}", cred.region);
        eprintln!("user       = {}", cred.user_name);
        eprintln!("expiresAt  = {}", cred.expires_at);
        eprintln!("expired    = {}", expired(&cred, now));
        assert!(!cred.token.is_empty(), "token 不该为空");
        // 真机上不该是过期状态(过期了说明桌面端太久没开,那是环境问题不是代码问题)
        assert!(!expired(&cred, now), "本机 Qoder 凭据已过期,先打开一次桌面端");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap();

        // ① 额度接口
        let (out, body) = rt.block_on(crate::checkin::call_with_body(
            &http,
            crate::checkin::find("qoder").expect("qoder 签到定义在表里"),
            &crate::checkin::Cred::Qoder(cred.token.clone()),
            false,
            None,
        ));
        // 这一条同时验证:凭据可用、请求头对、响应结构可解析
        assert!(
            !matches!(out, crate::checkin::Outcome::AuthNeeded),
            "凭据被平台拒绝(401):{out:?}"
        );
        if let Some(b) = &body {
            let streak = crate::checkin::qoder_streak(b);
            eprintln!("streak_days = {streak:?}");
            eprintln!("claimable   = {:?}", b.get("claimable"));
            // 把活动列表原样打出来:actionType / claimStatus 是判定的全部依据,
            // 上游哪天换名字,这条用例会立刻显形(而不是静默判成"没活动")
            let kinds: Vec<String> = b
                .get("campaigns")
                .and_then(|c| c.as_array())
                .map(|a| {
                    a.iter()
                        .map(|c| {
                            format!(
                                "{}:{}",
                                c.get("actionType").and_then(|x| x.as_str()).unwrap_or("-"),
                                c.get("claimStatus").and_then(|x| x.as_str()).unwrap_or("-")
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            eprintln!("campaigns   = {kinds:?}");
            let raw = b.to_string();
            eprintln!("raw         = {}", &raw[..raw.len().min(1400)]);
            let pick = crate::checkin::qoder_pick(b);
            eprintln!(
                "可领活动 = {:?} · 已领 = {} · 有活动 = {}",
                pick.claim_id, pick.already, pick.has_campaign
            );
        }
        eprintln!("签到状态判定 = {out:?}");
    }
}
