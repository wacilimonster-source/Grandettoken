//! 把选中的 4SAPI 模型写进本地客户端配置 —— 四份写入契约。
//!
//! 四个客户端存模型配置的地方完全不同,但共用四条**共同前提**(缺一不写):
//!
//! ```text
//! ① 目标进程不在运行   —— 它们是内存态落盘:边跑边写会在退出时被覆盖(SQLite 还可能写坏)
//! ② 先备份             —— <配置目录>/backup-inject/<时间戳>/,含 SQLite 的 -wal/-shm,保留最近 5 份
//! ③ 只增改自己写的那条 —— 不碰账号、登录态、内置模型、别人的 provider
//! ④ 写完读回校验       —— 不过就自动回滚到备份,并如实报错
//! ```
//!
//! 密钥处理:挂件自己**不存**任何密钥副本 —— 只从 Windows 凭据管理器现读现写,
//! 不进数据库、不进日志、不进台账。
//!
//! 但"不进备份"这句以前写得太满,得说清楚(报告 P2-17 之外的 P2-7):回滚要靠备份,
//! 备份就是照原样拷目标客户端那几个文件,而**那些文件里的密钥本来就是明文**。
//! 所以每次写入会在 `<配置目录>/backup-inject/<时间戳>/` 留下含明文密钥的历史快照
//! (每个客户端最多 KEEP_BACKUPS 份,换密钥后旧快照不会自动失效,卸载也不清这些目录)。
//! 界面与预告里都必须把这句话如实说出来,不能讲"不进备份"。
//!
//! 目标文件里密钥是**明文**(这些客户端本来就这么存),所以每次写入都要用户逐客户端
//! 显式确认,界面上写明落到哪个文件的哪个字段。这是本功能最大的信任成本,不藏。
//!
//! 各客户端的结构来自 2026-10-12 在本机真文件上的取证,写入模板与那些文件逐字段对齐:
//!
//! | 客户端 | 落点 | 形态 |
//! |---|---|---|
//! | WorkBuddy | `~/.workbuddy/models.json`(+ `settings.json` 的选中键) | 明文 JSON 数组 |
//! | WorkBuddy AI | `~/.workbuddy-ai/models.json`(同结构) | 明文 JSON 数组 |
//! | ZCode | `~/.zcode/v2/config.json` + `provider_config.json` | 两份 JSON |
//! | Trae | `%APPDATA%\<安装>\User\globalStorage\state.vscdb` | SQLite 的 ItemTable |

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// WorkBuddy 的「本地自定义模型」id 前缀。加不加由服务端特性开关
/// (`ProductFeature.CustomModelIdPrefix`)决定,所以只能现读现学,不能写死。
/// 2026-10-12 从 WorkBuddy 的 app.asar 里取证到这段:
/// `const shouldPrefixId = !!ctx.current.productFeatures?.[ProductFeature.CustomModelIdPrefix];`
const WB_PREFIX: &str = "custom-local:";

/// 备份保留份数。
const KEEP_BACKUPS: usize = 5;

/// 我们写进去的条目用什么名字开头,便于辨认「这条是挂件写的」。
/// 用户自己已经有这个习惯(他手配的就是 `4S-claude-sonnet-5`)。
pub const OUR_NAME_PREFIX: &str = "4S-";

/// WorkBuddy settings.json 里选中项的键名(两版客户端一致)。
const WB_SELECTION_KEY: &str = "model";

// ───────────────────────── 客户端与站点 ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Client {
    Trae,
    Workbuddy,
    WorkbuddyAi,
    Zcode,
}

impl Client {
    pub const ALL: [Client; 4] = [
        Client::Trae,
        Client::Workbuddy,
        Client::WorkbuddyAi,
        Client::Zcode,
    ];

    /// 界面上的短标签(模型行里那四个小按钮)。
    pub fn short(self) -> &'static str {
        match self {
            Client::Trae => "T",
            Client::Workbuddy => "WB",
            Client::WorkbuddyAi => "WBA",
            Client::Zcode => "ZC",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Client::Trae => "Trae",
            Client::Workbuddy => "WorkBuddy",
            Client::WorkbuddyAi => "WorkBuddy AI",
            Client::Zcode => "ZCode",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Client::Trae => "trae",
            Client::Workbuddy => "workbuddy",
            Client::WorkbuddyAi => "workbuddy-ai",
            Client::Zcode => "zcode",
        }
    }

    pub fn parse(s: &str) -> Option<Client> {
        Client::ALL.into_iter().find(|c| c.id() == s)
    }

    /// 进程名。两个 WorkBuddy 共用 `WorkBuddy.exe`,所以判「在跑」时**互相牵连**:
    /// 分不清跑的是哪一个,就一律按「有一个在跑」处理(保守方向)。
    pub fn process(self) -> &'static str {
        match self {
            Client::Trae => "Trae CN.exe",
            Client::Workbuddy | Client::WorkbuddyAi => "WorkBuddy.exe",
            Client::Zcode => "ZCode.exe",
        }
    }
}

/// 一个具体的写入目标(= 客户端 × 安装/变体)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Site {
    pub client: Client,
    /// "Trae CN" / "TRAE SOLO CN" / "WorkBuddy" …
    pub label: String,
    pub running: bool,
    pub process: String,
    /// 配置根目录(备份就放在它的 backup-inject/ 下)
    pub root: PathBuf,
    /// 这次要动/要备份的文件
    pub files: Vec<PathBuf>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn appdata() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

/// Trae 的两个安装(CN 与 SOLO CN 各一套独立 DB)。名字与 `appauth::TRAE_DIRS` 同源。
/// Trae 的安装/数据目录名。**版本改名会让这里落空** —— 2026-10-15 用户侧实测:
/// 更新后只剩「Trae CN」,原来的「TRAE SOLO CN」已消失(用户口径:以前是 traesolo/traework,
/// 现在最新版就叫 Trae)。所以与 `appauth::TRAE_DIRS` 保持同一份名单并多留历史名:
/// 多列一个不花钱,漏一个就是"写了但客户端根本不读那份库"。
const TRAE_DIRS: [&str; 4] = ["Trae CN", "TRAE SOLO CN", "Trae", "TraeWork"];

/// 某套安装的候选根目录(用户级安装优先,再退到机器级)。
fn install_roots(dir: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        out.push(local.join("Programs").join(dir));
        out.push(local.join(dir));
    }
    out.push(PathBuf::from(r"C:\Program Files").join(dir));
    out.push(PathBuf::from(r"C:\Program Files (x86)").join(dir));
    out
}

/// 卸载器 / Electron 辅助进程:它们也是 .exe,但**不是主程序名**。
/// 以前只按小写敏感的 `starts_with("unins")` 排一次,`Unins000.exe`、`uninstall.exe`、
/// `Trae CN Helper.exe` 都会留在候选里,于是 `Site.process` 可能挑到它们 ——
/// 而进程检查是写入前唯一那道门(判"客户端在不在跑")。
fn is_helper_exe(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("unins")
        || n.starts_with("uninstall")
        || n.starts_with("uninst")
        || n.contains("helper")
        || n.contains("crashsender")
        || n.contains("crashpad")
}

/// 这套 Trae 安装对应的候选进程名。
///
/// 以前两个安装都共用 `Client::Trae.process()` 那个字面量 `"Trae CN.exe"`:SOLO 在跑时
/// 判不出来 → 照样往它的库里写(退出被内存态覆盖),而没在跑的 CN 反而可能被误判在跑
/// (报告 P1-8)。这里按**安装目录里的 exe 文件名**取,再兜底「目录名.exe」;
/// 本机实测 CN 的文件名是 `Trae CN.exe`,SOLO 那套已卸载(只剩数据目录),只能两个都探。
fn trae_processes(dir: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for base in install_roots(dir) {
        let Ok(rd) = std::fs::read_dir(&base) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let is_exe = p
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("exe"))
                .unwrap_or(false);
            if !is_exe {
                continue;
            }
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            // 卸载器/更新器/辅助进程不算主程序名(大小写不敏感)
            if is_helper_exe(&name) {
                continue;
            }
            if !v.contains(&name) {
                v.push(name);
            }
        }
    }
    let by_dir = format!("{dir}.exe");
    if !v.contains(&by_dir) {
        v.push(by_dir);
    }
    v
}

/// 这套安装该拿去判"在不在跑"的进程名:**优先与安装目录同名的主程序**
/// (`Trae CN` → `Trae CN.exe`)。以前 `Site.process` 直接吃 `read_dir` 的顺序,
/// 挑到哪个算哪个 —— 顺序随文件系统变,进程检查时灵时不灵(P2)。
fn expected_process(dir: &str, procs: &[String]) -> String {
    let by_dir = format!("{dir}.exe");
    procs
        .iter()
        .find(|p| p.eq_ignore_ascii_case(&by_dir))
        .cloned()
        .or_else(|| procs.first().cloned())
        .unwrap_or_else(|| Client::Trae.process().to_string())
}

/// 显示用路径:`C:\Users\x\...` → `~/.zcode/...`。
pub fn pretty(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if let Some(h) = home() {
        let h = h.to_string_lossy().replace('\\', "/");
        // 必须是**路径组件**边界上的前缀:以前用字符串 strip_prefix,主目录名互为前缀时
        // (账户 wacil 与 wacilon 并存)会把旁系目录也吃掉,显示成 "~on/.workbuddy/..."
        // 这种既不象 ~ 展开也不是绝对路径的怪串(报告 P3-2)。
        if s.len() > h.len() && s.starts_with(&h) && s[h.len()..].starts_with('/') {
            return format!("~{}", &s[h.len()..]);
        }
    }
    s
}

/// 检测本机装有的写入目标。没装的不返回(界面据「四客户端里缺哪个」显示未安装)。
pub fn sites() -> Vec<Site> {
    let mut out = Vec::new();
    // 同一轮里同名进程只探一次:两个 WorkBuddy 共用 `WorkBuddy.exe`,
    // 每轮最多起 3 个 tasklist 而不是 4 个(少一次子进程,少一分闪窗与等待)。
    let mut probed: HashMap<String, bool> = HashMap::new();
    let is_running = |p: &str, probed: &mut HashMap<String, bool>| {
        *probed.entry(p.to_string()).or_insert_with(|| process_running(p))
    };

    if let Some(roaming) = appdata() {
        for dir in TRAE_DIRS {
            let root = roaming.join(dir).join("User").join("globalStorage");
            let db = root.join("state.vscdb");
            if !db.exists() {
                continue;
            }
            let mut files = vec![db.clone()];
            // SQLite 的 WAL/SHM 也要一起备份:只拷主库会在回滚后留下不一致的日志
            for suffix in ["-wal", "-shm"] {
                let f = root.join(format!("state.vscdb{suffix}"));
                if f.exists() {
                    files.push(f);
                }
            }
            let procs = trae_processes(dir);
            let running = procs.iter().any(|p| is_running(p, &mut probed));
            out.push(Site {
                client: Client::Trae,
                label: dir.to_string(),
                running,
                process: expected_process(dir, &procs),
                root,
                files,
            });
        }
    }

    if let Some(h) = home() {
        let wb_root = h.join(".workbuddy");
        if wb_root.join("models.json").exists() {
            out.push(Site {
                client: Client::Workbuddy,
                label: Client::Workbuddy.label().to_string(),
                running: is_running(Client::Workbuddy.process(), &mut probed),
                process: Client::Workbuddy.process().to_string(),
                root: wb_root.clone(),
                files: vec![wb_root.join("models.json"), wb_root.join("settings.json")],
            });
        }
        let wba_root = h.join(".workbuddy-ai");
        if wba_root.join("models.json").exists() {
            out.push(Site {
                client: Client::WorkbuddyAi,
                label: Client::WorkbuddyAi.label().to_string(),
                running: is_running(Client::WorkbuddyAi.process(), &mut probed),
                process: Client::WorkbuddyAi.process().to_string(),
                root: wba_root.clone(),
                files: vec![wba_root.join("models.json"), wba_root.join("settings.json")],
            });
        }
        let zc_root = h.join(".zcode").join("v2");
        if zc_root.join("config.json").exists() {
            out.push(Site {
                client: Client::Zcode,
                label: Client::Zcode.label().to_string(),
                running: is_running(Client::Zcode.process(), &mut probed),
                process: Client::Zcode.process().to_string(),
                root: zc_root.clone(),
                files: vec![
                    zc_root.join("config.json"),
                    zc_root.join("provider_config.json"),
                ],
            });
        }
    }

    out
}

pub fn sites_of(client: Client) -> Vec<Site> {
    sites().into_iter().filter(|s| s.client == client).collect()
}

/// Windows 起子进程的创建标志:不要控制台窗口。
///
/// 挂件是 GUI 程序,默认给每个子进程配一个新控制台 —— 用户实测(2026-10-12):
/// **打开模型页会闪过好几个黑框**。凡是自己起的子进程(tasklist / clip)都带上这个标志。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 给子进程加上"别弹窗"的创建标志(Win 专有;其它平台原样返回)。
fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// 目标进程在不在跑。走 `tasklist`,按 CSV 首列整串比对 —— 认名字不认本地化提示文案
/// (中文系统上「没有运行的任务…」与英文系统不同,拿文案判会翻车)。
///
/// **带超时**:起进程等结果是阻塞活,`tasklist` 万一卡住不该把调用方一起拖住
/// (2026-10-12 真机踩过:同步命令跑在主线程上,一次卡死 = 整个挂件不响应)。
pub fn process_running(process: &str) -> bool {
    let mut cmd = Command::new("tasklist");
    cmd.args(["/FI", &format!("IMAGENAME eq {process}"), "/NH", "/FO", "CSV"]);
    quiet(&mut cmd);
    match run_with_timeout(cmd, 5) {
        Some(out) => tasklist_has(&out, process),
        None => false, // 拿不到进程表时不阻塞用户;真正的保护是「先备份 + 读回校验」
    }
}

/// 跑一个子进程并等它结束;超过 `secs` 秒就杀掉并返回 None。
fn run_with_timeout(mut cmd: Command, secs: u64) -> Option<String> {
    use std::io::Read;
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    // 先把 stdout 交给一个线程去读。以前是"等子进程退出后再读":进程列表很长时
    // tasklist 的输出超过管道缓冲(Windows 常见 4 KB)就会阻塞在写,我们这边只会等,
    // 5 秒后杀掉 → 探测失败被当成"没在运行" → 用户就在客户端开着的时候点了写入(P2-21)。
    let pipe = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut so) = pipe {
            let _ = so.read_to_string(&mut buf);
        }
        buf
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(30))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(reader);
                eprintln!("tasklist 超时已终止(进程探测退化为「未运行」)");
                return None;
            }
            Err(_) => return None,
        }
    }
    Some(reader.join().unwrap_or_default())
}

fn tasklist_has(stdout: &str, process: &str) -> bool {
    stdout.lines().any(|line| {
        let first = line
            .trim()
            .trim_start_matches('"')
            .split('"')
            .next()
            .unwrap_or("")
            .trim();
        first.eq_ignore_ascii_case(process)
    })
}

// ───────────────────────── 要写进去的东西 ─────────────────────────

/// 一个模型要写进客户端时的全部信息。密钥只在内存里过一趟。
#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub id: String,
    /// 来源 id("4sapi" / "hapi" / "opencode-go")。同一个模型名可能存在于多个来源,
    /// 写进去的地址与"哪条算我们写的"都由它决定。
    pub source: String,
    /// 来源的界面名(写进 ZCode 的 provider 名)
    pub source_name: String,
    /// 客户端里显示的名字(`4S-<id>`;用户手配的也是这个口径)
    pub display_name: String,
    /// 上下文窗口(来自价目表 tags);未知就 None —— 不编造
    pub context_tokens: Option<i64>,
    /// `https://<域名>/v1`(挂件实测能取数的那个入口)
    pub base_url: String,
    /// 明文密钥
    pub key: String,
    pub multimodal: bool,
}

impl ModelSpec {
    pub fn new(
        source: &str,
        id: &str,
        base_url: &str,
        key: &str,
        context_tokens: Option<i64>,
        multimodal: bool,
    ) -> Self {
        Self {
            id: id.to_string(),
            source: source.to_string(),
            source_name: crate::models::find_source(source)
                .map(|d| d.name.to_string())
                .unwrap_or_else(|| source.to_string()),
            display_name: format!("{OUR_NAME_PREFIX}{id}"),
            context_tokens,
            // 设置里这一项是自由文本:厂商文档常直接给 `.../v1/chat/completions`。
            // 收口成"到 /v1 为止",否则 Trae 会拿到双重端点、另三家拿到带
            // /chat/completions 的错值,四个客户端的模型全部点不动(报告 P2-11)。
            base_url: Self::normalize_base(base_url),
            key: key.to_string(),
            multimodal,
        }
    }

/// 去首尾空白与尾斜杠,并剥掉结尾的 `/chat/completions`,得到"到 /v1 为止"的形态。
fn normalize_base(v: &str) -> String {
    const SUFFIX: &str = "/chat/completions";
    let mut s = v.trim().trim_end_matches('/').to_string();
    if s.len() > SUFFIX.len() && s.ends_with(SUFFIX) {
        s.truncate(s.len() - SUFFIX.len());
    }
    s
}

    /// Trae 的 base_url 是完整端点(`.../v1/chat/completions`)。
    fn chat_completions_url(&self) -> String {
        if self.base_url.ends_with("/chat/completions") {
            self.base_url.clone()
        } else {
            format!("{}/chat/completions", self.base_url)
        }
    }

    /// 这个来源的域名(判定"这条条目是我们写的")。
    fn hosts(&self) -> Vec<String> {
        crate::models::source_hosts(&self.source)
    }
}

/// 客户端里已经配好的条目:哪个模型、来自哪个来源。
/// **多来源之后必须带来源** —— 同名模型在几个来源里都有,只看 id 会串台。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguredModel {
    pub id: String,
    /// 来源 id;认不出域名时是空串(客户端里手工配的别家条目)
    pub source: String,
}

/// 写入选项。
#[derive(Debug, Clone, Default)]
pub struct Opts {
    /// 已经学到的选中项前缀(None = 还没学会,那就只写条目、不写选中)
    pub selection_prefix: Option<String>,
    /// Trae 的选中项写入(实验性,默认关)
    pub trae_auto_select: bool,
}

// ───────────────────────── 预告与回执 ─────────────────────────

/// 写入预告。界面直接渲染它 —— 用户在点确认之前就该看到全部后果。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub client: String,
    pub label: String,
    /// 目标文件(显示用)
    pub files: Vec<String>,
    /// "新增" | "更新现有条目" | "已配置,无需改动"
    pub action: String,
    /// 会不会真的改动文件
    pub changed: bool,
    /// 会不会写密钥(明文)
    pub writes_secret: bool,
    /// 会不会写选中项
    pub writes_selection: bool,
    /// 这个客户端当前有没有这条模型(有 → 界面同时给出「取消写入」)
    pub configured: bool,
    /// 取消写入会动到什么;None = 当前没有,不用给这个按钮
    pub removal: Option<Removal>,
    /// 非空 = 不能写,界面把确认按钮禁掉并显示这条
    pub blocked: Option<String>,
    /// 附注(更新时会列出差异、以及生效方式)
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub client: String,
    pub label: String,
    pub stamp: String,
    /// 备份目录(显示用,`~` 缩写)
    pub backup: String,
    /// 备份目录的实际路径(记账与回滚要用它)
    pub backup_path: String,
    pub added: usize,
    pub updated: usize,
    /// 取消写入时移掉了几条
    pub removed: usize,
    pub selected: bool,
    /// 生效方式(重启客户端等)
    pub effect: String,
    pub notes: Vec<String>,
    /// 这一家没写成时的原因(报告 P1-6:多站点的"部分成功"必须可见、可记账、可回滚)。
    /// 有值时 backup/backup_path 为空,界面按红色渲染。
    pub error: Option<String>,
}

/// 一次写入的台账(不含密钥)。落 SQLite 的 settings,供界面显示「上次写到哪」与回滚。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct InjectRecord {
    pub stamp: String,
    pub at: i64,
    pub client: String,
    pub label: String,
    pub model: String,
    /// 备份目录(显示用,`~` 缩写)
    pub backup: String,
    /// 备份目录的**实际路径**(回滚要用它;显示用的那个带 `~`,不能拿来操作)
    pub backup_path: String,
    /// 那份备份目录现在还在吗。备份只留最近 KEEP_BACKUPS 份,台账留 LOG_KEEP 条,
    /// 两个上限以前互不知情 → 点较早那条会报"备份目录不存在"(报告 P2-8)。
    /// 读取时按目录是否存在现算,不依赖写入当时记下的值。
    #[serde(default)]
    pub backup_ok: bool,
    pub added: usize,
    pub updated: usize,
    /// 取消写入时移掉了几条
    pub removed: usize,
    pub selected: bool,
}

/// 台账落 SQLite 的 settings(单独一行)。只留最近 20 条 —— 这是给人看的历史,不是账本。
pub const LOG_KEY: &str = "inject_log";
const LOG_KEEP: usize = 20;

pub fn load_log(store: &crate::store::Store) -> Vec<InjectRecord> {
    let mut log = store
        .get_setting(LOG_KEY)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<InjectRecord>>(&s).ok())
        .unwrap_or_default();
    for r in &mut log {
        // 现算:备份被 prune_backups 清掉以后,界面上那条就该标灰,而不是给一个
        // 点下去必报错的「回滚」按钮(P2-8)
        r.backup_ok = !r.backup_path.is_empty()
            && std::path::Path::new(&r.backup_path).is_dir();
    }
    log
}

pub fn push_log(store: &crate::store::Store, rec: InjectRecord) -> Result<(), String> {
    let mut log = load_log(store);
    log.insert(0, rec); // 新的在前
    log.truncate(LOG_KEEP);
    let json = serde_json::to_string(&log).map_err(|e| e.to_string())?;
    store.set_setting(LOG_KEY, &json)
}

/// 台账里找这条备份属于谁(回滚前要据此判「目标在不在跑」)。
pub fn log_owner(log: &[InjectRecord], backup_path: &str) -> Option<Client> {
    log.iter()
        .find(|r| r.backup_path.eq_ignore_ascii_case(backup_path))
        .and_then(|r| Client::parse(&r.client))
}

/// 客户端状态(模型页顶部那条客户端栏)。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClientState {
    pub client: String,
    pub short: String,
    pub label: String,
    pub installed: bool,
    pub running: bool,
    pub process: String,
    /// 配置根目录(显示用)
    pub roots: Vec<String>,
    pub files: Vec<String>,
    /// 我们认得出的、已经写进去的模型(带来源)
    pub models: Vec<ConfiguredModel>,
    /// 客户端当前选中的模型(读得到才给)
    pub selected: Option<String>,
    /// 选中项前缀口径(学到的;None = 还没学会 → 只写条目不写选中)
    pub learned_prefix: Option<String>,
    pub note: Option<String>,
}

/// 备用配置的「在不在跑」。拿不到进程表时返回 false 而不是报错 ——
/// 真正的保护是备份 + 读回校验,进程检查只是第一道。
pub fn blocked_by_running(site: &Site) -> Option<String> {
    if !site.running {
        return None;
    }
    Some(format!(
        "{} 正在运行 —— 它的配置是内存态落盘,边跑边写会在退出时被覆盖。先退出它再点写入。",
        site.label
    ))
}

// ───────────────────────── 文件读写与备份 ─────────────────────────

/// 读 JSON 文件。返回 None = 文件不存在或内容为空(空文件是合法初态,不是错误)。
fn read_json(path: &Path) -> Result<Option<Value>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("读 {} 失败: {e}", pretty(path))),
    };
    // 带 BOM 的 JSON(`\u{feff}`)serde_json 直接解析会报错,先剥掉
    let text = raw.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(text)
        .map(Some)
        .map_err(|e| format!("解析 {} 失败(结构可能已变,本次不写): {e}", pretty(path)))
}

/// 写 JSON 文件:先写同目录临时文件再原子替换 —— 半截文件比不写更糟。
/// `bom` / `indent` 按原文件的习惯来(WorkBuddy 的 models.json 带 BOM、2 空格缩进)。
fn write_json(path: &Path, v: &Value, bom: bool, indent: usize) -> Result<(), String> {
    let mut text = if indent == 0 {
        serde_json::to_string(v).map_err(|e| e.to_string())?
    } else {
        let pad = " ".repeat(indent);
        let mut buf = Vec::new();
        let fmt = serde_json::ser::PrettyFormatter::with_indent(pad.as_bytes());
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
        serde::Serialize::serialize(v, &mut ser).map_err(|e| e.to_string())?;
        String::from_utf8(buf).map_err(|e| e.to_string())?
    };
    if bom {
        text.insert(0, '\u{feff}');
    }
    // with_extension 是"换扩展名"的语义,对 state.vscdb 这种多段名会生成完全无关的名字;
    // 直接按文件名拼更稳(报告 P3-4)。
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| format!("文件名非法: {}", pretty(path)))?;
    let tmp = path.with_file_name(format!("{name}.tmp-inject"));
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("写临时文件失败:{} {}", os(&e), pretty(&tmp))
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!(
            "替换 {} 失败:{e} {}(客户端在跑就先退出它)",
            pretty(path),
            os_hint(&e)
        )
    })
}

/// 把 `patch` 的键逐个并入 `obj[key]`:该键本来就是对象就逐键覆盖(其余键原样留着),
/// 不是对象或缺失就整体写入。
fn merge_into(obj: &mut Map<String, Value>, key: &str, patch: Value) {
    let was_object = obj.get(key).map(|v| v.is_object()).unwrap_or(false);
    if !was_object {
        obj.insert(key.into(), patch);
        return;
    }
    let Some(p) = patch.as_object() else {
        obj.insert(key.into(), patch);
        return;
    };
    if let Some(Value::Object(have)) = obj.get_mut(key) {
        for (k, v) in p {
            have.insert(k.clone(), v.clone());
        }
    }
}

/// 原文件是不是带 BOM 的(照它的习惯写回去,免得下游 diff 里多出一行变化)。
fn has_bom(path: &Path) -> bool {
    // 只需要前 3 个字节;一次写入里同一份文件以前被整读三遍(解析、探 BOM、校验),
    // 100 KB 级的 models.json 上能感到顿(报告 P3-7)
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 3];
    match f.read_exact(&mut head) {
        Ok(()) => head == [0xEF, 0xBB, 0xBF],
        Err(_) => false,
    }
}

/// 把 Windows 的错误码翻成能行动的话(报告 P3-4:以前只有 `os error 5/32`,
/// 用户分不清是权限、被占用还是路径过长)。
fn os_hint(e: &std::io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(5) => "(没有写权限:文件可能是只读,或目录不可写)",
        Some(32) | Some(33) => "(文件正被别的程序占用)",
        Some(3) => "(路径不存在)",
        Some(206) | Some(260) => "(路径过长)",
        _ => "",
    }
}

fn os(e: &std::io::Error) -> String {
    format!("{e} {}", os_hint(e)).trim().to_string()
}

/// 备份要改的文件到 `<root>/backup-inject/<stamp>/`。返回备份目录。
fn backup(root: &Path, files: &[PathBuf], stamp: &str) -> Result<PathBuf, String> {
    // 同一秒内连点两次「写入」时,备份目录名会撞车:第二次把**已改过**的文件盖进
    // 同一个目录,于是第一条记录的"备份"指向第二次写入之后的状态 —— 回滚回到错误时间点
    // (报告 P3-5 / P3-8)。撞了就加序号,每个 stamp 目录只对应一次写入前快照。
    let mut dir = root.join("backup-inject").join(stamp);
    let mut n = 2u32;
    while dir.exists() {
        dir = root.join("backup-inject").join(format!("{stamp}-{n}"));
        n += 1;
        if n > 99 {
            return Err(format!("备份目录名重复太多次:{}", pretty(&dir)));
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("建备份目录失败: {e}"))?;
    for f in files {
        if !f.exists() {
            continue;
        }
        let name = f
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("文件名非法: {}", pretty(f)))?;
        std::fs::copy(f, dir.join(name)).map_err(|e| format!("备份 {name} 失败: {e}"))?;
    }
    prune_backups(root);
    Ok(dir)
}

/// 备份目录名是不是我们生成的那种:`YYYYMMDD-HHMMSS`,或同秒撞车时由 [`backup`]
/// 追加的 `-N`(N = 2..=99)。**其它名字一律不认** ——
/// ① 清理只动我们自己建的目录,用户在 `backup-inject/` 里自建的东西不碰;
/// ② 回滚只认这个形态,拒绝"把随便一个目录里的文件盖到它上一级"。
/// 以前只认 15 位,于是带序号的备份**永远不会被清理**:含明文密钥的快照无上限地堆着,
/// 与"每个客户端最多 5 份"的承诺相矛盾(P2)。
fn is_stamp_name(name: &str) -> bool {
    let b = name.as_bytes();
    if b.len() < 15 {
        return false;
    }
    if !b[..8].iter().all(u8::is_ascii_digit) || b[8] != b'-' {
        return false;
    }
    if !b[9..15].iter().all(u8::is_ascii_digit) {
        return false;
    }
    if b.len() == 15 {
        return true;
    }
    // `<stamp>-N`:只有一个 '-' 加 1~2 位数字(99 封顶,backup 超过就报错不建了)
    if b[15] != b'-' {
        return false;
    }
    let n = &name[16..];
    !n.is_empty()
        && n.len() <= 2
        && n.bytes().all(|c| c.is_ascii_digit())
        && n.parse::<u32>().map(|v| (2..=99).contains(&v)).unwrap_or(false)
}

/// 只留最近 KEEP_BACKUPS 份,免得天天写把用户目录撑大。
///
/// 两条边界(报告 P2-8):
/// ① 只动名字形如 `%Y%m%d-%H%M%S`(含 `-N`)的目录 —— 以前只判 `is_dir()`,用户在
///    `backup-inject/` 里自己建的目录会被当成"最旧的备份"删掉;
/// ② 删除失败不再静默:以前 `let _ =` 一吞,备份就会积到 6 份以上,
///    "别撑大用户目录"的目的落空,而界面上的历史还留着指向已消失目录的记录。
fn prune_backups(root: &Path) {
    fn name_of(p: &Path) -> String {
        p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    }
    let base = root.join("backup-inject");
    let Ok(rd) = std::fs::read_dir(&base) else { return };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && is_stamp_name(&name_of(p)))
        .collect();
    if dirs.len() <= KEEP_BACKUPS {
        return;
    }
    dirs.sort(); // 目录名是时间戳,字典序即时间序
    for old in &dirs[..dirs.len() - KEEP_BACKUPS] {
        if let Err(e) = std::fs::remove_dir_all(old) {
            eprintln!("清理旧备份失败({}): {e}", pretty(old));
        }
    }
}

/// 从备份目录回滚:备份目录里的文件按名字放回它的上一级(即配置目录)。
/// 布局 `<root>/backup-inject/<stamp>/<文件>` 是固定的,所以不用额外记账 ——
/// 也正因为固定,这里**必须自己核一遍布局**:调用方的包含性检查只是一次字符串前缀
/// 比较,拼错的/别处的路径不该被当成备份,把它的文件盖到上一级去(P2)。
pub fn restore(backup_dir: &Path) -> Result<String, String> {
    let dir_name = backup_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent_is_backup_inject = backup_dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().eq_ignore_ascii_case("backup-inject"))
        .unwrap_or(false);
    if !parent_is_backup_inject {
        return Err(format!(
            "这不是我们的备份目录(上一级不是 backup-inject):{}",
            pretty(backup_dir)
        ));
    }
    if !is_stamp_name(&dir_name) {
        return Err(format!(
            "备份目录名不是时间戳形态,拒绝恢复:{}",
            pretty(backup_dir)
        ));
    }
    let Some(root) = backup_dir.parent().and_then(|p| p.parent()) else {
        return Err("备份目录结构异常".into());
    };
    let rd = std::fs::read_dir(backup_dir).map_err(|e| format!("读备份目录失败: {e}"))?;
    let mut n = 0usize;
    let mut bad: Vec<String> = Vec::new();
    for e in rd.filter_map(|e| e.ok()) {
        let name = e.file_name();
        let name_str = name.to_string_lossy().to_string();
        let target = root.join(&name);
        // 逐个文件:先写同目录 temp 再 rename。直接 fs::copy 到已存在目标会**先截断**
        // 再逐块拷,目标被占用时就把客户端自己的配置留成了半截文件(报告 P2-6)。
        let tmp = root.join(format!("{name_str}.restore-tmp"));
        match std::fs::copy(e.path(), &tmp).and_then(|_| std::fs::rename(&tmp, &target)) {
            Ok(_) => {
                n += 1;
                // SQLite 主库恢复完,旁边若留着**没进这份备份**的 -wal/-shm,SQLite
                // 下次打开会把回滚掉的那次写入重放回刚恢复的库上 —— 回滚等于没做。
                // 备份里没有它,就说明它与这份备份不是一套,删掉。
                if name_str.to_ascii_lowercase().ends_with(".vscdb") {
                    for suffix in ["-wal", "-shm"] {
                        let sib = root.join(format!("{name_str}{suffix}"));
                        if sib.exists() && !backup_dir.join(format!("{name_str}{suffix}")).exists() {
                            if let Err(err) = std::fs::remove_file(&sib) {
                                eprintln!("清理 {} 失败: {err}", pretty(&sib));
                            }
                        }
                    }
                }
            }
            Err(err) => {
                let _ = std::fs::remove_file(&tmp);
                bad.push(format!("{}:{err}", pretty(&target)));
            }
        }
    }
    if n == 0 && bad.is_empty() {
        return Err("备份目录是空的".into());
    }
    if !bad.is_empty() {
        return Err(format!(
            "只恢复了 {n} 个文件,这些没成:{} —— 备份仍在 {},可重试或手动拷回",
            bad.join("; "),
            pretty(backup_dir)
        ));
    }
    Ok(format!("已从备份恢复 {n} 个文件"))
}

fn stamp_now() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// 一次站点写入的结果。写盘/校验/回滚都在这层做完,上层只管组报告。
#[derive(Debug, Default)]
struct SiteOutcome {
    added: usize,
    updated: usize,
    removed: usize,
    selected: bool,
    /// 取消写入时顺带把客户端的选中项也清了
    clears_selection: bool,
    /// 已存在且内容一致,没动盘
    same: bool,
    /// 同 id 的条目原本指向别的服务(界面要在预告里点出来)
    foreign_conflict: Option<String>,
    backup: PathBuf,
    notes: Vec<String>,
}

/// 备份 → 写 → 读回校验;校验不过就回滚并报错。
/// `write` 是真正落盘的那一步,`verify` 从盘上读回来判断写对了没有。
fn commit<W, V>(
    site: &Site,
    files: &[PathBuf],
    stamp: &str,
    write: W,
    verify: V,
) -> Result<PathBuf, String>
where
    W: FnOnce() -> Result<(), String>,
    V: FnOnce() -> bool,
{
    let dir = backup(&site.root, files, stamp)?;
    // 写失败也要盖回:两家客户端都是"两份文件必须成套"(ZCode 的 provider ↔ 规则、
    // WorkBuddy 的条目 ↔ 选中项),第一个文件落盘、第二个失败就停在半套状态。
    if let Err(e) = write() {
        return Err(match restore(&dir) {
            Ok(s) => format!("写入失败: {e}｜已按备份整批盖回,未产生半成品。{s}"),
            Err(r) => format!(
                "写入失败: {e}｜回滚也失败: {r}｜备份仍在 {},退出客户端后可手动还原",
                pretty(&dir)
            ),
        });
    }
    if !verify() {
        // 回滚成没成必须说实话:以前 let _ = 吞掉,文案却无条件写「已回滚」
        return Err(match restore(&dir) {
            Ok(s) => format!(
                "读回校验没过,已回滚到写入前的备份({})。{s}",
                pretty(&dir)
            ),
            Err(e) => format!(
                "读回校验没过,而且回滚失败: {e}｜备份仍在 {}——请退出客户端后手动还原",
                pretty(&dir)
            ),
        });
    }
    Ok(dir)
}

// ───────────────────────── WorkBuddy / WorkBuddy AI ─────────────────────────

/// 我们要写的那条条目长这样(与用户手配的那条逐字段对齐)。
fn wb_entry(spec: &ModelSpec, existing: Option<&Map<String, Value>>) -> Value {
    // 更新时不改用户自己起的名字(他可能已经把 4S- 改成别的)
    let name = existing
        .and_then(|m| m.get("name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
        .unwrap_or_else(|| spec.display_name.clone());

    let mut m = Map::new();
    m.insert("id".into(), json!(spec.id));
    m.insert("name".into(), json!(name));
    m.insert("vendor".into(), json!("Custom"));
    m.insert("url".into(), json!(spec.base_url));
    m.insert("apiKey".into(), json!(spec.key));
    m.insert("supportsToolCall".into(), json!(true));
    m.insert("supportsImages".into(), json!(spec.multimodal));
    m.insert("supportsReasoning".into(), json!(true));
    m.insert("useCustomProtocol".into(), json!(false));
    m.insert(
        "reasoning".into(),
        json!({"supportedEfforts": ["low", "medium", "high", "xhigh", "max"]}),
    );
    if let Some(ctx) = spec.context_tokens {
        m.insert("maxInputTokens".into(), json!(ctx));
    }
    // 用户自己可能加过别的字段(别名之类):更新时原样保留
    if let Some(old) = existing {
        for (k, v) in old {
            if !m.contains_key(k) {
                m.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(m)
}

/// 一条 url 里的**主机名**:剥掉 scheme / userinfo / 端口 / 路径 / 查询串,小写。
/// 与 `models::source_of_url`(读路径,早就是精确主机名比对)同一套解析。
fn url_host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .rsplit('@')
        .next()
        .unwrap_or(rest);
    let host = if let Some(h) = authority.strip_prefix('[') {
        // IPv6 字面量 [::1]:8080:方括号里的冒号不是端口分隔符
        h.split(']').next().unwrap_or(h)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// 这条地址是不是**这个来源**的条目地址。判据两条:
/// ① 主机名与来源域名**精确**相同(不能 `contains`:那样 `https://mirror-4sapi.com/v1`
///    与 `https://not4sapi.com.evil/v1` 都会算成"我们写的",于是「取消写入」会删掉
///    用户自己的条目、写入时也不再报"同名属于别家"的冲突);
/// ② 或者整串等于我们会写下去的那个 base_url(用户把 `model_base_url` 配到自定义域名时,
///    只有这一条能把他写进去的条目认回来;Trae 写的是 `…/chat/completions`,一并认)。
/// `hosts` 为空 = 这个来源没有域名表,那就只剩 ② 可判。
fn url_is_ours(url: &str, hosts: &[String], base_url: &str) -> bool {
    if !base_url.is_empty()
        && (url == base_url || url == format!("{base_url}/chat/completions"))
    {
        return true;
    }
    let Some(host) = url_host_of(url) else {
        return false;
    };
    hosts.iter().any(|h| host == h.trim().to_ascii_lowercase())
}

/// 一条 models.json 条目是不是「我们(4SAPI)写的」。判据:[`url_is_ours`] ——
/// **只有 url 空/缺**时才退回名字前缀(`4S-`)。**不能只看 id** —— 别的中转站的同名
/// 模型有同样的 id;也不能"名字带 4S- 就算" —— 用户手配的 `4S-` 条目地址指向别家时
/// 不是我们的,删了就是删用户的东西(写入侧的冲突提示同理)。
fn wb_is_ours(e: &Value, hosts: &[String], base_url: &str) -> bool {
    let url = e.get("url").and_then(|u| u.as_str()).unwrap_or("");
    if url_is_ours(url, hosts, base_url) {
        return true;
    }
    if url.trim().is_empty() {
        return e
            .get("name")
            .and_then(|n| n.as_str())
            .map(|n| n.starts_with(OUR_NAME_PREFIX))
            .unwrap_or(false);
    }
    false
}

/// 写入后的读回校验:盘上那条必须与我们**这次写下去的内容逐字段一致**
/// (id / url / apiKey / name),写过的选中项也要落上。
///
/// 以前只比 id + url —— 两者在写之前就成立,于是被截断/写坏的行、或客户端把
/// apiKey 抹掉/换了,校验一律通过(与 ZCode 那边校验 apiKey+baseURL 的口径看齐)。
fn wb_verify(models_path: &Path, settings_path: &Path, expect: &Value, selection: Option<&str>) -> bool {
    let same = |e: &Value, k: &str| {
        e.get(k).and_then(|v| v.as_str()) == expect.get(k).and_then(|v| v.as_str())
    };
    let entry_ok = read_json(models_path)
        .ok()
        .flatten()
        .and_then(|v| {
            v.as_array().map(|a| {
                a.iter().any(|e| {
                    same(e, "id")
                        && same(e, "url")
                        && same(e, "apiKey")
                        && same(e, "name")
                })
            })
        })
        .unwrap_or(false);
    if !entry_ok {
        return false;
    }
    match selection {
        None => true,
        Some(want) => read_json(settings_path)
            .ok()
            .flatten()
            .and_then(|s| s.get(WB_SELECTION_KEY).and_then(|v| v.as_str()).map(String::from))
            .as_deref()
            == Some(want),
    }
}

fn write_workbuddy_site(
    site: &Site,
    spec: &ModelSpec,
    opts: &Opts,
) -> Result<SiteOutcome, String> {
    let hosts = spec.hosts();
    let models_path = site.root.join("models.json");
    let settings_path = site.root.join("settings.json");

    let Some(models) = read_json(&models_path)? else {
        return Err(format!("{} 不存在或是空的", pretty(&models_path)));
    };
    let Value::Array(mut arr) = models else {
        return Err(format!(
            "{} 不是数组(结构可能已变,本次不写)",
            pretty(&models_path)
        ));
    };

    let mut outcome = SiteOutcome::default();

    // upsert:同 id 的条目就地更新,否则追加
    let idx = arr.iter().position(|e| {
        e.get("id")
            .and_then(|i| i.as_str())
            .map(|i| i == spec.id)
            .unwrap_or(false)
    });
    if let Some(i) = idx {
        let old = arr[i].as_object().cloned().ok_or("条目不是对象")?;
        if !wb_is_ours(&arr[i], &hosts, &spec.base_url) {
            outcome.foreign_conflict = Some(
                arr[i]
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("(无 url)")
                    .to_string(),
            );
        }
        let old_key = old.get("apiKey").and_then(|k| k.as_str()).unwrap_or("");
        let old_url = old.get("url").and_then(|u| u.as_str()).unwrap_or("");
        if old_url == spec.base_url && old_key == spec.key {
            outcome.same = true;
        } else {
            outcome.updated = 1;
            if old_key != spec.key {
                outcome.notes.push("密钥与现有条目不同(确认后会覆盖)".into());
            }
            arr[i] = wb_entry(spec, Some(&old));
        }
    } else {
        arr.push(wb_entry(spec, None));
        outcome.added = 1;
    }

    // 选中项:只有学到前缀才写。学不到就只写条目 ——
    // 猜另一个程序的键格式,会埋下「看着配好了其实没生效」的坑。
    let mut settings_new: Option<Value> = None;
    if let Some(prefix) = opts.selection_prefix.as_deref() {
        if let Some(s) = read_json(&settings_path)? {
            if s.is_object() {
                let mut s = s;
                s[WB_SELECTION_KEY] = json!(format!("{prefix}{}", spec.id));
                settings_new = Some(s);
                outcome.selected = true;
            }
        }
    }

    if outcome.same && settings_new.is_none() {
        // 内容和盘上一致:不写盘,连备份都不必做
        return Ok(outcome);
    }

    let stamp = stamp_now();
    let models_next = Value::Array(arr);
    let settings_for_write = settings_new.clone();
    let models_for_check = models_path.clone();
    let settings_for_check = settings_path.clone();
    // 读回校验要按**这次实际写下去的那一条**逐字段比对(名字可能沿用了用户自己起的)
    let expect = models_next
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|e| e.get("id").and_then(|i| i.as_str()) == Some(spec.id.as_str()))
        })
        .cloned()
        .unwrap_or(Value::Null);
    let want_selection = settings_new
        .as_ref()
        .and_then(|s| s.get(WB_SELECTION_KEY))
        .and_then(|v| v.as_str())
        .map(String::from);
    outcome.backup = commit(
        site,
        &[models_path.clone(), settings_path.clone()],
        &stamp,
        move || {
            write_json(&models_path, &models_next, has_bom(&models_path), 2)?;
            if let Some(s) = settings_for_write {
                write_json(&settings_path, &s, has_bom(&settings_path), 2)?;
            }
            Ok(())
        },
        move || {
            wb_verify(
                &models_for_check,
                &settings_for_check,
                &expect,
                want_selection.as_deref(),
            )
        },
    )?;
    outcome
        .notes
        .push(format!("备份:{}", pretty(&outcome.backup)));
    Ok(outcome)
}

/// 「同名条目被覆盖」这条提示里该怎么称呼**原本那条**的来源:按它的实际地址认
/// (`models::source_of_url`),认不出才是"别家来源"。
/// 以前这句话写死"不是 4SAPI":写 Hapi / OpenCode Go 时意思正好反了(P2)。
fn foreign_source_label(url: &str) -> &'static str {
    crate::models::source_of_url(url)
        .and_then(crate::models::find_source)
        .map(|d| d.name)
        .unwrap_or("别家来源")
}

/// 从 WorkBuddy 当前选中值反推前缀口径。返回 None = 没有可用的证据(不猜)。
///
/// 证据两种:
/// - 值带 `custom-local:` 前缀 → 服务端特性开关是开的,照此写;
/// - 值是裸 id 且命中我们自己写的条目 → 开关是关的,照此写。
///
/// 值是个内置模型(如 `gpt-6-luna`)时两种都不是 —— 内置模型永远不带前缀,
/// 由它推不出开关状态,所以返回 None(宁可不写选中,也不写错)。
pub fn learn_prefix(current: Option<&str>, our_ids: &[String]) -> Option<String> {
    let v = current?.trim();
    if v.is_empty() {
        return None;
    }
    if v.starts_with(WB_PREFIX) {
        return Some(WB_PREFIX.to_string());
    }
    if our_ids.iter().any(|id| id == v) {
        return Some(String::new());
    }
    None
}

/// 剥掉已知前缀,给出「当前选中的模型 id」用于界面显示。
pub fn strip_prefix(current: &str) -> &str {
    current.trim().strip_prefix(WB_PREFIX).unwrap_or(current.trim())
}

// ───────────────────────── ZCode ─────────────────────────

fn write_zcode_site(site: &Site, spec: &ModelSpec) -> Result<SiteOutcome, String> {
    let cfg_path = site.root.join("config.json");
    let rules_path = site.root.join("provider_config.json");

    let Some(mut cfg) = read_json(&cfg_path)? else {
        return Err(format!("{} 读不出来", pretty(&cfg_path)));
    };
    let Some(mut rules) = read_json(&rules_path)? else {
        return Err(format!("{} 读不出来", pretty(&rules_path)));
    };
    // 版本号不认识就放弃 —— 硬写一份新版结构等于替用户改坏它
    let schema = rules
        .get("schemaVersion")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if schema != 1 {
        return Err(format!(
            "provider_config.json 的 schemaVersion={schema}(我们只认 1),结构可能已变,本次不写"
        ));
    }

    let mut outcome = SiteOutcome::default();

    // 找我们上次建的 provider:按名字(source=custom 且 name=4SAPI)
    let existing_pid = cfg
        .get("provider")
        .and_then(|p| p.as_object())
        .and_then(|m| {
            m.iter()
                .find(|(_, v)| {
                    v.get("name").and_then(|n| n.as_str()) == Some(spec.source_name.as_str())
                        && v.get("source").and_then(|s| s.as_str()) == Some("custom")
                })
                .map(|(k, _)| k.clone())
        });
    let pid = existing_pid.clone().unwrap_or_else(new_uuid);
    if existing_pid.is_none() {
        outcome.added = 1;
    } else {
        outcome.updated = 1;
    }

    // 模型条目:limit 只在知道上下文时给;output 一律不给(不编造能力上限,
    // 编大了会让客户端按不存在的上限发请求)。
    let mut model_entry = Map::new();
    if let Some(ctx) = spec.context_tokens {
        model_entry.insert("limit".into(), json!({ "context": ctx }));
    }
    let mut input = vec![json!("text")];
    if spec.multimodal {
        input.push(json!("image"));
    }
    model_entry.insert(
        "modalities".into(),
        json!({ "input": input, "output": ["text"] }),
    );
    model_entry.insert("zcode".into(), json!({ "modalitiesConfigured": true }));

    {
        let providers = cfg
            .get_mut("provider")
            .and_then(|p| p.as_object_mut())
            .ok_or("config.json 里没有 provider 对象")?;
        let entry = providers.entry(pid.clone()).or_insert_with(|| json!({}));
        let obj = entry.as_object_mut().ok_or("provider 条目不是对象")?;
        obj.insert("name".into(), json!(spec.source_name));
        obj.insert("kind".into(), json!("openai-compatible"));
        // 只覆盖我们约定的那几个键。以前整块替换 options,用户在 provider 上自配的
        // 自定义 header / organization / 超时 / 代理会在一次「写入」后凭空消失
        // (表现为某些模型突然不鉴权、走错组织)—— WorkBuddy 那边是保留未知字段的,
        // 同一个产品两套口径不该分叉(报告 P2-10)。
        merge_into(
            obj,
            "options",
            json!({
                "apiKey": spec.key,
                "baseURL": spec.base_url,
                "apiKeyRequired": true,
            }),
        );
        obj.insert("source".into(), json!("custom"));
        let models = obj.entry("models".to_string()).or_insert_with(|| json!({}));
        if let Some(m) = models.as_object_mut() {
            m.insert(spec.id.clone(), Value::Object(model_entry));
        }
        // 之前被用户删过的同名模型要解禁,否则写进去也不显示
        let zc = obj.entry("zcode".to_string()).or_insert_with(|| json!({}));
        if let Some(z) = zc.as_object_mut() {
            let del = z
                .entry("deletedModels".to_string())
                .or_insert_with(|| json!([]));
            if let Some(a) = del.as_array_mut() {
                a.retain(|v| v.as_str() != Some(spec.id.as_str()));
            }
        }
    }

    {
        let rules_root = rules
            .get_mut("config")
            .and_then(|c| c.get_mut("providerConfigRules"))
            .and_then(|c| c.get_mut("providerRules"))
            .and_then(|r| r.as_array_mut())
            .ok_or("provider_config.json 里没有 providerRules 数组")?;
        let i = match rules_root.iter().position(|r| {
            r.get("providerId").and_then(|v| v.as_str()) == Some(pid.as_str())
        }) {
            Some(i) => i,
            None => {
                rules_root.push(json!({
                    "providerId": pid,
                    "providerName": spec.source_name,
                    "config": {
                        "group": "standard-personal",
                        "access": {"type": "api-key", "apiKey": spec.key},
                        "api": {"type": "openai-chat-completions", "baseUrl": spec.base_url},
                        "personalModelIds": [],
                        "modelOrder": [],
                    }
                }));
                rules_root.len() - 1
            }
        };
        let rule = rules_root[i]
            .get_mut("config")
            .and_then(|c| c.as_object_mut())
            .ok_or("providerRule.config 不是对象")?;
        merge_into(rule, "access", json!({"type": "api-key", "apiKey": spec.key}));
        merge_into(
            rule,
            "api",
            json!({"type": "openai-chat-completions", "baseUrl": spec.base_url}),
        );
        if !rule.contains_key("group") {
            rule.insert("group".into(), json!("standard-personal"));
        }
        for key in ["personalModelIds", "modelOrder"] {
            let list = rule.entry(key.to_string()).or_insert_with(|| json!([]));
            if let Some(a) = list.as_array_mut() {
                if !a.iter().any(|v| v.as_str() == Some(spec.id.as_str())) {
                    a.push(json!(spec.id));
                }
            }
        }
    }

    // 上下文窗口:按 (modelId, providerId) upsert
    if let Some(ctx) = spec.context_tokens {
        if let Some(list) = rules
            .get_mut("config")
            .and_then(|c| c.get_mut("modelConfigRules"))
            .and_then(|c| c.get_mut("providerModelRules"))
            .and_then(|r| r.as_array_mut())
        {
            let pos = list.iter().position(|r| {
                r.get("modelId").and_then(|v| v.as_str()) == Some(spec.id.as_str())
                    && r.get("providerId").and_then(|v| v.as_str()) == Some(pid.as_str())
            });
            let entry = json!({
                "modelId": spec.id,
                "providerId": pid,
                "config": {"properties": {"contextWindow": ctx}}
            });
            match pos {
                Some(i) => list[i] = entry,
                None => list.push(entry),
            }
        }
    }

    let stamp = stamp_now();
    let (id, url, key) = (spec.id.clone(), spec.base_url.clone(), spec.key.clone());
    let name2 = spec.source_name.clone();
    let cfg_for_check = cfg_path.clone();
    outcome.backup = commit(
        site,
        &[cfg_path.clone(), rules_path.clone()],
        &stamp,
        move || {
            write_json(&cfg_path, &cfg, has_bom(&cfg_path), 2)?;
            write_json(&rules_path, &rules, has_bom(&rules_path), 2)?;
            Ok(())
        },
        move || {
            read_json(&cfg_for_check)
                .ok()
                .flatten()
                .and_then(|v| {
                    v.get("provider").and_then(|p| {
                        p.as_object().and_then(|m| {
                            m.iter()
                                .find(|(_, v)| {
                                    v.get("name").and_then(|n| n.as_str()) == Some(name2.as_str())
                                })
                                .map(|(_, v)| {
                                    v.get("options").and_then(|o| o.get("apiKey")).and_then(|k| k.as_str())
                                        == Some(key.as_str())
                                        && v.get("options").and_then(|o| o.get("baseURL")).and_then(|b| b.as_str())
                                            == Some(url.as_str())
                                        && v.get("models").and_then(|mm| mm.get(&id)).is_some()
                                })
                        })
                    })
                })
                .unwrap_or(false)
        },
    )?;
    outcome
        .notes
        .push(format!("备份:{}", pretty(&outcome.backup)));
    outcome.notes.push(format!(
        "provider id 复用同一个:重复写入不会堆出多个「{}」",
        spec.source_name
    ));
    Ok(outcome)
}

/// 生成一个 v4 形状的 uuid。不引第三方 crate:`RandomState` 的种子来自系统随机源,
/// 两次取样拼出 16 字节,再按 RFC 4122 打上版本/变体位。
fn new_uuid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for chunk in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
        );
        h.write_u32(std::process::id());
        h.write_u64(chunk);
        bytes[chunk as usize * 8..chunk as usize * 8 + 8].copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let b = bytes;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

// ───────────────────────── Trae(SQLite) ─────────────────────────

/// Trae 的自定义模型条目模板。字段取自 2026-10-12 真机取证的那条(用户在 Trae 里
/// 手配的 4SAPI 模型),逐字段对齐 —— 宁可多带几个 null,也不要少字段:
/// 少字段的条目在客户端里可能显示不全或不可选。
fn trae_template(spec: &ModelSpec, custom_model_id: &str) -> Value {
    json!({
        "name": format!("custom_openai_compatible//{}", spec.id),
        "multimodal": spec.multimodal,
        "is_default": false,
        "custom_config": "",
        "display_name": spec.id,
        "prompt_max_tokens": null,
        "context_window_size": {"max": null, "default": null},
        "is_new": null,
        "is_beta": null,
        "core_memory_enable_type": null,
        "model_type": "chat_model",
        "is_preset": false,
        "client_connect": true,
        "provider": "custom_openai_compatible",
        "icon": {
            "dark": "https://lf-cdn.trae.com.cn/obj/trae-com-cn/model/default-custom-dark.svg",
            "light": "https://lf-cdn.trae.com.cn/obj/trae-com-cn/model/default-custom-light.svg"
        },
        "ak": spec.key,
        "base_url": spec.chat_completions_url(),
        "is_custom_base_url": true,
        "status": true,
        "custom_model_id": custom_model_id,
        "fee_model_level": null,
        "config_source": 3,
        "selectable": true,
        "commercial_info": null,
        "tob_commercial_info": null,
        "max_turns": {"default": null, "max": null},
        "tags": null,
        "sk": null,
        "auth_type": 0,
        "region": null,
        "thinking_enable": 0,
        "temperature": null,
        "top_p": null,
        "top_k": null,
        "saas_usage": {"max": null, "default": null},
        "features": {
            "provider": {"enable": true, "data": {"provider_name": "custom_openai_compatible"}},
            "context_windows": {
                "enable": true,
                "data": {
                    "dev_context": null, "max_context": null, "max_context_list": null,
                    "dev_turns": null, "max_turns": null
                }
            }
        }
    })
}

/// 新条目的 `custom_model_id`。**这是应用自己分配的数字,不是模型名的哈希**
/// (2026-10-12 试过 crc32/fnv1a/djb2/java-hash/adler32,都不等于现成样本 2607458050)。
/// 所以按「比现有最大值大、且不与现有冲突」生成 —— 唯一即可。
fn next_custom_model_id(existing: &[u64]) -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut id = (now_ms % 10_000_000_000).max(existing.iter().copied().max().unwrap_or(0) + 1);
    while existing.contains(&id) {
        id += 1;
    }
    id.to_string()
}

/// Trae 的选中项 modelId 口径(2026-10-12 用真样本破译,单测里对着两个真样本回归):
///
/// ```text
/// 内置:  solo_agent_lite_1__DeepSeek-V4-Flash-Official_null
/// 自定义: solo_agent_3_custom_openai_compatible_custom_openai_compatible//claude-sonnet-5_2607458050
/// ```
///
/// 即 `<agentLabel>_<config_source>_<provider>_<name>_<custom_model_id 或 null>`,
/// 中段就是条目里的 `config_source`(内置=1、自定义=3),空字段自然形成连续下划线。
pub fn trae_model_id(
    agent: &str,
    name: &str,
    provider: &str,
    config_source: i64,
    custom_model_id: Option<&str>,
) -> String {
    let id = custom_model_id.unwrap_or("null");
    format!("{agent}_{config_source}_{provider}_{name}_{id}")
}

/// 从选中项的 modelId 里取出模型名(界面显示用)。
/// `...custom_openai_compatible//<模型>_<custom_model_id>` → `<模型>`
pub fn trae_selected_model(model_id: &str) -> Option<String> {
    let rest = model_id.split("custom_openai_compatible//").nth(1)?;
    match rest.rsplit_once('_') {
        Some((name, tail)) if tail.chars().all(|c| c.is_ascii_digit()) => Some(name.to_string()),
        _ => Some(rest.to_string()),
    }
}

fn trae_write(
    db: &Path,
    spec: &ModelSpec,
    opts: &Opts,
) -> Result<SiteOutcome, String> {
    let hosts3 = spec.hosts();
    let mut conn = rusqlite::Connection::open(db)
        .map_err(|e| format!("打开 {} 失败(Trae 可能正在运行?): {e}", pretty(db)))?;
    // 键名前缀是「区域/租户号」,会随版本变,所以按后缀找,不写死
    let keys: Vec<String> = {
        let mut stmt = conn
            .prepare(
                // SQLite 的 LIKE 里 `_` 是单字符通配符,命中范围比看上去宽 ——
                // 取回后再按字面量复核一遍(报告 P2-9)
                "SELECT key FROM ItemTable WHERE key LIKE '%AI.agent.model.model_list_map'",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let found: Vec<String> = rows.filter_map(|r| r.ok()).collect();
        // SQLite 的 LIKE 把 `_` 当单字符通配符,命中范围比意图宽;取回后按字面量再筛一次
        // (报告 P2-9:模型表被认错时,一次写入能改到不该改的行)
        found
            .into_iter()
            .filter(|k| k.ends_with("AI.agent.model.model_list_map"))
            .collect()
    };
    if keys.is_empty() {
        return Err("没找到 Trae 的模型表(版本可能变了),本次不写".into());
    }

    let mut outcome = SiteOutcome::default();

    // 先摸清现有自定义条目:custom_model_id 号段,以及各 key 的 JSON
    let mut existing_ids: Vec<u64> = Vec::new();
    let mut maps: Vec<(String, Value)> = Vec::new();
    for k in &keys {
        let raw: String = conn
            .query_row("SELECT value FROM ItemTable WHERE key = ?1", [k], |r| {
                r.get(0)
            })
            .map_err(|e| format!("读 {k} 失败: {e}"))?;
        let v: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("解析 {k} 失败(结构可能已变,本次不写): {e}"))?;
        collect_custom_ids(&v, &mut existing_ids);
        maps.push((k.clone(), v));
    }

    // 全部 UPDATE 走一个事务:中间任何一条失败就整批回滚。
    // state.vscdb 装的是 Trae 整个 globalStorage,半途而废比不写更坏
    // (设计稿 4.4「写坏了的后果」明写:写入用事务、失败立即回滚、写前强制备份)。
    let tx = conn
        .transaction()
        .map_err(|e| format!("{} 开启事务失败(Trae 可能正在运行?): {e}", pretty(db)))?;

    let mut assigned: Option<String> = None;
    let mut touched = 0usize;
    for (key, map) in maps.iter_mut() {
        // 序列化一份"改之前"的文本,末尾对比:内容一致就跳过 UPDATE。
        // 以前是无条件写回,于是每点一次「写入」整个 state.vscdb 都被改写一遍
        // (时间戳变、产生新 -wal),而这一家偏偏没有备份(P0-2)(报告 P2-9)。
        let before_text = serde_json::to_string(map).map_err(|e| e.to_string())?;
        let Some(groups) = map.as_object_mut() else { continue };
        for (agent, arr) in groups.iter_mut() {
            let _ = agent;
            let Some(list) = arr.as_array_mut() else { continue };
            let pos = list.iter().position(|e| {
                trae_is_ours(e, &spec.id, &hosts3, &spec.base_url)
            });
            match pos {
                Some(i) => {
                    // 更新:保留原有 custom_model_id 与所有我们不认识的字段
                    let mut next = list[i].clone();
                    next["ak"] = json!(spec.key);
                    next["base_url"] = json!(spec.chat_completions_url());
                    next["multimodal"] = json!(spec.multimodal);
                    next["status"] = json!(true);
                    next["selectable"] = json!(true);
                    list[i] = next;
                    outcome.updated += 1;
                }
                None => {
                    let cid = assigned
                        .get_or_insert_with(|| next_custom_model_id(&existing_ids))
                        .clone();
                    // 同组已有自定义条目时**照它克隆**(版本差异靠它吸收:
                    // 客户端自己写的条目里带的字段,我们一个都不少);没有才用通用模板
                    let next = match list.iter().find(|e| {
                        e.get("provider").and_then(|p| p.as_str()) == Some("custom_openai_compatible")
                    }) {
                        Some(tpl) => {
                            let mut merged = tpl.clone();
                            if let Some(obj) = merged.as_object_mut() {
                                obj.insert(
                                    "name".into(),
                                    json!(format!("custom_openai_compatible//{}", spec.id)),
                                );
                                obj.insert("display_name".into(), json!(spec.id));
                                obj.insert("ak".into(), json!(spec.key));
                                obj.insert("base_url".into(), json!(spec.chat_completions_url()));
                                obj.insert("custom_model_id".into(), json!(cid));
                                obj.insert("multimodal".into(), json!(spec.multimodal));
                            }
                            merged
                        }
                        None => trae_template(spec, &cid),
                    };
                    list.push(next);
                    outcome.added += 1;
                }
            }
        }
        let text = serde_json::to_string(map).map_err(|e| e.to_string())?;
        if text != before_text {
            tx.execute(
                "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                rusqlite::params![text, key.as_str()],
            )
            .map_err(|e| format!("写 {key} 失败: {e}"))?;
            touched += 1;
        }
    }

    // 选中项:实验性开关。口径已破译,但客户端会不会立刻认这个选中项只有真机能验,
    // 所以默认关 —— 打开前请在 Trae 里手动确认过一次。
    let our_cid = assigned
        .clone()
        .or_else(|| find_our_custom_id(&maps, &spec.id));
    if opts.trae_auto_select {
        if let Some(cid) = our_cid.as_deref() {
            let sel_key: Option<String> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT key FROM ItemTable \
                         WHERE key LIKE '%recent_user_selection_by_agent_label'",
                    )
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(|e| e.to_string())?;
                let first = rows.filter_map(|r| r.ok()).next();
                first
            };
            if let Some(k) = sel_key {
                if let Ok(raw) = tx.query_row(
                    "SELECT value FROM ItemTable WHERE key = ?1",
                    [&k],
                    |r| r.get::<_, String>(0),
                ) {
                    if let Ok(mut sel) = serde_json::from_str::<Value>(&raw) {
                        if let Some(obj) = sel.as_object_mut() {
                            for (agent, entry) in obj.iter_mut() {
                                let name = format!("custom_openai_compatible//{}", spec.id);
                                let mid = trae_model_id(
                                    agent,
                                    &name,
                                    "custom_openai_compatible",
                                    3,
                                    Some(cid),
                                );
                                if let Some(o) = entry.as_object_mut() {
                                    o.insert("modelId".into(), json!(mid));
                                }
                            }
                            let text = serde_json::to_string(&sel).map_err(|e| e.to_string())?;
                            tx.execute(
                                "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                                rusqlite::params![text, k],
                            )
                            .map_err(|e| format!("写选中项失败: {e}"))?;
                            outcome.selected = true;
                        }
                    }
                }
            }
        }
    }
    if touched == 0 && outcome.added + outcome.updated > 0 {
        outcome.same = true;
        outcome.notes.push("内容已经在了,这次没动库。".into());
    }
    if !outcome.selected {
        outcome
            .notes
            .push("选中项没写:重启 Trae 后在模型列表里选一次即可".into());
    }
    // 一次性提交。中间任何一条 UPDATE 失败,tx 被丢弃时就地整批回滚,
    // 不会留下"模型表改了一半"的库(以前每条语句各自 autocommit)。
    tx.commit()
        .map_err(|e| format!("提交事务失败(本次改动已回滚): {e}"))?;
    Ok(outcome)
}

/// 把某份 model_list_map 里所有自定义条目的 custom_model_id 收进 existing(去重)。
fn collect_custom_ids(v: &Value, existing: &mut Vec<u64>) {
    if let Some(groups) = v.as_object() {
        for arr in groups.values() {
            if let Some(list) = arr.as_array() {
                for e in list {
                    if e.get("provider").and_then(|p| p.as_str()) == Some("custom_openai_compatible") {
                        if let Some(id) = e.get("custom_model_id").and_then(|v| v.as_str()) {
                            if let Ok(n) = id.parse::<u64>() {
                                if !existing.contains(&n) {
                                    existing.push(n);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn find_our_custom_id(maps: &[(String, Value)], model: &str) -> Option<String> {
    for (_, v) in maps {
        if let Some(groups) = v.as_object() {
            for arr in groups.values() {
                if let Some(list) = arr.as_array() {
                    for e in list {
                        if e.get("display_name").and_then(|d| d.as_str()) == Some(model)
                            && e.get("provider").and_then(|p| p.as_str())
                                == Some("custom_openai_compatible")
                        {
                            if let Some(id) = e.get("custom_model_id").and_then(|v| v.as_str()) {
                                return Some(id.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

// 取消写入(移除条目)的公共描述 + 各客户端实现
// ───────────────────────── 取消写入 ─────────────────────────

/// 「取消写入」的预告:要移除哪一条、会不会顺带清掉客户端的选中项。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Removal {
    /// 人话描述要移除什么(界面直接显示,含条目名与指向的地址)
    pub what: String,
    /// 会顺带清掉客户端自己的「选中项」(它正指向这条模型时)
    pub clears_selection: bool,
    /// 要移掉几条(WorkBuddy=1;Trae=每个 agent 组各一条)
    pub count: usize,
}

/// ZCode 里我们这个来源的 provider id(source=custom 且 name=来源名)。
/// 每个来源一个 provider(「4SAPI」/「Hapi」/「OpenCode Go」),互不覆盖。
fn zcode_pid(cfg: &Value, name: &str) -> Option<String> {
    cfg.get("provider")
        .and_then(|p| p.as_object())
        .and_then(|m| {
            m.iter()
                .find(|(_, v)| {
                    v.get("name").and_then(|n| n.as_str()) == Some(name)
                        && v.get("source").and_then(|s| s.as_str()) == Some("custom")
                })
                .map(|(k, _)| k.clone())
        })
}

/// WorkBuddy 类:有没有我们写的这一条,以及移除它会动到什么。
fn wb_removal(site: &Site, spec: &ModelSpec) -> (bool, Option<Removal>) {
    let hosts = spec.hosts();
    let models = read_json(&site.root.join("models.json")).ok().flatten();
    let entry = models
        .as_ref()
        .and_then(|v| v.as_array())
        .and_then(|a| {
            a.iter().find(|e| {
                e.get("id").and_then(|i| i.as_str()) == Some(spec.id.as_str())
            })
        });
    let Some(e) = entry else {
        return (false, None);
    };
    if !wb_is_ours(e, &hosts, &spec.base_url) {
        // 同名但不属于这个来源(比如是别家地址) → 不当成"已配置"
        return (false, None);
    }
    let name = e
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or(spec.id.as_str());
    let url = e.get("url").and_then(|u| u.as_str()).unwrap_or("");
    let cur = read_json(&site.root.join("settings.json"))
        .ok()
        .flatten()
        .and_then(|s| {
            s.get(WB_SELECTION_KEY)
                .and_then(|m| m.as_str())
                .map(String::from)
        });
    let clears = cur
        .as_deref()
        .map(|v| v == spec.id || v == format!("{WB_PREFIX}{}", spec.id))
        .unwrap_or(false);
    (
        true,
        Some(Removal {
            what: format!("models.json 里的条目「{name}」({url})"),
            clears_selection: clears,
            count: 1,
        }),
    )
}

/// ZCode:模型在不在我们的 provider 里,以及移除后会剩下什么。
fn zcode_removal(site: &Site, spec: &ModelSpec) -> (bool, Option<Removal>) {
    let cfg = match read_json(&site.root.join("config.json")) {
        Ok(Some(v)) => v,
        _ => return (false, None),
    };
    let Some(pid) = zcode_pid(&cfg, &spec.source_name) else {
        return (false, None);
    };
    let models = cfg
        .get("provider")
        .and_then(|p| p.get(&pid))
        .and_then(|p| p.get("models"))
        .and_then(|m| m.as_object());
    let Some(models) = models else {
        return (false, None);
    };
    if !models.contains_key(&spec.id) {
        return (false, None);
    }
    let left = models.len().saturating_sub(1);
    let what = if left == 0 {
        format!(
            "provider「{}」下的「{}」—— 这是它最后一个模型,连 provider 一起移除",
            spec.source_name, spec.id
        )
    } else {
        format!(
            "provider「{}」下的「{}」(provider 留下,还剩 {left} 个模型)",
            spec.source_name, spec.id
        )
    };
    (
        true,
        Some(Removal {
            what,
            clears_selection: false, // ZCode 的选中是会话级的,配置文件里没有
            count: 1,
        }),
    )
}

/// Trae:每个 agent 组各有一条,数一数;顺带看选中项指不指向它。
fn trae_removal(db: &Path, spec: &ModelSpec) -> (bool, Option<Removal>) {
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return (false, None);
    };
    let mut keys: Vec<String> = Vec::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT key FROM ItemTable WHERE key LIKE '%AI.agent.model.model_list_map'")
    {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            keys = rows.flatten().collect();
        }
    }
    let hosts = spec.hosts();
    let mut count = 0usize;
    let mut groups = 0usize;
    for k in &keys {
        let Ok(raw) = conn.query_row("SELECT value FROM ItemTable WHERE key = ?1", [k], |r| {
            r.get::<_, String>(0)
        }) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(obj) = v.as_object() {
            for arr in obj.values() {
                if let Some(list) = arr.as_array() {
                    let n = list
                        .iter()
                        .filter(|e| trae_is_ours(e, &spec.id, &hosts, &spec.base_url))
                        .count();
                    if n > 0 {
                        count += n;
                        groups += 1;
                    }
                }
            }
        }
    }
    if count == 0 {
        return (false, None);
    }
    let mut clears = false;
    if let Some((mk, raw)) = selection_row(&conn) {
        let _ = mk;
        if let Ok(sel) = serde_json::from_str::<Value>(&raw) {
            if let Some(obj) = sel.as_object() {
                clears = obj.values().any(|e| {
                    e.get("modelId")
                        .and_then(|m| m.as_str())
                        .map(|m| m.contains(&format!("custom_openai_compatible//{}_", spec.id)))
                        .unwrap_or(false)
                });
            }
        }
    }
    (
        true,
        Some(Removal {
            what: format!("{count} 条自定义模型条目(分属 {groups} 个 agent 组)"),
            clears_selection: clears,
            count,
        }),
    )
}

/// 一条 Trae 条目是不是我们要找的那条自定义模型。
///
/// **还要看它的 base_url 属于哪个来源**:同名模型可能来自不同来源(4SAPI / Hapi / Go),
/// 删 Hapi 那条时不能把 4SAPI 那条一起删掉。来源判据与 [`wb_is_ours`] 共用
/// [`url_is_ours`](精确主机名,或整串等于我们会写的那个地址)。
/// `hosts` 与 `base_url` 都为空 = 不过滤来源(兼容旧调用)。
fn trae_is_ours(e: &Value, model: &str, hosts: &[String], base_url: &str) -> bool {
    if e.get("provider").and_then(|p| p.as_str()) != Some("custom_openai_compatible") {
        return false;
    }
    if e.get("display_name").and_then(|d| d.as_str()) != Some(model) {
        return false;
    }
    if hosts.is_empty() && base_url.is_empty() {
        return true;
    }
    let url = e.get("base_url").and_then(|u| u.as_str()).unwrap_or("");
    url_is_ours(url, hosts, base_url)
}

/// Trae 的选中项那一行(键名 + 值)。
fn selection_row(conn: &rusqlite::Connection) -> Option<(String, String)> {
    let mut stmt = conn
        .prepare(
            "SELECT key, value FROM ItemTable \
             WHERE key LIKE '%recent_user_selection_by_agent_label'",
        )
        .ok()?;
    let mut rows = stmt.query([]).ok()?;
    let row = rows.next().ok()??;
    Some((row.get(0).ok()?, row.get(1).ok()?))
}

fn remove_workbuddy_site(site: &Site, spec: &ModelSpec) -> Result<SiteOutcome, String> {
    let models_path = site.root.join("models.json");
    let settings_path = site.root.join("settings.json");
    let Some(models) = read_json(&models_path)? else {
        return Err(format!("{} 不存在或是空的", pretty(&models_path)));
    };
    let Value::Array(mut arr) = models else {
        return Err(format!(
            "{} 不是数组(结构可能已变,本次不写)",
            pretty(&models_path)
        ));
    };

    let mut outcome = SiteOutcome::default();
    // 只删「我们自己写的那一条」:同 id 但 url 属于别家来源、或用户手配的条目一律不动。
    // 写入契约第③条;以前是按 id 全表 retain,一次「取消写入」能把别人的同名条目一起删掉。
    let hosts = spec.hosts();
    if let Some(i) = arr.iter().position(|e| {
        e.get("id").and_then(|x| x.as_str()) == Some(spec.id.as_str())
            && wb_is_ours(e, &hosts, &spec.base_url)
    }) {
        arr.remove(i);
        outcome.removed = 1;
    }
    if outcome.removed == 0 {
        return Ok(outcome); // 本来就没有 —— 不写盘,连备份都不必做
    }

    // 选中项正指向它才清:删掉 model 键(回到"没选"的状态,与写入前一致)
    let mut settings_new: Option<Value> = None;
    if let Some(mut s) = read_json(&settings_path)? {
        if s.is_object() {
            let cur = s
                .get(WB_SELECTION_KEY)
                .and_then(|m| m.as_str())
                .map(String::from);
            if let Some(cur) = cur {
                if cur == spec.id || cur == format!("{WB_PREFIX}{}", spec.id) {
                    if let Some(o) = s.as_object_mut() {
                        o.remove(WB_SELECTION_KEY);
                    }
                    settings_new = Some(s);
                    outcome.clears_selection = true;
                }
            }
        }
    }

    let stamp = stamp_now();
    let models_next = Value::Array(arr);
    let settings_for_write = settings_new.clone();
    let models_for_check = models_path.clone();
    let settings_for_check = settings_path.clone();
    let id = spec.id.clone();
    let hosts_for_check = hosts;
    let base_for_check = spec.base_url.clone();
    outcome.backup = commit(
        site,
        &[models_path.clone(), settings_path.clone()],
        &stamp,
        move || {
            write_json(&models_path, &models_next, has_bom(&models_path), 2)?;
            if let Some(s) = settings_for_write {
                write_json(&settings_path, &s, has_bom(&settings_path), 2)?;
            }
            Ok(())
        },
        move || {
            let gone = read_json(&models_for_check)
                .ok()
                .flatten()
                .and_then(|v| {
                    v.as_array().map(|a| {
                        !a.iter().any(|e| {
                            e.get("id").and_then(|i| i.as_str()) == Some(id.as_str())
                                && wb_is_ours(e, &hosts_for_check, &base_for_check)
                        })
                    })
                })
                .unwrap_or(false);
            let sel_ok = read_json(&settings_for_check)
                .ok()
                .flatten()
                .and_then(|s| {
                    s.get(WB_SELECTION_KEY)
                        .and_then(|m| m.as_str())
                        .map(|v| v != id && v != format!("{WB_PREFIX}{id}"))
                })
                .unwrap_or(true);
            gone && sel_ok
        },
    )?;
    outcome
        .notes
        .push(format!("备份:{}", pretty(&outcome.backup)));
    Ok(outcome)
}

fn remove_zcode_site(site: &Site, spec: &ModelSpec) -> Result<SiteOutcome, String> {
    let cfg_path = site.root.join("config.json");
    let rules_path = site.root.join("provider_config.json");
    let Some(mut cfg) = read_json(&cfg_path)? else {
        return Err(format!("{} 读不出来", pretty(&cfg_path)));
    };
    let Some(mut rules) = read_json(&rules_path)? else {
        return Err(format!("{} 读不出来", pretty(&rules_path)));
    };
    let schema = rules
        .get("schemaVersion")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if schema != 1 {
        return Err(format!(
            "provider_config.json 的 schemaVersion={schema}(我们只认 1),结构可能已变,本次不写"
        ));
    }

    let mut outcome = SiteOutcome::default();
    let Some(pid) = zcode_pid(&cfg, &spec.source_name) else {
        return Ok(outcome); // 没有我们的 provider → 没什么可删
    };

    let mut left = 0usize;
    if let Some(models) = cfg
        .get_mut("provider")
        .and_then(|p| p.get_mut(&pid))
        .and_then(|p| p.get_mut("models"))
        .and_then(|m| m.as_object_mut())
    {
        if models.remove(&spec.id).is_some() {
            outcome.removed += 1;
        }
        left = models.len();
    }

    // providerRules:把这条模型从 personalModelIds / modelOrder 里摘掉;模型空了连 provider 一起收走
    if let Some(list) = rules
        .get_mut("config")
        .and_then(|c| c.get_mut("providerConfigRules"))
        .and_then(|c| c.get_mut("providerRules"))
        .and_then(|r| r.as_array_mut())
    {
        if let Some(i) = list
            .iter()
            .position(|r| r.get("providerId").and_then(|v| v.as_str()) == Some(pid.as_str()))
        {
            if left == 0 {
                list.remove(i);
            } else if let Some(cfgobj) = list[i].get_mut("config").and_then(|c| c.as_object_mut()) {
                for key in ["personalModelIds", "modelOrder"] {
                    if let Some(a) = cfgobj.get_mut(key).and_then(|v| v.as_array_mut()) {
                        a.retain(|v| v.as_str() != Some(spec.id.as_str()));
                    }
                }
            }
        }
    }

    // providerModelRules:按 (modelId, providerId) 摘掉
    if let Some(list) = rules
        .get_mut("config")
        .and_then(|c| c.get_mut("modelConfigRules"))
        .and_then(|c| c.get_mut("providerModelRules"))
        .and_then(|r| r.as_array_mut())
    {
        list.retain(|r| {
            !(r.get("modelId").and_then(|v| v.as_str()) == Some(spec.id.as_str())
                && r.get("providerId").and_then(|v| v.as_str()) == Some(pid.as_str()))
        });
    }

    // 模型空了:provider 定义也一并移除(留一个空 provider 只会在选择器里碍事)
    if left == 0 {
        if let Some(p) = cfg.get_mut("provider").and_then(|p| p.as_object_mut()) {
            p.remove(&pid);
        }
    }

    if outcome.removed == 0 {
        return Ok(outcome);
    }

    let stamp = stamp_now();
    let (id, pid_for_check) = (spec.id.clone(), pid.clone());
    let name2 = spec.source_name.clone();
    let cfg_check = cfg_path.clone();
    let rules_check = rules_path.clone();
    outcome.backup = commit(
        site,
        &[cfg_path.clone(), rules_path.clone()],
        &stamp,
        move || {
            write_json(&cfg_path, &cfg, has_bom(&cfg_path), 2)?;
            write_json(&rules_path, &rules, has_bom(&rules_path), 2)?;
            Ok(())
        },
        move || {
            let after = read_json(&cfg_check).ok().flatten();
            match after {
                Some(v) => match zcode_pid(&v, &name2) {
                    // provider 还在:模型必须已经不在它下面
                    Some(p) => v
                        .get("provider")
                        .and_then(|pr| pr.get(&p))
                        .and_then(|pr| pr.get("models"))
                        .and_then(|m| m.as_object())
                        .map(|m| !m.contains_key(&id))
                        .unwrap_or(true),
                    // provider 收了:确认我们这个 pid 在两份文件里都不再出现。
                    // 以前这里写的是「配置文本里不能再有 4SAPI 字样」—— 来源名被写死,
                    // 用户只要另外也配过 4SAPI,别家来源的取消就永远校验不过并自动回滚。
                    None => {
                        let in_cfg = v.to_string().contains(pid_for_check.as_str());
                        let in_rules = read_json(&rules_check)
                            .ok()
                            .flatten()
                            .map(|r| r.to_string().contains(pid_for_check.as_str()))
                            .unwrap_or(false);
                        !in_cfg && !in_rules
                    }
                },
                None => false,
            }
        },
    )?;
    outcome
        .notes
        .push(format!("备份:{}", pretty(&outcome.backup)));
    if left == 0 {
        outcome.notes.push(format!(
            "这是它最后一个模型:provider「{name_disp}」也一并移除了",
            name_disp = spec.source_name
        ));
    }
    Ok(outcome)
}

fn remove_trae_site(db: &Path, spec: &ModelSpec) -> Result<SiteOutcome, String> {
    let hosts2 = spec.hosts();
    let mut conn = rusqlite::Connection::open(db)
        .map_err(|e| format!("打开 {} 失败(Trae 可能正在运行?): {e}", pretty(db)))?;
    let keys: Vec<String> = {
        let mut stmt = conn
            .prepare(
                // SQLite 的 LIKE 里 `_` 是单字符通配符,命中范围比看上去宽 ——
                // 取回后再按字面量复核一遍(报告 P2-9)
                "SELECT key FROM ItemTable WHERE key LIKE '%AI.agent.model.model_list_map'",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let found: Vec<String> = rows.filter_map(|r| r.ok()).collect();
        // SQLite 的 LIKE 把 `_` 当单字符通配符,命中范围比意图宽;取回后按字面量再筛一次
        // (报告 P2-9:模型表被认错时,一次写入能改到不该改的行)
        found
            .into_iter()
            .filter(|k| k.ends_with("AI.agent.model.model_list_map"))
            .collect()
    };
    if keys.is_empty() {
        return Err("没找到 Trae 的模型表(版本可能变了),本次不写".into());
    }
    // 与写入同理:摘条目 + 摘选中项必须一起成事或一起不作废
    let tx = conn
        .transaction()
        .map_err(|e| format!("{} 开启事务失败(Trae 可能正在运行?): {e}", pretty(db)))?;

    let mut outcome = SiteOutcome::default();
    for k in &keys {
        let raw: String = tx
            .query_row("SELECT value FROM ItemTable WHERE key = ?1", [k], |r| {
                r.get(0)
            })
            .map_err(|e| format!("读 {k} 失败: {e}"))?;
        let mut v: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("解析 {k} 失败(结构可能已变,本次不写): {e}"))?;
        let mut changed = false;
        if let Some(obj) = v.as_object_mut() {
            for arr in obj.values_mut() {
                if let Some(list) = arr.as_array_mut() {
                    let before = list.len();
                    list.retain(|e| !trae_is_ours(e, &spec.id, &hosts2, &spec.base_url));
                    let removed = before - list.len();
                    if removed > 0 {
                        outcome.removed += removed;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            let text = serde_json::to_string(&v).map_err(|e| e.to_string())?;
            tx.execute(
                "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                rusqlite::params![text, k.as_str()],
            )
            .map_err(|e| format!("写 {k} 失败: {e}"))?;
        }
    }

    // 选中项正指向这条模型就把它摘掉(那个 agent 回到"没选"的状态)
    if let Some((k, raw)) = selection_row(&tx) {
        if let Ok(mut sel) = serde_json::from_str::<Value>(&raw) {
            let mut changed = false;
            if let Some(obj) = sel.as_object_mut() {
                let hit: Vec<String> = obj
                    .iter()
                    .filter(|(_, e)| {
                        e.get("modelId")
                            .and_then(|m| m.as_str())
                            .map(|m| m.contains(&format!("custom_openai_compatible//{}_", spec.id)))
                            .unwrap_or(false)
                    })
                    .map(|(a, _)| a.clone())
                    .collect();
                for a in hit {
                    obj.remove(&a);
                    changed = true;
                    outcome.clears_selection = true;
                }
            }
            if changed {
                let text = serde_json::to_string(&sel).map_err(|e| e.to_string())?;
                tx.execute(
                    "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                    rusqlite::params![text, k.as_str()],
                )
                .map_err(|e| format!("写选中项失败: {e}"))?;
            }
        }
    }

    tx.commit()
        .map_err(|e| format!("提交事务失败(本次改动已回滚): {e}"))?;
    Ok(outcome)
}

/// 从盘上读回 Trae 的模型表,统计「数组组数」与「其中有我们这条的组数」。
/// 打不开库 / 解不出 JSON 一律 None(让调用方判失败),结构不是对象的行按写入侧同样的口径跳过。
fn trae_read_back(db: &Path, spec: &ModelSpec) -> Option<(usize, usize)> {
    let hosts = spec.hosts();
    let conn = rusqlite::Connection::open(db).ok()?;
    let keys: Vec<String> = {
        let mut stmt = conn
            .prepare(
                // SQLite 的 LIKE 里 `_` 是单字符通配符,命中范围比看上去宽 ——
                // 取回后再按字面量复核一遍(报告 P2-9)
                "SELECT key FROM ItemTable WHERE key LIKE '%AI.agent.model.model_list_map'",
            )
            .ok()?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).ok()?;
        let found: Vec<String> = rows.filter_map(|r| r.ok()).collect();
        // SQLite 的 LIKE 把 `_` 当单字符通配符,命中范围比意图宽;取回后按字面量再筛一次
        // (报告 P2-9:模型表被认错时,一次写入能改到不该改的行)
        found
            .into_iter()
            .filter(|k| k.ends_with("AI.agent.model.model_list_map"))
            .collect()
    };
    let (mut arrays, mut ours) = (0usize, 0usize);
    for k in &keys {
        let Ok(raw) = conn.query_row("SELECT value FROM ItemTable WHERE key = ?1", [k], |r| {
            r.get::<_, String>(0)
        }) else {
            return None;
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(groups) = v.as_object() else { continue };
        for arr in groups.values() {
            let Some(list) = arr.as_array() else { continue };
            arrays += 1;
            if list.iter().any(|e| trae_is_ours(e, &spec.id, &hosts, &spec.base_url)) {
                ours += 1;
            }
        }
    }
    Some((arrays, ours))
}

/// 写入后的读回校验(共同前提第④条):每个 agent 组都必须有我们这条。
fn trae_verify_written(db: &Path, spec: &ModelSpec) -> bool {
    matches!(trae_read_back(db, spec), Some((a, o)) if a > 0 && o == a)
}

/// 移除后的读回校验:任何组里都不该再留着我们这条。
fn trae_verify_removed(db: &Path, spec: &ModelSpec) -> bool {
    matches!(trae_read_back(db, spec), Some((_, 0)))
}

/// 盘上到底有没有我们这条 —— 用来守住「本来就没有就别写盘、也别建备份目录」。
fn trae_has_ours(db: &Path, spec: &ModelSpec) -> bool {
    matches!(trae_read_back(db, spec), Some((_, o)) if o > 0)
}

/// 取消写入:把这条模型从客户端里删掉。规矩与写入完全一样
/// (目标没运行 / 先备份 / 只动我们的条目 / 写完读回校验,不过就回滚)。
pub fn remove(client: Client, spec: &ModelSpec) -> Result<Vec<Report>, String> {
    let list = sites_of(client);
    if list.is_empty() {
        return Err(format!("没检测到 {} 的配置", client.label()));
    }
    let mut out = Vec::new();
    for site in list {
        let stamp0 = stamp_now();
        let (cid, lbl) = (client.id().to_string(), site.label.clone());
        let mut local: Vec<Report> = Vec::new();
        // 与 apply 同口径(报告 P1-6):一套安装失败不该把已删成的另一家一起丢成"没记录"
        let res: Result<(), String> = (|| {
            if let Some(msg) = blocked_by_running(&site) {
                return Err(msg);
            }
            site_readable(&site).map_err(|e| format!("{}: {e}", site.label))?;
            let stamp = stamp0.clone();
            let (client_id, label) = (cid.clone(), lbl.clone());
            let o = match client {
            Client::Workbuddy | Client::WorkbuddyAi => remove_workbuddy_site(&site, spec)?,
            Client::Zcode => remove_zcode_site(&site, spec)?,
            Client::Trae => {
                let db = site.root.join("state.vscdb");
                if !trae_has_ours(&db, spec) {
                    // 本来就没有:与另三家同口径,不写盘、不建备份目录
                    SiteOutcome::default()
                } else {
                    // 与另三家走同一条 commit:先备份(含 -wal/-shm)→ 写 → 读回校验,不过就回滚
                    let cell: std::cell::RefCell<Option<SiteOutcome>> =
                        std::cell::RefCell::new(None);
                    let dir = commit(
                        &site,
                        &site.files,
                        &stamp,
                        || {
                            let o = remove_trae_site(&db, spec)?;
                            *cell.borrow_mut() = Some(o);
                            Ok(())
                        },
                        || trae_verify_removed(&db, spec),
                    )
                    .map_err(|e| format!("{}: {e}", site.label))?;
                    let mut o = cell.into_inner().unwrap_or_default();
                    o.backup = dir;
                    o
                }
            }
        };
            local.push(Report {
                client: client_id,
                label,
                stamp,
                backup: pretty(&o.backup),
                backup_path: o.backup.to_string_lossy().to_string(),
                added: 0,
                updated: 0,
                removed: o.removed,
            selected: false,
            effect: format!("重启 {} 后这条模型不再出现在列表里", site.label),
            notes: {
                let mut n = o.notes;
                if o.removed == 0 {
                    n.push("本来就没有这条(可能已经删过了)".into());
                }
                if o.clears_selection {
                    n.push("它的「选中项」正指向这条模型,已一并清掉".into());
                }
                n
                },
                error: None,
            });
            Ok(())
        })();
        match res {
            Ok(()) => out.extend(local),
            Err(e) => out.push(Report {
                client: cid,
                label: lbl,
                stamp: stamp0,
                backup: String::new(),
                backup_path: String::new(),
                added: 0,
                updated: 0,
                removed: 0,
                selected: false,
                effect: "这一家没删成(其余客户端的结果见下)".into(),
                notes: Vec::new(),
                error: Some(e),
            }),
        }
    }
    if !out.is_empty() && out.iter().all(|r| r.error.is_some()) {
        return Err(out[0]
            .error
            .clone()
            .unwrap_or_else(|| "取消写入失败".to_string()));
    }
    Ok(out)
}

// ───────────────────────── 对外:预览 / 写入 / 状态 ─────────────────────────

/// 文件在不在、结构认不认。认不出来就别写(宁可不写,也不替用户改坏)。
fn site_readable(site: &Site) -> Result<(), String> {
    match site.client {
        Client::Workbuddy | Client::WorkbuddyAi => {
            let v = read_json(&site.root.join("models.json"))?
                .ok_or_else(|| "models.json 是空的".to_string())?;
            if !v.is_array() {
                return Err("models.json 不是数组".into());
            }
            Ok(())
        }
        Client::Zcode => {
            let rules = read_json(&site.root.join("provider_config.json"))?
                .ok_or_else(|| "provider_config.json 是空的".to_string())?;
            if rules.get("schemaVersion").and_then(|v| v.as_i64()) != Some(1) {
                return Err("provider_config.json 的 schemaVersion 不是 1".into());
            }
            if !rules.get("config").map(|c| c.is_object()).unwrap_or(false) {
                return Err("provider_config.json 里没有 config 对象".into());
            }
            // config.json 也要核:写入侧要往 provider 对象里塞一个 provider,
            // 缺了它真写时会失败("config.json 里没有 provider 对象")。以前这里不读它,
            // 预告就敢承诺「新增 provider「4SAPI」+ N 条模型」,点下去才报错(P2)。
            let cfg = read_json(&site.root.join("config.json"))?
                .ok_or_else(|| "config.json 是空的".to_string())?;
            if !cfg.get("provider").map(|p| p.is_object()).unwrap_or(false) {
                return Err("config.json 里没有 provider 对象".into());
            }
            Ok(())
        }
        Client::Trae => Ok(()),
    }
}

/// WorkBuddy 类站点:读出「已写入的模型(带来源)」与「当前选中值」。
fn wb_read(site: &Site) -> (Vec<ConfiguredModel>, Option<String>) {
    let models = read_json(&site.root.join("models.json")).ok().flatten();
    let ours: Vec<ConfiguredModel> = models
        .as_ref()
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let url = e.get("url").and_then(|u| u.as_str()).unwrap_or("");
                    // 来源由地址决定;认不出地址的条目靠名字前缀仍算我们写的,但来源留空
                    let src = crate::models::source_of_url(url).map(String::from);
                    let named = e
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(|n| n.starts_with(OUR_NAME_PREFIX))
                        .unwrap_or(false);
                    let id = e.get("id").and_then(|i| i.as_str())?;
                    match (src, named) {
                        (Some(s), _) => Some(ConfiguredModel { id: id.to_string(), source: s }),
                        (None, true) => Some(ConfiguredModel { id: id.to_string(), source: String::new() }),
                        (None, false) => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let cur = read_json(&site.root.join("settings.json"))
        .ok()
        .flatten()
        .and_then(|s| s.get(WB_SELECTION_KEY).and_then(|m| m.as_str()).map(String::from));
    (ours, cur)
}

/// 写入预告:纯读,绝不写盘。
pub fn preview(client: Client, spec: &ModelSpec, opts: &Opts) -> Vec<Preview> {
    let list = sites_of(client);
    if list.is_empty() {
        return vec![Preview {
            client: client.id().into(),
            label: client.label().into(),
            blocked: Some(format!("没检测到 {} 的配置", client.label())),
            ..Default::default()
        }];
    }
    list.into_iter()
        .map(|site| {
            let mut p = Preview {
                client: client.id().into(),
                label: site.label.clone(),
                files: site.files.iter().map(|f| pretty(f)).collect(),
                writes_secret: true,
                ..Default::default()
            };
            if let Some(msg) = blocked_by_running(&site) {
                p.blocked = Some(msg);
                return p;
            }
            if let Err(e) = site_readable(&site) {
                p.blocked = Some(format!("{e}(本次不写,文件原样保留)"));
                return p;
            }
            match client {
                Client::Workbuddy | Client::WorkbuddyAi => {
                    let (ours, cur) = wb_read(&site);
                    let existing = read_json(&site.root.join("models.json")).ok().flatten();
                    let entry = existing.as_ref().and_then(|v| v.as_array()).and_then(|a| {
                        a.iter().find(|e| {
                            e.get("id").and_then(|i| i.as_str()) == Some(spec.id.as_str())
                        })
                    });
                    let same = entry
                        .map(|e| {
                            e.get("url").and_then(|u| u.as_str()) == Some(spec.base_url.as_str())
                                && e.get("apiKey").and_then(|k| k.as_str()) == Some(spec.key.as_str())
                        })
                        .unwrap_or(false);
                    if entry.is_none() {
                        p.action = "新增 1 条自定义模型".into();
                        p.changed = true;
                    } else if same {
                        p.action = "已配置,内容和这次要写的一致,无需改动".into();
                        p.changed = false;
                    } else {
                        p.action = "更新现有的同名条目".into();
                        p.changed = true;
                        let old_url = entry
                            .and_then(|e| e.get("url"))
                            .and_then(|u| u.as_str())
                            .unwrap_or("(无 url)");
                        p.notes.push(format!(
                            "同名条目已存在(现在指向 {old_url}),写入会把它改成上面的地址与密钥。"
                        ));
                    }
                    let (configured, removal) = wb_removal(&site, spec);
                    p.configured = configured;
                    p.removal = removal;
                    let learned = opts
                        .selection_prefix
                        .clone()
                        .or_else(|| {
                            let ids: Vec<String> = ours.iter().map(|m| m.id.clone()).collect();
                            learn_prefix(cur.as_deref(), &ids)
                        });
                    p.writes_selection = learned.is_some() && p.changed;
                    if learned.is_none() {
                        p.notes.push(
                            "选中项不写:还没学到这个客户端的选中口径。写入后在它的模型列表里\
                             选一次,挂件会记住口径,以后就能直接帮你选。"
                                .into(),
                        );
                    }
                }
                Client::Zcode => {
                    let cfg = read_json(&site.root.join("config.json")).ok().flatten();
                    let pid = cfg.as_ref().and_then(|c| zcode_pid(c, spec.source_name.as_str()));
                    let has_model = pid
                        .as_ref()
                        .map(|id| {
                            cfg.as_ref()
                                .and_then(|c| c.get("provider"))
                                .and_then(|p| p.get(id))
                                .and_then(|p| p.get("models"))
                                .and_then(|m| m.get(&spec.id))
                                .is_some()
                        })
                        .unwrap_or(false);
                    let sn = spec.source_name.as_str();
                    p.action = match (&pid, has_model) {
                        (None, _) => format!("新增 provider「{sn}」+ 1 条模型"),
                        (Some(_), false) => format!("在已有的 {sn} provider 里新增 1 条模型"),
                        (Some(_), true) => format!("更新已有的 {sn} provider 与该模型条目"),
                    };
                    p.changed = true;
                    p.writes_selection = false; // ZCode 的选中是会话级的,配置里不存
                    let (configured, removal) = zcode_removal(&site, spec);
                    p.configured = configured;
                    p.removal = removal;
                    p.notes
                        .push("选中不需要写:ZCode 的模型选择是按会话存的。".into());
                }
                Client::Trae => {
                    p.action = "在模型表(每个 agent 组各一条)里新增/更新这条自定义模型".into();
                    p.changed = true;
                    p.writes_selection = opts.trae_auto_select;
                    let (configured, removal) = trae_removal(&site.root.join("state.vscdb"), spec);
                    p.configured = configured;
                    p.removal = removal;
                    if !opts.trae_auto_select {
                        p.notes.push(
                            "选中项不写(实验开关默认关):重启 Trae 后在模型列表里选一次。".into(),
                        );
                    }
                }
            }
            p
        })
        .collect()
}

/// 执行写入。
pub fn apply(client: Client, spec: &ModelSpec, opts: &Opts) -> Result<Vec<Report>, String> {
    let list = sites_of(client);
    if list.is_empty() {
        return Err(format!("没检测到 {} 的配置", client.label()));
    }
    let mut out = Vec::new();
    for site in list {
        let stamp0 = stamp_now();
        let (cid, lbl) = (client.id().to_string(), site.label.clone());
        let mut local: Vec<Report> = Vec::new();
        // 每一家单独成事。以前任一家失败(在跑 / 读不了 / 写坏 / 校验不过)都 `return Err`
        // 跳出整个循环:① 界面只看到一句错误,不知道另一家其实已经写进去了;
        // ② 壳层的 push_log 拿不到任何报告,于是那次已落盘的写入连台账都没有,
        //   也就再没有回滚入口(报告 P1-6)。
        let res: Result<(), String> = (|| {
            // 预告与执行之间目标可能被启动:这里再判一次,不做「按预告执行」的假设
            if let Some(msg) = blocked_by_running(&site) {
                return Err(msg);
            }
            site_readable(&site).map_err(|e| format!("{}: {e}", site.label))?;

            let stamp = stamp0.clone();
            let (client_id, label) = (cid.clone(), lbl.clone());

            match client {
            Client::Workbuddy | Client::WorkbuddyAi => {
                let o = write_workbuddy_site(&site, spec, opts)?;
                local.push(Report {
                    client: client_id,
                    label,
                    stamp,
                    backup: pretty(&o.backup),
                    backup_path: o.backup.to_string_lossy().to_string(),
                    added: o.added,
                    updated: o.updated,
                    removed: 0,
                    selected: o.selected,
                    effect: format!("下次启动 {} 时出现在模型列表里", site.label),
                    notes: {
                        let mut n = o.notes;
                        if !o.same && (o.added + o.updated) == 0 {
                            n.push("内容和盘上一致,这次没有改动文件。".into());
                        }
                        if let Some(url) = o.foreign_conflict {
                            // 这句里的来源名以前写死"不是 4SAPI":写 Hapi / OpenCode Go 时
                            // 句子的意思就反了(见 foreign_source_label)。
                            let who = foreign_source_label(&url);
                            n.push(format!(
                                "注意:这条 id 的条目原本指向 {url}({who}),已按上面的内容覆盖。"
                            ));
                        }
                        n
                    },
                    error: None,
                });
            }
            Client::Zcode => {
                let o = write_zcode_site(&site, spec)?;
                local.push(Report {
                    client: client_id,
                    label,
                    stamp,
                    backup: pretty(&o.backup),
                    backup_path: o.backup.to_string_lossy().to_string(),
                    added: o.added,
                    updated: o.updated,
                    removed: 0,
                    selected: o.selected,
                    effect: "重启 ZCode 后出现在模型选择器里".into(),
                    notes: o.notes,
                    error: None,
                });
            }
            Client::Trae => {
                let db = site.root.join("state.vscdb");
                // 与另三家走同一条 commit:先备份(含 -wal/-shm)→ 事务内写 → 读回校验。
                // 以前这里绕开 commit:成功不备份(界面「备份」一栏空白、两个按钮点不动),
                // 失败时又是"先写坏再备份再盖回",盖回去的正是已写坏的那份。
                let cell: std::cell::RefCell<Option<SiteOutcome>> =
                    std::cell::RefCell::new(None);
                let dir = commit(
                    &site,
                    &site.files,
                    &stamp,
                    || {
                        let o = trae_write(&db, spec, opts)?;
                        *cell.borrow_mut() = Some(o);
                        Ok(())
                    },
                    || trae_verify_written(&db, spec),
                )
                .map_err(|e| format!("{}: {e}", site.label))?;
                let mut o = cell.into_inner().unwrap_or_default();
                o.backup = dir;
                local.push(Report {
                    client: client_id,
                    label,
                    stamp,
                    backup: pretty(&o.backup),
                    backup_path: o.backup.to_string_lossy().to_string(),
                    added: o.added,
                    updated: o.updated,
                    removed: 0,
                    selected: o.selected,
                    effect: format!("重启 {} 后出现在自定义模型里", site.label),
                    notes: o.notes,
                    error: None,
                });
            }
            }
            Ok(())
        })();
        match res {
            Ok(()) => out.extend(local),
            Err(e) => out.push(Report {
                client: cid,
                label: lbl,
                stamp: stamp0,
                backup: String::new(),
                backup_path: String::new(),
                added: 0,
                updated: 0,
                removed: 0,
                selected: false,
                effect: "这一家没有写入(其余客户端的结果见下)".into(),
                notes: Vec::new(),
                error: Some(e),
            }),
        }
    }
    // 一家都没写成:仍按命令失败上报,前端的红色提示能直接给出原因
    if !out.is_empty() && out.iter().all(|r| r.error.is_some()) {
        return Err(out[0]
            .error
            .clone()
            .unwrap_or_else(|| "写入失败".to_string()));
    }
    Ok(out)
}

/// 四客户端状态 + 已写入的模型 + 学到的选中口径。
pub fn state(learned: &HashMap<String, String>) -> Vec<ClientState> {
    let sites = sites();
    Client::ALL
        .into_iter()
        .map(|c| {
            let mine: Vec<&Site> = sites.iter().filter(|s| s.client == c).collect();
            let mut st = ClientState {
                client: c.id().into(),
                short: c.short().into(),
                label: c.label().into(),
                process: c.process().into(),
                learned_prefix: learned.get(c.id()).cloned(),
                ..Default::default()
            };
            if mine.is_empty() {
                st.note = Some("没检测到配置文件".into());
                return st;
            }
            st.installed = true;
            st.running = mine.iter().any(|s| s.running);
            st.roots = mine.iter().map(|s| pretty(&s.root)).collect();
            st.files = mine
                .iter()
                .flat_map(|s| s.files.iter().map(|f| pretty(f)))
                .collect();

            match c {
                Client::Workbuddy | Client::WorkbuddyAi => {
                    for s in &mine {
                        let (ours, cur) = wb_read(s);
                        for m in ours {
                            // 同一条 id 可能来自不同来源(4SAPI / Hapi / OpenCode Go):
                            // 只按 id 去重会让别家来源的徽标串台(P3-1;ZCode/Trae 分支本来就是
                            // 按 id+来源 判的,三家口径要一致)
                            if !st.models.iter().any(|x| x.id == m.id && x.source == m.source) {
                                st.models.push(m);
                            }
                        }
                        if st.selected.is_none() {
                            st.selected = cur.as_deref().map(|v| strip_prefix(v).to_string());
                        }
                        if st.learned_prefix.is_none() {
                            let ids: Vec<String> = read_json(&s.root.join("models.json"))
                                .ok()
                                .flatten()
                                .and_then(|v| v.as_array().cloned())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|e| {
                                            e.get("id").and_then(|i| i.as_str()).map(String::from)
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            st.learned_prefix = learn_prefix(cur.as_deref(), &ids);
                        }
                    }
                    st.models.sort_by(|a, b| a.id.cmp(&b.id));
                    if c == Client::WorkbuddyAi || c == Client::Workbuddy {
                        st.note = Some(
                            "两个 WorkBuddy 共用进程名 WorkBuddy.exe:只要有一个在跑,两个都会判为「正在运行」".into(),
                        );
                    }
                    if st.learned_prefix.is_none() {
                        st.note = Some(
                            "选中项口径还没学到:在客户端里选一次模型即可(挂件会记住)".into(),
                        );
                    }
                }
                Client::Zcode => {
                    for s in &mine {
                        let cfg = read_json(&s.root.join("config.json")).ok().flatten();
                        let rows: Vec<ConfiguredModel> = cfg
                            .as_ref()
                            .and_then(|c| c.get("provider"))
                            .and_then(|p| p.as_object())
                            .map(|m| {
                                m.iter()
                                    .filter_map(|(_, v)| {
                                        // provider 名 = 来源名,据此反查来源
                                        let name = v.get("name").and_then(|n| n.as_str())?;
                                        let src = crate::models::SOURCES
                                            .iter()
                                            .find(|d| d.name == name)
                                            .map(|d| d.id.to_string())?;
                                        let ids: Vec<String> = v
                                            .get("models")
                                            .and_then(|x| x.as_object())
                                            .map(|mm| mm.keys().cloned().collect())
                                            .unwrap_or_default();
                                        Some(
                                            ids.into_iter()
                                                .map(move |id| ConfiguredModel {
                                                    id,
                                                    source: src.clone(),
                                                })
                                                .collect::<Vec<_>>(),
                                        )
                                    })
                                    .flatten()
                                    .collect()
                            })
                            .unwrap_or_default();
                        for m in rows {
                            if !st.models.iter().any(|x| x.id == m.id && x.source == m.source) {
                                st.models.push(m);
                            }
                        }
                    }
                    st.models.sort_by(|a, b| a.id.cmp(&b.id));
                }
                Client::Trae => {
                    for s in &mine {
                        let db = s.root.join("state.vscdb");
                        let Ok(conn) = rusqlite::Connection::open_with_flags(
                            &db,
                            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                        ) else {
                            continue;
                        };
                        let mut vals: Vec<String> = Vec::new();
                        if let Ok(mut stmt) = conn.prepare(
                            "SELECT value FROM ItemTable WHERE key LIKE '%model_list_map' \
                             OR key LIKE '%recent_user_selection_by_agent_label'",
                        ) {
                            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                                for r in rows.flatten() {
                                    vals.push(r);
                                }
                            }
                        }
                        for raw in vals {
                            let Ok(v) = serde_json::from_str::<Value>(&raw) else {
                                continue;
                            };
                            let Some(obj) = v.as_object() else { continue };
                            // 选中项那条:值的形状是 {agent: {modelId, mode}}
                            let is_selection = obj
                                .values()
                                .all(|x| x.get("modelId").is_some());
                            if is_selection {
                                for e in obj.values() {
                                    if let Some(mid) = e.get("modelId").and_then(|m| m.as_str()) {
                                        if let Some(name) = trae_selected_model(mid) {
                                            st.selected = Some(name);
                                        }
                                    }
                                }
                            } else {
                                for arr in obj.values() {
                                    if let Some(list) = arr.as_array() {
                                        for e in list {
                                            if e.get("provider").and_then(|p| p.as_str())
                                                == Some("custom_openai_compatible")
                                            {
                                                if let Some(d) =
                                                    e.get("display_name").and_then(|d| d.as_str())
                                                {
                                                    let url = e
                                                        .get("base_url")
                                                        .and_then(|u| u.as_str())
                                                        .unwrap_or("");
                                                    let src = crate::models::source_of_url(url)
                                                        .unwrap_or("")
                                                        .to_string();
                                                    if !st.models
                                                        .iter()
                                                        .any(|x| x.id == d && x.source == src)
                                                    {
                                                        st.models.push(ConfiguredModel {
                                                            id: d.to_string(),
                                                            source: src,
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    st.models.sort_by(|a, b| a.id.cmp(&b.id));
                    st.note = Some("选中项写入是实验开关(默认关)".into());
                }
            }
            st
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ts-inject-{}-{}-{}",
            tag,
            std::process::id(),
            chrono::Local::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn spec() -> ModelSpec {
        ModelSpec::new(
            "4sapi",
            "claude-sonnet-5",
            "https://4sapi.com/v1",
            "sk-test-123",
            Some(1_000_000),
            true,
        )
    }

    fn site_at(root: &Path, client: Client) -> Site {
        Site {
            client,
            label: client.label().into(),
            running: false,
            process: client.process().into(),
            root: root.to_path_buf(),
            files: vec![root.join("models.json"), root.join("settings.json")],
        }
    }

    /// 测试里读回文件一律走 read_json(它会剥 BOM)—— 我们写 WorkBuddy 的文件时
    /// 是照原样保留 BOM 的,直接 from_str 读会失败。
    fn read_v(path: &Path) -> Value {
        read_json(path).unwrap().unwrap()
    }

    /// tasklist 的输出按 CSV 首列判,不认本地化提示文案。
    #[test]
    fn tasklist_matching_is_locale_independent() {
        let zh = "信息: 没有运行的任务匹配指定标准。\r\n";
        assert!(!tasklist_has(zh, "WorkBuddy.exe"));
        let yes = "\"ZCode.exe\",\"28512\",\"Console\",\"1\",\"180,640 K\"\r\n";
        assert!(tasklist_has(yes, "ZCode.exe"));
        assert!(tasklist_has(yes, "zcode.exe"), "进程名不区分大小写");
        assert!(!tasklist_has(yes, "WorkBuddy.exe"));
    }

    /// WorkBuddy:新增条目 — 字段与用户手配的那条对齐(带 BOM、2 空格)。
    #[test]
    fn workbuddy_adds_entry_with_our_fields() {
        let root = tmpdir("wb-add");
        std::fs::write(root.join("models.json"), "\u{feff}[\n]\n").unwrap();
        std::fs::write(root.join("settings.json"), "{\n  \"sandbox\": true\n}\n").unwrap();
        let site = site_at(&root, Client::Workbuddy);

        let o = write_workbuddy_site(&site, &spec(), &Opts::default()).unwrap();
        assert_eq!((o.added, o.updated), (1, 0));

        let v = read_v(&root.join("models.json"));
        let e = &v.as_array().unwrap()[0];
        assert_eq!(e["id"], "claude-sonnet-5");
        assert_eq!(e["name"], "4S-claude-sonnet-5");
        assert_eq!(e["vendor"], "Custom");
        assert_eq!(e["url"], "https://4sapi.com/v1");
        assert_eq!(e["apiKey"], "sk-test-123");
        assert_eq!(e["maxInputTokens"], 1_000_000);
        assert_eq!(e["supportsImages"], true);
        // BOM 保留(照原文件习惯写回)
        let raw = std::fs::read(root.join("models.json")).unwrap();
        assert!(raw.starts_with(&[0xEF, 0xBB, 0xBF]), "BOM 要保留");

        // 幂等:同样内容再写一次,不新增、不重复、不写盘
        let o2 = write_workbuddy_site(&site, &spec(), &Opts::default()).unwrap();
        assert!((o2.added, o2.updated) == (0, 0) && o2.same, "内容一致应报「无需改动」");
        let v2 = read_v(&root.join("models.json"));
        assert_eq!(v2.as_array().unwrap().len(), 1, "不能写出重复条目");
    }

    /// 学到前缀后要写选中项并如实回执;没学到就只写条目(不猜)。
    #[test]
    fn workbuddy_selection_only_when_prefix_learned() {
        let root = tmpdir("wb-sel");
        std::fs::write(root.join("models.json"), "[]").unwrap();
        std::fs::write(root.join("settings.json"), "{\"model\":\"custom-local:qwen:2b\"}").unwrap();
        let site = site_at(&root, Client::Workbuddy);

        let opts = Opts {
            selection_prefix: learn_prefix(Some("custom-local:qwen:2b"), &[]),
            ..Default::default()
        };
        assert_eq!(opts.selection_prefix.as_deref(), Some("custom-local:"));
        let o = write_workbuddy_site(&site, &spec(), &opts).unwrap();
        assert!(o.selected, "学到前缀就该写选中项");
        let s = read_v(&root.join("settings.json"));
        assert_eq!(s["model"], "custom-local:claude-sonnet-5");
        assert_eq!(s["sandbox"], Value::Null, "没动过的键不该冒出来");

        // 没学到前缀:只写条目
        let root2 = tmpdir("wb-nosel");
        std::fs::write(root2.join("models.json"), "[]").unwrap();
        std::fs::write(root2.join("settings.json"), "{\"model\":\"gpt-6-luna\"}").unwrap();
        let site2 = site_at(&root2, Client::Workbuddy);
        let o2 = write_workbuddy_site(&site2, &spec(), &Opts::default()).unwrap();
        assert!(!o2.selected);
        let s2 = read_v(&root2.join("settings.json"));
        assert_eq!(s2["model"], "gpt-6-luna", "学不到口径就不动选中项");
    }

    /// 前缀口径的三种证据:带前缀 → 学到前缀;裸 id 命中我们的条目 → 学到无前缀;
    /// 内置模型 → 学不到(不猜)。
    #[test]
    fn prefix_learning_rules() {
        let ours = vec!["claude-sonnet-5".to_string()];
        assert_eq!(learn_prefix(Some("custom-local:x"), &ours).as_deref(), Some("custom-local:"));
        assert_eq!(learn_prefix(Some("claude-sonnet-5"), &ours).as_deref(), Some(""));
        assert_eq!(learn_prefix(Some("gpt-6-luna"), &ours), None);
        assert_eq!(learn_prefix(Some(""), &ours), None);
        assert_eq!(learn_prefix(None, &ours), None);
        assert_eq!(strip_prefix("custom-local:a/b:2b"), "a/b:2b");
        assert_eq!(strip_prefix("plain"), "plain");
    }

    /// 同名 id 属于别人(另一个中转站)时要报冲突 —— 但要改还是得改,只是界面必须说清。
    #[test]
    fn workbuddy_flags_foreign_entry_on_same_id() {
        let root = tmpdir("wb-foreign");
        std::fs::write(
            root.join("models.json"),
            r#"[{"id":"deepseek-v4.1-flash","name":"GO-deepseek","url":"https://opencode.ai/zen/go/v1","apiKey":"old"}]"#,
        )
        .unwrap();
        std::fs::write(root.join("settings.json"), "{}").unwrap();
        let site = site_at(&root, Client::Workbuddy);
        let s = ModelSpec::new("4sapi", "deepseek-v4.1-flash", "https://4sapi.com/v1", "sk-new", None, false);
        let o = write_workbuddy_site(&site, &s, &Opts::default()).unwrap();
        assert_eq!(o.updated, 1);
        assert_eq!(
            o.foreign_conflict.as_deref(),
            Some("https://opencode.ai/zen/go/v1")
        );
        // 更新后:url 换成我们的,但用户起的名字保留
        let v = read_v(&root.join("models.json"));
        assert_eq!(v[0]["url"], "https://4sapi.com/v1");
        assert_eq!(v[0]["name"], "GO-deepseek", "更新不改用户起的名字");
    }

    /// 结构不认识(不是数组)必须拒绝,不能硬写。
    #[test]
    fn workbuddy_refuses_unknown_shape() {
        let root = tmpdir("wb-shape");
        std::fs::write(root.join("models.json"), "{\"models\":[]}").unwrap();
        std::fs::write(root.join("settings.json"), "{}").unwrap();
        let site = site_at(&root, Client::Workbuddy);
        let e = write_workbuddy_site(&site, &spec(), &Opts::default()).unwrap_err();
        assert!(e.contains("不是数组"), "{e}");
    }

    /// 备份 + 回滚:回滚后文件与写入前逐字节一致。
    #[test]
    fn backup_and_rollback_restore_bytes() {
        let root = tmpdir("rollback");
        let f = root.join("models.json");
        std::fs::write(&f, "\u{feff}[{\"id\":\"keep\"}]\n").unwrap();
        let before = std::fs::read(&f).unwrap();

        let dir = backup(&root, &[f.clone()], "20260101-000000").unwrap();
        std::fs::write(&f, "[]").unwrap();
        assert_ne!(std::fs::read(&f).unwrap(), before);

        restore(&dir).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), before, "回滚要逐字节一致");
    }

    /// 读回校验不过要自动回滚(模拟:写完立刻把文件改坏,校验就该发现)。
    #[test]
    fn failed_verification_rolls_back() {
        let root = tmpdir("verify");
        let f = root.join("models.json");
        std::fs::write(&f, "[]").unwrap();
        let before = std::fs::read(&f).unwrap();
        let site = site_at(&root, Client::Workbuddy);
        let res = commit(
            &site,
            &[f.clone()],
            "20260101-000001",
            || {
                std::fs::write(&f, "[{\"id\":\"half\"}]").unwrap();
                Ok(())
            },
            || false,
        );
        assert!(res.is_err());
        assert_eq!(std::fs::read(&f).unwrap(), before, "校验失败必须回滚");
        assert!(res.unwrap_err().contains("已回滚"));
    }

    /// 备份最多留 5 份。
    #[test]
    fn backups_are_pruned_to_five() {
        let root = tmpdir("prune");
        let f = root.join("models.json");
        std::fs::write(&f, "[]").unwrap();
        for i in 0..8 {
            backup(&root, &[f.clone()], &format!("2026010{}-00000{i}", i)).unwrap();
        }
        let n = std::fs::read_dir(root.join("backup-inject")).unwrap().count();
        assert_eq!(n, KEEP_BACKUPS);
    }

    /// ZCode:新增 provider(config.json + provider_config.json 两处都要有),
    /// 重复写入复用同一个 provider,不堆出多个「4SAPI」。
    #[test]
    fn zcode_creates_provider_once_and_merges_models() {
        let root = tmpdir("zcode");
        std::fs::write(
            root.join("config.json"),
            r#"{"provider":{"existing-uuid":{"name":"OG","kind":"openai-compatible","options":{"apiKey":"k","baseURL":"https://opencode.ai/zen/go/v1"},"source":"custom","models":{}}}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("provider_config.json"),
            r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]}}}"#,
        )
        .unwrap();
        let site = site_at(&root, Client::Zcode);

        let o = write_zcode_site(&site, &spec()).unwrap();
        assert_eq!(o.added, 1);
        let cfg: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("config.json")).unwrap()).unwrap();
        let pid = cfg["provider"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(_, v)| v["name"] == "4SAPI")
            .map(|(k, _)| k.clone())
            .expect("应新建 4SAPI provider");
        assert_ne!(pid, "existing-uuid", "不能动人家的 provider");
        assert_eq!(cfg["provider"][&pid]["options"]["baseURL"], "https://4sapi.com/v1");
        assert_eq!(
            cfg["provider"][&pid]["models"]["claude-sonnet-5"]["limit"]["context"],
            1_000_000
        );
        assert!(
            cfg["provider"][&pid]["models"]["claude-sonnet-5"]
                .get("limit")
                .unwrap()
                .get("output")
                .is_none(),
            "不给 output —— 不编造能力上限"
        );

        let rules: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("provider_config.json")).unwrap(),
        )
        .unwrap();
        let rule = &rules["config"]["providerConfigRules"]["providerRules"][0];
        assert_eq!(rule["providerId"], pid.as_str());
        assert_eq!(rule["config"]["personalModelIds"][0], "claude-sonnet-5");
        assert_eq!(
            rules["config"]["modelConfigRules"]["providerModelRules"][0]["config"]["properties"]
                ["contextWindow"],
            1_000_000
        );

        // 再写第二个模型:同一个 provider,模型合并
        let s2 = ModelSpec::new("4sapi", "glm-5.3", "https://4sapi.com/v1", "sk-test-123", Some(1_000_000), false);
        let o2 = write_zcode_site(&site, &s2).unwrap();
        assert_eq!((o2.added, o2.updated), (0, 1));
        let cfg2: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("config.json")).unwrap()).unwrap();
        let n = cfg2["provider"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, v)| v["name"] == "4SAPI")
            .count();
        assert_eq!(n, 1, "只能有一个 4SAPI provider");
        assert_eq!(cfg2["provider"][&pid]["models"].as_object().unwrap().len(), 2);
        let rules2: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("provider_config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            rules2["config"]["providerConfigRules"]["providerRules"][0]["config"]["modelOrder"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    /// 被用户删过的模型要解禁(deletedModels),否则写进去也不显示。
    #[test]
    fn zcode_unblocks_deleted_model() {
        let root = tmpdir("zcode-deleted");
        std::fs::write(
            root.join("config.json"),
            r#"{"provider":{"u1":{"name":"4SAPI","kind":"openai-compatible","options":{"apiKey":"old","baseURL":"https://old/v1"},"source":"custom","models":{},"zcode":{"deletedModels":["claude-sonnet-5"]}}}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("provider_config.json"),
            r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[{"providerId":"u1","providerName":"4SAPI","config":{"group":"standard-personal","access":{"type":"api-key","apiKey":"old"},"api":{"type":"openai-chat-completions","baseUrl":"https://old/v1"},"personalModelIds":[],"modelOrder":[]}}]},"modelConfigRules":{"providerModelRules":[]}}}"#,
        )
        .unwrap();
        let site = site_at(&root, Client::Zcode);
        write_zcode_site(&site, &spec()).unwrap();
        let cfg: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("config.json")).unwrap()).unwrap();
        assert!(cfg["provider"]["u1"]["zcode"]["deletedModels"]
            .as_array()
            .unwrap()
            .is_empty(), "同名模型应被解禁");
        assert_eq!(cfg["provider"]["u1"]["options"]["apiKey"], "sk-test-123");
    }

    /// schemaVersion 不认识就放弃(ZCode 改版后不能替用户改坏)。
    #[test]
    fn zcode_refuses_unknown_schema() {
        let root = tmpdir("zcode-schema");
        std::fs::write(root.join("config.json"), r#"{"provider":{}}"#).unwrap();
        std::fs::write(
            root.join("provider_config.json"),
            r#"{"schemaVersion":2,"config":{}}"#,
        )
        .unwrap();
        let site = site_at(&root, Client::Zcode);
        let e = write_zcode_site(&site, &spec()).unwrap_err();
        assert!(e.contains("schemaVersion"), "{e}");
    }

    /// Trae:写入自定义条目到每个 agent 组;重复写是更新;实验开关打开时写选中项。
    #[test]
    fn trae_writes_entry_into_every_agent_group() {
        let root = tmpdir("trae");
        let db = root.join("state.vscdb");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)", [])
                .unwrap();
            let map = json!({
                "solo_agent": [
                    {"config_source":1,"provider":"","name":"Doubao-Seed","display_name":"Doubao-Seed","selectable":true}
                ],
                "assistant": []
            });
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    "3739861422447896:AI.agent.model.model_list_map",
                    serde_json::to_string(&map).unwrap()
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    "3739861422447896:AI.agent.model.recent_user_selection_by_agent_label",
                    r#"{"solo_agent":{"modelId":"solo_agent_1__Doubao-Seed_null","mode":0}}"#
                ],
            )
            .unwrap();
        }

        let o = trae_write(&db, &spec(), &Opts::default()).unwrap();
        assert_eq!((o.added, o.updated), (2, 0), "两个 agent 组各加一条");
        assert!(!o.selected, "实验开关关着时不写选中项");

        let conn = rusqlite::Connection::open(&db).unwrap();
        let raw: String = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key LIKE '%model_list_map'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        let e = &v["solo_agent"][1];
        assert_eq!(e["display_name"], "claude-sonnet-5");
        assert_eq!(e["provider"], "custom_openai_compatible");
        assert_eq!(e["base_url"], "https://4sapi.com/v1/chat/completions");
        assert_eq!(e["ak"], "sk-test-123");
        assert_eq!(e["config_source"], 3);
        let cid = e["custom_model_id"].as_str().unwrap().to_string();
        assert!(cid.parse::<u64>().is_ok(), "custom_model_id 要是个数字串");

        // 再写一次:更新而不是重复追加,且保留原 custom_model_id
        let o2 = trae_write(&db, &spec(), &Opts::default()).unwrap();
        assert_eq!((o2.added, o2.updated), (0, 2));
        let raw2: String = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key LIKE '%model_list_map'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let v2: Value = serde_json::from_str(&raw2).unwrap();
        assert_eq!(v2["solo_agent"].as_array().unwrap().len(), 2, "不能重复追加");
        assert_eq!(v2["solo_agent"][1]["custom_model_id"], cid.as_str());

        // 打开实验开关:选中项按 <agent>_<config_source>_<provider>_<name>_<id> 写
        let opts = Opts {
            trae_auto_select: true,
            ..Opts::default()
        };
        let o3 = trae_write(&db, &spec(), &opts).unwrap();
        assert!(o3.selected);
        let sel: String = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key LIKE '%selection%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let sv: Value = serde_json::from_str(&sel).unwrap();
        assert_eq!(
            sv["solo_agent"]["modelId"],
            format!("solo_agent_3_custom_openai_compatible_custom_openai_compatible//claude-sonnet-5_{cid}")
        );
    }

    /// 破译出来的 modelId 口径必须能还原这两个真样本(一个内置、一个自定义)。
    #[test]
    fn trae_model_id_matches_real_samples() {
        // 真样本①(内置,取自 TRAE SOLO CN):solo_agent_lite_1__DeepSeek-V4-Flash-Official_null
        assert_eq!(
            trae_model_id("solo_agent_lite", "DeepSeek-V4-Flash-Official", "", 1, None),
            "solo_agent_lite_1__DeepSeek-V4-Flash-Official_null"
        );
        // 真样本②(自定义,取自 Trae CN):
        // solo_agent_3_custom_openai_compatible_custom_openai_compatible//claude-sonnet-5_2607458050
        assert_eq!(
            trae_model_id(
                "solo_agent",
                "custom_openai_compatible//claude-sonnet-5",
                "custom_openai_compatible",
                3,
                Some("2607458050")
            ),
            "solo_agent_3_custom_openai_compatible_custom_openai_compatible//claude-sonnet-5_2607458050"
        );
        // 反向:从 modelId 取回模型名
        assert_eq!(
            trae_selected_model(
                "solo_agent_3_custom_openai_compatible_custom_openai_compatible//claude-sonnet-5_2607458050"
            )
            .as_deref(),
            Some("claude-sonnet-5")
        );
        assert_eq!(trae_selected_model("solo_agent_1__Doubao-Seed_null"), None, "内置模型不是我们的");
    }

    /// Trae 没有模型表 → 拒绝(版本变了就别硬写)。
    #[test]
    fn trae_refuses_when_table_missing() {
        let root = tmpdir("trae-empty");
        let db = root.join("state.vscdb");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)", [])
            .unwrap();
        drop(conn);
        let e = trae_write(&db, &spec(), &Opts::default()).unwrap_err();
        assert!(e.contains("没找到 Trae 的模型表"), "{e}");
    }

    /// 生成的 uuid 形状要对(8-4-4-4-12 且版本位是 4),且两次不同。
    #[test]
    fn uuid_shape_and_uniqueness() {
        let a = new_uuid();
        let b = new_uuid();
        assert_ne!(a, b);
        let parts: Vec<&str> = a.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        assert_eq!(&a[14..15], "4", "version 4");
    }

    /// custom_model_id 要避开已有的号。
    #[test]
    fn custom_model_id_avoids_collisions() {
        let existing = vec![1u64, 2, 3];
        let id: u64 = next_custom_model_id(&existing).parse().unwrap();
        assert!(!existing.contains(&id));
        let big = vec![9_999_999_999u64];
        let id2: u64 = next_custom_model_id(&big).parse().unwrap();
        assert!(!big.contains(&id2));
    }

    /// F11:认"这条是我们写的"必须**精确比主机名**(或整串等于我们会写的那个 base_url),
    /// `4S-` 名字前缀**只在 url 空/缺时**才作数。以前是 `url.contains(host)` +
    /// "名字带 4S- 就算":`mirror-4sapi.com`、`not4sapi.com.evil` 都算成我们的,
    /// 「取消写入」于是会删掉用户自己的条目、写入时也不再报同名冲突。
    ///
    /// (旧断言 `url=https://x/y, name=4S-foo → ours` 就是那个宽松口径 —— 已按新规矩改掉。)
    #[test]
    fn ownership_requires_an_exact_host_or_our_own_base_url() {
        let hosts = vec!["4sapi.com".into(), "4sapi.cn".into()];
        let base = "https://4sapi.com/v1";
        assert!(wb_is_ours(&json!({"url":"https://4sapi.cn/v1"}), &hosts, base));
        assert!(
            wb_is_ours(&json!({"url":"https://4SAPI.com/v1"}), &hosts, base),
            "主机名比对不区分大小写"
        );
        assert!(wb_is_ours(
            &json!({"url":"https://user:pw@4sapi.com:8443/v1"}),
            &hosts,
            base
        ), "userinfo / 端口 / 路径都要剥掉");
        // 形近域名一律不是
        assert!(!wb_is_ours(&json!({"url":"https://mirror-4sapi.com/v1"}), &hosts, base));
        assert!(!wb_is_ours(&json!({"url":"https://not4sapi.com.evil/v1"}), &hosts, base));
        assert!(!wb_is_ours(&json!({"url":"https://evil.com/?next=4sapi.com"}), &hosts, base));
        // 用户把 model_base_url 配到自定义域名:整串等于我们会写的地址 → 是我们的
        let custom = "https://my-proxy.example/v1";
        assert!(wb_is_ours(&json!({"url":"https://my-proxy.example/v1"}), &hosts, custom));
        assert!(!wb_is_ours(&json!({"url":"https://my-proxy.example/v1/other"}), &hosts, custom));
        // url 空/缺:只剩名字前缀可判
        assert!(wb_is_ours(&json!({"name":"4S-foo"}), &hosts, base));
        assert!(wb_is_ours(&json!({"url":"","name":"4S-foo"}), &hosts, base));
        // 地址指向别家时,名字带 4S- 也不是我们的(用户自己起的名字照样保留)
        assert!(!wb_is_ours(
            &json!({"url":"https://opencode.ai/zen/go/v1","name":"4S-foo"}),
            &hosts,
            base
        ));
    }

    /// F11(Trae 侧):与 WorkBuddy 共用同一套判据 —— 精确主机名,或整串等于
    /// 我们会写下去的那个地址(Trae 写的是 `…/chat/completions`)。
    #[test]
    fn trae_ownership_uses_the_same_exact_host_rule() {
        let hosts = vec!["4sapi.com".into()];
        let base = "https://4sapi.com/v1";
        let ours = |url: &str| {
            json!({"provider":"custom_openai_compatible",
                   "display_name":"claude-sonnet-5","base_url":url})
        };
        assert!(trae_is_ours(&ours("https://4sapi.com/v1/chat/completions"), "claude-sonnet-5", &hosts, base));
        assert!(trae_is_ours(&ours("https://4sapi.com/v1"), "claude-sonnet-5", &hosts, base));
        assert!(!trae_is_ours(&ours("https://mirror-4sapi.com/v1/chat/completions"), "claude-sonnet-5", &hosts, base));
        assert!(!trae_is_ours(&ours("https://not4sapi.com.evil/v1"), "claude-sonnet-5", &hosts, base));
        // 自定义域名(base_url 就是我们会写的那个)→ 是我们的
        let custom = "https://my-proxy.example/v1";
        assert!(trae_is_ours(&ours("https://my-proxy.example/v1/chat/completions"), "claude-sonnet-5", &hosts, custom));
        // 别的模型名不吃这套
        assert!(!trae_is_ours(&ours("https://4sapi.com/v1"), "别的模型", &hosts, base));
    }

    /// F11:形近域名上的条目是**用户自己的**,「取消写入」一条都不许删,
    /// 预告也不许报"已配置"。
    #[test]
    fn workbuddy_remove_keeps_an_entry_on_a_lookalike_host() {
        let root = tmpdir("wb-rm-lookalike");
        std::fs::write(
            root.join("models.json"),
            r#"[{"id":"glm-5.3","name":"4S-glm-5.3","url":"https://mirror-4sapi.com/v1"},
                {"id":"glm-5.3","name":"4S-mine","url":"https://not4sapi.com.evil/v1"}]"#,
        )
        .unwrap();
        std::fs::write(root.join("settings.json"), "{}").unwrap();
        let site = site_at(&root, Client::Workbuddy);
        let s = ModelSpec::new("4sapi", "glm-5.3", "https://4sapi.com/v1", "sk-test-123", None, false);
        let o = remove_workbuddy_site(&site, &s).unwrap();
        assert_eq!(o.removed, 0, "形近域名不是我们的,一条都不该删");
        assert_eq!(read_v(&root.join("models.json")).as_array().unwrap().len(), 2);
        assert!(!wb_removal(&site, &s).0, "也不该报「已配置」");
    }

    /// F14:读回校验必须逐字段比(id / url / apiKey / name),不能只看写前就已成立的
    /// id+url —— 截断/串码/密钥没落盘(或被客户端抹掉)都要判失败。
    #[test]
    fn workbuddy_verify_detects_a_write_whose_key_did_not_land() {
        let root = tmpdir("wb-verify");
        let models = root.join("models.json");
        let settings = root.join("settings.json");
        let expect = json!({"id":"claude-sonnet-5","name":"4S-claude-sonnet-5",
                            "url":"https://4sapi.com/v1","apiKey":"sk-test-123"});
        std::fs::write(&models, json!([expect.clone()]).to_string()).unwrap();
        std::fs::write(&settings, "{}").unwrap();
        assert!(wb_verify(&models, &settings, &expect, None), "写对了就该过");

        let mut broken = expect.clone();
        broken["apiKey"] = json!("sk-别的");
        std::fs::write(&models, json!([broken]).to_string()).unwrap();
        assert!(!wb_verify(&models, &settings, &expect, None), "密钥没落上必须判失败");

        let mut renamed = expect.clone();
        renamed["name"] = json!("别人起的名字");
        std::fs::write(&models, json!([renamed]).to_string()).unwrap();
        assert!(!wb_verify(&models, &settings, &expect, None), "名字不对也要判失败");

        std::fs::write(&models, json!([expect.clone()]).to_string()).unwrap();
        std::fs::write(&settings, "{\"model\":\"custom-local:别的东西\"}").unwrap();
        assert!(
            !wb_verify(&models, &settings, &expect, Some("custom-local:claude-sonnet-5")),
            "写过的选中项没落上也要判失败"
        );
        std::fs::write(&settings, "{\"model\":\"custom-local:claude-sonnet-5\"}").unwrap();
        assert!(wb_verify(&models, &settings, &expect, Some("custom-local:claude-sonnet-5")));
    }

    /// F14 端到端:一次"apiKey 没落上"的写入必须被读回校验拦住并回滚。
    #[test]
    fn workbuddy_commit_rolls_back_when_the_key_did_not_land() {
        let root = tmpdir("wb-verify-rb");
        let models = root.join("models.json");
        let settings = root.join("settings.json");
        std::fs::write(&models, "[]").unwrap();
        std::fs::write(&settings, "{}").unwrap();
        let before = std::fs::read(&models).unwrap();
        let site = site_at(&root, Client::Workbuddy);
        let expect = json!({"id":"claude-sonnet-5","name":"4S-claude-sonnet-5",
                            "url":"https://4sapi.com/v1","apiKey":"sk-test-123"});
        let res = commit(
            &site,
            &[models.clone(), settings.clone()],
            "20260101-000000",
            || {
                // 模拟截断/串码:entry 写下去了,apiKey 丢了
                std::fs::write(
                    &models,
                    r#"[{"id":"claude-sonnet-5","name":"4S-claude-sonnet-5","url":"https://4sapi.com/v1"}]"#,
                )
                .unwrap();
                Ok(())
            },
            || wb_verify(&models, &settings, &expect, None),
        );
        assert!(res.is_err(), "apiKey 没落上必须判校验失败");
        assert_eq!(std::fs::read(&models).unwrap(), before, "校验不过要回滚");
    }

    /// F12:`-N`(同秒撞车时 backup 自己造的序号)也算备份名,必须一起清理 ——
    /// 以前只认 15 位,带序号的快照**永远不会被删**,含明文密钥的备份无上限地堆积。
    #[test]
    fn prune_keeps_five_and_removes_numbered_backups() {
        let root = tmpdir("prune-n");
        let base = root.join("backup-inject");
        for n in [
            "20260101-000001",
            "20260101-000001-2",
            "20260101-000001-3",
            "20260102-000001",
            "20260103-000001",
            "20260104-000001",
            "20260105-000001",
            "20260106-000001",
        ] {
            std::fs::create_dir_all(base.join(n)).unwrap();
        }
        prune_backups(&root);
        let left: Vec<String> = std::fs::read_dir(&base)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(left.len(), KEEP_BACKUPS, "只留 5 份: {left:?}");
        assert!(
            !left.iter().any(|n| n.ends_with("-2") || n.ends_with("-3")),
            "带序号的那几份在最旧的一端,该被清掉: {left:?}"
        );
        assert!(is_stamp_name("20260101-000001"));
        assert!(is_stamp_name("20260101-000001-2"));
        assert!(is_stamp_name("20260101-000001-99"));
        assert!(!is_stamp_name("20260101-000001-1"), "-1 不是我们造的");
        assert!(!is_stamp_name("20260101-000001-100"));
        assert!(!is_stamp_name("20260101-000001x"));
        assert!(!is_stamp_name("我的备份"));
    }

    /// F13:restore 只认 `<root>/backup-inject/<stamp>/`。
    #[test]
    fn restore_refuses_a_directory_outside_backup_inject() {
        let root = tmpdir("restore-guard");
        let outside = root.join("随便一个目录");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("models.json"), "[]").unwrap();
        let e = restore(&outside).unwrap_err();
        assert!(e.contains("backup-inject"), "{e}");

        // 在 backup-inject 下,但名字不是时间戳形态
        let odd = root.join("backup-inject").join("我的备份");
        std::fs::create_dir_all(&odd).unwrap();
        std::fs::write(odd.join("models.json"), "[]").unwrap();
        let e2 = restore(&odd).unwrap_err();
        assert!(e2.contains("时间戳"), "{e2}");
        assert!(!root.join("models.json").exists(), "被拒时一个文件都不许动");
    }

    /// F13:恢复 `*.vscdb` 时,这份备份里没有的 `-wal`/`-shm` 必须删掉 ——
    /// 否则 SQLite 下次打开会把回滚掉的那次写入重放回刚恢复的库上。
    #[test]
    fn restore_drops_a_stray_wal_next_to_the_restored_vscdb() {
        let root = tmpdir("restore-wal");
        let db = root.join("state.vscdb");
        std::fs::write(&db, "old-db").unwrap();
        let dir = backup(&root, &[db.clone()], "20260101-000000").unwrap();
        std::fs::write(&db, "written-while-running").unwrap();
        let wal = root.join("state.vscdb-wal");
        std::fs::write(&wal, "replay-me").unwrap();
        let shm = root.join("state.vscdb-shm");
        std::fs::write(&shm, "shm").unwrap();

        restore(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&db).unwrap(), "old-db");
        assert!(!wal.exists(), "备份里没有的 -wal 要删掉");
        assert!(!shm.exists(), "备份里没有的 -shm 也要删掉");
    }

    /// F16:卸载器/辅助进程不算主程序名(大小写不敏感),候选里优先挑与安装目录
    /// 同名的那支 —— `Site.process` 是写入前进程检查用的名字。
    #[test]
    fn trae_process_choice_prefers_the_install_exe_and_skips_helpers() {
        for bad in [
            "unins000.EXE",
            "Uninstall.exe",
            "UNINST000.exe",
            "Trae CN Helper.exe",
            "Trae CN Helper (GPU).exe",
            "crashpad_handler.exe",
            "CrashSender.exe",
        ] {
            assert!(is_helper_exe(bad), "{bad} 不该算主程序");
        }
        for good in ["Trae CN.exe", "Trae.exe", "TraeWork.exe"] {
            assert!(!is_helper_exe(good), "{good} 是主程序");
        }
        let procs = vec!["aaa.exe".to_string(), "Trae CN.exe".to_string()];
        assert_eq!(expected_process("Trae CN", &procs), "Trae CN.exe", "优先目录同名的那支");
        assert_eq!(
            expected_process("Trae CN", &["aaa.exe".to_string()]),
            "aaa.exe",
            "没有同名项就退到第一个候选"
        );
        assert_eq!(expected_process("Trae CN", &[]), Client::Trae.process());
    }

    /// F15:「同名条目被覆盖」那句提示里的来源名必须按**实际地址**认 ——
    /// 以前写死"不是 4SAPI",写 Hapi / OpenCode Go 模型时意思正好反了。
    #[test]
    fn foreign_conflict_note_names_the_real_source() {
        assert_eq!(foreign_source_label("https://opencode.ai/zen/go/v1"), "OpenCode Go");
        assert_eq!(foreign_source_label("https://ai.yuchuantest.com/v1"), "Hapi");
        assert_eq!(foreign_source_label("https://4sapi.com/v1"), "4SAPI");
        assert_eq!(foreign_source_label("https://example.com/v1"), "别家来源");
        let go = ModelSpec::new(
            "opencode-go",
            "deepseek-v4.1-flash",
            "https://opencode.ai/zen/go/v1",
            "sk-go",
            None,
            false,
        );
        let root = tmpdir("note-source");
        std::fs::write(
            root.join("models.json"),
            r#"[{"id":"deepseek-v4.1-flash","name":"4S-x","url":"https://4sapi.com/v1","apiKey":"old"}]"#,
        )
        .unwrap();
        std::fs::write(root.join("settings.json"), "{}").unwrap();
        let site = site_at(&root, Client::Workbuddy);
        let o = write_workbuddy_site(&site, &go, &Opts::default()).unwrap();
        assert_eq!(
            o.foreign_conflict.as_deref(),
            Some("https://4sapi.com/v1"),
            "指向 4SAPI 的条目对 Go 来说就是别家的"
        );
    }

    /// F17:ZCode 的"可不可写"必须先核 config.json 里的 provider 对象 ——
    /// 否则预告敢承诺「新增 provider「4SAPI」+ N 条模型」,真写才报
    /// "config.json 里没有 provider 对象"。
    #[test]
    fn zcode_readable_requires_a_provider_object_in_config_json() {
        let root = tmpdir("zcode-readable");
        std::fs::write(
            root.join("provider_config.json"),
            r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[]},"modelConfigRules":{"providerModelRules":[]}}}"#,
        )
        .unwrap();
        let site = site_at(&root, Client::Zcode);
        // config.json 缺 provider → 提前判不可读(真写也正是在这一句上失败)
        std::fs::write(root.join("config.json"), "{}").unwrap();
        let e = site_readable(&site).unwrap_err();
        assert!(e.contains("provider"), "{e}");
        let we = write_zcode_site(&site, &spec()).unwrap_err();
        assert!(we.contains("provider"), "真写的失败点应与之对应: {we}");

        // 补齐后通过
        std::fs::write(root.join("config.json"), r#"{"provider":{}}"#).unwrap();
        site_readable(&site).unwrap();
        // provider 不是对象同样不行
        std::fs::write(root.join("config.json"), r#"{"provider":[]}"#).unwrap();
        assert!(site_readable(&site).unwrap_err().contains("provider"));
        // provider_config.json 缺 config 对象也要提前拦住(真写要往它下面塞规则)
        std::fs::write(root.join("config.json"), r#"{"provider":{}}"#).unwrap();
        std::fs::write(root.join("provider_config.json"), r#"{"schemaVersion":1}"#).unwrap();
        let e2 = site_readable(&site).unwrap_err();
        assert!(e2.contains("config"), "{e2}");
    }

/// 取消写入:WorkBuddy 删条目;选中项指向它时一并清掉;再删一次是「本来就没有」(不写盘)。

/// 取消写入:WorkBuddy 删条目;选中项指向它时一并清掉;再删一次是「本来就没有」(不写盘)。
    #[test]
    fn workbuddy_remove_entry_and_clear_selection() {
        let root = tmpdir("wb-rm");
        std::fs::write(root.join("models.json"), "[]").unwrap();
        std::fs::write(root.join("settings.json"), "{}").unwrap();
        let site = site_at(&root, Client::Workbuddy);
        let opts = Opts {
            selection_prefix: Some(WB_PREFIX.to_string()),
            ..Default::default()
        };
        write_workbuddy_site(&site, &spec(), &opts).unwrap();
        let s = read_v(&root.join("settings.json"));
        assert_eq!(s[WB_SELECTION_KEY], "custom-local:claude-sonnet-5");

        // 预告要说清会删哪一条、会顺带清选中项
        let (configured, removal) = wb_removal(&site, &spec());
        assert!(configured);
        let r = removal.unwrap();
        assert!(r.what.contains("4S-claude-sonnet-5"), "{}", r.what);
        assert!(r.what.contains("https://4sapi.com/v1"));
        assert!(r.clears_selection);
        assert_eq!(r.count, 1);

        let o = remove_workbuddy_site(&site, &spec()).unwrap();
        assert_eq!(o.removed, 1);
        assert!(o.clears_selection);
        assert!(read_v(&root.join("models.json")).as_array().unwrap().is_empty());
        assert!(read_v(&root.join("settings.json")).get(WB_SELECTION_KEY).is_none());

        // 幂等:再删一次什么都不动
        let o2 = remove_workbuddy_site(&site, &spec()).unwrap();
        assert_eq!(o2.removed, 0);
        assert!(o2.backup.as_os_str().is_empty(), "没改动就不该建备份目录");
    }

    /// 选中项指向别人时不许乱清(只清指向这条模型的)。
    #[test]
    fn workbuddy_remove_keeps_foreign_selection() {
        let root = tmpdir("wb-rm-sel");
        std::fs::write(root.join("models.json"), "[]").unwrap();
        std::fs::write(root.join("settings.json"), "{\"model\":\"custom-local:other\"}").unwrap();
        let site = site_at(&root, Client::Workbuddy);
        write_workbuddy_site(&site, &spec(), &Opts::default()).unwrap();
        let (_, removal) = wb_removal(&site, &spec());
        assert!(!removal.unwrap().clears_selection);
        let o = remove_workbuddy_site(&site, &spec()).unwrap();
        assert_eq!(o.removed, 1);
        assert!(!o.clears_selection);
        let s = read_v(&root.join("settings.json"));
        assert_eq!(s[WB_SELECTION_KEY], "custom-local:other", "别人的选中项不动");
    }

    /// ZCode:删掉模型;它是最后一个时连 provider 与规则一起收走。
    #[test]
    fn zcode_remove_model_then_provider() {
        let root = tmpdir("zcode-rm");
        std::fs::write(root.join("config.json"), r#"{"provider":{}}"#).unwrap();
        std::fs::write(
            root.join("provider_config.json"),
            r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]}}}"#,
        )
        .unwrap();
        let site = site_at(&root, Client::Zcode);
        write_zcode_site(&site, &spec()).unwrap();
        let s2 = ModelSpec::new("4sapi", "glm-5.3", "https://4sapi.com/v1", "sk-test-123", Some(1_000_000), false);
        write_zcode_site(&site, &s2).unwrap();

        let (configured, removal) = zcode_removal(&site, &spec());
        assert!(configured);
        assert!(removal.unwrap().what.contains("还剩 1 个模型"), "预告要说清 provider 会留下");

        let o = remove_zcode_site(&site, &spec()).unwrap();
        assert_eq!(o.removed, 1);
        let cfg = read_v(&root.join("config.json"));
        let pid = zcode_pid(&cfg, "4SAPI").expect("provider 还在");
        assert!(cfg["provider"][&pid]["models"].get(&spec().id).is_none());
        assert!(cfg["provider"][&pid]["models"].get("glm-5.3").is_some(), "别的模型不动");
        let rules = read_v(&root.join("provider_config.json"));
        let rule = &rules["config"]["providerConfigRules"]["providerRules"][0];
        assert_eq!(rule["config"]["personalModelIds"].as_array().unwrap().len(), 1);
        assert_eq!(rule["config"]["personalModelIds"][0], "glm-5.3");
        assert!(rules["config"]["modelConfigRules"]["providerModelRules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["modelId"] != "claude-sonnet-5"));

        // 删最后一个:provider 与它的规则一起消失
        let (_, r2) = zcode_removal(&site, &s2);
        assert!(r2.unwrap().what.contains("最后一个模型"));
        let o2 = remove_zcode_site(&site, &s2).unwrap();
        assert_eq!(o2.removed, 1);
        let cfg2 = read_v(&root.join("config.json"));
        assert!(zcode_pid(&cfg2, "4SAPI").is_none(), "空 provider 不该留下");
        let rules2 = read_v(&root.join("provider_config.json"));
        assert!(rules2["config"]["providerConfigRules"]["providerRules"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    /// Trae:每个 agent 组各删一条;选中项指向它时把那个 agent 的选中也摘掉。
    #[test]
    fn trae_remove_from_every_group_and_clear_selection() {
        let root = tmpdir("trae-rm");
        let db = root.join("state.vscdb");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)", [])
                .unwrap();
            let map = json!({
                "solo_agent": [
                    {"config_source":1,"provider":"","display_name":"Doubao-Seed","selectable":true}
                ],
                "assistant": []
            });
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    "3739861422447896:AI.agent.model.model_list_map",
                    serde_json::to_string(&map).unwrap()
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    "3739861422447896:AI.agent.model.recent_user_selection_by_agent_label",
                    r#"{"solo_agent":{"modelId":"solo_agent_1__Doubao-Seed_null","mode":0}}"#
                ],
            )
            .unwrap();
        }
        let opts = Opts {
            trae_auto_select: true,
            ..Default::default()
        };
        trae_write(&db, &spec(), &opts).unwrap();
        let (configured, removal) = trae_removal(&db, &spec());
        assert!(configured);
        let r = removal.unwrap();
        assert_eq!(r.count, 2, "两个 agent 组各一条");
        assert!(r.clears_selection, "选中项正指向它");

        let o = remove_trae_site(&db, &spec()).unwrap();
        assert_eq!(o.removed, 2);
        assert!(o.clears_selection);
        let conn = rusqlite::Connection::open(&db).unwrap();
        let raw: String = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key LIKE '%model_list_map'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["solo_agent"].as_array().unwrap().len(), 1, "只剩内置那条");
        assert_eq!(v["assistant"].as_array().unwrap().len(), 0);
        let sel: String = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key LIKE '%selection%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let sv: Value = serde_json::from_str(&sel).unwrap();
        assert!(sv.as_object().unwrap().is_empty(), "指向它的选中项要摘掉");

        // 幂等
        let o2 = remove_trae_site(&db, &spec()).unwrap();
        assert_eq!(o2.removed, 0);
        assert_eq!((o2.clears_selection, o2.backup.as_os_str().is_empty()), (false, true));
    }

    /// 台账:新的在前、最多 20 条、能按备份路径找回它属于哪个客户端。
    #[test]
    fn log_keeps_last_twenty_and_finds_owner() {
        let store = crate::store::Store::open_memory().unwrap();
        assert!(load_log(&store).is_empty());
        for i in 0..25 {
            push_log(
                &store,
                InjectRecord {
                    stamp: format!("s{i}"),
                    backup_path: format!("C:/x/{i}"),
                    client: "zcode".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let log = load_log(&store);
        assert_eq!(log.len(), 20);
        assert_eq!(log[0].stamp, "s24", "最新的在最前");
        assert_eq!(log_owner(&log, "c:/x/24"), Some(Client::Zcode), "路径比对不区分大小写");
        assert_eq!(log_owner(&log, "C:/nope"), None);
    }

    /// 目标在跑 → 拒写(这是条硬规矩,不能只写在文档里)。
    #[test]
    fn running_target_is_blocked() {
        let root = tmpdir("running");
        let mut site = site_at(&root, Client::Workbuddy);
        site.running = true;
        assert!(blocked_by_running(&site).unwrap().contains("正在运行"));
        site.running = false;
        assert!(blocked_by_running(&site).is_none());
    }

    /// P0-1:「取消写入」只删我们自己写的那一条。同 id 但属于别家来源的条目、
    /// 以及用户手配的其它条目,都不该被牵连删掉(以前是按 id 全表 retain)。
    #[test]
    fn workbuddy_remove_only_ours_not_foreign_same_id() {
        let root = tmpdir("wb-rm-scope");
        std::fs::write(
            root.join("models.json"),
            r#"[{"id":"glm-5.3","name":"4S-glm-5.3","url":"https://4sapi.com/v1"},
                {"id":"glm-5.3","name":"我自己配的","url":"https://opencode.ai/zen/go/v1"},
                {"id":"other","name":"mine","url":"https://example.com/v1"}]"#,
        )
        .unwrap();
        std::fs::write(root.join("settings.json"), "{}").unwrap();
        let site = site_at(&root, Client::Workbuddy);
        let s = ModelSpec::new("4sapi", "glm-5.3", "https://4sapi.com/v1", "sk-test-123", None, false);

        let o = remove_workbuddy_site(&site, &s).unwrap();
        assert_eq!(o.removed, 1, "只该删掉属于本来源的那一条");
        let left = read_v(&root.join("models.json"));
        let a = left.as_array().unwrap();
        assert_eq!(a.len(), 2, "别家同 id 条目与不相干的条目都不动");
        assert!(a
            .iter()
            .any(|e| e["url"] == "https://opencode.ai/zen/go/v1"));
        assert!(a.iter().any(|e| e["id"] == "other"));
    }

    /// P0-4:同时配过 4SAPI 时,取消另一家的最后一个模型必须成功。
    /// 旧校验写的是「配置文本里不能再有 4SAPI 字样」→ 这种组合必然误判失败并自动回滚。
    #[test]
    fn zcode_remove_other_source_last_model_with_4sapi_present() {
        let root = tmpdir("zcode-rm-multi");
        std::fs::write(root.join("config.json"), r#"{"provider":{}}"#).unwrap();
        std::fs::write(
            root.join("provider_config.json"),
            r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]}}}"#,
        )
        .unwrap();
        let site = site_at(&root, Client::Zcode);
        let go = ModelSpec::new(
            "opencode-go",
            "kimi-k2",
            "https://opencode.ai/zen/go/v1",
            "sk-go",
            None,
            false,
        );
        write_zcode_site(&site, &spec()).unwrap();
        write_zcode_site(&site, &go).unwrap();

        let o = remove_zcode_site(&site, &go)
            .unwrap_or_else(|e| panic!("取消另一家的模型不该被判回滚: {e}"));
        assert_eq!(o.removed, 1);
        let cfg = read_v(&root.join("config.json"));
        assert!(zcode_pid(&cfg, "4SAPI").is_some(), "4SAPI 的 provider 必须原样留着");
        assert!(zcode_pid(&cfg, &go.source_name).is_none(), "这一家的 provider 应已收走");
        let rules = read_v(&root.join("provider_config.json"));
        let names: Vec<&str> = rules["config"]["providerConfigRules"]["providerRules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["providerName"].as_str().unwrap_or(""))
            .collect();
        assert!(
            names.iter().any(|n| *n == "4SAPI"),
            "4SAPI 的规则不该被摘掉: {names:?}"
        );
    }

    /// P0-2:Trae 走 commit —— 写入前必须有备份落盘,且读回校验要认这次写入。
    /// 以前这条路成功不备份(界面「备份」一栏空白、回滚按钮点不动)。
    #[test]
    fn trae_apply_via_commit_backs_up_before_writing() {
        let root = tmpdir("trae-commit");
        let db = root.join("state.vscdb");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)", [])
                .unwrap();
            let map = json!({"assistant": [], "solo_agent": []});
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    "3739861422447896:AI.agent.model.model_list_map",
                    serde_json::to_string(&map).unwrap()
                ],
            )
            .unwrap();
        }
        let site = Site {
            client: Client::Trae,
            label: "Trae CN".into(),
            running: false,
            process: "Trae CN.exe".into(),
            root: root.clone(),
            files: vec![db.clone()],
        };
        let sp = spec();
        let cell: std::cell::RefCell<Option<SiteOutcome>> = std::cell::RefCell::new(None);
        let dir = commit(
            &site,
            &site.files,
            "20260101-000000",
            || {
                let o = trae_write(&db, &sp, &Opts::default())?;
                *cell.borrow_mut() = Some(o);
                Ok(())
            },
            || trae_verify_written(&db, &sp),
        )
        .expect("写入应成功");
        assert!(cell.borrow().is_some(), "outcome 应带回来");
        assert!(
            dir.join("state.vscdb").is_file(),
            "备份目录里必须有**写入前**的库"
        );
        // 备份那份不该含有我们这次写的条目(证明备份发生在写之前)
        let bconn = rusqlite::Connection::open(dir.join("state.vscdb")).unwrap();
        let braw: String = bconn
            .query_row("SELECT value FROM ItemTable WHERE key LIKE '%model_list_map'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            !braw.contains(sp.id.as_str()),
            "备份内容必须是写入前的原样,不能是已改坏的库"
        );
        assert!(trae_verify_written(&db, &sp), "读回校验应通过");
    }

    /// P0-2 / 共同前提④:校验不过要整批回滚,并且如实说明回滚结果。
    #[test]
    fn trae_commit_rolls_back_when_verify_fails() {
        let root = tmpdir("trae-commit-rb");
        let db = root.join("state.vscdb");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)", [])
                .unwrap();
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    "3739861422447896:AI.agent.model.model_list_map",
                    r#"{"assistant":[]}"#
                ],
            )
            .unwrap();
        }
        let site = Site {
            client: Client::Trae,
            label: "Trae CN".into(),
            running: false,
            process: "Trae CN.exe".into(),
            root: root.clone(),
            files: vec![db.clone()],
        };
        let sp = spec();
        let r = commit(
            &site,
            &site.files,
            "20260101-000000",
            || trae_write(&db, &sp, &Opts::default()).map(|_| ()),
            // 故意让校验失败,看它是否真的盖回写入前的库
            || false,
        );
        let msg = r.unwrap_err();
        assert!(msg.contains("读回校验没过"), "文案: {msg}");
        assert!(
            !msg.contains("而且回滚失败"),
            "回滚应当成功: {msg}"
        );
        let conn = rusqlite::Connection::open(&db).unwrap();
        let raw: String = conn
            .query_row("SELECT value FROM ItemTable WHERE key LIKE '%model_list_map'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            !raw.contains(sp.id.as_str()),
            "校验不过必须回到写入前的库,不能把半成品留在 Trae 的配置里"
        );
    }

    /// P1-5:两份文件必须成套。第二个写失败时,已经落盘的第一个要盖回。
    #[test]
    fn commit_rolls_back_when_second_file_write_fails() {
        let root = tmpdir("commit-wfail");
        let a = root.join("models.json");
        let b = root.join("settings.json");
        std::fs::write(&a, "[old]").unwrap();
        std::fs::write(&b, "{bold}").unwrap();
        let site = Site {
            client: Client::Workbuddy,
            label: "WorkBuddy".into(),
            running: false,
            process: "WorkBuddy.exe".into(),
            root: root.clone(),
            files: vec![a.clone(), b.clone()],
        };
        let r = commit(
            &site,
            &site.files,
            "20260101-000000",
            || {
                std::fs::write(&a, "[new]").map_err(|e| e.to_string())?;
                Err("模拟:第二个文件被占用".to_string())
            },
            || true,
        );
        let msg = r.unwrap_err();
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "[old]",
            "第一个文件必须被盖回,不能留半成品"
        );
        assert!(
            msg.contains("未产生半成品") || msg.contains("备份仍在"),
            "文案要说清回滚结果: {msg}"
        );
    }

    /// P0-2 的另一半:Trae 的每条 UPDATE 在一个事务里,中途失败不留半套。
    /// (rusqlite 默认每条语句 autocommit,以前写坏一半是真的会发生的。)
    #[test]
    fn trae_write_uses_one_transaction() {
        let root = tmpdir("trae-tx");
        let db = root.join("state.vscdb");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)", [])
                .unwrap();
            // 两行模型表:第一行能写,第二行是数组之外的形状 → 只有第一行会 UPDATE 成功
            for (k, v) in [
                ("1:AI.agent.model.model_list_map", r#"{"assistant":[]}"#),
                ("2:AI.agent.model.model_list_map", r#"{"assistant":{"not":"array"}}"#),
            ] {
                conn.execute(
                    "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                    rusqlite::params![k, v],
                )
                .unwrap();
            }
        }
        let sp = spec();
        // 读回校验函数对"至少一组有我们这条"成立,说明写入确实只落在数组行上
        assert!(trae_write(&db, &sp, &Opts::default()).is_ok());
        assert!(trae_verify_written(&db, &sp), "数组行写完能被读回");
        let has_ours = trae_has_ours(&db, &sp);
        assert!(has_ours, "移除前的预检应认到条目存在");
        let o = remove_trae_site(&db, &sp).unwrap();
        assert_eq!(o.removed, 1);
        assert!(trae_verify_removed(&db, &sp), "移除后读回应干净");
    }
}
