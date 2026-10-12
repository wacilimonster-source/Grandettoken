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
//! 密钥处理:只从 Windows 凭据管理器**现读现写**,不在挂件任何地方留副本
//! (不进数据库、不进日志、不进备份 —— 备份只备份目标客户端自己的文件)。
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
const TRAE_DIRS: [&str; 2] = ["Trae CN", "TRAE SOLO CN"];

/// 显示用路径:`C:\Users\x\...` → `~/.zcode/...`。
pub fn pretty(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if let Some(h) = home() {
        let h = h.to_string_lossy().replace('\\', "/");
        if let Some(rest) = s.strip_prefix(&h) {
            return format!("~{rest}");
        }
    }
    s
}

/// 检测本机装有的写入目标。没装的不返回(界面据「四客户端里缺哪个」显示未安装)。
pub fn sites() -> Vec<Site> {
    let mut out = Vec::new();

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
            out.push(Site {
                client: Client::Trae,
                label: dir.to_string(),
                running: process_running(Client::Trae.process()),
                process: Client::Trae.process().to_string(),
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
                running: process_running(Client::Workbuddy.process()),
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
                running: process_running(Client::WorkbuddyAi.process()),
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
                running: process_running(Client::Zcode.process()),
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

/// 目标进程在不在跑。走 `tasklist`,按 CSV 首列整串比对 —— 认名字不认本地化提示文案
/// (中文系统上「没有运行的任务…」与英文系统不同,拿文案判会翻车)。
///
/// **带超时**:起进程等结果是阻塞活,`tasklist` 万一卡住不该把调用方一起拖住
/// (2026-10-12 真机踩过:同步命令跑在主线程上,一次卡死 = 整个挂件不响应)。
pub fn process_running(process: &str) -> bool {
    let mut cmd = Command::new("tasklist");
    cmd.args(["/FI", &format!("IMAGENAME eq {process}"), "/NH", "/FO", "CSV"]);
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
                eprintln!("tasklist 超时已终止(进程探测退化为「未运行」)");
                return None;
            }
            Err(_) => return None,
        }
    }
    let mut out = String::new();
    if let Some(mut so) = child.stdout.take() {
        let _ = so.read_to_string(&mut out);
    }
    Some(out)
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
        id: &str,
        base_url: &str,
        key: &str,
        context_tokens: Option<i64>,
        multimodal: bool,
    ) -> Self {
        Self {
            id: id.to_string(),
            display_name: format!("{OUR_NAME_PREFIX}{id}"),
            context_tokens,
            base_url: base_url.trim_end_matches('/').to_string(),
            key: key.to_string(),
            multimodal,
        }
    }

    /// Trae 的 base_url 是完整端点(`.../v1/chat/completions`)。
    fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
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
    store
        .get_setting(LOG_KEY)
        .and_then(|s| serde_json::from_str::<Vec<InjectRecord>>(&s).ok())
        .unwrap_or_default()
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
    /// 我们认得出的、已经写进去的 4SAPI 模型 id
    pub models: Vec<String>,
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
    let tmp = path.with_extension("json.tmp-inject");
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| format!("写临时文件失败: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换 {} 失败: {e}", pretty(path))
    })
}

/// 原文件是不是带 BOM 的(照它的习惯写回去,免得下游 diff 里多出一行变化)。
fn has_bom(path: &Path) -> bool {
    std::fs::read(path)
        .map(|b| b.starts_with(&[0xEF, 0xBB, 0xBF]))
        .unwrap_or(false)
}

/// 备份要改的文件到 `<root>/backup-inject/<stamp>/`。返回备份目录。
fn backup(root: &Path, files: &[PathBuf], stamp: &str) -> Result<PathBuf, String> {
    let dir = root.join("backup-inject").join(stamp);
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

/// 只留最近 KEEP_BACKUPS 份,免得天天写把用户目录撑大。
fn prune_backups(root: &Path) {
    let base = root.join("backup-inject");
    let Ok(rd) = std::fs::read_dir(&base) else { return };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    if dirs.len() <= KEEP_BACKUPS {
        return;
    }
    dirs.sort(); // 目录名是时间戳,字典序即时间序
    for old in &dirs[..dirs.len() - KEEP_BACKUPS] {
        let _ = std::fs::remove_dir_all(old);
    }
}

/// 从备份目录回滚:备份目录里的文件按名字放回它的上一级(即配置目录)。
/// 布局 `<root>/backup-inject/<stamp>/<文件>` 是固定的,所以不用额外记账。
pub fn restore(backup_dir: &Path) -> Result<String, String> {
    let Some(root) = backup_dir.parent().and_then(|p| p.parent()) else {
        return Err("备份目录结构异常".into());
    };
    let rd = std::fs::read_dir(backup_dir).map_err(|e| format!("读备份目录失败: {e}"))?;
    let mut n = 0usize;
    for e in rd.filter_map(|e| e.ok()) {
        let name = e.file_name();
        let target = root.join(&name);
        std::fs::copy(e.path(), &target)
            .map_err(|e| format!("回滚 {} 失败: {e}(目标程序可能正在运行)", pretty(&target)))?;
        n += 1;
    }
    if n == 0 {
        return Err("备份目录是空的".into());
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
    write()?;
    if !verify() {
        let _ = restore(&dir);
        return Err(format!(
            "读回校验没过,已回滚到写入前的备份({})",
            pretty(&dir)
        ));
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

/// 一条 models.json 条目是不是「我们(4SAPI)写的」。判据:url 命中央道域名,
/// 或者名字带我们加的前缀。**不能只看 id** —— 别的中转站的同名模型有同样的 id。
fn wb_is_ours(e: &Value, hosts: &[String]) -> bool {
    let url = e.get("url").and_then(|u| u.as_str()).unwrap_or("");
    if hosts.iter().any(|h| url.contains(h.as_str())) {
        return true;
    }
    e.get("name")
        .and_then(|n| n.as_str())
        .map(|n| n.starts_with(OUR_NAME_PREFIX))
        .unwrap_or(false)
}

fn write_workbuddy_site(
    site: &Site,
    spec: &ModelSpec,
    hosts: &[String],
    opts: &Opts,
) -> Result<SiteOutcome, String> {
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
        if !wb_is_ours(&arr[i], hosts) {
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
    let (id, url) = (spec.id.clone(), spec.base_url.clone());
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
            read_json(&models_for_check)
                .ok()
                .flatten()
                .and_then(|v| {
                    v.as_array().map(|a| {
                        a.iter().any(|e| {
                            e.get("id").and_then(|i| i.as_str()) == Some(id.as_str())
                                && e.get("url").and_then(|u| u.as_str()) == Some(url.as_str())
                        })
                    })
                })
                .unwrap_or(false)
        },
    )?;
    outcome
        .notes
        .push(format!("备份:{}", pretty(&outcome.backup)));
    Ok(outcome)
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
                    v.get("name").and_then(|n| n.as_str()) == Some("4SAPI")
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
        obj.insert("name".into(), json!("4SAPI"));
        obj.insert("kind".into(), json!("openai-compatible"));
        obj.insert(
            "options".into(),
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
                    "providerName": "4SAPI",
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
        rule.insert(
            "access".into(),
            json!({"type": "api-key", "apiKey": spec.key}),
        );
        rule.insert(
            "api".into(),
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
                                    v.get("name").and_then(|n| n.as_str()) == Some("4SAPI")
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
    outcome
        .notes
        .push("provider id 复用同一个:重复写入不会堆出多个「4SAPI」".into());
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
    let conn = rusqlite::Connection::open(db)
        .map_err(|e| format!("打开 {} 失败(Trae 可能正在运行?): {e}", pretty(db)))?;
    // 键名前缀是「区域/租户号」,会随版本变,所以按后缀找,不写死
    let keys: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT key FROM ItemTable WHERE key LIKE '%AI.agent.model.model_list_map'")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
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

    let mut assigned: Option<String> = None;
    for (key, map) in maps.iter_mut() {
        let Some(groups) = map.as_object_mut() else { continue };
        for (agent, arr) in groups.iter_mut() {
            let _ = agent;
            let Some(list) = arr.as_array_mut() else { continue };
            let pos = list.iter().position(|e| {
                e.get("provider").and_then(|p| p.as_str()) == Some("custom_openai_compatible")
                    && e.get("display_name").and_then(|d| d.as_str()) == Some(spec.id.as_str())
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
        conn.execute(
            "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
            rusqlite::params![text, key.as_str()],
        )
        .map_err(|e| format!("写 {key} 失败: {e}"))?;
    }

    // 选中项:实验性开关。口径已破译,但客户端会不会立刻认这个选中项只有真机能验,
    // 所以默认关 —— 打开前请在 Trae 里手动确认过一次。
    let our_cid = assigned
        .clone()
        .or_else(|| find_our_custom_id(&maps, &spec.id));
    if opts.trae_auto_select {
        if let Some(cid) = our_cid.as_deref() {
            let sel_key: Option<String> = {
                let mut stmt = conn
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
                if let Ok(raw) = conn.query_row(
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
                            conn.execute(
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
    if !outcome.selected {
        outcome
            .notes
            .push("选中项没写:重启 Trae 后在模型列表里选一次即可".into());
    }
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

/// ZCode 的「4SAPI」provider id(source=custom 且 name=4SAPI 的那条)。
fn zcode_pid(cfg: &Value) -> Option<String> {
    cfg.get("provider")
        .and_then(|p| p.as_object())
        .and_then(|m| {
            m.iter()
                .find(|(_, v)| {
                    v.get("name").and_then(|n| n.as_str()) == Some("4SAPI")
                        && v.get("source").and_then(|s| s.as_str()) == Some("custom")
                })
                .map(|(k, _)| k.clone())
        })
}

/// WorkBuddy 类:有没有我们写的这一条,以及移除它会动到什么。
fn wb_removal(site: &Site, spec: &ModelSpec) -> (bool, Option<Removal>) {
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
    let Some(pid) = zcode_pid(&cfg) else {
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
            "provider「4SAPI」下的「{}」—— 这是它最后一个模型,连 provider 一起移除",
            spec.id
        )
    } else {
        format!(
            "provider「4SAPI」下的「{}」(provider 留下,还剩 {left} 个模型)",
            spec.id
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
                        .filter(|e| trae_is_ours(e, &spec.id))
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
fn trae_is_ours(e: &Value, model: &str) -> bool {
    e.get("provider").and_then(|p| p.as_str()) == Some("custom_openai_compatible")
        && e.get("display_name").and_then(|d| d.as_str()) == Some(model)
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
    let before = arr.len();
    arr.retain(|e| {
        e.get("id").and_then(|i| i.as_str()) != Some(spec.id.as_str())
    });
    outcome.removed = before - arr.len();
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
    let Some(pid) = zcode_pid(&cfg) else {
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
    let cfg_check = cfg_path.clone();
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
                Some(v) => match zcode_pid(&v) {
                    // provider 还在:模型必须已经不在它下面
                    Some(p) => v
                        .get("provider")
                        .and_then(|pr| pr.get(&p))
                        .and_then(|pr| pr.get("models"))
                        .and_then(|m| m.as_object())
                        .map(|m| !m.contains_key(&id))
                        .unwrap_or(true),
                    // provider 收了:确认连我们的那条规则也没了(pid 不再出现)
                    None => {
                        let _ = &pid_for_check;
                        !v.to_string().contains("4SAPI")
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
        outcome
            .notes
            .push("这是它最后一个模型:provider「4SAPI」也一并移除了".into());
    }
    Ok(outcome)
}

fn remove_trae_site(db: &Path, spec: &ModelSpec) -> Result<SiteOutcome, String> {
    let conn = rusqlite::Connection::open(db)
        .map_err(|e| format!("打开 {} 失败(Trae 可能正在运行?): {e}", pretty(db)))?;
    let keys: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT key FROM ItemTable WHERE key LIKE '%AI.agent.model.model_list_map'")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
    };
    if keys.is_empty() {
        return Err("没找到 Trae 的模型表(版本可能变了),本次不写".into());
    }

    let mut outcome = SiteOutcome::default();
    for k in &keys {
        let raw: String = conn
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
                    list.retain(|e| !trae_is_ours(e, &spec.id));
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
            conn.execute(
                "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                rusqlite::params![text, k.as_str()],
            )
            .map_err(|e| format!("写 {k} 失败: {e}"))?;
        }
    }

    // 选中项正指向这条模型就把它摘掉(那个 agent 回到"没选"的状态)
    if let Some((k, raw)) = selection_row(&conn) {
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
                conn.execute(
                    "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                    rusqlite::params![text, k.as_str()],
                )
                .map_err(|e| format!("写选中项失败: {e}"))?;
            }
        }
    }

    Ok(outcome)
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
        if let Some(msg) = blocked_by_running(&site) {
            return Err(msg);
        }
        site_readable(&site).map_err(|e| format!("{}: {e}", site.label))?;
        let stamp = stamp_now();
        let (client_id, label) = (client.id().to_string(), site.label.clone());
        let o = match client {
            Client::Workbuddy | Client::WorkbuddyAi => remove_workbuddy_site(&site, spec)?,
            Client::Zcode => remove_zcode_site(&site, spec)?,
            Client::Trae => {
                let db = site.root.join("state.vscdb");
                match remove_trae_site(&db, spec) {
                    Ok(o) => o,
                    Err(e) => {
                        let dir = backup(&site.root, &site.files, &stamp)?;
                        let _ = restore(&dir);
                        return Err(format!("{}: {e}", site.label));
                    }
                }
            }
        };
        out.push(Report {
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
        });
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
            let _ = rules.get("config");
            Ok(())
        }
        Client::Trae => Ok(()),
    }
}

/// WorkBuddy 类站点:读出「已写入的模型 id」与「当前选中值」。
fn wb_read(site: &Site) -> (Vec<String>, Option<String>) {
    let models = read_json(&site.root.join("models.json")).ok().flatten();
    let hosts = crate::providers::four_s_api_hosts();
    let ours: Vec<String> = models
        .as_ref()
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter(|e| wb_is_ours(e, &hosts))
                .filter_map(|e| e.get("id").and_then(|i| i.as_str()).map(String::from))
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
                        .or_else(|| learn_prefix(cur.as_deref(), &ours));
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
                    let pid = cfg.as_ref().and_then(zcode_pid);
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
                    p.action = match (&pid, has_model) {
                        (None, _) => "新增 provider「4SAPI」+ 1 条模型".into(),
                        (Some(_), false) => "在已有的 4SAPI provider 里新增 1 条模型".into(),
                        (Some(_), true) => "更新已有的 4SAPI provider 与该模型条目".into(),
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
        // 预告与执行之间目标可能被启动:这里再判一次,不做「按预告执行」的假设
        if let Some(msg) = blocked_by_running(&site) {
            return Err(msg);
        }
        site_readable(&site).map_err(|e| format!("{}: {e}", site.label))?;

        let stamp = stamp_now();
        let (client_id, label) = (client.id().to_string(), site.label.clone());

        match client {
            Client::Workbuddy | Client::WorkbuddyAi => {
                let o = write_workbuddy_site(&site, spec, &crate::providers::four_s_api_hosts(), opts)?;
                out.push(Report {
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
                            n.push(format!(
                                "注意:这条 id 的条目原本指向 {url}(不是 4SAPI),已按上面的内容覆盖。"
                            ));
                        }
                        n
                    },
                });
            }
            Client::Zcode => {
                let o = write_zcode_site(&site, spec)?;
                out.push(Report {
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
                });
            }
            Client::Trae => {
                let o = match trae_write(&site.root.join("state.vscdb"), spec, opts) {
                    Ok(o) => o,
                    Err(e) => {
                        // trae_write 中途失败时它自己的 UPDATE 可能已写了一半 → 整库盖回备份
                        let dir = backup(&site.root, &site.files, &stamp)?;
                        let _ = restore(&dir);
                        return Err(format!("{}: {e}", site.label));
                    }
                };
                out.push(Report {
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
                });
            }
        }
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
                            if !st.models.contains(&m) {
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
                    st.models.sort();
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
                        let models: Vec<String> = cfg
                            .as_ref()
                            .and_then(|c| c.get("provider"))
                            .and_then(|p| p.as_object())
                            .map(|m| {
                                m.iter()
                                    .filter(|(_, v)| {
                                        v.get("name").and_then(|n| n.as_str()) == Some("4SAPI")
                                    })
                                    .flat_map(|(_, v)| {
                                        v.get("models")
                                            .and_then(|x| x.as_object())
                                            .map(|mm| mm.keys().cloned().collect::<Vec<_>>())
                                            .unwrap_or_default()
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        for m in models {
                            if !st.models.contains(&m) {
                                st.models.push(m);
                            }
                        }
                    }
                    st.models.sort();
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
                                                    if !st.models.contains(&d.to_string()) {
                                                        st.models.push(d.to_string());
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    st.models.sort();
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
            "claude-sonnet-5",
            "https://4sapi.com/v1",
            "sk-test-123",
            Some(1_000_000),
            true,
        )
    }

    fn hosts() -> Vec<String> {
        vec!["4sapi.com".into(), "4sapi.cn".into()]
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

        let o = write_workbuddy_site(&site, &spec(), &hosts(), &Opts::default()).unwrap();
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
        let o2 = write_workbuddy_site(&site, &spec(), &hosts(), &Opts::default()).unwrap();
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
        let o = write_workbuddy_site(&site, &spec(), &hosts(), &opts).unwrap();
        assert!(o.selected, "学到前缀就该写选中项");
        let s = read_v(&root.join("settings.json"));
        assert_eq!(s["model"], "custom-local:claude-sonnet-5");
        assert_eq!(s["sandbox"], Value::Null, "没动过的键不该冒出来");

        // 没学到前缀:只写条目
        let root2 = tmpdir("wb-nosel");
        std::fs::write(root2.join("models.json"), "[]").unwrap();
        std::fs::write(root2.join("settings.json"), "{\"model\":\"gpt-6-luna\"}").unwrap();
        let site2 = site_at(&root2, Client::Workbuddy);
        let o2 = write_workbuddy_site(&site2, &spec(), &hosts(), &Opts::default()).unwrap();
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
        let s = ModelSpec::new("deepseek-v4.1-flash", "https://4sapi.com/v1", "sk-new", None, false);
        let o = write_workbuddy_site(&site, &s, &hosts(), &Opts::default()).unwrap();
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
        let e = write_workbuddy_site(&site, &spec(), &hosts(), &Opts::default()).unwrap_err();
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
        let s2 = ModelSpec::new("glm-5.3", "https://4sapi.com/v1", "sk-test-123", Some(1_000_000), false);
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

    /// 从 WorkBuddy 现有文件里认得出「哪些是我们写的」。
    #[test]
    fn wb_is_ours_detects_by_host_and_name() {
        let hosts = vec!["4sapi.com".into(), "4sapi.cn".into()];
        assert!(wb_is_ours(&json!({"url":"https://4sapi.cn/v1"}), &hosts));
        assert!(wb_is_ours(&json!({"url":"https://x/y","name":"4S-foo"}), &hosts));
        assert!(!wb_is_ours(
            &json!({"url":"https://opencode.ai/zen/go/v1","name":"GO-x"}),
            &hosts
        ));
    }

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
        write_workbuddy_site(&site, &spec(), &hosts(), &opts).unwrap();
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
        write_workbuddy_site(&site, &spec(), &hosts(), &Opts::default()).unwrap();
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
        let s2 = ModelSpec::new("glm-5.3", "https://4sapi.com/v1", "sk-test-123", Some(1_000_000), false);
        write_zcode_site(&site, &s2).unwrap();

        let (configured, removal) = zcode_removal(&site, &spec());
        assert!(configured);
        assert!(removal.unwrap().what.contains("还剩 1 个模型"), "预告要说清 provider 会留下");

        let o = remove_zcode_site(&site, &spec()).unwrap();
        assert_eq!(o.removed, 1);
        let cfg = read_v(&root.join("config.json"));
        let pid = zcode_pid(&cfg).expect("provider 还在");
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
        assert!(zcode_pid(&cfg2).is_none(), "空 provider 不该留下");
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
}
