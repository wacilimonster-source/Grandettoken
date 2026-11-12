//! MiniMax Code 渠道:本机凭据、官方客户端签名、积分与逐笔到期、每日签到。
//!
//! 凭据来自本机 MiniMax Code 客户端的**明文** OAuth 文件(与 Trae 的 storage.json 同一
//! 信任级别:同一 Windows 账户本来就能读),不需要解密:
//!
//! ```text
//! ~/.minimax/auth/prod/<cn|io|com|en>/mcode-public/auth.json
//!   records: { "<recordKey>": { accessToken, refreshToken, expiresAtMs, generation, … } }
//! ```
//!
//! 三件事必须一起说清楚(两个独立参考实现交叉验证:CreditDaddy 的 minimaxClient.js
//! 与 dsh-connect-minimaxcode 的 credits.ts,另有青龙脚本 minimax_checkin.py):
//!
//! 1. **令牌两种,别拿错**:账务端点只认 `auth.json` 里的 `mmoat_` OAuth 令牌;
//!   `local-runtime.auth.json` 里那个 JWT 是模型网关专用,打账务端点恒定 401。
//! 2. **refresh token 一次性轮换**:刷新成功旧链当场作废。纪律是**只在临近过期时刷新**
//!   (dsh-connect 口径:余量 ≤ 2 分钟),平时绝不动链 —— 不与客户端抢同一条 grant;
//!   刷新后**原子回写**同一份文件(保留 `generation`/`loginEpoch`,客户端靠它判断
//!   凭据归属)。设 `MINIMAX_NOWRITEBACK=1` 进入只读模式:不刷新也不回写 —— 只读模式
//!   必须连刷新一起停,否则刷新消费了轮换又不回写,客户端的下次续期必死。
//! 3. **账户端点要官方客户端签名**(取自官方 app.asar,CreditDaddy 与青龙脚本独立
//!   逆向结果逐字节一致):`x-signature = md5(秒时间戳 + 密钥 + body)`,
//!   `yy = md5(encodeURIComponent(path+query) + "_" + body或"{}" + md5(毫秒时间戳) + "ooui")`。
//!   dsh-connect(最新)不带签名也能用 —— 服务端大概率不校验;带签名是官方客户端自己的
//!   口径,签错会被拒,所以 Golden 向量测试钉死了算法(见 `sign_request` 的测试)。
//!
//! 签到端点要 **IANA 时区名**(`timezone_id=Asia/Shanghai`):dsh-connect 实证"给
//! offset 或不给会被拒 `invalid timezone_id`"。会员/账本端点按 CreditDaddy 口径带
//! `timezone_offset`。两套查询串各有 Golden 向量。
//!
//! 排查日志只记「长度 + 指纹」,令牌永不落日志(与其它渠道同一红线)。

use crate::checkin::Outcome;
use crate::providers::{AccountExpiry, AccountExpiryState, Expiring, FetchResult, Kind};
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::{json, Value};
use std::path::PathBuf;

/// 官方客户端的签名密钥(公开于其 app.asar,两个独立逆向项目交叉一致)。
pub const SIGN_SECRET: &str = "I*7Cf%WZ#S&%1RlZJ&C2";
pub const OAUTH_CLIENT_ID: &str = "mcode-public";
pub const OAUTH_SCOPE: &str = "agent.default";
pub const OAUTH_AUDIENCE: &str = "agent-backend";
/// 请求头里的 UA:官方客户端就是这个串(CreditDaddy 实证)。
pub const APP_UA: &str = "MiniMaxCode";

/// 令牌剩余寿命低于这个值才算"临近过期",才值得刷新(2 分钟,dsh-connect 口径)。
const REFRESH_MARGIN_MS: i64 = 2 * 60 * 1000;
/// 账本分页上限:免费账号每天一笔、30 天有效,同时存活的批次几十笔;5 页兜住。
const LEDGER_MAX_PAGES: usize = 5;
/// 逐笔到期列表上限:界面逐笔展示不需要无限长,截断即如实(`expiry_complete` 不因此说谎,
/// 列表截断只影响"更远的批次")。
const LEDGER_MAX_LOTS: usize = 60;

// ---------------------------------------------------------------- region

/// MiniMax 的三个部署区。账号不互通(与 Qoder 两版同理),凭据文件在哪个目录、
/// 请求就走哪套域名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Cn,
    Io,
    Com,
}

impl Region {
    pub fn as_str(self) -> &'static str {
        match self {
            Region::Cn => "cn",
            Region::Io => "io",
            Region::Com => "com",
        }
    }

    /// 账务 / 签到端点的网关。
    pub fn agent_host(self) -> &'static str {
        match self {
            Region::Cn => "agent.minimax.cn",
            Region::Io => "agent.minimax.io",
            Region::Com => "agent.minimaxi.com",
        }
    }

    /// OAuth 授权服务器(刷新令牌用)。
    pub fn account_host(self) -> &'static str {
        match self {
            Region::Cn => "account.minimax.cn",
            Region::Io => "account.minimax.io",
            Region::Com => "account.minimaxi.com",
        }
    }

    fn parse(s: &str) -> Option<Region> {
        match s {
            "cn" => Some(Region::Cn),
            "io" => Some(Region::Io),
            "com" => Some(Region::Com),
            _ => None,
        }
    }

    /// 客户端的语言口径(CreditDaddy:国内 zh、国际 en)。
    fn lang(self) -> &'static str {
        match self {
            Region::Cn => "zh",
            _ => "en",
        }
    }
}

/// 客户端凭据目录名 → 区域。`cn` 是国内构建;dsh-connect 实证国际目录是 `io`/`com`,
/// CreditDaddy 还提过 `en` —— 一并扫上,命中哪个算哪个。
const REGION_DIRS: &[(&str, Region)] = &[("cn", Region::Cn), ("io", Region::Io), ("en", Region::Io), ("com", Region::Com)];

// ---------------------------------------------------------------- 凭据

/// 从本机客户端凭据文件解出的一条登录态。
#[derive(Debug, Clone)]
pub struct LocalCred {
    pub access: String,
    pub refresh: Option<String>,
    /// 客户端记录的过期时刻(毫秒)。缺了就当新鲜 —— 没有证据不刷新。
    pub expires_at_ms: Option<i64>,
    pub region: Region,
    /// JWT payload 里的 user.id(签名查询串的 `user_id`;缺了按官方口径退化为 "0")。
    pub uid: Option<String>,
}

/// 凭据键的传输格式:fetch 与 checkin 两条链路共用的同一份字符串编码,
/// 区域随键走(域名是区域决定的,静态 ProviderDef 表达不了,放进键里最省事 ——
/// 与 WorkBuddy 桌面端令牌的 `desktop:` 前缀同一先例)。
pub fn encode_key(cred: &LocalCred) -> String {
    format!("mm:{}:{}", cred.region.as_str(), cred.access)
}

/// 解凭据键:`mm:<region>:<token>`。解析失败返回 None(调用方要如实报错,不能猜)。
pub fn parse_key(key: &str) -> Option<(Region, &str)> {
    let rest = key.trim().strip_prefix("mm:")?;
    let (region, token) = rest.split_once(':')?;
    let region = Region::parse(region)?;
    if token.trim().is_empty() {
        return None;
    }
    Some((region, token))
}

/// 数据目录候选。客户端本体只认 `~/.minimax`;三个环境变量是两个参考实现
/// 约定的覆盖口,顺手支持(也为测试与自定义安装留门)。
fn data_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for key in ["MINIMAX_DATA_DIR", "MAVIS_DATA_DIR", "MINIMAX_HOME"] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                out.push(PathBuf::from(v));
            }
        }
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    if let Some(h) = home {
        out.push(h.join(".minimax"));
    }
    out
}

/// 全部候选凭据文件路径(cn 优先,其后 io / en / com)。
pub fn auth_file_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in data_roots() {
        for (dir, _) in REGION_DIRS {
            out.push(root.join("auth").join("prod").join(dir).join("mcode-public").join("auth.json"));
        }
    }
    out
}

/// 解析客户端的凭据文件:records 里第一条带非空 accessToken 的记录。
/// 键名形如 `com.minimax.mcode.oauth.prod.cn\0<随机串>`(含 NUL),按**值**找记录,
/// 不按键名(键名含每次安装生成的随机串,写死了就是另一台机器的键)。
fn parse_auth_document(text: &str, region: Region) -> Option<LocalCred> {
    let doc: Value = serde_json::from_str(text).ok()?;
    let records = doc.get("records")?.as_object()?;
    for rec in records.values() {
        let access = rec.get("accessToken").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if access.is_empty() {
            continue;
        }
        let uid = jwt_uid(&access);
        let refresh = rec
            .get("refreshToken")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let expires_at_ms = rec.get("expiresAtMs").and_then(Value::as_i64);
        return Some(LocalCred {
            access,
            refresh,
            expires_at_ms,
            region,
            uid,
        });
    }
    None
}

fn read_credential() -> Option<LocalCred> {
    for path in auth_file_candidates() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let region = REGION_DIRS
            .iter()
            .find(|(dir, _)| path.to_string_lossy().contains(&format!("\\prod\\{dir}\\"))
                || path.to_string_lossy().contains(&format!("/prod/{dir}/")))
            .map(|(_, r)| *r)
            .unwrap_or(Region::Cn);
        if let Some(cred) = parse_auth_document(&text, region) {
            return Some(cred);
        }
    }
    None
}

/// JWT payload 里的 `user.id`。只解不验:签名本来就要发回签发方,防篡改不靠我们。
/// id 可能是字符串也可能是数字,统一成字符串。
fn jwt_uid(token: &str) -> Option<String> {
    let part = token.split('.').nth(1)?;
    let payload = URL_SAFE_NO_PAD
        .decode(part)
        .or_else(|_| URL_SAFE.decode(part))
        .ok()?;
    let v: Value = serde_json::from_slice(&payload).ok()?;
    let id = v.get("user")?.get("id")?;
    match id {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// 供 fetch 管线(`appauth::candidates`)用的候选列表。
/// **单测绝不碰本机文件与网络**(workbuddy 桌面端读取的同款纪律)。
pub fn app_credentials() -> Vec<crate::appauth::Credential> {
    if cfg!(test) {
        return Vec::new();
    }
    match blocking_credential() {
        Some(cred) => vec![crate::appauth::Credential {
            token: encode_key(&cred),
            source: "MiniMax Code 本机登录态".into(),
            account_id: cred.uid,
            device_id: None,
        }],
        None => Vec::new(),
    }
}

/// 读凭据,临近过期就刷新并原子回写。**阻塞**(可能发一次刷新请求),只能在
/// 非主线程调用 —— 取数与签到两条路的凭据解析本来就都跑在 spawn_blocking 上。
///
/// 只读模式(`MINIMAX_NOWRITEBACK=1`):不刷新也不回写。
pub fn blocking_credential() -> Option<LocalCred> {
    let cred = read_credential()?;
    if readonly_mode() {
        return Some(cred);
    }
    let now = now_ms();
    let stale = cred
        .expires_at_ms
        .map(|exp| exp - now <= REFRESH_MARGIN_MS)
        .unwrap_or(false);
    if !stale {
        return Some(cred);
    }
    match refresh_blocking(&cred) {
        Ok(fresh) => {
            write_back(&cred, &fresh);
            Some(LocalCred {
                access: fresh.access,
                refresh: fresh.refresh,
                expires_at_ms: Some(fresh.expires_at_ms),
                region: cred.region,
                uid: cred.uid,
            })
        }
        Err(e) => {
            // 旧的未必真死了(本机时钟偏移也会触发"临近过期"),先用旧令牌,401 由
            // 取数管线如实上报;刷新失败原因落日志。
            crate::diaglog::write_keyed("minimax", "refresh", &format!("刷新失败,沿用旧令牌: {e}"));
            Some(cred)
        }
    }
}

/// 托管模式的进程内开关:config 加载/保存时同步进来(`set_writeback_enabled`)。
/// 默认开(2026-11-13 用户裁决);`MINIMAX_NOWRITEBACK=1` 是硬开关,优先于设置。
static WRITEBACK_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_writeback_enabled(on: bool) {
    WRITEBACK_ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn readonly_mode() -> bool {
    if std::env::var("MINIMAX_NOWRITEBACK").map(|v| v.trim() == "1").unwrap_or(false) {
        return true;
    }
    !WRITEBACK_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---------------------------------------------------------------- 续期

struct Refreshed {
    access: String,
    refresh: Option<String>,
    expires_at_ms: i64,
}

async fn refresh_request(
    http: &reqwest::Client,
    region: Region,
    refresh_token: &str,
) -> Result<Refreshed, String> {
    let url = format!("https://{}/oauth2/token", region.account_host());
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", OAUTH_CLIENT_ID),
        ("scope", OAUTH_SCOPE),
        ("audience", OAUTH_AUDIENCE),
    ];
    let resp = http
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .timeout(std::time::Duration::from_secs(20))
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status();
    let raw = resp.bytes().await.map_err(|e| format!("读响应失败: {e}"))?;
    if !status.is_success() {
        // 400/401 = 那条 refresh 链已被作废(invalid_grant):回复登录的是客户端,不是我们
        let code = status.as_u16();
        let auth_hint = if matches!(code, 400 | 401) { " · 凭据链已失效,打开一次 MiniMax Code 重新登录" } else { "" };
        return Err(format!("HTTP {code}{auth_hint}"));
    }
    let v: Value = serde_json::from_slice(&raw).map_err(|_| "响应不是 JSON".to_string())?;
    let access = v
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("响应里没有 access_token")?
        .to_string();
    let refresh = v
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let expires_in = v.get("expires_in").and_then(Value::as_i64).unwrap_or(3600).max(60);
    Ok(Refreshed {
        access,
        refresh,
        expires_at_ms: now_ms() + expires_in * 1000,
    })
}

fn refresh_blocking(cred: &LocalCred) -> Result<Refreshed, String> {
    let Some(refresh_token) = cred.refresh.as_deref().filter(|s| !s.trim().is_empty()) else {
        return Err("凭据文件里没有 refreshToken".into());
    };
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(25))
        .build()
        .map_err(|e| e.to_string())?;
    let region = cred.region;
    let refresh_token = refresh_token.to_string();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(refresh_request(&http, region, &refresh_token))
}

/// 把轮换后的新令牌对**原子回写**进客户端自己的凭据文件:
/// 只动那一条 record 的 `accessToken` / `refreshToken` / `tokenType` / `expiresAtMs`,
/// `generation` / `loginEpoch` / `schemaVersion` 与其余所有内容原样保留
/// (generation 是客户端判断凭据归属的字段,动了它客户端会以为"在别处登录过")。
/// 临时文件 + 重命名,不存在写一半的中间态。返回是否实际写入。
fn write_back(cred: &LocalCred, fresh: &Refreshed) -> bool {
    // 回写目标 = 这条凭据实际读出来的那份文件(与读取同一条候选链,不猜路径)
    let Some(path) = auth_file_candidates().into_iter().find(|p| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|t| parse_auth_document(&t, cred.region))
            .map(|c| c.access == cred.access)
            .unwrap_or(false)
    }) else {
        crate::diaglog::write_keyed("minimax", "writeback", "找不到原凭据文件,回写跳过");
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Some(new_text) = patch_auth_document(&text, &fresh.access, fresh.refresh.as_deref(), fresh.expires_at_ms)
    else {
        crate::diaglog::write_keyed("minimax", "writeback", "文件结构认不出来,回写跳过");
        return false;
    };
    let tmp = path.with_extension("json.grt-tmp");
    if std::fs::write(&tmp, new_text).is_err() {
        return false;
    }
    match std::fs::rename(&tmp, &path) {
        Ok(_) => true,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            crate::diaglog::write_keyed("minimax", "writeback", &format!("原子替换失败: {e}"));
            false
        }
    }
}

/// 回写的纯逻辑部分(独立出来便于单测):整份文档重写,只补目标字段。
fn patch_auth_document(text: &str, access: &str, refresh: Option<&str>, expires_at_ms: i64) -> Option<String> {
    let mut doc: Value = serde_json::from_str(text).ok()?;
    let records = doc.get_mut("records")?.as_object_mut()?;
    let target = records
        .iter()
        .find(|(_, v)| {
            v.get("accessToken")
                .and_then(Value::as_str)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        })
        .map(|(k, _)| k.clone())?;
    let rec = records.get_mut(&target)?.as_object_mut()?;
    rec.insert("accessToken".into(), json!(access));
    if let Some(r) = refresh {
        rec.insert("refreshToken".into(), json!(r));
    }
    rec.insert("tokenType".into(), json!("Bearer"));
    rec.insert("expiresAtMs".into(), json!(expires_at_ms));
    serde_json::to_string_pretty(&doc).ok()
}

// ---------------------------------------------------------------- 签名

/// encodeURIComponent 的 Rust 克隆:JS 只放行 `A-Za-z0-9-_.!~*'()`,
/// 其余按 UTF-8 转成大写 %XX。签名值绑定这个编码,差一个字符都过不了。
fn enc_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || "-_.!~*'()".contains(c) {
            out.push(c);
        } else {
            // 按 UTF-8 字节展开成大写 %XX(JS encodeURIComponent 的行为)
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn md5_hex(s: &str) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(s.as_bytes());
    let d = h.finalize();
    let mut out = String::with_capacity(32);
    for b in d {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// 签名查询串里的固定设备参数(顺序敏感:官方客户端就是这个序,
/// Golden 向量按它生成)。`offset` 为 None 时**不发** `timezone_offset`
/// (签到端点服务端按 offset 拒收,见模块头注)。
fn device_query(now: i64, offset: Option<i32>, lang: &str, uid: &str) -> Vec<(&'static str, String)> {
    let mut p: Vec<(&'static str, String)> = vec![
        ("device_platform", "mcode".into()),
        ("biz_id", "3".into()),
        ("app_id", "3001".into()),
        ("version_code", "22201".into()),
        ("unix", now.to_string()),
    ];
    if let Some(o) = offset {
        p.push(("timezone_offset", o.to_string()));
    }
    p.extend([
        ("sys_language", lang.to_string()),
        ("lang", lang.to_string()),
        ("device_id", "0".to_string()),
        ("os_name", "win32".to_string()),
        ("browser_name", "mcode".to_string()),
        ("user_id", uid.to_string()),
        ("client", "mcode".to_string()),
    ]);
    p
}

/// 签名结果:最终 URL(path + 参与签名的查询串)与三个签名头。
/// Authorization 不在这里 —— 调用方按自己的凭据链路加。
pub struct SignedRequest {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
}

/// 签名参数。`offset_sec`:`Some` = 发 `timezone_offset`(账务端点,测试钉值 /
/// 运行时取本地偏移);`None` = 不发(签到端点)。
pub struct SignParams<'a> {
    pub url: &'a str,
    /// POST 的确切发送体;GET 传 None(签名与 yy 都按无 body 口径)。
    pub body: Option<&'a str>,
    pub uid: Option<&'a str>,
    pub offset_sec: Option<i32>,
    pub lang: &'a str,
    /// 追加在固定参数之后的查询参数(签到时区名、账本分页)。
    pub extra_query: &'a [(&'a str, String)],
    pub now_ms: i64,
}

/// URL 的 path 部分(**不含 host**:官方算法的 `at` 是 pathname+search,
/// 带上 host 签名就错了 —— Golden 向量钉住这一条)。
fn path_of(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    match rest.find('/') {
        Some(i) => &rest[i..],
        None => "/",
    }
    .split('?')
    .next()
    .unwrap_or("/")
}

/// 构造签名请求(纯函数,Golden 向量钉死)。算法口径:
/// `x-signature = md5(秒 + 密钥 [+ body])`,`yy = md5(enc(path+query) + "_" + body或"{}" + md5(毫秒) + "ooui")`。
pub fn sign_request(p: SignParams) -> SignedRequest {
    let base = p.url.split('?').next().unwrap_or(p.url);
    let uid = p.uid.unwrap_or("0").trim();
    let uid = if uid.is_empty() { "0" } else { uid };
    let mut query_pairs = device_query(p.now_ms, p.offset_sec, p.lang, uid);
    for (k, v) in p.extra_query {
        query_pairs.push((k, v.clone()));
    }
    let query = query_pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", enc_component(v)))
        .collect::<Vec<_>>()
        .join("&");
    let at = format!("{}?{query}", path_of(base));
    let ts = p.now_ms / 1000;
    let ts_str = ts.to_string();
    // body 参与 x-signature;yy 的 body 槽位在 GET 时按官方口径填 "{}"
    let sig = match p.body {
        Some(b) => md5_hex(&format!("{ts_str}{}{b}", SIGN_SECRET)),
        None => md5_hex(&format!("{ts_str}{}", SIGN_SECRET)),
    };
    let yy_body = p.body.unwrap_or("{}");
    let yy = md5_hex(&format!("{}_{}{}ooui", enc_component(&at), yy_body, md5_hex(&p.now_ms.to_string())));
    SignedRequest {
        url: format!("{base}?{query}"),
        headers: vec![
            ("yy", yy),
            ("x-timestamp", ts_str),
            ("x-signature", sig),
        ],
    }
}

/// 本地时区偏移(东正秒,如 28800 = UTC+8),与 JS `getTimezoneOffset() * -60` 同口径。
fn local_offset_sec() -> i32 {
    use chrono::Offset;
    chrono::Local::now().offset().fix().local_minus_utc()
}

/// 签到要发的 IANA 时区名(服务端用它决定"今天是哪天";给 offset 或不给会被拒,
/// dsh-connect 实证)。取不到系统名时按本应用的主要用户群退化为上海。
pub fn timezone_id() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "Asia/Shanghai".into())
}

/// fetch 管线用:给"这一个大 URL"现算签名。凭据键必须是 `mm:<region>:<token>`
/// 格式(区域决定域名,签名只绑 path+query,host 无关)。
pub fn sign_for_fetch(
    url: &str,
    method: crate::providers::Method,
    key: &str,
    account_id: Option<&str>,
) -> Option<SignedRequest> {
    let (region, _) = parse_key(key)?; // 键格式校验;Authorization 由调用方从同一键解出
    let body = match method {
        crate::providers::Method::Get => None,
        crate::providers::Method::PostJson(f) => Some(f()),
    };
    Some(sign_request(SignParams {
        url,
        body: body.as_deref(),
        uid: account_id,
        offset_sec: Some(local_offset_sec()),
        lang: region.lang(),
        extra_query: &[],
        now_ms: now_ms(),
    }))
}

/// 签到用:域名跟凭据区域走 + 签到端点带 IANA 时区名(不带 offset,见模块头注)。
pub fn signed_checkin(url: &str, is_post: bool, key: &str, uid: Option<&str>) -> Option<SignedRequest> {
    let (region, _) = parse_key(key)?;
    let swapped = swap_host(url, region.agent_host());
    let tz = timezone_id();
    Some(sign_request(SignParams {
        url: &swapped,
        body: if is_post { Some("{}") } else { None },
        uid,
        offset_sec: None,
        lang: region.lang(),
        extra_query: &[("timezone_id", tz)],
        now_ms: now_ms(),
    }))
}

/// 域名替换(保留 path 与 query)。字符串级替换即可:URL 由我们自己拼,不含转义歧义。
pub fn swap_host(url: &str, host: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => match rest.find('/') {
            Some(i) => format!("{scheme}://{host}{}", &rest[i..]),
            None => format!("{scheme}://{host}/"),
        },
        None => url.to_string(),
    }
}

// ---------------------------------------------------------------- 取数解析

fn fnum(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|x| x.is_finite())
}

fn fpath<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for k in keys {
        cur = cur.get(k)?;
    }
    Some(cur)
}

/// 上游时间戳:一律毫秒(dsh-connect/ CreditDaddy 两个字段口径都是 ms),
/// 但按全仓统一规则归一(>1e11 视为毫秒),上游改秒也不用改代码。
fn ts_sec(v: f64) -> i64 {
    if v > 1e11 {
        (v / 1000.0) as i64
    } else {
        v as i64
    }
}

fn fmt_points(n: f64) -> String {
    if n.fract() == 0.0 {
        return format!("{}", n as i64);
    }
    // 截到两位后去尾零:321.5 → "321.5",不渲染成 "321.50" 那种假精度
    format!("{n:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

/// 会员/积分汇总响应(`POST /matrix/api/v1/commerce/get_membership_info`)。
/// 到期积分不在这里 —— 在账本端点,由 [`merge_expiry`] 合并。
pub fn extract_membership(body: &Value) -> FetchResult {
    let mut r = FetchResult {
        kind: Kind::Points,
        unit: "credits".into(),
        ..Default::default()
    };
    // 业务信封:HTTP 200 不代表成功,base_resp.status_code 才是(与签到同一信封)
    if let Some(code) = fpath(body, &["base_resp", "status_code"]).and_then(fnum) {
        if code != 0.0 {
            let msg = body
                .get("base_resp")
                .and_then(|b| b.get("status_msg"))
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            r.error = Some(format!("MiniMax 业务错误({code}): {msg}"));
            return r;
        }
    }
    // 汇总:op_credit_summary 优先;老账号的扁平字段兜底(dsh-connect 同一条回退链)
    let total = fpath(body, &["op_credit_summary", "total_remaining_amount"])
        .and_then(fnum)
        .or_else(|| body.get("opcredit_balance").and_then(fnum))
        .or_else(|| body.get("total_remains_credit").and_then(fnum));
    let Some(total) = total else {
        r.error = Some("响应里没有积分汇总字段".into());
        return r;
    };
    r.valid = true;
    r.remaining = Some(total);
    let free = fpath(body, &["op_credit_summary", "free_remaining_amount"]).and_then(fnum);
    let purchased = fpath(body, &["op_credit_summary", "purchased_remaining_amount"]).and_then(fnum);
    if let Some(f) = free {
        r.extra.push(("免费积分".into(), fmt_points(f)));
    }
    if let Some(p) = purchased {
        r.extra.push(("购买积分".into(), fmt_points(p)));
    }
    // 套餐:订阅档位与到期日。免费账号没有套餐字段(或为 0),不编造"长期有效"
    let tier = ["token_plan_tier", "plan_name"]
        .iter()
        .find_map(|k| body.get(*k).and_then(Value::as_str).map(str::to_string))
        .filter(|s| !s.is_empty());
    if let Some(t) = tier {
        r.extra.push(("套餐".into(), t));
    }
    let expiry_ms = ["token_plan_expires_at", "expires_at"]
        .iter()
        .find_map(|k| body.get(*k).and_then(fnum))
        .filter(|v| *v > 0.0);
    if let Some(ms) = expiry_ms {
        r.account_expiry = Some(AccountExpiry {
            state: AccountExpiryState::Dated,
            at: Some(ts_sec(ms)),
        });
    }
    r
}

/// 逐笔积分账本(`GET /minimax-cloud/api/v1/credit/details`):每笔授予一个批次,
/// 各带自己的到期时刻。到期的风险主体就是这些批次(免费积分每天发放、30 天到期)。
pub fn parse_lots(body: &Value, now_sec: i64) -> Vec<Expiring> {
    let Some(details) = body.get("details").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for d in details {
        let Some(expire_ms) = d.get("expire_at_ms").and_then(fnum) else {
            // 没有到期时刻的批次不进表:没有日期的批次会被误当"永不过期"
            continue;
        };
        let at = ts_sec(expire_ms);
        if at <= now_sec {
            continue; // 已经过期(上游默认视图本就该排除,双保险)
        }
        let remaining = d.get("remaining_amount").and_then(fnum).unwrap_or(0.0);
        if remaining <= 0.0 {
            continue; // 用完的批次到期时没有可损失的量,不制造虚假的"临期"
        }
        out.push(Expiring {
            at,
            amount: remaining,
            label: "积分批次".into(),
        });
        if out.len() >= LEDGER_MAX_LOTS {
            break;
        }
    }
    out.sort_by_key(|e| e.at);
    out
}

/// 取数管线的追加步:主响应(会员汇总)成功后拉逐笔账本,**尽力而为** ——
/// 账本失败不拖垮余额(口径同 Codex 明细:`expiry_complete=false` 表示
/// "空列表 ≠ 没有到期")。`used_url` 是这次成功请求实际用的地址,账本跟它同域。
pub async fn merge_expiry(
    client: &reqwest::Client,
    key: &str,
    used_url: &str,
    account_id: Option<&str>,
    result: &mut FetchResult,
) {
    let Some((region, token)) = parse_key(key) else {
        result.expiry_complete = false;
        return;
    };
    let host = crate::providers::url_host(used_url).unwrap_or_else(|| region.agent_host().to_string());
    let mut lots: Vec<Expiring> = Vec::new();
    let mut total: f64 = f64::INFINITY;
    let mut page = 1usize;
    while lots.len() < total as usize && page <= LEDGER_MAX_PAGES {
        let url = format!("https://{host}/minimax-cloud/api/v1/credit/details");
        let signed = sign_request(SignParams {
            url: &url,
            body: None,
            uid: account_id,
            offset_sec: Some(local_offset_sec()),
            lang: region.lang(),
            extra_query: &[
                ("page_number", page.to_string()),
                ("page_size", "100".to_string()),
            ],
            now_ms: now_ms(),
        });
        let req = client
            .get(&signed.url)
            .header("Authorization", format!("Bearer {}", token.trim()))
            .header("User-Agent", APP_UA)
            .header("Accept", "application/json");
        let req = signed
            .headers
            .into_iter()
            .fold(req, |r, (k, v)| r.header(k, v));
        let resp = match req.timeout(std::time::Duration::from_secs(8)).send().await {
            Ok(r) => r,
            Err(e) => {
                crate::diaglog::write_keyed("minimax", "ledger", &format!("第 {page} 页请求失败: {e}"));
                result.expiry_complete = false;
                return;
            }
        };
        if !resp.status().is_success() {
            crate::diaglog::write_keyed(
                "minimax",
                "ledger",
                &format!("第 {page} 页 HTTP {}", resp.status().as_u16()),
            );
            result.expiry_complete = false;
            return;
        }
        let raw = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                crate::diaglog::write_keyed("minimax", "ledger", &format!("第 {page} 页读响应失败: {e}"));
                result.expiry_complete = false;
                return;
            }
        };
        let body: Value = match serde_json::from_slice(&raw) {
            Ok(v) => v,
            Err(_) => {
                crate::diaglog::write_keyed("minimax", "ledger", "响应不是 JSON");
                result.expiry_complete = false;
                return;
            }
        };
        let batch = parse_lots(&body, chrono::Utc::now().timestamp());
        let before = lots.len();
        lots.extend(batch);
        if let Some(t) = body.get("total_count").and_then(fnum) {
            total = t;
        }
        if lots.len() == before {
            break; // 空页:到此为止,别死循环
        }
        page += 1;
    }
    result.expiring = lots;
    result.expiry_complete = true;
}

// ---------------------------------------------------------------- 签到解析

/// 签到信封的三个日期枚举值(dsh-connect 从官方卡片面板实证):
/// 1=未到,2=今日可领,3=已领,4=今日无奖励。
const DAY_UPCOMING: f64 = 1.0;
const DAY_CLAIMABLE: f64 = 2.0;
const DAY_CLAIMED: f64 = 3.0;
const DAY_DISABLED: f64 = 4.0;

fn today_points(day: &Value) -> Option<f64> {
    let p = day.get("points").and_then(fnum).unwrap_or(0.0);
    let b = day.get("bonus_points").and_then(fnum).unwrap_or(0.0);
    Some(p + b)
}

/// 签到状态机(status / claim 两个响应都从这里出)。
/// 判因只看结构化字段(status_code / claim_result / status 数值),不看文案。
pub fn parse_minimax(body: &Value, claiming: bool) -> Outcome {
    // 鉴权失败的业务码(青龙脚本对 web 端点实证的码表;OAuth 端点的鉴权失败
    // 走 HTTP 401,由 call_with_body 统一处理,这里只兜业务层)
    if let Some(code) = fpath(body, &["base_resp", "status_code"]).and_then(fnum) {
        if code != 0.0 {
            if code == 1000021.0 || code == 1000022.0 || code == 1000023.0 {
                return Outcome::AuthNeeded;
            }
            return Outcome::Retry(format!("biz:{code}"));
        }
    }
    let data = body.get("data").filter(|d| d.is_object());
    let Some(data) = data else {
        return Outcome::BadShape("响应缺 data 对象".into());
    };
    if claiming {
        // claim 响应:claim_id 必须在场 —— 没有它就无法确认是"本次领取"的答复,
        // 宁可 BadShape 也不能把未知响应写成"已领"(全仓红线:结果回查才算数)
        let Some(claim_id) = data.get("claim_id").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return Outcome::BadShape("claim 响应缺 claim_id".into());
        };
        let _ = claim_id;
        match data.get("claim_result").and_then(fnum) {
            // 1 = 本次领取成功;2 = 今日已领过(重复领取,幂等)
            Some(r) if r == 1.0 => Outcome::Claimed(today_points(data)),
            Some(r) if r == 2.0 => Outcome::Already(today_points(data)),
            other => Outcome::BadShape(format!("claim_result 认不出来: {other:?}")),
        }
    } else {
        let Some(days) = data.get("days").and_then(Value::as_array) else {
            return Outcome::BadShape("status 响应缺 days 数组".into());
        };
        let today = days.iter().find(|d| d.get("is_today").and_then(Value::as_bool) == Some(true));
        let Some(today) = today else {
            // 周期面板在,却没标今天 —— 口径未知,不猜,等下一轮
            return Outcome::Retry("no_today".into());
        };
        match today.get("status").and_then(fnum) {
            Some(s) if s == DAY_CLAIMED => Outcome::Already(today_points(today)),
            Some(s) if s == DAY_CLAIMABLE => Outcome::Retry("ready".into()),
            Some(s) if s == DAY_UPCOMING => Outcome::Retry("wait".into()),
            Some(s) if s == DAY_DISABLED => Outcome::Inactive("今日无签到奖励".into()),
            other => Outcome::BadShape(format!("day status 认不出来: {other:?}")),
        }
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkin::Outcome;

    /// Golden 向量:由参考实现(CreditDaddy minimaxClient.js 的 signedMiniMaxRequest
    /// 算法)在固定时间戳下逐字节算出。改算法必须先解释为什么向量还该成立。
    const NOW_MS: i64 = 1_700_000_000_000;

    fn sign_membership() -> SignedRequest {
        sign_request(SignParams {
            url: "https://agent.minimax.cn/matrix/api/v1/commerce/get_membership_info",
            body: Some("{}"),
            uid: Some("42"),
            offset_sec: Some(28800),
            lang: "zh",
            extra_query: &[],
            now_ms: NOW_MS,
        })
    }

    fn sign_signin_status() -> SignedRequest {
        sign_request(SignParams {
            url: "https://agent.minimax.cn/minimax-cloud/api/v1/signin/status",
            body: None,
            uid: Some("42"),
            offset_sec: None, // 签到查询串不发 offset(dsh-connect 实证服务端拒收)
            lang: "zh",
            extra_query: &[("timezone_id", "Asia/Shanghai".into())],
            now_ms: NOW_MS,
        })
    }

    /// 哨兵:已知正确输入必须全绿;任何字段改动都该红。
    #[test]
    fn signature_golden_membership_post() {
        let s = sign_membership();
        assert_eq!(
            s.url.split_once('?').unwrap().1,
            "device_platform=mcode&biz_id=3&app_id=3001&version_code=22201&unix=1700000000000&timezone_offset=28800&sys_language=zh&lang=zh&device_id=0&os_name=win32&browser_name=mcode&user_id=42&client=mcode"
        );
        let hdr = |k: &str| s.headers.iter().find(|(n, _)| *n == k).unwrap().1.clone();
        assert_eq!(hdr("x-timestamp"), "1700000000");
        assert_eq!(hdr("x-signature"), "b0c74b46e28053fb22e53239b1061505");
        assert_eq!(hdr("yy"), "fd31c9c7e7c73efb0599d15792ca61d5");
    }

    #[test]
    fn signature_golden_signin_status_get() {
        let s = sign_signin_status();
        assert_eq!(
            s.url.split_once('?').unwrap().1,
            "device_platform=mcode&biz_id=3&app_id=3001&version_code=22201&unix=1700000000000&sys_language=zh&lang=zh&device_id=0&os_name=win32&browser_name=mcode&user_id=42&client=mcode&timezone_id=Asia%2FShanghai"
        );
        let hdr = |k: &str| s.headers.iter().find(|(n, _)| *n == k).unwrap().1.clone();
        assert_eq!(hdr("x-signature"), "05064c6d0266350eeac121b673536c14");
        assert_eq!(hdr("yy"), "d821f357e412bfa0fea8597ff835259a");
    }

    /// 双向验证的"红"侧:换密钥,签名必须变 —— 否则这套测试证明不了任何事。
    #[test]
    fn signature_changes_with_secret() {
        let s = sign_membership();
        let hdr = |k: &str| s.headers.iter().find(|(n, _)| *n == k).unwrap().1.clone();
        // 结构对账:我们的 x-signature 恰是 md5(秒 + 密钥 + body)
        assert_eq!(
            hdr("x-signature"),
            md5_hex(&format!("1700000000{}{{}}", SIGN_SECRET))
        );
        // 换一把错密钥,结果必须不同
        assert_ne!(hdr("x-signature"), md5_hex("1700000000WRONG-SECRET{}"));
        // yy 的构造同理:enc(at)_body_md5(毫秒)ooui
        let now_str = NOW_MS.to_string();
        assert_eq!(
            hdr("yy"),
            md5_hex(&format!(
                "{}_{}{}ooui",
                enc_component("/matrix/api/v1/commerce/get_membership_info?device_platform=mcode&biz_id=3&app_id=3001&version_code=22201&unix=1700000000000&timezone_offset=28800&sys_language=zh&lang=zh&device_id=0&os_name=win32&browser_name=mcode&user_id=42&client=mcode"),
                "{}",
                md5_hex(&now_str)
            ))
        );
    }

    #[test]
    fn signature_golden_claim_post_uid0() {
        // 领取:POST + body "{}" + uid 缺省退化 "0"(青龙脚本同口径)
        let claim = sign_request(SignParams {
            url: "https://agent.minimax.cn/minimax-cloud/api/v1/signin/claim",
            body: Some("{}"),
            uid: None,
            offset_sec: None,
            lang: "zh",
            extra_query: &[("timezone_id", "Asia/Shanghai".into())],
            now_ms: NOW_MS,
        });
        let hdr = |k: &str| claim.headers.iter().find(|(n, _)| *n == k).unwrap().1.clone();
        // POST 的 x-signature 拼了 body,与 GET(上一条向量)必然不同
        assert_eq!(hdr("x-signature"), "b0c74b46e28053fb22e53239b1061505");
        assert_eq!(hdr("yy"), "ad47012a815a06c105e070cce96ee308");
    }

    #[test]
    fn enc_component_matches_js() {
        assert_eq!(enc_component("abc-_.!~*'()"), "abc-_.!~*'()");
        assert_eq!(enc_component("a=b&c?d/e"), "a%3Db%26c%3Fd%2Fe");
        assert_eq!(enc_component("Asia/Shanghai"), "Asia%2FShanghai");
        assert_eq!(enc_component("中"), "%E4%B8%AD");
    }

    #[test]
    fn swap_host_keeps_path_and_query() {
        assert_eq!(
            swap_host("https://agent.minimax.cn/a/b?x=1", "agent.minimax.io"),
            "https://agent.minimax.io/a/b?x=1"
        );
        assert_eq!(swap_host("https://agent.minimax.cn", "h"), "https://h/");
    }

    #[test]
    fn parse_key_roundtrip() {
        let cred = LocalCred {
            access: "mmoat_x".into(),
            refresh: None,
            expires_at_ms: None,
            region: Region::Io,
            uid: None,
        };
        let key = encode_key(&cred);
        assert_eq!(key, "mm:io:mmoat_x");
        assert_eq!(parse_key(&key), Some((Region::Io, "mmoat_x")));
        assert_eq!(parse_key("garbage"), None);
        assert_eq!(parse_key("mm:xx:t"), None);
        assert_eq!(parse_key("mm:cn:"), None);
    }

    #[test]
    fn auth_document_parse_and_first_record() {
        let text = r#"{
            "schemaVersion": 3,
            "records": {
                "com.minimax.mcode.oauth.prod.cn\u0000abc": {"accessToken": "", "refreshToken": "r0"},
                "com.minimax.mcode.oauth.prod.cn\u0000def": {
                    "accessToken": "mmoat_1", "refreshToken": "r1", "expiresAtMs": 123,
                    "clientId": "mcode-public", "scopes": ["agent.default"],
                    "audience": "agent-backend", "generation": 7
                }
            }
        }"#;
        let cred = parse_auth_document(text, Region::Cn).expect("应解出第二条记录");
        assert_eq!(cred.access, "mmoat_1");
        assert_eq!(cred.refresh.as_deref(), Some("r1"));
        assert_eq!(cred.expires_at_ms, Some(123));
        assert_eq!(cred.region, Region::Cn);
        assert!(parse_auth_document("{\"records\":{}}", Region::Cn).is_none());
        assert!(parse_auth_document("not json", Region::Cn).is_none());
    }

    #[test]
    fn jwt_uid_variants() {
        let mk = |payload: &str| {
            let b64 = URL_SAFE_NO_PAD.encode(payload.as_bytes());
            format!("h.{b64}.s")
        };
        assert_eq!(jwt_uid(&mk(r#"{"user":{"id":"42"}}"#)), Some("42".into()));
        assert_eq!(jwt_uid(&mk(r#"{"user":{"id":42}}"#)), Some("42".into()));
        assert_eq!(jwt_uid(&mk(r#"{"user":{}}"#)), None);
        assert_eq!(jwt_uid("garbage"), None);
    }

    #[test]
    fn patch_preserves_generation_and_other_records() {
        let text = r#"{
            "schemaVersion": 3,
            "records": {
                "k\u0000a": {"accessToken": "old", "refreshToken": "rold", "generation": 7, "loginEpoch": "e1"}
            }
        }"#;
        let out = patch_auth_document(text, "new", Some("rnew"), 999).expect("应产出新文档");
        let doc: Value = serde_json::from_str(&out).unwrap();
        let rec = doc["records"]["k\u{0000}a"].as_object().unwrap();
        assert_eq!(rec["accessToken"], "new");
        assert_eq!(rec["refreshToken"], "rnew");
        assert_eq!(rec["tokenType"], "Bearer");
        assert_eq!(rec["expiresAtMs"], 999);
        assert_eq!(rec["generation"], 7, "generation 不能动");
        assert_eq!(rec["loginEpoch"], "e1");
        assert_eq!(doc["schemaVersion"], 3);
    }

    #[test]
    fn patch_without_token_record_is_none() {
        assert!(patch_auth_document(r#"{"records":{}}"#, "a", None, 1).is_none());
    }

    fn membership_body() -> Value {
        serde_json::json!({
            "base_resp": {"status_code": 0, "status_msg": "ok"},
            "op_credit_summary": {
                "total_remaining_amount": 4321.5,
                "free_remaining_amount": 321.5,
                "purchased_remaining_amount": 4000
            },
            "token_plan_tier": "Go",
            "has_token_plan": true,
            "token_plan_expires_at": 1800000000000_i64
        })
    }

    #[test]
    fn membership_extracts_summary_plan_expiry() {
        let r = extract_membership(&membership_body());
        assert!(r.valid);
        assert_eq!(r.kind, Kind::Points);
        assert_eq!(r.remaining, Some(4321.5));
        assert_eq!(r.unit, "credits");
        assert!(r.extra.contains(&("免费积分".into(), "321.5".into())));
        assert!(r.extra.contains(&("购买积分".into(), "4000".into())));
        assert!(r.extra.contains(&("套餐".into(), "Go".into())));
        assert_eq!(r.account_expiry.and_then(|a| a.at), Some(1_800_000_000_i64));
        assert!(!r.expiry_complete, "账本未合并前不能说完整");
    }

    #[test]
    fn membership_business_error_is_invalid() {
        let body = serde_json::json!({"base_resp": {"status_code": 1004, "status_msg": "bad"}});
        let r = extract_membership(&body);
        assert!(!r.valid);
        assert!(r.error.unwrap().contains("1004"));
    }

    #[test]
    fn membership_flat_fallback_fields() {
        let body = serde_json::json!({"total_remains_credit": "88"});
        let r = extract_membership(&body);
        assert!(r.valid);
        assert_eq!(r.remaining, Some(88.0));
    }

    #[test]
    fn membership_no_summary_is_honest_error() {
        let r = extract_membership(&serde_json::json!({"base_resp": {"status_code": 0}}));
        assert!(!r.valid);
        assert!(r.error.is_some());
    }

    #[test]
    fn lots_filter_and_sort() {
        let now = 1_700_000_000_i64;
        let body = serde_json::json!({
            "details": [
                {"granted_amount": 400, "remaining_amount": 0, "expire_at_ms": 1_700_100_000_000_i64},
                {"granted_amount": 400, "remaining_amount": 120, "expire_at_ms": 1_700_200_000_000_i64},
                {"granted_amount": 400, "remaining_amount": 300, "expire_at_ms": 1_700_050_000_000_i64},
                {"granted_amount": 400, "remaining_amount": 400, "expire_at_ms": 1_699_900_000_000_i64},
                {"granted_amount": 100, "remaining_amount": 90}
            ],
            "total_count": 5
        });
        let lots = parse_lots(&body, now);
        assert_eq!(lots.len(), 2, "用尽的与过期的与无到期日的都不进表");
        assert_eq!(lots[0].at, 1_700_050_000_i64);
        assert_eq!(lots[0].amount, 300.0);
        assert_eq!(lots[1].at, 1_700_200_000_i64);
        assert_eq!(lots[1].amount, 120.0);
    }

    fn status_body(status: f64) -> Value {
        serde_json::json!({
            "base_resp": {"status_code": 0},
            "data": {"scene": 1, "days": [
                {"day_no": 1, "points": 400, "bonus_points": 100, "status": 3, "is_today": false},
                {"day_no": 2, "points": 400, "bonus_points": 0, "status": status, "is_today": true}
            ]}
        })
    }

    #[test]
    fn signin_status_states() {
        assert_eq!(parse_minimax(&status_body(2.0), false), Outcome::Retry("ready".into()));
        assert_eq!(parse_minimax(&status_body(3.0), false), Outcome::Already(Some(400.0)));
        assert_eq!(parse_minimax(&status_body(1.0), false), Outcome::Retry("wait".into()));
        assert_eq!(
            parse_minimax(&status_body(4.0), false),
            Outcome::Inactive("今日无签到奖励".into())
        );
    }

    #[test]
    fn signin_status_shapes() {
        let no_today = serde_json::json!({
            "base_resp": {"status_code": 0},
            "data": {"days": [{"day_no": 1, "points": 400, "status": 3, "is_today": false}]}
        });
        assert_eq!(parse_minimax(&no_today, false), Outcome::Retry("no_today".into()));
        let no_days = serde_json::json!({"base_resp": {"status_code": 0}, "data": {}});
        assert!(matches!(parse_minimax(&no_days, false), Outcome::BadShape(_)));
        let no_data = serde_json::json!({"base_resp": {"status_code": 0}});
        assert!(matches!(parse_minimax(&no_data, false), Outcome::BadShape(_)));
    }

    fn claim_body(result: f64) -> Value {
        serde_json::json!({
            "base_resp": {"status_code": 0},
            "data": {
                "claim_id": "c-1",
                "claim_result": result,
                "day_no": 3,
                "points": 400,
                "bonus_points": 1000,
                "expire_at_ms": 1_800_000_000_000_i64
            }
        })
    }

    #[test]
    fn signin_claim_results() {
        assert_eq!(parse_minimax(&claim_body(1.0), true), Outcome::Claimed(Some(1400.0)));
        assert_eq!(parse_minimax(&claim_body(2.0), true), Outcome::Already(Some(1400.0)));
        let no_id = serde_json::json!({
            "base_resp": {"status_code": 0},
            "data": {"claim_result": 1, "points": 400}
        });
        assert!(matches!(parse_minimax(&no_id, true), Outcome::BadShape(_)));
    }

    #[test]
    fn signin_auth_codes_map_to_auth_needed() {
        for code in [1000021, 1000022, 1000023] {
            let body = serde_json::json!({"base_resp": {"status_code": code}});
            assert_eq!(parse_minimax(&body, false), Outcome::AuthNeeded, "code {code}");
        }
        let body = serde_json::json!({"base_resp": {"status_code": 500}});
        assert_eq!(parse_minimax(&body, false), Outcome::Retry("biz:500".into()));
    }

    #[test]
    fn region_hosts_and_dirs() {
        assert_eq!(Region::Cn.agent_host(), "agent.minimax.cn");
        assert_eq!(Region::Io.agent_host(), "agent.minimax.io");
        assert_eq!(Region::Com.agent_host(), "agent.minimaxi.com");
        assert_eq!(Region::Com.account_host(), "account.minimaxi.com");
        assert_eq!(Region::Cn.lang(), "zh");
        assert_eq!(Region::Io.lang(), "en");
    }

    #[test]
    fn signed_checkin_swaps_host_by_region() {
        let key = "mm:io:mmoat_x";
        let s = signed_checkin("https://agent.minimax.cn/minimax-cloud/api/v1/signin/status", false, key, Some("9")).expect("io 键应可签");
        assert!(s.url.starts_with("https://agent.minimax.io/minimax-cloud/api/v1/signin/status?"));
        assert!(s.url.contains("timezone_id="));
        assert!(!s.url.contains("timezone_offset"), "签到查询串不带 offset(dsh-connect 实证)");
    }

    /// 时间是签名输入:同一输入重复签名必须逐字节一致(幂等),不同时间必须不同。
    #[test]
    fn sign_is_deterministic() {
        let a = sign_membership();
        let b = sign_membership();
        assert_eq!(a.url, b.url);
        assert_eq!(a.headers, b.headers);
    }
}
