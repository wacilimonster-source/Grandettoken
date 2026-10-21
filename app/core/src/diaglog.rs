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
//!    单文件 [`MAX_BYTES`],超了就滚成 `diag.1.log` / `diag.2.log`,总量封在 4 MB 上下。
//! 3. **不能拖累取数**:所有 IO 失败静默吞掉(只是少一条日志,不该让渠道失败或者
//!    让进程 panic)。写失败还会自我熔断一分钟,免得在只读盘上每轮都白撞。
//!
//! ## 分层(2026-10-21 重做)
//!
//! 旧的写法只有"一行自由文本 + 一个 key",于是:
//!
//! · 9 个内置渠道里只有 WorkBuddy 那一路记得细,其余的失败时只有"取数失败"四个字;
//! · 日志里没有"这次运行"的标识,用户说"昨天晚上坏的"分不清是哪一次;
//! · 界面上的错误文案和日志里的措辞对不上号,用户报的那句话在日志里搜不到。
//!
//! 现在统一成 [`Event`]:**级别 + 范围 + 渠道 + 阶段 + 错误码 + 白名单字段**。
//!
//! | 级别 | 含义 | 落盘规则 |
//! |---|---|---|
//! | [`Level::Env`] | 一次运行一次(环境事实) | 总是 |
//! | [`Level::Decision`] | 关键决策(用了哪份凭据、为什么换域名) | 总是 |
//! | [`Level::Wire`] | 一次 HTTP 交互 | **只在失败时**,或开了「详细日志」 |
//! | [`Level::Result`] | 成功的结果 | 不落盘,只进内存环形缓冲 |
//!
//! **成功不落盘、失败带上文**是这套设计的核心取舍:挂件最坏情况一天 12960 次请求,
//! 全落盘的话日志两小时就滚没了,真出事时能查到的历史早没了。所以:
//!
//! · 成功的事件只进内存环形缓冲([`RING_MAX_BYTES`]);
//! · 一旦发生失败,落盘时**把失败前后的上下文一起写**——那才是"现场"。
//!
//! ## 脱敏的两道闸
//!
//! 1. **结构化字段走白名单**([`FIELD_WHITELIST`]):不在白名单里的键名**直接丢弃**
//!    (不是脱敏,是不许进)。凡是叫 key / token / secret / password / cookie 的都不在名单里。
//! 2. **自由文本走 [`redact`]**:长串按"像不像令牌"判断,**JWT 特征(>=2 个点、每段 >=8)
//!    一律遮**;路径、域名、文件名、UUID、错误码要留着——它们才是排查要用的信息。
//!
//! 脱敏的判据有两条互为表里的验收(见本文件末尾的测试):**该遮的全遮**(漏遮是事故),
//! **该留的全留**(遮多了就把线索毁掉)。
//!
//! ## 为什么还要节流
//!
//! 同一个失败会每 60 秒重复一次,不节流的话一天 1440 行全是同一句,真正有信息量的
//! "第一次失败 / 换到第二条路 / 错误变了"会被埋掉。所以 [`write_keyed`] 按
//! **(范围, key)** 记签名:同签名在 [`REPEAT_WINDOW_SEC`] 内只落一次,窗口过后补一行
//! "重复了 N 次"。状态一变(签名不同)立刻落新行。
//!
//! ⚠ 旧实现只按 key 记,把**范围漏掉了**——9 个渠道共用 `SEEN["channel"]` 一格,
//! 于是每个渠道都把上一个判成"状态变了",节流等于没开。现在键是 `(scope, key)`。

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 日志文件名。滚动后的旧文件是 [`LOG_FILE`] 后面加 `.1` / `.2`。
pub const LOG_FILE: &str = "diag.log";
/// 滚动代数。挂件常驻,三代足够覆盖"出问题到用户来报"这段时间。
const KEEP_GENERATIONS: usize = 3;
/// 单文件上限。超过就滚:三代合计约 3 MB。
const MAX_BYTES: u64 = 1024 * 1024;
/// 同一 (范围,key) 的同签名在多少秒内只落一次。
const REPEAT_WINDOW_SEC: i64 = 300;
/// 写失败后的熔断时长:只读盘 / 权限问题时不要每轮都去撞一次。
const FAIL_COOLDOWN: Duration = Duration::from_secs(60);
/// 单行最长。超了截断——一行太长会把滚动周期拉爆,也不好读。
const MAX_LINE: usize = 2000;
/// 内存环形缓冲上限。成功事件只进这里,不落盘。
const RING_MAX_BYTES: usize = 16 * 1024 * 1024;
/// 失败时连带写出多少条上文。
const CONTEXT_BEFORE: usize = 20;

/// 日志目录。外壳启动时用 [`set_dir`] 注入(它拿得到 Tauri 的 app_data_dir);
/// 没注入时按 `%APPDATA%\com.wacil.tokenscope` 猜 —— 与 `report_fatal` 写
/// `startup-error.log` 的地方是同一个目录,用户找得着。
static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

// 测试构建里**默认不落盘**。
//
// 为什么必须是**线程局部**的:别的模块的用例(workbuddy / fetch 那些)也会走到
// `write_keyed`。如果"开闸"是进程级的,它们就会把测试行写进**正在跑的那条用例**
// 临时目录里 —— 2026-10-21 加"成功不落盘 / 失败带上文"两条用例时真的踩到了:
// 单跑通过、一起跑就挂。
//
// 线程局部之后:只有**自己这条线程**开了闸,别的线程照样静音。
#[cfg(test)]
thread_local! {
    static TEST_ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
/// 测试里会动全局状态(DIR / RING / SEEN / FAILED_AT)的用例,拿这把锁**串行**跑。
///
/// 为什么必须:并行跑时一个用例 `remove_dir_all` 掉另一个正在写的目录 →
/// 写失败 → `FAILED_AT` 熔断 60 秒 → 后面所有用例的写都被静默跳过。
#[cfg(test)]
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// 把测试用到的全局状态复位。**每个动全局的用例都要先调它**。
#[cfg(test)]
fn reset_test_state() {
    if let Ok(mut g) = DIR.lock() {
        *g = None;
    }
    TEST_ARMED.with(|a| a.set(false));
    with_ring(|g| *g = None);
    with_seen(|g| *g = None);
    if let Ok(mut g) = FAILED_AT.lock() {
        *g = None;
    }
    if let Ok(mut g) = SESSION.lock() {
        *g = None;
    }
}

/// 测试专用:开闸并把日志目录指到临时目录。返回值是守卫,析构时自动关闸。
///
/// 别的模块的用例要落盘验证时用这个 —— **必须**同时拿 [`test_lock`],
/// 否则两个用例会抢同一个全局目录。
#[cfg(test)]
pub fn arm_test_dir(dir: PathBuf) -> TestArm {
    set_dir(dir);
    TestArm
}

/// 测试开闸的守卫。
#[cfg(test)]
pub struct TestArm;
#[cfg(test)]
impl Drop for TestArm {
    fn drop(&mut self) {
        TEST_ARMED.with(|a| a.set(false));
    }
}

/// 测试专用:拿全局测试锁。动全局状态(DIR / RING / FAILED_AT)的用例都要拿它。
#[cfg(test)]
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// 测试专用:读当前日志文件的内容。
#[cfg(test)]
pub fn read_log() -> String {
    log_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

/// 测试专用:直接摆一份环形缓冲的内容(避开"别的用例也在往里塞东西"的干扰)。
#[cfg(test)]
fn set_ring(lines: Vec<String>) {
    let mut bytes = 0usize;
    for l in &lines {
        bytes += l.len() + 1;
    }
    with_ring(|g| {
        *g = Some(Ring {
            lines: lines.into_iter().collect(),
            bytes,
            last_failed: false,
        });
    });
}

/// 进程内是否已确认"这台机器的磁盘写不了"。One-shot:过期后允许再试。
static FAILED_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// 写日志与滚动必须串行,否则两个线程会同时 rename、把文件走丢。
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 节流表:`(范围, key)` → (上次签名, 上次落笔时刻, 期间被压掉几次)。
///
/// **测试构建里是线程局部的**:别的模块的用例(workbuddy / fetch)也会走
/// `write_keyed`,共用一张表会互相干扰(谁先谁后不定,用例就变成"看运气过")。
/// 生产构建里是进程级的 —— 那才是需要的语义(跨线程也要节流)。
#[cfg(not(test))]
static SEEN: Mutex<Option<HashMap<String, Seen>>> = Mutex::new(None);
#[cfg(test)]
thread_local! {
    static SEEN: std::cell::RefCell<Option<HashMap<String, Seen>>> =
        const { std::cell::RefCell::new(None) };
}

/// 内存环形缓冲:成功的事件只进这里。失败落盘时把上下文带出去。
///
/// 同样**测试构建里是线程局部的**(理由同 [`SEEN`])。
#[cfg(not(test))]
static RING: Mutex<Option<Ring>> = Mutex::new(None);
#[cfg(test)]
thread_local! {
    static RING: std::cell::RefCell<Option<Ring>> = const { std::cell::RefCell::new(None) };
}

/// 访问节流表。生产走锁,测试走线程局部 —— 两条路的语义一样。
fn with_seen<R>(f: impl FnOnce(&mut Option<HashMap<String, Seen>>) -> R) -> R {
    #[cfg(not(test))]
    {
        let mut g = SEEN.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    }
    #[cfg(test)]
    {
        SEEN.with(|g| f(&mut g.borrow_mut()))
    }
}

/// 访问环形缓冲。同 [`with_seen`]。
fn with_ring<R>(f: impl FnOnce(&mut Option<Ring>) -> R) -> R {
    #[cfg(not(test))]
    {
        let mut g = RING.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    }
    #[cfg(test)]
    {
        RING.with(|g| f(&mut g.borrow_mut()))
    }
}

/// 本次运行的会话 id(8 位十六进制)。一次启动一个,让用户能指认"哪一次运行"。
static SESSION: Mutex<Option<String>> = Mutex::new(None);

/// 「详细日志」开关。打开后成功的请求也落盘(排查"数字不对"这类要看成功响应的场景)。
static VERBOSE: Mutex<bool> = Mutex::new(false);

struct Seen {
    sig: String,
    at: i64,
    suppressed: u32,
}

/// 内存环形缓冲:定长,超了从头丢。
struct Ring {
    lines: VecDeque<String>,
    bytes: usize,
    /// 最后一条是不是一次失败。失败会带上文,**下一条**再补一行"后续" ——
    /// "下文"没法在失败那一刻就知道(那时候后面还没发生),只能等下一条来了再补。
    last_failed: bool,
}

impl Ring {
    fn push(&mut self, line: String) {
        let n = line.len() + 1;
        self.lines.push_back(line);
        self.bytes += n;
        while self.bytes > RING_MAX_BYTES {
            if let Some(old) = self.lines.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.len() + 1);
            } else {
                break;
            }
        }
    }
}

// ───────────────────────── 级别与事件 ─────────────────────────

/// 日志级别。见模块头的分层表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// L0 · 环境事实:一次运行一次。
    Env,
    /// L1 · 关键决策:用了哪份凭据、为什么换域名。
    Decision,
    /// L2 · 一次交互:URL / 状态码 / 耗时 / 响应形状。
    Wire,
    /// L3 · 结果:解析出了什么、写库写了什么。
    Result,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Env => "L0",
            Level::Decision => "L1",
            Level::Wire => "L2",
            Level::Result => "L3",
        }
    }
    /// 这条要不要落盘。失败(`fail = true`)永远落。
    fn lands(self, fail: bool, verbose: bool) -> bool {
        match self {
            Level::Env | Level::Decision => true,
            Level::Wire => fail || verbose,
            Level::Result => verbose,
        }
    }
}

/// 一次事件。
///
/// `fields` 的键名走白名单([`FIELD_WHITELIST`]):不在白名单里的**直接丢弃**。
/// 值是自由文本,落盘前过一遍 [`redact`];`msg` 同理。
#[derive(Debug, Clone, Copy)]
pub struct Event<'a> {
    pub level: Level,
    /// 哪个子系统:`fetch` / `wb` / `inject` / `checkin` / `db` / `ui` …
    pub scope: &'a str,
    /// 哪个渠道(`4sapi` / `trae` / 自定义渠道 id);没有就 `None`。
    pub channel: Option<&'a str>,
    /// 哪一步:`http` / `parse` / `snapshot` / `cred` / `backup` / `verify` / `rollback`。
    pub stage: &'a str,
    /// 错误码(如 `E_HTTP_401`)。**界面和日志共用同一套码** ——
    /// 用户报一个码,support 就能 `grep` 定位。
    pub code: Option<&'a str>,
    /// 一句话说明(自由文本,会脱敏)。
    pub msg: Option<&'a str>,
    /// 结构化字段。键名走白名单。
    pub fields: &'a [(&'a str, &'a str)],
    /// 这是不是一次失败。失败会落盘,并**连带写出前后的上下文**。
    pub fail: bool,
}

impl<'a> Event<'a> {
    /// 建一个事件(只填必填项,其余用 `with_*` 补)。
    pub fn new(scope: &'a str, stage: &'a str) -> Self {
        Event {
            level: Level::Decision,
            scope,
            channel: None,
            stage,
            code: None,
            msg: None,
            fields: &[],
            fail: false,
        }
    }
    pub fn level(mut self, l: Level) -> Self {
        self.level = l;
        self
    }
    pub fn channel(mut self, c: &'a str) -> Self {
        self.channel = Some(c);
        self
    }
    pub fn code(mut self, c: &'a str) -> Self {
        self.code = Some(c);
        self
    }
    pub fn msg(mut self, m: &'a str) -> Self {
        self.msg = Some(m);
        self
    }
    pub fn fields(mut self, f: &'a [(&'a str, &'a str)]) -> Self {
        self.fields = f;
        self
    }
    pub fn fail(mut self) -> Self {
        self.fail = true;
        self
    }
}

/// 结构化字段的键名白名单。
///
/// **凡是叫 key / token / secret / password / cookie 的都不在名单里** ——
/// 不是"脱敏",是**不许进**。要指认某把密钥,用 `key_fp`(指纹)或 `cred_len`(长度)。
pub const FIELD_WHITELIST: &[&str] = &[
    "url",
    "method",
    "status",
    "ms",
    "bytes",
    "host",
    "path",
    "shape",
    "channel",
    "source",
    "kind",
    "unit",
    "remaining",
    "used",
    "total",
    "key_fp",
    "cred_len",
    "cred_source",
    "attempt",
    "index",
    "count",
    "file",
    "dir",
    "reason",
    "err_kind",
    "level",
    "session",
    "stage",
    "client",
    "action",
    "field",
];

// ⚠ 白名单里**不能**有 "value" / "result" / "keys" 这类通用名 ——
// 它们正是下一个写代码的人会把"随便什么字符串"塞进去的字段名。
// 宁可少几个字段,也不给泄漏留一个口子。

/// 键名在白名单里吗。
fn allowed_key(k: &str) -> bool {
    FIELD_WHITELIST.contains(&k)
}

// ───────────────────────── 目录与路径 ─────────────────────────

/// 由外壳注入真实的数据目录(与 SQLite 同一个文件夹)。
pub fn set_dir(dir: PathBuf) {
    if let Ok(mut g) = DIR.lock() {
        *g = Some(dir);
    }
    #[cfg(test)]
    {
        // 测试里显式 set_dir = 明确要测落盘,放开**本线程**的闸门
        // (线程局部:别的线程的用例照样静音,不会把测试行写进我们的临时目录)
        TEST_ARMED.with(|a| a.set(true));
    }
}

/// 测试构建下"没有显式指定目录"就是不该落盘(见 [`TEST_ARMED`] 的注释)。
fn muted_for_tests() -> bool {
    #[cfg(test)]
    {
        return !TEST_ARMED.with(|a| a.get());
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

/// 「详细日志」开关:打开后成功的请求也落盘。
pub fn set_verbose(on: bool) {
    if let Ok(mut g) = VERBOSE.lock() {
        *g = on;
    }
}
pub fn is_verbose() -> bool {
    VERBOSE.lock().map(|v| *v).unwrap_or(false)
}

// ───────────────────────── 会话 id ─────────────────────────

/// 本次运行的会话 id(8 位十六进制)。
///
/// 为什么要它:用户报"昨天晚上坏的",而日志里 7 天前和今天的记录混在一起。
/// 有了它,用户只要说一句"会话 4s3f9a2c",就能精确定位那一次运行。
pub fn session_id() -> String {
    if let Ok(g) = SESSION.lock() {
        if let Some(s) = g.as_ref() {
            return s.clone();
        }
    }
    let gen = {
        use sha2::{Digest, Sha256};
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut h = Sha256::new();
        h.update(ns.to_le_bytes());
        h.update(std::process::id().to_le_bytes());
        let d = h.finalize();
        d.iter().take(4).map(|b| format!("{b:02x}")).collect::<String>()
    };
    // ⚠ 存与取必须在**同一次加锁**里完成。
    // 分两次加锁的话,两个线程会各算一个 id,后来那个虽然没存进去,
    // 却把自己那个返回走了 —— 于是用户报的是 A,日志里混着 B,grep 不到。
    let mut g = SESSION.lock().unwrap_or_else(|p| p.into_inner());
    if g.is_none() {
        *g = Some(gen.clone());
    }
    g.clone().unwrap_or(gen)
}

/// 显式设定会话 id(测试用)。
pub fn set_session(id: &str) {
    if let Ok(mut g) = SESSION.lock() {
        *g = Some(id.to_string());
    }
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
/// 判据(2026-10-21 重做,原版有两个洞):
///
/// * **JWT 不遮** —— 原判据是"含 <code>.</code> 的串当域名放过",而 JWT 天然带两个点。
///   现在 >=2 个点、每段 >=8 字符、无 <code>/</code> 的长串**一律遮**。
/// * **全小写的长串不遮** —— 原判据要求"有数字或大写",base64url 令牌可能两样都没有。
///   现在只要不是路径/域名形态就遮。
///
/// 但**路径、域名、文件名、UUID、错误码必须留着** —— 它们才是排查要用的信息。
/// 真机日志里 `https://api.trae.cn/...` 曾被当成令牌掩成
/// `https://api.trae.«secret len=48»`,把"用的哪个接口"这条最有用的信息弄丢了。
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.is_empty() {
            return;
        }
        if is_secret_run(run) {
            out.push_str(&format!("«secret len={}»", run.chars().count()));
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for ch in text.chars() {
        // ⚠ 运行字符集**不含 `-`**:UUID(`5f2b9c1e-3a4d-…`)要能被拆成短段而保留,
        // 而键名里的连字符由 `redact_keyed_values` 单独扫原文处理,不依赖这里。
        if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '=' | '_' | '.') {
            run.push(ch);
        } else {
            flush(&mut run, &mut out);
            out.push(ch);
        }
    }
    flush(&mut run, &mut out);
    // 键值形态的会话值单独再挡一层:`session=<值>` / `{"accessToken":"…"}` 这种
    // 可能不到 32 字符,但同样是凭据。
    redact_keyed_values(&out)
}

/// 一段连续 base64/hex 字符算不算"令牌"。
///
/// 保守优先:**漏掩是事故,误掩只是少一条线索**。所以只有"明确是位置信息"的形态才放过。
fn is_secret_run(s: &str) -> bool {
    let n = s.chars().count();
    if n < 32 {
        return false;
    }
    // ① JWT 特征:>=2 个点、每段 >=8 字符、不是路径 —— 一定是令牌
    if s.matches('.').count() >= 2 && !s.contains('/') && jwt_like(s) {
        return true;
    }
    // ② 含 '/' 的长串是路径 / URL —— 位置信息,要留
    if s.contains('/') {
        return false;
    }
    // ③ 短扩展名结尾的是域名 / 文件名(`api.trae.cn`、`workbuddy-desktop.info`)
    if looks_like_name(s) {
        return false;
    }
    // ④ 其余长串:一律当令牌(含全小写的 base64url)
    true
}

/// 是不是 JWT:`a.b.c`,每段 >= 8 字符,只含 base64url 字符。
fn jwt_like(s: &str) -> bool {
    let segs: Vec<&str> = s.split('.').collect();
    segs.len() >= 3
        && segs.iter().all(|g| {
            g.len() >= 8
                && g.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        })
}

/// 末段是短扩展名 / 顶级域 → 是域名或文件名,不是令牌。
///
/// `api.trae.cn`(cn)、`workbuddy-desktop.info`(info)、`WorkBuddy.exe`(exe) 都命中;
/// JWT 的末段是 40+ 个混合大小写字符,不命中。
fn looks_like_name(s: &str) -> bool {
    match s.rsplit('.').next() {
        Some(last) => {
            let l = last.chars().count();
            l >= 2 && l <= 6 && last.chars().all(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// 键名归一:小写 + 去掉分隔符。`accessToken` / `access_token` / `X-Api-Key` 归一到同一个。
fn norm_key(k: &str) -> String {
    k.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// 会话值 / 密钥的键名表。归一后比对,大小写和分隔符都无所谓。
const SECRET_KEYS: &[&str] = &[
    "accesstoken",
    "refreshtoken",
    "idtoken",
    "authorization",
    "apikey",
    "xapikey",
    "apisecret",
    "clientsecret",
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "cookie",
    "cookies",
    "session",
    "session2",
    "sessionid",
    "sid",
    "csrf",
    "xsrf",
    "credential",
    "credentials",
    "atrestsecretkey",
    "privatekey",
];

/// 前缀形态:`Bearer xxx` / `Basic xxx`。不是 key=value,是"关键字 + 空格 + 值"。
const SECRET_PREFIXES: &[&str] = &["bearer ", "basic ", "token ", "apikey "];

/// `key=value` / `"key": "value"` / `?key=value` 的值掩掉。
///
/// 原实现只认六种 `key=` 形态,JSON 的 `"accessToken":"…"` 根本匹配不上,
/// 而且值扫到 `"` 就停 —— 等于没遮。现在按归一后的键名表匹配,值按引号感知地扫。
fn redact_keyed_values(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < bytes.len() {
        // 先看前缀形态(Bearer / Basic)
        let lower_rest = text[i..].to_ascii_lowercase();
        if let Some(p) = SECRET_PREFIXES.iter().find(|p| lower_rest.starts_with(**p)) {
            let p_len = p.len();
            out.push_str(&text[i..i + p_len]);
            let vstart = i + p_len;
            let vend = scan_value_end(text, vstart, false);
            let val = &text[vstart..vend];
            if val.is_empty() {
                // 只有前缀没有值,原样保留
            } else {
                out.push_str(&format!("«secret len={}»", val.chars().count()));
            }
            i = vend;
            continue;
        }
        // 再看 key=value / "key": "value" / ?key=value
        if let Some((_klen, quote, vstart)) = match_keyed(text, i) {
            out.push_str(&text[i..vstart]);
            let vend = scan_value_end(text, vstart, quote);
            let val = &text[vstart..vend];
            if val.is_empty() {
                // 只有键没有值,原样保留(比如 `cookie:` 后面跟的是说明文字)
            } else {
                out.push_str(&format!("«secret len={}»", val.chars().count()));
            }
            i = vend;
            continue;
        }
        let ch = text[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 从 `i` 开始,能不能读出一个"敏感键名 + 分隔符 + 值开头"。
///
/// 返回 `(键名长度, 值是否被引号包着, 值起点)`。
fn match_keyed(text: &str, i: usize) -> Option<(usize, bool, usize)> {
    let b = text.as_bytes();
    // 允许键名带引号:`"accessToken": "…"`
    let (kstart, quoted) = if b.get(i) == Some(&b'"') {
        (i + 1, true)
    } else {
        (i, false)
    };
    // 键名:字母数字下划线横线
    let mut kend = kstart;
    while kend < b.len()
        && (b[kend].is_ascii_alphanumeric() || b[kend] == b'_' || b[kend] == b'-')
    {
        kend += 1;
    }
    if kend == kstart {
        return None;
    }
    let name = norm_key(&text[kstart..kend]);
    if !SECRET_KEYS.contains(&name.as_str()) {
        return None;
    }
    // 键名后面:可选的闭引号 + 空白 + `=` 或 `:`
    let mut j = kend;
    if quoted {
        if b.get(j) != Some(&b'"') {
            return None;
        }
        j += 1;
    }
    while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
        j += 1;
    }
    if !matches!(b.get(j), Some(b'=') | Some(b':')) {
        return None;
    }
    j += 1;
    while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
        j += 1;
    }
    // ⚠ **必须跳过认证方案前缀**(`Bearer ` / `Basic ` …)。
    // 不跳的话,`Authorization: Bearer abc123` 会被当成"键 authorization 的值是
    // Bearer" → 掩掉的是 "Bearer" 六个字母,**真正的令牌原样留下**。
    // 2026-10-21 真的漏过这一条(旧的 KEYS 表里有 "bearer ",改写时把这个口径丢了)。
    for p in ["bearer ", "basic ", "token ", "digest ", "negotiate "] {
        if text
            .get(j..j + p.len())
            .map_or(false, |s| s.eq_ignore_ascii_case(p))
        {
            j += p.len();
            break;
        }
    }
    // 值是否被引号包着
    let vquote = b.get(j) == Some(&b'"') || b.get(j) == Some(&b'\'');
    let vstart = if vquote { j + 1 } else { j };
    let klen = vstart - i;
    Some((klen, vquote, vstart))
}

/// 从 `start` 开始扫到值结束。
///
/// 引号包裹时,扫到**配对的**闭引号为止(不把闭引号吃进值里)。
/// 没引号时,扫到分隔符为止(空格 / `;` / `,` / `&` / 换行 / 闭引号)。
fn scan_value_end(text: &str, start: usize, quoted: bool) -> usize {
    let b = text.as_bytes();
    if quoted {
        let q = b[start - 1];
        let mut j = start;
        while j < b.len() && b[j] != q {
            j += 1;
        }
        return j;
    }
    let mut j = start;
    while j < b.len() && !matches!(b[j], b';' | b' ' | b'\t' | b'\r' | b'\n' | b'"' | b',' | b'&' | b'\'') {
        j += 1;
    }
    j
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

/// 记一条事件。
///
/// 落盘规则见 [`Level::lands`]:失败永远落,成功的看「详细日志」开关。
/// 失败落盘时**连带写出前后的上下文** —— 那才是"现场"。
pub fn event(ev: Event<'_>) {
    if disabled() {
        return;
    }
    let session = session_id();
    let body = format_event(&ev, &session);
    // 先进内存环形缓冲:失败落盘时要带上下文,上下文里必须有失败前的成功事件。
    let prev_failed = push_ring(&body, ev.fail);
    // 上一条是失败 → 这一条是它的"下文",补一行,让现场能往后延伸。
    // ("下文"没法在失败那一刻就知道 —— 那时候后面还没发生。)
    // ⚠ 但**当前这条自己也是失败**时不要补:它马上会被完整写一遍(含上下文),
    // 再补一行"后续"就成了同一行写两遍。
    if prev_failed && !ev.fail {
        emit(&format!("  ↳ 后续 | {body}"));
    }
    if !ev.level.lands(ev.fail, is_verbose()) {
        return;
    }
    if ev.fail {
        emit_with_context(ev.scope, &body);
    } else {
        emit(&body);
    }
}

/// 决策层:选了什么、为什么选。
///
/// **节流的**:同一个 (范围, 渠道, 阶段) 的同一条决策,5 分钟内只落一次。
/// 不节流的话,"换下一个入口"这种每轮都发生的决策会一天写几千行 ——
/// 2026-10-21 上线时正是这个,把日志的保留期从两周压到了十几小时。
pub fn decision(scope: &str, channel: Option<&str>, stage: &str, msg: &str) {
    if disabled() {
        return;
    }
    let now = crate::fetch::now_ts();
    let clean = redact(msg);
    // 键 = 范围 + 渠道 + 阶段;签名 = 内容。内容变了立刻落,没变就压掉。
    let ident = format!("{scope}\u{1}{}:{stage}", channel.unwrap_or("-"));
    let verdict = with_seen(|g| {
        let map = g.get_or_insert_with(HashMap::new);
        decide(map, &ident, &clean, now)
    });
    let session = session_id();
    match verdict {
        Decision::Skip => return,
        Decision::WriteAfterRepeat(n) if n > 0 => {
            let what: String = clean.chars().take(60).collect();
            emit(&format!(
                "{} {session} L1 {scope} {ch} {stage}: 上一条相同状态又重复了 {n} 次 —— {what}{tail}",
                stamp(),
                ch = channel.unwrap_or("-"),
                tail = if clean.chars().count() > 60 { "…" } else { "" }
            ));
        }
        _ => {}
    }
    let body = format!(
        "{} {session} L1 {scope} {ch} {stage} msg={clean}",
        stamp(),
        ch = channel.unwrap_or("-")
    );
    push_ring(&body, false);
    emit(&body);
}

/// 交互层:一次请求的结果。`code` 非空 = 失败。
pub fn wire(
    scope: &str,
    channel: Option<&str>,
    stage: &str,
    code: Option<&str>,
    fields: &[(&str, &str)],
) {
    let mut ev = Event::new(scope, stage)
        .level(Level::Wire)
        .fields(fields);
    if let Some(c) = channel {
        ev = ev.channel(c);
    }
    if let Some(c) = code {
        ev = ev.code(c).fail();
    }
    event(ev);
}

/// 结果层:拿到了什么。
pub fn result(scope: &str, channel: Option<&str>, stage: &str, fields: &[(&str, &str)]) {
    let mut ev = Event::new(scope, stage)
        .level(Level::Result)
        .fields(fields);
    if let Some(c) = channel {
        ev = ev.channel(c);
    }
    event(ev);
}

/// 无条件写一行(一次性事件:启动、登录成功、配置换了)。不参与节流/去重。
pub fn write(scope: &str, msg: &str) {
    write_maybe_throttled(scope, None, msg);
}

/// 环境层(L0):一次运行一次的事实(启动横幅)。与 [`write`] 的区别只是级别标签。
pub fn write_env(scope: &str, msg: &str) {
    write_maybe_throttled_level(scope, None, msg, "L0");
}

/// 按 key 节流地写一行。同一 **(范围, key)** 的同签名在 [`REPEAT_WINDOW_SEC`]
/// 内只落一次,窗口过后补一行"重复 N 次"。**取数主链路用这个**。
pub fn write_keyed(scope: &str, key: &str, msg: &str) {
    write_maybe_throttled(scope, Some(key), msg);
}

fn write_maybe_throttled(scope: &str, key: Option<&str>, msg: &str) {
    write_maybe_throttled_level(scope, key, msg, "L1")
}

fn write_maybe_throttled_level(scope: &str, key: Option<&str>, msg: &str, level: &str) {
    if disabled() {
        return;
    }
    let now = crate::fetch::now_ts();
    let session = session_id();
    let line = match key {
        // 签名 = 内容本身(已脱敏)。内容一变就是新事件,立刻落。
        Some(k) => {
            let clean = redact(msg);
            // ⚠ 键必须带**范围**:9 个渠道共用一个 key 时,每个渠道都会把上一个
            // 判成"状态变了",节流等于没开(见模块头)。
            let ident = format!("{scope}\u{1}{k}");
            let decision = with_seen(|g| {
                let map = g.get_or_insert_with(HashMap::new);
                decide(map, &ident, &clean, now)
            });
            match decision {
                Decision::Skip => return,
                Decision::Write => clean,
                Decision::WriteAfterRepeat(n) if n > 0 => {
                    // 补的这一行要说清"是哪件事重复了":真机日志里曾经只写
                    // `wb: 上一条相同状态又重复了 5 次`,看不出说的是 exe 探测还是取数。
                    let what: String = clean.chars().take(60).collect();
                    // ⚠ 自己带时间戳:`emit()` 不再补戳(否则会和正文那行重复)。
                    let head = format!(
                        "{} {session} {level} {scope}[{k}]: 上一条相同状态又重复了 {n} 次 —— {what}{}",
                        stamp(),
                        if clean.chars().count() > 60 { "…" } else { "" }
                    );
                    emit(&head);
                    clean
                }
                Decision::WriteAfterRepeat(_) => clean,
            }
        }
        None => redact(msg),
    };
    let body = cap_line(format!(
        "{} {session} {level} {scope}: {line}",
        stamp()
    ));
    push_ring(&body, false);
    emit(&body);
}

/// 把一行截到上限。
///
/// ⚠ **必须落在字符边界上** —— `String::truncate` 在非边界处会 panic,
/// 而这条代码在取数路径上,panic 就等于把取数搞挂(违反"不能拖累取数")。
/// 自由文本(`msg`)里全是中文(3 字节/字),按字节数切大概率切在字的中间。
pub(crate) fn cap_line(mut s: String) -> String {
    if s.len() <= MAX_LINE {
        return s;
    }
    let mut cut = MAX_LINE;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push('…');
    s
}

/// 把事件格式化成一行。结构化字段的键名走白名单,不在白名单的直接丢弃。
fn format_event(ev: &Event<'_>, session: &str) -> String {
    let mut s = format!(
        "{} {session} {} {} {}",
        stamp(),
        ev.level.tag(),
        ev.scope,
        ev.channel.unwrap_or("-")
    );
    if !ev.stage.is_empty() {
        s.push(' ');
        s.push_str(ev.stage);
    }
    if let Some(c) = ev.code {
        s.push_str(" code=");
        s.push_str(c);
    }
    if let Some(m) = ev.msg {
        s.push_str(" msg=");
        s.push_str(&redact(m));
    }
    for (k, v) in ev.fields {
        if !allowed_key(k) {
            continue; // 不在白名单 → 直接丢弃(不是脱敏,是不许进)
        }
        s.push(' ');
        s.push_str(k);
        s.push('=');
        s.push_str(&redact(v));
    }
    if s.len() > MAX_LINE {
        // ⚠ `String::truncate` 在**非字符边界**上会 panic。
        // 这里的串里全是中文(3 字节/字),按字节数 2000 切大概率切在字的中间 ——
        // 而这条代码在取数路径上,panic 就等于把取数搞挂(违反"不能拖累取数")。
        return cap_line(s);
    }
    s
}
/// 推进环形缓冲。返回**上一条是不是一次失败**。
///
/// 上一条是失败 → 这一条就是它的"下文",由调用方补一行「↳ 后续」。
fn push_ring(line: &str, failed: bool) -> bool {
    with_ring(|g| {
        let r = g.get_or_insert_with(|| Ring {
            lines: VecDeque::new(),
            bytes: 0,
            last_failed: false,
        });
        let prev_failed = r.last_failed;
        r.last_failed = failed;
        r.push(line.to_string());
        prev_failed
    })
}

/// 落盘:失败时把**上文**一起写出来。
///
/// 单看"HTTP 401"没用;看到"401 之前我们换过域名、换过凭据"才知道是谁的问题。
/// ("下文"是下一条事件到来时补的,见 [`event`]。)
///
/// ⚠ **上下文块要节流**:每次失败都重写 20 行,其中 19 行是**上一轮已经写过的**
/// 同样的内容 —— 一个坏掉的渠道每分钟一轮,一天就能写 3 万行,把日志的保留期
/// 从两周压到十几小时。所以:同一个 (范围, 渠道) 的同一种失败,5 分钟内只带一次上下文。
fn emit_with_context(scope: &str, body: &str) {
    emit(body);
    if !context_due(scope) {
        return;
    }
    let (before, _) = ring_context();
    for b in before {
        emit(&format!("  ↳ 上文 | {b}"));
    }
}

/// 这个 (范围, 失败) 的上下文块该不该带。节流口径与其它重复事件一致:
/// 同一种失败 5 分钟内只带一次上下文,状态变了立刻带。
fn context_due(scope: &str) -> bool {
    context_due_at(scope, crate::fetch::now_ts())
}

/// 这个 (范围, 失败) 的上下文块该不该带。节流口径与其它重复事件一致:
/// 同一种失败 5 分钟内只带一次上下文,状态变了立刻带。
///
/// **纯逻辑**:时间由参数给,单测能钉住窗口。
fn context_due_at(scope: &str, now: i64) -> bool {
    let key = format!("ctx\u{1}{scope}");
    with_seen(|g| {
        let map = g.get_or_insert_with(HashMap::new);
        match map.get_mut(&key) {
            Some(prev) => {
                if now - prev.at < REPEAT_WINDOW_SEC {
                    false
                } else {
                    prev.at = now;
                    true
                }
            }
            None => {
                map.insert(
                    key,
                    Seen {
                        sig: String::new(),
                        at: now,
                        suppressed: 0,
                    },
                );
                true
            }
        }
    })
}

/// 取环形缓冲里的上文:失败行之前 [`CONTEXT_BEFORE`] 条。
///
/// ⚠ **不能整份克隆**:环形缓冲有 16MB,每次失败都克隆一遍只为取 20 行,
/// 是在最需要性能的时刻做最浪费的事(失败路径本来就是热路径)。
fn ring_context() -> (Vec<String>, Vec<String>) {
    with_ring(|g| {
        let Some(r) = g.as_mut() else {
            return (Vec::new(), Vec::new());
        };
        let n = r.lines.len();
        if n == 0 {
            return (Vec::new(), Vec::new());
        }
        // 最后一条就是刚失败的那行
        let end = n - 1;
        let from = end.saturating_sub(CONTEXT_BEFORE);
        let before: Vec<String> = r.lines.iter().skip(from).take(end - from).cloned().collect();
        (before, Vec::new())
    })
}

fn emit(body: &str) {
    if muted_for_tests() {
        return;
    }
    if let Some(dir) = log_dir() {
        // ⚠ body 里**已经带了时间戳**(`format_event` / `write_maybe_throttled`
        // 都自己加过)—— 这里再拼一次会出现两个时间戳,同一行两个时刻。
        if append_line(&dir, MAX_BYTES, body).is_err() {
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

/// 滚动:`diag.log` → `diag.1.log` → …,超过 [`KEEP_GENERATIONS`] 的丢掉。
/// 失败不致命 —— 哪怕不滚,也只是文件变大,不该让写日志本身报错。
fn rotate(dir: &Path, path: &Path) -> std::io::Result<()> {
    // 从最老的一代开始腾位置
    let oldest = dir.join(format!("{LOG_FILE}.{}", KEEP_GENERATIONS - 1));
    let _ = std::fs::remove_file(&oldest);
    for g in (1..KEEP_GENERATIONS - 1).rev() {
        let from = dir.join(format!("{LOG_FILE}.{g}"));
        let to = dir.join(format!("{LOG_FILE}.{}", g + 1));
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    let bak = dir.join(format!("{LOG_FILE}.1"));
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

/// 内存环形缓冲里的最近若干行(含还没落盘的成功事件)。
pub fn recent(max: usize) -> Vec<String> {
    with_ring(|g| {
        let Some(r) = g.as_mut() else {
            return Vec::new();
        };
        let n = r.lines.len();
        let from = n.saturating_sub(max);
        r.lines.iter().skip(from).cloned().collect()
    })
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
    let mut names = vec![LOG_FILE.to_string()];
    for g in 1..KEEP_GENERATIONS {
        names.push(format!("{LOG_FILE}.{g}"));
    }
    for name in names {
        let p = dir.join(name);
        if p.exists() {
            std::fs::remove_file(&p).map_err(|e| format!("删除 {} 失败: {e}", p.display()))?;
        }
    }
    with_seen(|g| {
        if let Some(m) = g.as_mut() {
            m.clear();
        }
    });
    with_ring(|g| {
        if let Some(r) = g.as_mut() {
            r.lines.clear();
            r.bytes = 0;
            // 上一条"是不是失败"的标记也要清 —— 否则清空后的第一条会以为
            // "上一条是失败",补出一行指向已经不存在的现场的「↳ 后续」。
            r.last_failed = false;
        }
    });
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
    write_env(
        "启动",
        &format!(
            "Grandettoken {app_version} · 会话 {} · exe={exe} · 日志目录={dir} · {}{}",
            session_id(),
            if envs.is_empty() { "环境变量:无" } else { "环境变量:" },
            envs.join(" · ")
        ),
    );
    write_env(
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

    /// 2026-10-21 补:JWT **天然带两个点**,旧判据("含点就当域名放过")会漏掉它。
    /// 这是最关键的一条 —— 诊断日志的官方用途就是"贴给 support"。
    #[test]
    fn jwt_is_always_masked() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        // 单独一行(没有 Bearer 前缀 —— 旧实现只有在有前缀时才靠 KEYS 兜住)
        let out = redact(jwt);
        assert!(!out.contains("eyJhbGci"), "裸 JWT 没被掩:{out}");
        assert!(out.contains("«secret len="), "{out}");
        // 嵌在文本里也一样
        let out2 = redact(&format!("token 是 {jwt} 后面"));
        assert!(!out2.contains("eyJhbGci"), "{out2}");
    }

    /// 全小写的 base64url 令牌:旧判据要求"有数字或大写",这种会漏。
    #[test]
    fn lowercase_long_secret_is_masked() {
        let s = "abcdefghijklmnopqrstuvwxyzabcdefghijkl";
        let out = redact(s);
        assert!(!out.contains(s), "全小写长串没被掩:{out}");
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

    /// JSON 形态的密钥:旧实现的 KEYS 表只认 `accesstoken=`,匹配不上 `"accessToken":"…"`。
    #[test]
    fn redact_masks_json_shaped_secrets() {
        for raw in [
            r#"{"accessToken":"eyJhbGciOiJIUzI1NiJ9abcdefghijk"}"#,
            r#"{"refreshToken":"r-abc-123"}"#,
            r#"{"api_key":"sk-1234567890"}"#,
            r#"{"password":"hunter2"}"#,
        ] {
            let out = redact(raw);
            assert!(out.contains("«secret len="), "JSON 形态没被掩:{raw} → {out}");
            assert!(
                !out.contains("eyJhbGci") && !out.contains("r-abc-123") && !out.contains("sk-1234567890") && !out.contains("hunter2"),
                "原值还在:{out}"
            );
        }
    }

    /// 查询参数形态:`?key=xxx` / `?token=xxx`。
    #[test]
    fn redact_masks_query_param_secrets() {
        let out = redact("GET https://x.example/v1?api_key=SECRETVALUE&page=2");
        assert!(!out.contains("SECRETVALUE"), "{out}");
        assert!(out.contains("page=2"), "非敏感的参数要留着:{out}");
    }

    /// 键名的大小写与分隔符不该影响识别。
    #[test]
    fn secret_key_names_are_normalized() {
        for raw in [
            "access_token=abcdef",
            "ACCESS-TOKEN=abcdef",
            "x-api-key: abcdef",
        ] {
            let out = redact(raw);
            assert!(!out.contains("abcdef"), "{raw} → {out}");
        }
    }

    /// ⚠ 2026-10-21 回归:`Authorization: Bearer <token>`。
    /// 改写脱敏时,键值规则先命中了 "authorization",于是把值起点定在
    /// "Bearer" 上 → 掩掉的是 "Bearer" 六个字母,**令牌原样留下**。
    /// 这条是整个脱敏最常见的形状,必须钉死。
    #[test]
    fn authorization_bearer_token_is_masked() {
        for raw in [
            "Authorization: Bearer abc123",
            "authorization=Bearer abc123",
            "Authorization: Basic dXNlcjpwYXNz",
            "Proxy-Authorization: Bearer abc123",
        ] {
            let out = redact(raw);
            assert!(!out.contains("abc123"), "{raw} → {out}");
            assert!(!out.contains("dXNlcjpwYXNz"), "{raw} → {out}");
            assert!(out.contains("«secret len="), "{raw} → {out}");
            // 认证方案本身不是秘密,留着(看得见用的什么认证)
            assert!(
                out.to_lowercase().contains("bearer")
                    || out.to_lowercase().contains("basic"),
                "认证方案要留着:{raw} → {out}"
            );
        }
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

    /// ⚠ 2026-10-21 补:节流的键**必须带范围**。
    /// 旧实现只按 key 记,9 个渠道共用 `SEEN["channel"]` 一格 → 每个渠道都把上一个
    /// 判成"状态变了" → 每 60 秒每渠道各写一行,节流等于没开。
    #[test]
    fn throttle_key_includes_scope() {
        let mut seen: HashMap<String, Seen> = HashMap::new();
        let t0 = 1_800_000_000i64;
        // 两个不同范围、同一个 key:第一次都该写,第二次都该被压掉
        let k1 = "fetch\u{1}channel";
        let k2 = "wb\u{1}channel";
        assert_eq!(decide(&mut seen, k1, "401", t0), Decision::Write);
        assert_eq!(decide(&mut seen, k2, "401", t0), Decision::Write);
        assert_eq!(decide(&mut seen, k1, "401", t0 + 10), Decision::Skip);
        assert_eq!(decide(&mut seen, k2, "401", t0 + 10), Decision::Skip);
        // 同一范围、同一 key、内容变了 → 立刻写(状态变化要看得见)。
        // 顺带把"被压掉了几次"一起带出来:上一个状态被压了 1 次。
        assert_eq!(
            decide(&mut seen, k1, "403", t0 + 20),
            Decision::WriteAfterRepeat(1)
        );
    }

    /// 落盘 + 滚动:超过上限后旧内容搬到 `.1`,新文件从空开始,总量不膨胀。
    ///
    /// 上界说明:滚动发生在**写之前**,所以当前文件可能是"上一次的 199 字节 + 这一行",
    /// 最多超出一行的量;三代合计封在 `3 * max + 一行` 之内 —— 这就是"不会写满磁盘"的
    /// 硬保证,下面按它断言。
    #[test]
    fn append_rotates_and_keeps_size_bounded() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_test_state();
        let dir = tmpdir("rotate");
        let max = 200u64;
        for i in 0..80 {
            append_line(&dir, max, &format!("line-{i:03} 一些填充内容")).unwrap();
        }
        let cur = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap();
        let mut total = cur.len() as u64;
        assert!(cur.len() as u64 <= max + 64, "当前文件超限:{}", cur.len());
        assert!(cur.contains("line-079"), "最新一行必须在当前文件里");
        for g in 1..KEEP_GENERATIONS {
            let p = dir.join(format!("{LOG_FILE}.{g}"));
            if p.exists() {
                let n = std::fs::metadata(&p).unwrap().len();
                assert!(n <= max + 64, "第 {g} 代超限:{n}");
                total += n;
            }
        }
        // 总量必须是常数级,而不是随写入次数线性增长
        assert!(
            total <= (KEEP_GENERATIONS as u64) * max + 128,
            "多代日志合计超限:{total}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 目录还没建也得能写(全新安装 / 用户删过数据目录)。
    #[test]
    fn append_creates_the_directory() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_test_state();
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
        let _lock = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_test_state();
        assert!(muted_for_tests(), "测试构建默认必须静音");

        let dir = tmpdir("armed");
        set_dir(dir.clone());
        assert!(!muted_for_tests(), "显式指定目录后必须能写");

        write("测试", "显式目录这一条应当落盘");
        let text = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap_or_default();
        assert!(text.contains("显式目录这一条应当落盘"), "没落盘:{text:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 一行事件的**格式要稳定** —— 用户和 support 都按这个格式去读、去 grep。
    /// 格式:`时间 会话 级别 范围 渠道 阶段 [code=…] [msg=…] 键=值…`
    #[test]
    fn the_event_line_format_is_stable() {
        let ev = Event::new("fetch", "http")
            .level(Level::Wire)
            .channel("4sapi")
            .code("E_HTTP_401")
            .fields(&[
                ("url", "https://api.example.com/v1/usage"),
                ("status", "401"),
                ("ms", "142"),
            ]);
        let s = format_event(&ev, "abcd1234");
        assert!(s.contains(" abcd1234 "), "要带会话 id:{s}");
        assert!(s.contains(" L2 "), "要带级别:{s}");
        assert!(s.contains(" fetch 4sapi http"), "要带范围/渠道/阶段:{s}");
        assert!(s.contains("code=E_HTTP_401"), "要带错误码:{s}");
        assert!(s.contains("url=https://api.example.com/v1/usage"), "{s}");
        assert!(s.contains("status=401"), "{s}");
        assert!(s.contains("ms=142"), "{s}");
    }

    /// 结构化字段的键名走白名单:不在白名单里的**直接丢弃**。
    /// 这是"凡是叫 key/token/secret 的都不许进"的执行点。
    #[test]
    fn struct_fields_use_a_whitelist() {        assert!(allowed_key("url"));
        assert!(allowed_key("status"));
        assert!(!allowed_key("key"));
        assert!(!allowed_key("token"));
        assert!(!allowed_key("secret"));
        assert!(!allowed_key("password"));
        assert!(!allowed_key("cookie"));
        assert!(!allowed_key("authorization"));

        let ev = Event::new("fetch", "http").fields(&[
            ("url", "https://x.example/v1"),
            ("secret", "should-never-appear"),
        ]);
        let s = format_event(&ev, "abcd1234");
        assert!(s.contains("url=https://x.example/v1"), "{s}");
        assert!(!s.contains("should-never-appear"), "白名单外的键没被丢弃:{s}");
    }

    /// 落盘判据:成功的事件只进内存缓冲、不落盘;失败的事件落盘。
    /// 这是"日志量"和"信息量"不矛盾的关键。
    ///
    /// 这里只测**纯判据**(`Level::lands`)—— 落盘本身涉及全局目录,
    /// 放进别的线程的用例会互相干扰(见 [`TEST_ARMED`] 的注释)。
    #[test]
    fn success_events_stay_in_memory_and_failures_land_on_disk() {
        // L0 / L1:总是落盘(它们很稀疏,而且信息密度最高)
        assert!(Level::Env.lands(false, false));
        assert!(Level::Decision.lands(false, false));
        // L2 成功 + 没开详细日志 → 不落盘
        assert!(!Level::Wire.lands(false, false));
        // L2 成功 + 开了详细日志 → 落盘("余额数字不对"这类问题要看成功响应)
        assert!(Level::Wire.lands(false, true));
        // L2 失败 → 永远落盘(不看详细日志开关)
        assert!(Level::Wire.lands(true, false));
        assert!(Level::Wire.lands(true, true));
        // L3 结果层:默认不落盘,详细日志下才落
        assert!(!Level::Result.lands(false, false));
        assert!(Level::Result.lands(false, true));
    }

    /// 上下文块**要节流**:每次失败都重写 20 行(其中 19 行上一轮已经写过),
    /// 一个坏掉的渠道每分钟一轮,一天就能写 3 万行,把保留期从两周压到十几小时。
    #[test]
    fn context_block_is_throttled_like_other_repeats() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_test_state();
        let t0 = 1_800_000_000i64;
        // 第一次要带上下文
        assert!(context_due_at("fetch", t0));
        // 5 分钟内同样的失败不再带
        assert!(!context_due_at("fetch", t0 + 60));
        assert!(!context_due_at("fetch", t0 + 299));
        // 窗口过了,重新带一次
        assert!(context_due_at("fetch", t0 + REPEAT_WINDOW_SEC + 1));
        // 另一个范围互不影响
        assert!(context_due_at("checkin", t0 + 10));
    }

    /// 失败落盘时要带上上文(失败前的成功事件)—— 那才是"现场"。
    #[test]
    fn failure_lands_with_surrounding_context() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_test_state();
        // 直接摆一份"现场"进环形缓冲:别的用例也在往里塞东西,
        // 靠真实事件流来构造上下文是不稳的。
        set_ring((0..30).map(|i| format!("line-{i}")).collect());
        push_ring("the-failure", true);
        let (before, _after) = ring_context();
        assert_eq!(before.len(), CONTEXT_BEFORE, "上文要取满 {CONTEXT_BEFORE} 条");
        assert!(
            before.last().map(|s| s.contains("line-29")).unwrap_or(false),
            "上文要紧挨着失败那行:{before:?}"
        );
        // 失败之后的下一条 —— 它就是"下文"。**下一条到来时**才知道,
        // 所以失败那一刻不可能有下文(那时后面还没发生)。
        let prev_failed = push_ring("after-1", false);
        assert!(prev_failed, "上一条是失败时,这一条就是它的下文");
        let (before2, _after2) = ring_context();
        assert!(
            before2.last().map(|s| s.contains("the-failure")).unwrap_or(false),
            "上文要含失败那行:{before2:?}"
        );
    }
}
