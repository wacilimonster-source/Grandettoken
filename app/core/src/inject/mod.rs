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
#[path = "process.rs"]
mod inject_process;
pub use inject_process::ProcessSnapshot;

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
        "gateway" => "GW-",
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

/// WorkBuddy settings.json 里选中项的键名(两版客户端一致)。
const WB_SELECTION_KEY: &str = "model";

// ───────────────────────── 客户端与站点 ─────────────────────────

mod types;
pub use types::{Client, Site};

mod sites;
pub use sites::{pretty, process_running, sites, sites_of};
use sites::{appdata, home};
#[cfg(test)]
use sites::{expected_process, is_helper_exe};

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
        Some(crate::models::ApiProtocol::AnthropicMessages) if client == Client::ClaudeCode => None,
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
    /// 在跑提示(3C:不拒写,界面二次确认+写后重启提示)
    pub running_warning: Option<String>,
    /// 接入前它现在跑在什么上(订阅/登录/key/provider 数)
    pub source: String,
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

/// 「在不在跑」的提示文案(3C/magpie 口径:不拒写,由界面二次确认 + 写后重启提示承担)。
/// 拿不到进程表时返回 false 而不是报错 —— 真正的保护是备份 + 读回校验。
/// DSH 是唯一例外:它收敛外部编辑、profile 按请求解析,所以边跑边写是它认的用法
/// (见 `Client::blocks_when_running`)。
pub fn blocked_by_running(site: &Site) -> Option<String> {
    if !site.running || !site.client.blocks_when_running() {
        return None;
    }
    Some(format!(
        "{} 正在运行（{}） —— 它的配置是内存态落盘,边跑边写会在退出时被覆盖。先退出它再点写入。",
        site.label, site.process
    ))
}

#[path = "io.rs"]
mod inject_io;
pub use inject_io::restore;
use inject_io::*;

#[path = "workbuddy.rs"]
mod inject_workbuddy;
pub use inject_workbuddy::{learn_prefix, name_is_ours, strip_prefix};
use inject_workbuddy::*;

#[path = "zcode.rs"]
mod inject_zcode;
use inject_zcode::*;

#[path = "trae.rs"]
mod inject_trae;
pub use inject_trae::{trae_model_id, trae_selected_model};
use inject_trae::*;

#[path = "dsh.rs"]
mod inject_dsh;
pub use inject_dsh::CatalogRoute;
use inject_dsh::*;

#[path = "stash.rs"]
mod inject_stash;
pub(crate) use inject_stash::agent_stand_in;

#[path = "discover.rs"]
mod inject_discover;

#[path = "source.rs"]
mod inject_source;

#[path = "drift.rs"]
mod inject_drift;
pub use inject_drift::drift_all;

#[path = "codex.rs"]
mod inject_codex;

#[path = "cli.rs"]
mod inject_cli;
use inject_cli::*;

#[path = "gateway.rs"]
mod inject_gateway;
pub use inject_gateway::{apply_gateway, preview_gateway, remove_gateway, GatewayInjectSpec};

// ───────────────────────── 取消写入 ─────────────────────────

mod remove;
mod adapter;
use adapter::ClientAdapter;
pub use remove::{remove, Removal};
use remove::{wb_removal, zcode_pid, zcode_removal};
#[cfg(test)]
use remove::{remove_workbuddy_site, remove_zcode_site};

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
        Client::ClaudeCode => claude_site_readable(site),
        Client::Codex => {
            let cfg_toml = site.root.join("config.toml");
            if cfg_toml.exists() {
                std::fs::read_to_string(&cfg_toml)
                    .map_err(|e| format!("读 {} 失败: {e}", pretty(&cfg_toml)))?;
            }
            Ok(())
        }
        Client::OpenCode => opencode_site_readable(site),
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
                    let src = if is_local_gateway_url(url) {
                        Some("gateway".to_string())
                    } else {
                        crate::models::source_of_url(url).map(String::from)
                    };
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
    let Ok(list) = ClientAdapter::discover(client) else {
        return vec![Preview {
            client: client.id().into(),
            label: client.label().into(),
            blocked: Some(format!("没检测到 {} 的配置", client.label())),
            ..Default::default()
        }];
    };
    list.into_iter()
        .map(|adapter| {
            let site = &adapter.site;
            let mut p = adapter.preview();
            p.source = inject_source::source(client, &site);
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
                Client::ClaudeCode => {
                    let cli_cfg = CliTargetConfig::from_model_spec(spec);
                    claude_preview(&site, &cli_cfg, &mut p);
                }
                Client::Codex => {
                    let cli_cfg = CliTargetConfig::from_model_spec(spec);
                    codex_preview(&site, &cli_cfg, &mut p);
                }
                Client::OpenCode => {
                    let cli_cfg = CliTargetConfig::from_model_spec(spec);
                    opencode_preview(&site, &cli_cfg, &mut p);
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
    let mut out = Vec::new();
    for adapter in ClientAdapter::discover(client)? {
        let site = &adapter.site;
        let stamp0 = stamp_now();
        let (cid, lbl) = (client.id().to_string(), site.label.clone());
        let mut local: Vec<Report> = Vec::new();
        // 每一家单独成事。以前任一家失败(在跑 / 读不了 / 写坏 / 校验不过)都 `return Err`
        // 跳出整个循环:① 界面只看到一句错误,不知道另一家其实已经写进去了;
        // ② 壳层的 push_log 拿不到任何报告,于是那次已落盘的写入连台账都没有,
        //   也就再没有回滚入口(报告 P1-6)。
        let running_warn = &adapter.running_warning;
        let res: Result<(), String> = (|| {
            // 3C:在跑不拒写(magpie 口径),提示交给界面二次确认与写后重启提示
            adapter.require_readable()?;

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
                let o = adapter.commit(
                    &stamp,
                    || trae_write(&db, spec, opts),
                    || trae_verify_written(&db, spec),
                )?;
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
            Client::ClaudeCode | Client::Codex | Client::OpenCode => {
                let cli_cfg = CliTargetConfig::from_model_spec(spec);
                let o = match client {
                    Client::ClaudeCode => write_claude_code_site(&site, &cli_cfg)?,
                    Client::Codex => write_codex_site(&site, &cli_cfg)?,
                    Client::OpenCode => write_opencode_site(&site, &cli_cfg)?,
                    _ => unreachable!(),
                };
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
                    effect: format!("打开或重启 {} 后立即生效", site.label),
                    notes: {
                        let mut n = o.notes;
                        if o.same {
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
            Ok(()) => {
                for r in &mut local {
                    if let Some(w) = &running_warn {
                        r.notes.push(w.clone());
                    }
                    if matches!(client, Client::ClaudeCode | Client::Codex | Client::OpenCode) {
                        r.notes.push(inject_source::notice(client).into());
                    }
                }
                out.extend(local)
            }
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
            if let Some(s) = mine.iter().find(|s| s.running) {
                st.process = s.process.clone();
            }
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
                                        let src = if name == "Grandettoken 本地网关" {
                                            "gateway".to_string()
                                        } else {
                                            crate::models::SOURCES
                                                .iter()
                                                .find(|d| d.name == name)
                                                .map(|d| d.id.to_string())?
                                        };
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
                                                    let src = if is_local_gateway_url(url) {
                                                        "gateway".to_string()
                                                    } else {
                                                        crate::models::source_of_url(url)
                                                            .unwrap_or("")
                                                            .to_string()
                                                    };
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
                Client::ClaudeCode | Client::Codex | Client::OpenCode => {
                    cli_read_state(c, &mine, &mut st);
                    st.models.sort_by(|a, b| a.id.cmp(&b.id));
                }
            }
            st
        })
        .collect()
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
#[cfg(test)]
#[path = "tests_trae_dsh.rs"]
mod tests_trae_dsh;

/// 卸载清理用的网关地址：从已保存的网关设置里读端口。
/// 读不到（库损坏/被占用）就返回 None —— 宁可不清理，也不拿「任意 127.0.0.1」去匹配，
/// 那会把别人（如 magpie）的本地网关配置当我们的删掉。
/// 只读打开：卸载时不该顺手建库或跑迁移。
fn saved_gateway_root() -> Option<String> {
    let db = appdata()?.join("com.wacil.tokenscope").join("tokenscope.db");
    if !db.is_file() {
        return None;
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    let raw: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![crate::gateway::CONFIG_KEY],
            |r| r.get(0),
        )
        .ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let port = v.get("port").and_then(Value::as_u64)?;
    Some(format!("http://127.0.0.1:{port}"))
}

/// 卸载时摘掉指向已消失网关的托管配置（对齐 magpie `TakeLeftovers`）。
/// 只动我们写过的块/键并还原 stash；失败只记日志，不阻断卸载。
/// 旧五家（Trae/WorkBuddy/ZCode/DSH）的条目请在卸载前用界面「取消写入」移除。
pub fn cleanup_leftovers() -> usize {
    let sites = inject_discover::detect_cli_sites(&ProcessSnapshot::capture());
    let gw_root = saved_gateway_root();
    let mut n = 0usize;
    for site in sites {
        // source 置空 = 关掉「任意本地网关地址都算我们的」那条宽松匹配，
        // 只认存下来的那个端口。
        let cfg = CliTargetConfig {
            model: String::new(),
            display_name: String::new(),
            base_url: String::new(),
            anthropic_base_url: gw_root.clone().unwrap_or_default(),
            api_key: String::new(),
            source: String::new(),
        };
        let res: Result<bool, String> = match site.client {
            Client::ClaudeCode => {
                if gw_root.is_none() {
                    Ok(false)
                } else {
                    remove_claude_code_site(&site, &cfg).map(|o| o.removed > 0)
                }
            }
            Client::Codex => cleanup_codex_site(&site),
            Client::OpenCode => cleanup_opencode_site(&site),
            _ => Ok(false),
        };
        match res {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) => crate::diaglog::write_keyed("inject", "cleanup", &e),
        }
    }
    n
}
