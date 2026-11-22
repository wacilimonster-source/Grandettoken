//! 把选中的 4SAPI 模型写进本地客户端配置 —— 四份写入契约。
//!
//! 四个客户端存模型配置的地方完全不同,但共用四条**共同前提**(缺一不写):
//!
//! ```text
//! ① 目标进程不在运行   —— 它们是内存态落盘:边跑边写会在退出时被覆盖(SQLite 还可能写坏)
//! ② 先备份(DPAPI 加密) —— <配置目录>/backup-inject/<时间戳>/,含 SQLite 的 -wal/-shm,保留最近 5 份
//! ③ 只增改自己写的那条 —— 不碰账号、登录态、内置模型、别人的 provider
//! ④ 写完读回校验       —— 不过就自动回滚到备份,并如实报错
//! ```
//!
//! 密钥处理:挂件自己**不存**任何密钥副本 —— 只从 Windows 凭据管理器现读现写,
//! 不进数据库、不进日志、不进台账。
//!
//! "不进备份"这句以前写得太满(报告 P2-17 之外的 P2-7):回滚必须留快照,而这些客户端
//! 配置文件里的密钥本来就是明文。**2026-11-12 起每份快照都用 Windows DPAPI 加密后
//! 落盘**(见 [`crate::backup_crypto`]),密钥由当前账户托管、不落盘;升级前的旧明文快照
//! 会在下一次写入时原地加密。快照仍是最多 KEEP_BACKUPS 份,换账户 / 重装后无法解密 ——
//! 界面与写入预告都必须如实说明,既不能讲"不进备份",也不能讲"绝对安全"。
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

/// 各来源在客户端里的显示名前缀。三家前缀不同,同一份模型列表里一眼看得出
/// 这条来自 4SAPI / Hapi / OpenCode Go(用户口径:4s 与 opencodego 要显示成不同的名字)。
/// 认不出来的来源退回通用前缀。
pub fn name_prefix(source: &str) -> &'static str {
    match source {
        "4sapi" => OUR_NAME_PREFIX,
        "hapi" => "HP-",
        "opencode-go" => "OG ",
        _ => OUR_NAME_PREFIX,
    }
}

/// 一条模型写进客户端时的显示名。
/// · `4SAPI` / `Hapi`:前缀 + 模型 id(`4S-claude-sonnet-5` / `HP-gpt-6-luna`);
/// · `OpenCode Go`:用目录里那份名字(`OG DeepSeek V4.1 Flash`),目录里没有就退回 id。
pub fn display_name_of(source: &str, id: &str) -> String {
    let prefix = name_prefix(source);
    match source {
        "opencode-go" => {
            let pretty = catalog_display(id);
            if pretty == id {
                format!("{prefix}{id}")
            } else {
                format!("{prefix}{pretty}")
            }
        }
        _ => format!("{prefix}{id}"),
    }
}

/// 目录里那个模型的人话名字(`deepseek-v4.1-flash` → `DeepSeek V4.1 Flash`)。
/// 单独列一份:目录 json 里的 `name` 字段不能拿来算,只能照抄(见 `DSH_CATALOG`)。
fn catalog_display(id: &str) -> String {
    CATALOG_DISPLAY
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, v)| (*v).to_string())
        .unwrap_or_else(|| id.to_string())
}

/// pi-ai 目录里的模型显示名(`name` 字段),照抄。
const CATALOG_DISPLAY: &[(&str, &str)] = &[
    ("claude-haiku-5-5", "Claude Haiku 5.5"),
    ("minimax-m3", "MiniMax-M3"),
    ("qwen3.7-plus", "Qwen3.7 Plus"),
    ("qwen3.8-flash", "Qwen3.8 Flash"),
    ("qwen3.8-max", "Qwen3.8 Max"),
    ("deepseek-v4-flash", "DeepSeek V4 Flash"),
    ("deepseek-v4-flash-vision-exp", "DeepSeek V4 Flash Vision Exp"),
    ("deepseek-v4-pro", "DeepSeek V4 Pro (New)"),
    ("deepseek-v4.1-flash", "DeepSeek V4.1 Flash"),
    ("glm-5.2", "GLM-5.2"),
    ("glm-5.3", "GLM-5.3"),
    ("glm-5.3-flash", "GLM-5.3-Flash"),
    ("hy3", "Hy3"),
    ("hy4-preview", "Hy4 preview"),
    ("kimi-k2.7-code", "Kimi K2.7 Code"),
    ("kimi-k3", "Kimi K3"),
    ("longcat-2.0", "LongCat-2.0"),
    ("longcat-2.5-preview-free", "LongCat 2.5 Preview Free"),
    ("mimo-v2.5", "MiMo V2.5"),
    ("mimo-v2.5-pro", "MiMo V2.5 Pro"),
    ("mimo-v2.6-flash", "MiMo-V2.6-Flash"),
    ("mimo-v2.6-pro", "MiMo-V2.6-Pro"),
    ("minimax-m2.7", "MiniMax-M2.7"),
    ("space-bunny", "Space Bunny"),
    ("gpt-5.6-luna", "GPT-5.6 Luna"),
    ("gpt-6-luna", "GPT-6 Luna"),
    ("grok-4.6", "Grok 4.6"),
    ("grok-4.7", "Grok 4.7"),
    ("muse-spark-1.2-contributor", "Muse Spark 1.2 Contributor"),
    ("muse-spark-1.3-contributor", "Muse Spark 1.3 Contributor"),
];

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
    Dsh,
}

impl Client {
    pub const ALL: [Client; 5] = [
        Client::Trae,
        Client::Workbuddy,
        Client::WorkbuddyAi,
        Client::Zcode,
        Client::Dsh,
    ];

    /// 界面上的短标签(模型行里那几个小按钮)。
    pub fn short(self) -> &'static str {
        match self {
            Client::Trae => "T",
            Client::Workbuddy => "WB",
            Client::WorkbuddyAi => "WBA",
            Client::Zcode => "ZC",
            Client::Dsh => "DSH",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Client::Trae => "Trae",
            Client::Workbuddy => "WorkBuddy",
            Client::WorkbuddyAi => "WorkBuddy AI",
            Client::Zcode => "ZCode",
            Client::Dsh => "DeepSeek Harness",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Client::Trae => "trae",
            Client::Workbuddy => "workbuddy",
            Client::WorkbuddyAi => "workbuddy-ai",
            Client::Zcode => "zcode",
            Client::Dsh => "dsh",
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
            Client::Dsh => "DeepSeek Harness.exe",
        }
    }

    /// 「目标在跑就不能写」这条规矩适不适用。
    ///
    /// 前四家是内存态落盘:边跑边写会在它们退出时被覆盖,所以一律拒写。
    /// DSH 不一样 —— 它的 profile 按请求解析,配置改动在下一个请求生效;
    /// 它自己的 settings / credentials 两层都**收敛外部编辑**(配置编辑器在派生候选前
    /// 先采纳外部改动,凭据存储在同一把写锁下合并行编辑),所以边跑边写是它认的用法,
    /// 挂件也照这个口径走(见 DESIGN.md 第 12 节「DSH 契约」)。
    pub fn blocks_when_running(self) -> bool {
        !matches!(self, Client::Dsh)
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

    // DeepSeek Harness:harness home 下挑一个 profile。两份文件不在同一层,
    // 所以 root 是 harness home、回滚靠备份清单(见 backup_layered)。
    if let Some(home) = dsh_home() {
        if let Some(profile) = dsh_profile(&home) {
            let patch = dsh_patch_of(&home, &profile);
            let cred = dsh_creds_of(&home);
            out.push(Site {
                client: Client::Dsh,
                label: format!("{} · {profile}", Client::Dsh.label()),
                running: is_running(Client::Dsh.process(), &mut probed),
                process: Client::Dsh.process().to_string(),
                root: home.clone(),
                files: vec![patch, cred],
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
    /// 客户端里显示的名字(`4S-<id>` / `HP-<id>` / `OG <目录名>`;用户手配的也是这个口径)
    pub display_name: String,
    /// 上下文窗口(来自价目表 tags);未知就 None —— 不编造
    pub context_tokens: Option<i64>,
    /// `https://<域名>/v1`(挂件实测能取数的那个入口)
    pub base_url: String,
    /// 明文密钥
    pub key: String,
    pub multimodal: bool,
    /// API protocol selected for this source/model pair. OpenCode Go models
    /// may use different wire formats; an unknown value is never auto-written.
    pub api_protocol: Option<crate::models::ApiProtocol>,
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
            // 各来源前缀不同:`4S-` / `HP-` / `OG `(见 name_prefix)
            display_name: display_name_of(source, id),
            context_tokens,
            // 设置里这一项是自由文本:厂商文档常直接给 `.../v1/chat/completions`。
            // 收口成"到 /v1 为止",否则 Trae 会拿到双重端点、另三家拿到带
            // /chat/completions 的错值,四个客户端的模型全部点不动(报告 P2-11)。
            base_url: Self::normalize_base(base_url),
            key: key.to_string(),
            multimodal,
            api_protocol: Some(crate::models::ApiProtocol::OpenAiChatCompletions),
        }
    }

    /// Use the documented protocol and endpoint for a source/model pair.
    pub fn with_api_protocol(mut self, protocol: Option<crate::models::ApiProtocol>) -> Self {
        self.api_protocol = protocol;
        if self.source == "opencode-go" {
            self.base_url = protocol
                .map(|p| p.base_url().to_string())
                .unwrap_or_default();
        }
        self
    }

    pub fn endpoint_url(&self) -> Option<String> {
        self.api_protocol
            .map(|p| p.endpoint(&self.base_url, &self.id))
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

/// Prevent a model from being added through a client that sends the wrong wire
/// protocol. DSH can use its installed Go catalog natively and supports explicit
/// custom routes for Chat Completions, Responses, and Anthropic Messages.
fn protocol_blocked(client: Client, spec: &ModelSpec) -> Option<String> {
    if spec.source != "opencode-go" {
        return None;
    }
    if client == Client::Dsh && dsh_catalog_route("opencode-go", &spec.id) {
        return None;
    }
    match spec.api_protocol {
        Some(crate::models::ApiProtocol::OpenAiChatCompletions) => None,
        Some(protocol) if client == Client::Dsh && protocol.dsh_api().is_some() => None,
        Some(protocol) if client == Client::Dsh => Some(format!(
            "{} 使用 {}，DeepSeek Harness 的自定义接入不支持该协议；本次未写入。",
            spec.id,
            protocol.label()
        )),
        Some(protocol) => Some(format!(
            "{} 使用 {}，而 {} 的自动接入目前只支持 OpenAI Chat Completions；本次未写入。",
            spec.id,
            protocol.label(),
            client.label()
        )),
        None => Some(format!(
            "OpenCode Go 官网端点文档未标注 {} 的 API 协议，无法确认端点；本次未自动写入 {}。",
            spec.id,
            client.label()
        )),
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
/// DSH 是唯一例外:它收敛外部编辑、profile 按请求解析,所以边跑边写是它认的用法
/// (见 `Client::blocks_when_running`)。
pub fn blocked_by_running(site: &Site) -> Option<String> {
    if !site.running || !site.client.blocks_when_running() {
        return None;
    }
    Some(format!(
        "{} 正在运行 —— 它的配置是内存态落盘,边跑边写会在退出时被覆盖。先退出它再点写入。",
        site.label
    ))
}

#[path = "inject_io.rs"]
mod inject_io;
pub use inject_io::restore;
use inject_io::*;

#[path = "inject_workbuddy.rs"]
mod inject_workbuddy;
pub use inject_workbuddy::{learn_prefix, name_is_ours, strip_prefix};
use inject_workbuddy::*;

#[path = "inject_zcode.rs"]
mod inject_zcode;
use inject_zcode::*;

#[path = "inject_trae.rs"]
mod inject_trae;
pub use inject_trae::{trae_model_id, trae_selected_model};
use inject_trae::*;

#[path = "inject_dsh.rs"]
mod inject_dsh;
pub use inject_dsh::CatalogRoute;
use inject_dsh::*;

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
            Client::Dsh => remove_dsh_site(&site, spec)?,
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
            effect: if client == Client::Dsh {
                "DSH 下一个请求就不在列表里了(不用重启)".to_string()
            } else {
                format!("重启 {} 后这条模型不再出现在列表里", site.label)
            },
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
        Client::Dsh => {
            let (patch, cred) = dsh_paths(site)?;
            if patch.exists() {
                let text = dsh_read_text(&patch);
                if !text.trim().is_empty() {
                    let root = crate::yamledge::parse(&crate::yamledge::split(&text))?;
                    if root.kind != crate::yamledge::Kind::Sequence {
                        return Err(format!(
                            "{} 的根不是「条目数组」,结构可能已变",
                            pretty(&patch)
                        ));
                    }
                }
            }
            if cred.exists() {
                let text = dsh_read_text(&cred);
                if !text.trim().is_empty() {
                    let root = crate::yamledge::parse(&crate::yamledge::split(&text))?;
                    if root.kind != crate::yamledge::Kind::Mapping {
                        return Err(format!("{} 的根不是 mapping", pretty(&cred)));
                    }
                }
            }
            Ok(())
        }
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
                        .map(name_is_ours)
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
            if let Some(msg) = protocol_blocked(client, spec) {
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
                Client::Dsh => {
                    let plan = match dsh_plan(&site, spec) {
                        Ok(v) => v,
                        Err(e) => {
                            // 结构认不出来 / 值不是块 —— 点下去必然失败,预告里就说清楚
                            p.blocked = Some(format!("{e}(本次不写)"));
                            return p;
                        }
                    };
                    let route = dsh_route_for_protocol(
                        &spec.source,
                        &spec.id,
                        spec.api_protocol,
                    );
                    p.action = if plan.same {
                        "已配置,内容和这次要写的一致,无需改动".into()
                    } else if plan.patch_action == PatchAction::Updated {
                        format!("更新 llm-pi-ai 里的 route「{route}」与这条模型")
                    } else {
                        format!("在 llm-pi-ai 里新增 route「{route}」与这条模型")
                    };
                    p.changed = !plan.same;
                    p.writes_selection = false;
                    p.notes.push(format!(
                        "落点:route「{route}」写在 {},密钥以 {} 的名字写进 {}(两份文件一次备份、一起校验)",
                        pretty(&plan.patch),
                        dsh_ref_for(&route),
                        pretty(&plan.cred)
                    ));
                    p.notes.push(format!(
                        "DSH 里的显示名:渠道「{}」用「{}」,这条模型叫「{}」。",
                        spec.source_name,
                        spec.source_name,
                        spec.display_name
                    ));
                    if plan.cred_action == CredAction::Updated {
                        p.notes.push(format!(
                            "注意:引用 {} 在凭据文件里已经存在,值与这次不同,写入会覆盖它。",
                            dsh_ref_for(&route)
                        ));
                    }
                    if dsh_catalog_route(&route, &spec.id) {
                        p.notes.push(
                            "这条模型在 pi-ai 的已安装目录里:协议、端点、上下文与图片能力都由目录给,\
                             我们只补显示名与密钥引用(不写 api/baseURL,写了反而与目录打架)。"
                                .into(),
                        );
                    } else if dsh_catalog(&dsh_route(&spec.source)).is_some() {
                        let protocol = spec
                            .api_protocol
                            .map(crate::models::ApiProtocol::label)
                            .unwrap_or("未知协议");
                        let api = spec
                            .api_protocol
                            .and_then(crate::models::ApiProtocol::dsh_api)
                            .unwrap_or("不支持");
                        p.notes.push(format!(
                            "「{}」不在 pi-ai 目录给 `{}` 的模型表里,按官网协议 {} 单独写到\
                             自建 route「{route}」(api: {api} + 对应端点)。目录模型仍留在 `{}`,\
                             两边互不影响;以后目录收录它后,再次写入会自动使用目录 route。",
                            spec.id,
                            dsh_route(&spec.source),
                            protocol,
                            dsh_route(&spec.source)
                        ));
                    } else {
                        let protocol = spec
                            .api_protocol
                            .map(crate::models::ApiProtocol::label)
                            .unwrap_or("未知协议");
                        let api = spec
                            .api_protocol
                            .and_then(crate::models::ApiProtocol::dsh_api)
                            .unwrap_or("不支持");
                        p.notes.push(
                            format!(
                                "这条 route 不在 pi-ai 的已安装目录里,按官网协议 {protocol} 写入\
                                 自定义接入(api: {api} + 对应端点)。"
                            ),
                        );
                    }
                    let (configured, removal) = dsh_removal(&site, spec);
                    p.configured = configured;
                    p.removal = removal;
                    if site.running {
                        p.notes.push(
                            "DSH 正在运行也能写(不用退出):它的 profile 按请求解析,而且它自己的\
                             设置层与凭据层都会收敛外部改动。"
                                .into(),
                        );
                    }
                    p.notes.push(
                        "选中项不写:DSH 的默认模型由 agent-default-model 那条决定。写入后回 DSH \
                         的模型选择器里选一次(挂件只读它,不替你改)。"
                            .into(),
                    );
                    if spec.source == "opencode-go" {
                        p.notes.push(
                            "若同一模型以前留在旧的 OpenCode Go 自建 route,更新时会迁移该模型;\
                             旧 route 还有其他模型时会保留它们。"
                                .into(),
                        );
                        p.notes.push(
                            "OpenCode Go 官方目前将 DeepSeek Harness 列为会话标识支持不完整的客户端;\
                             部分请求可能缺少 x-opencode-session,因此端点正确也不保证 Go 路由与用量稳定。"
                                .into(),
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
    if let Some(msg) = protocol_blocked(client, spec) {
        return Err(msg);
    }
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
            Client::Dsh => {
                let o = write_dsh_site(&site, spec)?;
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
                    effect: "DSH 下一个请求就生效(不用重启)".into(),
                    notes: {
                        let mut n = o.notes;
                        if !o.same && (o.added + o.updated) == 0 {
                            n.push("内容和盘上一致,这次没有改动文件。".into());
                        }
                        n
                    },
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
                Client::Dsh => {
                    for s in &mine {
                        let Ok((patch, _cred)) = dsh_paths(s) else {
                            continue;
                        };
                        let text = dsh_read_text(&patch);
                        for m in dsh_read_configured(&text) {
                            // 同名模型可能在两个来源里都有:只按 id 去重会让来源徽标串台
                            if !st.models.iter().any(|x| x.id == m.id && x.source == m.source) {
                                st.models.push(m);
                            }
                        }
                        if st.selected.is_none() {
                            st.selected = dsh_read_selected(&text);
                        }
                    }
                    st.models.sort_by(|a, b| a.id.cmp(&b.id));
                    st.note = Some(
                        "边跑边写:DSH 的 profile 按请求解析,不用退出它。默认模型只读不写".into(),
                    );
                }
            }
            st
        })
        .collect()
}

#[cfg(test)]
#[path = "inject_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "inject_tests_trae_dsh.rs"]
mod tests_trae_dsh;
