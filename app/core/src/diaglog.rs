//! 排查用落盘日志:**当用户报"某渠道取不到数"时,唯一能还原现场的东西**。
//!
//! ## 为什么需要它
//!
//! 2026-10-20 用户反馈:装上之后 WorkBuddy 渠道一直取不到数,而他的 WorkBuddy 装在
//! **D 盘**。挂件的失败原因是分层描述的(找不到 exe / 问不出密钥 / keyId 不匹配 /
//! 信封认证失败 / 凭据过期 / 401),但这些信息以前只走 `eprintln!` —— release 是
//! GUI 子系统,`stderr` 根本没人看得见(同一个坑 P2-14 已经踩过一次)。结果只能靠
//! 远程猜:让他去翻注册表、翻目录、把每一步截图回来。
//!
//! 所以这里落一份**人能直接读、能整段贴回来**的日志。设计上守三条:
//!
//! 1. **绝不写密钥**:accessToken / refreshToken / Cookie / atRestSecretKey 一律不进来。
//!    要定位时记的是"长度 + sha256 前 8 位"([`secret_brief`]),不同令牌可区分,
//!    但拿不到原文。写入前还会过一遍 [`redact`] 做兜底 —— 万一手滑传了令牌,
//!    它也会被掩成 `«secret len=…»`。
//! 2. **不能把磁盘写满**:挂件是 7×24 常驻,每 60 秒一轮,不限量就是无底洞。
//!    单文件 [`MAX_BYTES`],超了就滚成 `diag.1.log`(只留一代),总量封在 2 MB 上下。
//! 3. **不能拖累取数**:所有 IO 失败静默吞掉(只是少一条日志,不该让渠道失败或者
//!    让进程 panic)。写失败还会自我熔断一分钟,免得在只读盘上每轮都白撞。
//!
//! ## 为什么还要节流
//!
//! 同一个失败会每 60 秒重复一次,不节流的话一天 1440 行全是同一句,真正有信息量的
//! "第一次失败 / 换到第二条路 / 错误变了"会被埋掉。所以 [`write_keyed`] 按 key 记
//! 签名:同签名在 [`REPEAT_WINDOW_SEC`] 内只落一次,窗口过后补一行"重复了 N 次"。
//! 状态一变(签名不同)立刻落新行。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 日志文件名。滚动后的旧文件是 [`LOG_FILE`] 前面加 `.1`。
pub const LOG_FILE: &str = "diag.log";
/// 滚动一代。挂件常驻,两代足够覆盖"出问题到用户来报"这段时间。
#[allow(dead_code)] // 只留一代(见 rotate),常量留着说明设计,别真去加代数
const KEEP_GENERATIONS: usize = 1;
/// 单文件上限。超过就滚:两代合计约 2 MB。
const MAX_BYTES: u64 = 1024 * 1024;
/// 同一 key 的同签名在多少秒内只落一次。
const REPEAT_WINDOW_SEC: i64 = 300;
/// 写失败后的熔断时长:只读盘 / 权限问题时不要每轮都去撞一次。
const FAIL_COOLDOWN: Duration = Duration::from_secs(60);

/// 日志目录。外壳启动时用 [`set_dir`] 注入(它拿得到 Tauri 的 app_data_dir);
/// 没注入时按 `%APPDATA%\com.wacil.tokenscope` 猜 —— 与 `report_fatal` 写
/// `startup-error.log` 的地方是同一个目录,用户找得着。
static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 测试构建里**默认不落盘**。
///
/// 为什么必须有这条:`workbuddy` 的用例会用临时目录伪造凭据、跑完整条读取链,
/// 那些调用会经过这里。第一次真机验证时就抓到了后果 —— 用户的 `diag.log` 里混进了
/// 一串 `C:\...\Temp\ts-wb-expired-*\...` 的行,真出问题时要在一堆测试噪声里找线索。
/// 要测落盘行为的用例显式调 [`set_dir`](临时目录),它们本来也是这么做的。
#[cfg(test)]
static TEST_ARMED: Mutex<bool> = Mutex::new(false);

/// 进程内是否已确认"这台机器的磁盘写不了"。One-shot:过期后允许再试。
static FAILED_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// 写日志与滚动必须串行,否则两个线程会同时 rename、把文件走丢。
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 节流表:key → (上次签名, 上次落笔时刻, 期间被压掉几次)。
static SEEN: Mutex<Option<HashMap<String, Seen>>> = Mutex::new(None);

struct Seen {
    sig: String,
    at: i64,
    suppressed: u32,
}

// ───────────────────────── 目录与路径 ─────────────────────────

/// 由外壳注入真实的数据目录(与 SQLite 同一个文件夹)。
pub fn set_dir(dir: PathBuf) {
    if let Ok(mut g) = DIR.lock() {
        *g = Some(dir);
    }
    #[cfg(test)]
    {
        // 测试里显式 set_dir = 明确要测落盘,放开闸门
        if let Ok(mut a) = TEST_ARMED.lock() {
            *a = true;
        }
    }
}

/// 测试构建下"没有显式指定目录"就是不该落盘(见 [`TEST_ARMED`] 的注释)。
fn muted_for_tests() -> bool {
    #[cfg(test)]
    {
        return !TEST_ARMED.lock().map(|a| *a).unwrap_or(false);
    }
    #[cfg(not(test))]
    false
}

/// 日志目录。顺序:外壳注入 → `%APPDATA%\com.wacil.tokenscope` → 临时目录。
/// 返回 None = 连临时目录都拿不到(理论上不会),调用方据此静默跳过。
pub fn log_dir() -> Option<PathBuf> {
    if let Ok(g) = DIR.lock() {
        if let Some(d) = g.as_ref() {
            return Some(d.clone());
        }
    }
    for var in ["APPDATA", "LOCALAPPDATA"] {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(v);
            if !p.as_os_str().is_empty() {
                return Some(p.join("com.wacil.tokenscope"));
            }
        }
    }
    let tmp = std::env::temp_dir();
    (!tmp.as_os_str().is_empty()).then_some(tmp.join("com.wacil.tokenscope"))
}

/// 当前这份日志的完整路径(界面要显示它,用户要能直接去开)。
pub fn log_path() -> Option<PathBuf> {
    log_dir().map(|d| d.join(LOG_FILE))
}

/// 日志功能是否被用户显式关掉(逃生门:`TOKENSCOPE_NODIAG=1`)。
fn disabled() -> bool {
    std::env::var_os("TOKENSCOPE_NODIAG")
        .map(|v| {
            let v = v.to_string_lossy().trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        })
        .unwrap_or(false)
}

/// 给界面看:日志是不是被关掉了(关着时设置页要如实说明,别让人以为有日志)。
pub fn is_disabled() -> bool {
    disabled()
}

// ───────────────────────── 脱敏 ─────────────────────────

/// 令牌的**可区分摘要**:长度 + sha256 前 8 位。
///
/// 排查时真正要回答的是"用的是哪一份令牌、换没换、长度对不对"(比如
/// `desktop:` 前缀丢了、Cookie 只剩半截),这些都不需要原文。
pub fn secret_brief(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let t = s.trim();
    if t.is_empty() {
        return "len=0".to_string();
    }
    let d = Sha256::digest(t.as_bytes());
    let fp: String = d.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("len={} fp={fp}", t.chars().count())
}

/// 兜底脱敏:把"长得像令牌/会话值"的串掩掉,只留长度。
///
/// 判据刻意保守 —— **必须**同时满足"连续 ≥ 32 个 base64/hex 字符",所以路径、
/// 域名、UUID(含 `-`)、错误码都不会被误伤。真的把 accessToken 传进来时能被它挡住。
///
/// 另外**紧跟在 `/` 后面的串一律不掩**:那是 URL 路径段(如
/// `https://api.trae.cn/trae/api/v2/pay/user_current_entitlement_list`),
/// 真机日志里它曾被当成令牌掩成 `https://api.trae.«secret len=48»`,
/// 把"用的哪个接口"这条最有用的信息弄丢了。令牌不会出现在 URL 路径里。
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    // 这一串前面是不是 `/`(URL 路径段)
    let mut after_slash = false;
    let flush = |run: &mut String, out: &mut String, after_slash: bool| {
        if run.is_empty() {
            return;
        }
        // 三段都不掩:
        //   ① 紧跟 `/` 的串 —— URL 路径段(如 `.../pay/user_current_entitlement_list`)
        //   ② 含 `.` 的串 —— 主机名 / 文件名(`api.trae.cn`、`workbuddy-desktop.info`)
        //   ③ 全小写且无数字 —— 普通单词
        // 真令牌(JWT / base64 / hex)既不会整段挂在 `/` 后面,也极少含 `.`,
        // 而这三条正好覆盖了"被误伤"的全部真机样本。
        let looks_like_path = after_slash || run.contains('.') || !is_secret_run(run);
        if looks_like_path {
            out.push_str(run);
        } else {
            out.push_str(&format!("«secret len={}»", run.chars().count()));
        }
        run.clear();
    };
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '=' | '_' | '.') {
            // 注意:`/` 与 `.` 本身也是 base64/域名的一部分,所以它们进入 run;
            // 但"串首前面是 `/`"这个事实只在这里判一次。
            if run.is_empty() {
                after_slash = out.ends_with('/');
            }
            run.push(ch);
        } else {
            flush(&mut run, &mut out, after_slash);
            out.push(ch);
        }
    }
    flush(&mut run, &mut out, after_slash);
    // 键值形态的会话值单独再挡一层:`session=<值>` / `cookie:<值>` 这种短值
    // 可能不到 32 字符,但同样是凭据。
    redact_keyed_values(&out)
}

/// 一段连续 base64/hex 字符算不算"令牌"。
///
/// 判据:长度 ≥ 32 **且**里面有数字或大写(路径段、单词、域名全小写无数字,不会中招)。
/// 为什么不更严:JWT 的第三段、uuid 之外的真令牌大多是这种形态;判严了漏掩才是事故。
fn is_secret_run(s: &str) -> bool {
    let n = s.chars().count();
    n >= 32 && s.chars().any(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
}

/// `session=...` / `session_2=...` 的值掩掉。
fn redact_keyed_values(text: &str) -> String {
    const KEYS: [&str; 6] = ["session_2=", "session=", "cookie:", "bearer ", "accesstoken=", "refreshtoken="];
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        let hit = KEYS.iter().find_map(|k| {
            lower[i..]
                .starts_with(k)
                .then_some((k.len(), k.ends_with('=') || k.ends_with(':') || k.ends_with(' ')))
        });
        match hit {
            Some((klen, _)) => {
                out.push_str(&text[i..i + klen]);
                let mut j = i + klen;
                while j < bytes.len() && !matches!(bytes[j], b';' | b' ' | b'\r' | b'\n' | b'"' | b',') {
                    j += 1;
                }
                let val = &text[i + klen..j];
                if val.is_empty() {
                    // 只有前缀没有值,原样保留(比如 `cookie:` 后面跟的是说明文字)
                } else {
                    out.push_str(&format!("«secret len={}»", val.chars().count()));
                }
                i = j;
            }
            None => {
                let ch = text[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

// ───────────────────────── 节流判据(可单测)─────────────────────────

/// 一行该不该落、落什么。抽成枚举是为了能只测判据、不碰文件。
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// 直接写这一行
    Write,
    /// 同签名,窗口内:只累加计数
    Skip,
    /// 窗口过了:先把"上一段重复了几次"补出来,再写这一行
    WriteAfterRepeat(u32),
}

/// 节流判定。**纯逻辑**:时间由参数给,单测能把 5 分钟窗口端到端钉住。
fn decide(seen: &mut HashMap<String, Seen>, key: &str, sig: &str, now: i64) -> Decision {
    match seen.get_mut(key) {
        Some(prev) if prev.sig == sig => {
            if now - prev.at < REPEAT_WINDOW_SEC {
                prev.suppressed = prev.suppressed.saturating_add(1);
                Decision::Skip
            } else {
                let n = prev.suppressed;
                prev.at = now;
                prev.suppressed = 0;
                Decision::WriteAfterRepeat(n)
            }
        }
        Some(prev) => {
            let n = prev.suppressed;
            prev.sig = sig.to_string();
            prev.at = now;
            prev.suppressed = 0;
            Decision::WriteAfterRepeat(n)
        }
        None => {
            seen.insert(
                key.to_string(),
                Seen {
                    sig: sig.to_string(),
                    at: now,
                    suppressed: 0,
                },
            );
            Decision::Write
        }
    }
}

// ───────────────────────── 写 ─────────────────────────

/// 无条件写一行(一次性事件:启动、登录成功、配置换了)。不参与节流/去重。
pub fn write(scope: &str, msg: &str) {
    write_maybe_throttled(scope, None, msg);
}

/// 按 key 节流地写一行。同一 key 的同签名在 [`REPEAT_WINDOW_SEC`] 内只落一次,
/// 窗口过后补一行"重复 N 次"。**取数主链路用这个**。
pub fn write_keyed(scope: &str, key: &str, msg: &str) {
    write_maybe_throttled(scope, Some(key), msg);
}

fn write_maybe_throttled(scope: &str, key: Option<&str>, msg: &str) {
    if disabled() {
        return;
    }
    let now = crate::fetch::now_ts();
    let line = match key {
        // 签名 = 内容本身(已脱敏)。内容一变就是新事件,立刻落。
        Some(k) => {
            let clean = redact(msg);
            let decision = {
                let mut g = SEEN.lock().unwrap_or_else(|p| p.into_inner());
                let map = g.get_or_insert_with(HashMap::new);
                decide(map, k, &clean, now)
            };
            match decision {
                Decision::Skip => return,
                Decision::Write => clean,
                Decision::WriteAfterRepeat(n) if n > 0 => {
                    let head = format!("{scope}: 上一条相同状态又重复了 {n} 次");
                    emit(&head);
                    clean
                }
                Decision::WriteAfterRepeat(_) => clean,
            }
        }
        None => redact(msg),
    };
    emit(&format!("{scope}: {line}"));
}

fn emit(body: &str) {
    if muted_for_tests() {
        return;
    }
    if let Some(dir) = log_dir() {
        let line = format!("{} {body}", stamp());
        if append_line(&dir, MAX_BYTES, &line).is_err() {
            // 写不了就熔断一分钟:只读盘 / 权限问题上不要每轮白撞。
            if let Ok(mut g) = FAILED_AT.lock() {
                *g = Some(Instant::now());
            }
        }
    }
}

fn cooling_down() -> bool {
    FAILED_AT
        .lock()
        .ok()
        .and_then(|g| *g)
        .map(|t| t.elapsed() < FAIL_COOLDOWN)
        .unwrap_or(false)
}

/// 本地时间戳,带毫秒与偏移 —— 用户报"几点几分出的问题"时能对齐。
fn stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f%:z").to_string()
}

/// 追加一行(必要时先滚动)。抽成独立函数是为了能用小上限把滚动测掉。
fn append_line(dir: &Path, max_bytes: u64, line: &str) -> std::io::Result<()> {
    if cooling_down() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let path = dir.join(LOG_FILE);
    let _g = WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() >= max_bytes {
            rotate(dir, &path)?;
        }
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// 滚动:`diag.log` → `diag.1.log`(旧的那一代直接丢)。失败不致命 —— 哪怕不滚,
/// 也只是文件变大,不该让写日志本身报错。
fn rotate(dir: &Path, path: &Path) -> std::io::Result<()> {
    let bak = dir.join(format!("{LOG_FILE}.1"));
    // 只留一代:先清旧备份,再把当前文件搬过去。删不掉(被别的进程占着)就算了,
    // 下面 rename 失败时调用方会把它当成"这次没滚成",下一行继续往老文件里追加。
    let _ = std::fs::remove_file(&bak);
    std::fs::rename(path, &bak)
}

// ───────────────────────── 读(界面/体检用)─────────────────────────

/// 取最近 `max` 行(给设置页预览与体检报告尾巴)。读不到就是空,不报错。
pub fn tail_lines(max: usize) -> Vec<String> {
    let Some(p) = log_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Vec::new();
    };
    let all: Vec<&str> = text.lines().collect();
    let from = all.len().saturating_sub(max);
    all[from..].iter().map(|s| s.to_string()).collect()
}

/// 日志大小(界面显示"日志 342 KB",用户据此判断要不要清)。
pub fn size_bytes() -> u64 {
    let Some(p) = log_path() else { return 0 };
    std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)
}

/// 清空日志(用户报完问题、或者想重新观察一段时用)。
pub fn clear() -> Result<(), String> {
    let Some(dir) = log_dir() else {
        return Err("拿不到日志目录".into());
    };
    let _g = WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    for name in [LOG_FILE.to_string(), format!("{LOG_FILE}.1")] {
        let p = dir.join(name);
        if p.exists() {
            std::fs::remove_file(&p).map_err(|e| format!("删除 {} 失败: {e}", p.display()))?;
        }
    }
    if let Ok(mut g) = SEEN.lock() {
        g.get_or_insert_with(HashMap::new).clear();
    }
    Ok(())
}

/// 启动时写一行环境事实:**用户报问题时最先要看的东西**。
/// 盘符、数据目录、exe 所在盘、环境变量是否设过 —— 这些决定了后面所有路径推断。
pub fn startup_banner(app_version: &str) {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("<取不到: {e}>"));
    let dir = log_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "<取不到>".into());
    let envs: Vec<String> = [
        "LOCALAPPDATA",
        "APPDATA",
        "USERPROFILE",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "WORKBUDDY_APP_EXECUTABLE",
        "WORKBUDDY_AUTH_DIR",
        "TOKENSCOPE_NODIAG",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| format!("{k}={v}")))
    .collect();
    write(
        "启动",
        &format!(
            "Grandettoken {app_version} · exe={exe} · 日志目录={dir} · {}{}",
            if envs.is_empty() { "环境变量:无" } else { "环境变量:" },
            envs.join(" · ")
        ),
    );
    write(
        "启动",
        &format!(
            "本机盘符:{}",
            crate::workbuddy::drive_letters()
                .iter()
                .map(|d| format!("{d}:"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ts-diag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建临时目录");
        d
    }

    /// 令牌摘要必须"能区分、拿不回原文"。
    #[test]
    fn secret_brief_is_a_digest_not_the_secret() {
        let tok = "eyJhbGciOiJIUzI1NiJ9.very-long-access-token-value-0123456789";
        let b = secret_brief(tok);
        assert!(!b.contains("eyJhbGci"), "摘要里不许出现原文:{b}");
        assert!(b.starts_with("len="), "{b}");
        assert_ne!(secret_brief("a"), secret_brief("b"), "不同令牌要能区分");
        assert_eq!(secret_brief("  "), "len=0");
        assert_eq!(secret_brief(tok), secret_brief(&format!(" {tok} ")), "前后空白不影响");
    }

    /// 兜底脱敏:长令牌掩掉,而路径 / 域名 / UUID 这些排查要用的不能被误伤。
    #[test]
    fn redact_masks_tokens_but_keeps_paths() {
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let line = format!("GET https://www.codebuddy.cn/x 带 Authorization: Bearer {token} 结束");
        let out = redact(&line);
        assert!(!out.contains("eyJhbGci"), "令牌没被掩:{out}");
        assert!(out.contains("«secret len="), "{out}");
        assert!(out.contains("https://www.codebuddy.cn/x"), "域名要留着:{out}");

        // 路径、UUID、短错误码不该被当成密钥
        for keep in [
            r"C:\Users\wacil\AppData\Local\CodeBuddyExtension\Data\Public\auth\workbuddy-desktop.info",
            "5f2b9c1e-3a4d-4c8e-9b7a-1d2e3f4a5b6c",
            "HTTP 401 code 10085",
            "D:/Programs/WorkBuddy/WorkBuddy.exe",
            // URL 路径段:真机日志里曾被误掩成 `https://api.trae.«secret len=48»`
            "https://api.trae.cn/trae/api/v2/pay/user_current_entitlement_list",
            "https://www.codebuddy.cn/v2/billing/meter/get-user-resource",
        ] {
            let got = redact(keep);
            assert!(got.contains(keep), "被误伤了:{keep} → {got}");
        }
    }

    /// 会话值:`session=<短值>` 也要掩(它可能不到 32 字符,单靠长度判据挡不住)。
    #[test]
    fn redact_masks_cookie_values() {
        let out = redact("凭据 cookie:session=abc123; session_2=def456 尾巴");
        assert!(!out.contains("abc123"), "{out}");
        assert!(!out.contains("def456"), "{out}");
        assert!(out.contains("«secret len=6»"), "{out}");
        assert!(out.contains("尾巴"));
    }

    /// 节流判据:同签名窗口内只落一次,窗口过后补"重复 N 次",签名一变立刻落。
    #[test]
    fn throttle_keeps_first_drop_and_flags_a_changed_state() {
        let mut seen: HashMap<String, Seen> = HashMap::new();
        let t0 = 1_800_000_000i64;
        assert_eq!(decide(&mut seen, "wb", "找不到 exe", t0), Decision::Write);
        assert_eq!(decide(&mut seen, "wb", "找不到 exe", t0 + 60), Decision::Skip);
        assert_eq!(decide(&mut seen, "wb", "找不到 exe", t0 + 120), Decision::Skip);
        // 同一签名超过窗口:补一行"重复了 2 次"
        assert_eq!(
            decide(&mut seen, "wb", "找不到 exe", t0 + REPEAT_WINDOW_SEC + 1),
            Decision::WriteAfterRepeat(2)
        );
        // 错误变了:立刻落新行
        assert_eq!(
            decide(&mut seen, "wb", "401 凭据过期", t0 + REPEAT_WINDOW_SEC + 2),
            Decision::WriteAfterRepeat(0)
        );
        // 不同 key 各自独立
        assert_eq!(decide(&mut seen, "trae", "401", t0 + 10), Decision::Write);
    }

    /// 落盘 + 滚动:超过上限后旧内容搬到 `.1`,新文件从空开始,总量不膨胀。
    ///
    /// 上界说明:滚动发生在**写之前**,所以当前文件可能是"上一次的 199 字节 + 这一行",
    /// 最多超出一行的量;两代合计封在 `2 * max + 一行` 之内 —— 这就是"不会写满磁盘"的
    /// 硬保证,下面按它断言。
    #[test]
    fn append_rotates_and_keeps_size_bounded() {
        let dir = tmpdir("rotate");
        let max = 200u64;
        for i in 0..40 {
            append_line(&dir, max, &format!("line-{i:03} 一些填充内容")).unwrap();
        }
        let cur = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap();
        let bak = std::fs::read_to_string(dir.join(format!("{LOG_FILE}.1"))).unwrap();
        let slack = 64u64; // 一行的余量
        assert!(cur.len() as u64 <= max + slack, "当前文件超限:{}", cur.len());
        assert!(bak.len() as u64 <= max + slack, "旧文件超限:{}", bak.len());
        // 总量必须是常数级,而不是随写入次数线性增长
        assert!(
            (cur.len() + bak.len()) as u64 <= 2 * max + 2 * slack,
            "两份日志合计超限:{}",
            cur.len() + bak.len()
        );
        assert!(cur.contains("line-039"), "最新一行必须在当前文件里");
        assert!(!bak.contains("line-039"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 目录还没建也得能写(全新安装 / 用户删过数据目录)。
    #[test]
    fn append_creates_the_directory() {
        let dir = tmpdir("mkdir").join("deep").join("nested");
        append_line(&dir, MAX_BYTES, "hello").unwrap();
        let got = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap();
        assert!(got.contains("hello"));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }

    /// 时间戳要带日期、毫秒与偏移:用户说"早上九点多"时要能对上。
    #[test]
    fn stamp_has_date_and_offset() {
        let s = stamp();
        assert_eq!(s.len() > 20, true, "{s}");
        assert!(s.contains('-') && s.contains(':'), "{s}");
        // 形如 2026-10-20 12:45:30.123+08:00
        assert!(s.contains('.') && (s.contains('+') || s.contains('-')), "{s}");
    }

    /// 测试构建里**默认不落盘**;显式 set_dir 之后才写 —— 两件事必须在同一条用例里验,
    /// 因为"是否静音"是进程级全局状态,拆成两条会互相污染(并行跑时谁先谁后不定)。
    ///
    /// 来由:第一次真机验证时,用户的 `diag.log` 里混进了一串
    /// `C:\...\Temp\ts-wb-expired-*\...` 的测试行 —— 真出问题时要在一堆噪声里找线索。
    #[test]
    fn logging_is_muted_in_tests_until_a_dir_is_set() {
        assert!(muted_for_tests(), "测试构建默认必须静音");

        let dir = tmpdir("armed");
        set_dir(dir.clone());
        assert!(!muted_for_tests(), "显式指定目录后必须能写");

        write("测试", "显式目录这一条应当落盘");
        let text = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap_or_default();
        assert!(text.contains("显式目录这一条应当落盘"), "没落盘:{text:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
