//! 4SAPI 可用模型:价目表解析、可用集合、单价换算。
//!
//! 两个接口都是**元数据接口**,不产生任何 token 计费(与「取数不消耗 token」同一条原则):
//!
//! ```text
//! GET /api/pricing            公开价目表(无需密钥)—— 说明/标签/厂商/倍率/分组/接口类型
//! GET /v1/models  Bearer      这把 key 真正能调的模型(网关按所属分组过滤)
//! ```
//!
//! 换算口径与站点自己前端**逐行一致**(2026-10-12 从站点 pricing 页的 JS 里摘出来的原文):
//!
//! ```text
//! let e = 2 * t.model_ratio * d;          // d = 分组倍率 → “输入价格 … / 1M Tokens”
//! 补全价格 = 2 * model_ratio * completion_ratio * 分组倍率
//! ```
//!
//! 恒等式的另一半来自 `/api/status`:`quota_per_unit=500000`、`quota_display_type=CNY` ——
//! 和挂件现有的 4SAPI 余额渠道用的是同一个换算(500000 积分 = ¥1),所以模型单价与
//! 面板上的余额是同一个货币,可以直接比较。货币符号由 quota_display_type 决定,本站实测 CNY。
//!
//! 两条纪律(产品的「不误报」原则在这里的落地):
//!   1. 倍率是站点声明的,不是我们算出来的 —— 口径行必须写清用的是哪个分组;
//!   2. 阶梯计价的模型(`billing_expr`)只标类型,不给「确定价」,界面显示要带星号。

use crate::providers::num;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// ───────────────────────── 来源(渠道)定义 ─────────────────────────
//
// 模型页从 v0.1.25 起支持多个来源:同一个模型名可能同时存在于几个来源里
// (`gpt-6-luna` 在 4SAPI / Hapi / OpenCode Go 都有),"已配置"必须按来源分别判定 ——
// 客户端条目里的 url 命中哪个来源的域名,它就属于哪个来源。

/// 一个模型来源。决定三件事:列表从哪来、写进客户端用哪个地址、哪些域名算"我们写的"。
pub struct SourceDef {
    pub id: &'static str,
    /// 界面上的名字,也是写进 ZCode 时那个 provider 的名字
    pub name: &'static str,
    /// 接口主机(列表/价目表都在它下面)
    pub api_host: &'static str,
    /// 列表路径
    pub list_path: &'static str,
    /// 列表是否公开(无需密钥)。Go 的列表是公开的 —— 没配密钥也能先看有什么模型
    pub list_public: bool,
    /// 价目表路径;None = 这个来源没有公开价目表(Hapi 的价目接口要登录态,不是 API key)
    pub pricing_path: Option<&'static str>,
    /// 写进客户端的接入地址
    pub base_url: &'static str,
    /// 除 api_host 之外还算这个来源的域名(4SAPI 走 source_hosts 从渠道定义转发)
    pub extra_hosts: &'static [&'static str],
}

/// The public OpenCode Go catalog only returns model IDs. The Go docs list the
/// API each model accepts, so keep that protocol metadata alongside the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApiProtocol {
    OpenAiChatCompletions,
    OpenAiResponses,
    AnthropicMessages,
    GoogleGenerative,
}

impl ApiProtocol {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "OpenAI Chat Completions",
            Self::OpenAiResponses => "OpenAI Responses",
            Self::AnthropicMessages => "Anthropic Messages",
            Self::GoogleGenerative => "Google Generative API",
        }
    }

    pub fn base_url(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions | Self::OpenAiResponses => {
                "https://opencode.ai/inference/go/openai/v1"
            }
            Self::AnthropicMessages => "https://opencode.ai/inference/go/anthropic/v1",
            Self::GoogleGenerative => "https://opencode.ai/inference/go/google/v1beta",
        }
    }

    pub fn endpoint(self, base_url: &str, model: &str) -> String {
        let base = base_url.trim_end_matches('/');
        match self {
            Self::OpenAiChatCompletions => format!("{base}/chat/completions"),
            Self::OpenAiResponses => format!("{base}/responses"),
            Self::AnthropicMessages => format!("{base}/messages"),
            Self::GoogleGenerative => {
                format!("{base}/models/{model}:generateContent")
            }
        }
    }

    pub fn is_openai_chat(self) -> bool {
        matches!(self, Self::OpenAiChatCompletions)
    }

    pub fn dsh_api(self) -> Option<&'static str> {
        match self {
            Self::OpenAiChatCompletions => Some("openai-completions"),
            Self::OpenAiResponses => Some("openai-responses"),
            Self::AnthropicMessages => Some("anthropic-messages"),
            Self::GoogleGenerative => None,
        }
    }
}

/// Protocol documented for a source/model pair. OpenCode Go's model-list API
/// does not include this field, so undocumented IDs intentionally return None.
pub fn api_protocol(source: &str, model: &str) -> Option<ApiProtocol> {
    if source != "opencode-go" {
        return Some(ApiProtocol::OpenAiChatCompletions);
    }
    let model = model.to_ascii_lowercase();
    use ApiProtocol::*;
    match model.as_str() {
        // OpenAI Responses API.
        "gpt-6-luna"
        | "gpt-5.6-luna"
        | "grok-4.7"
        | "muse-spark-1.2-contributor"
        | "muse-spark-1.3-contributor" => Some(OpenAiResponses),

        // Anthropic Messages API.
        "claude-haiku-5-5"
        | "minimax-m3"
        | "qwen3.7-plus"
        | "qwen3.8-flash"
        | "qwen3.8-max" => Some(AnthropicMessages),

        // Google Generative API.
        "gemini-4-argon-contributor" => Some(GoogleGenerative),

        // OpenAI Chat Completions API.
        "beam-free"
        | "cat-jam-free"
        | "deepseek-v4-pro"
        | "deepseek-v4.1-flash"
        | "glm-5.2"
        | "glm-5.3"
        | "glm-5.3-flash"
        | "hy3"
        | "hy4-preview"
        | "kimi-k2.7-code"
        | "kimi-k3"
        | "longcat-2.0"
        | "longcat-2.5-preview-free"
        | "mimo-v2.5"
        | "mimo-v2.5-pro"
        | "mimo-v2.6-flash"
        | "mimo-v2.6-pro"
        | "minimax-m3.1-flash-free"
        | "space-bunny"
        | "step-5-preview"
        | "step-5-preview-free" => Some(OpenAiChatCompletions),
        _ => None,
    }
}

pub const SOURCES: &[SourceDef] = &[
    SourceDef {
        id: "4sapi",
        name: "4SAPI",
        api_host: "4sapi.com",
        list_path: "/v1/models",
        list_public: false,
        pricing_path: Some("/api/pricing"),
        base_url: "https://4sapi.com/v1",
        // 4SAPI 的四个域名在渠道定义里,这里不重复写(见 source_hosts)
        extra_hosts: &[],
    },
    SourceDef {
        id: "hapi",
        name: "Hapi",
        api_host: "ai.yuchuantest.com",
        list_path: "/v1/models",
        list_public: false,
        pricing_path: None,
        base_url: "https://ai.yuchuantest.com/v1",
        extra_hosts: &[],
    },
    SourceDef {
        id: "opencode-go",
        name: "OpenCode Go",
        api_host: "opencode.ai",
        list_path: "/inference/go/v1/models",
        // 实测公开:不带密钥也返回 38 个模型
        list_public: true,
        pricing_path: None,
        // Go 的真实端点按模型协议区分;这里仅保留 Chat Completions 的默认值。
        base_url: "https://opencode.ai/inference/go/openai/v1",
        extra_hosts: &[],
    },
];

pub fn find_source(id: &str) -> Option<&'static SourceDef> {
    SOURCES.iter().find(|s| s.id == id)
}

/// 某个来源的域名列表(判定"这条条目是我们写的"用)。
///
/// 4SAPI 的域名只在渠道定义里写一份(`providers::four_s_api_hosts`),这里转发过去 ——
/// 域名多一处拷贝,就多一处忘记同步的机会。
pub fn source_hosts(id: &str) -> Vec<String> {
    match id {
        "4sapi" => crate::providers::four_s_api_hosts(),
        other => match find_source(other) {
            Some(d) => std::iter::once(d.api_host)
                .chain(d.extra_hosts.iter().copied())
                .map(String::from)
                .collect(),
            None => Vec::new(),
        },
    }
}

/// 这条客户端条目(看它的 url)属于哪个来源。认不出来返回 None。
pub fn source_of_url(url: &str) -> Option<&'static str> {
    // 只比主机名。以前是 `url.contains(host)`:URL 里任何位置出现这串字符都算这家的条目,
    // 于是别人手配的 https://fake-4sapi.com.example.cn/v1、或查询串里恰好带这串的,
    // 都会被认成"挂件写的",被算进管理/回滚范围(报告 P3-17)
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .rsplit('@')
        .next()
        .unwrap_or(rest);
    let host = authority
        .split(':')
        .next()
        .unwrap_or(authority)
        .to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    SOURCES
        .iter()
        .find(|d| {
            source_hosts(d.id).iter().any(|h| {
                let h = h.to_ascii_lowercase();
                host == h || host.ends_with(&format!(".{h}"))
            })
        })
        .map(|d| d.id)
}

/// 上下文长度标签(1M / 256K / 128K…)能不能算上下文标签。
///
/// K 档要求 ≥ 16K:图像/视频模型的分辨率也写成 `2K` / `4K`,那不是上下文长度。
/// (这类模型本来就不进默认列表,但口径不能因此含糊。)
///
/// 全程按**字节**判再切片:`多模态` 这类中文 tag 的最后 3 字节是一个字符,
/// 直接 `split_at(len-1)` 会 panic(实测踩过)。
fn is_context_tag(t: &str) -> bool {
    let Some((digits, unit)) = split_last_ascii(t) else {
        return false;
    };
    match unit {
        b'K' | b'k' => digits.parse::<i64>().map(|n| n >= 16).unwrap_or(false),
        b'M' | b'm' => digits.parse::<i64>().map(|n| n >= 1).unwrap_or(false),
        _ => false,
    }
}

/// 末尾是一个 ASCII 字节、且前面全是 ASCII 数字时,把它切成 (数字部分, 单位字节)。
/// 不满足就返回 None —— 这样切片永远落在字符边界上。
fn split_last_ascii(t: &str) -> Option<(&str, u8)> {
    let b = t.as_bytes();
    let last = *b.last()?;
    if !last.is_ascii() || b.len() < 2 {
        return None;
    }
    if !b[..b.len() - 1].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((&t[..t.len() - 1], last))
}

/// 上下文标签 → token 数(1M → 1_000_000;256K → 256_000)。
/// 认不出来返回 None:客户端条目里的 maxInputTokens 宁可缺省,也不填一个编造的数。
pub fn context_tokens(ctx: &str) -> Option<i64> {
    let t = ctx.trim();
    let (digits, unit) = split_last_ascii(t)?;
    let n: i64 = digits.parse().ok()?;
    match unit {
        b'K' | b'k' => Some(n.saturating_mul(1_000)),
        b'M' | b'm' => Some(n.saturating_mul(1_000_000)),
        _ => None,
    }
}

/// 这些 tag 一出,就不是「能当对话/编码模型用」的东西。默认过滤就按它排。
const NON_CHAT_TAGS: [&str; 4] = ["图像", "视频", "音频", "向量"];

/// 能不能当对话/编码模型用:有 openai/anthropic 接口,且不是图像/视频/音频/向量。
///
/// 站点 160 个模型里 159 个都带 openai 接口,真正要排掉的是图画/视频/嵌入这类 ——
/// 它们出现在「配到编辑器里用」的列表里只会添乱(但**不隐藏**:开关一开就显示全部)。
pub fn is_chat_model(tags: &[String], endpoints: &[String]) -> bool {
    let ep_ok = endpoints.iter().any(|e| e == "openai" || e == "anthropic");
    let blocked = tags
        .iter()
        .any(|t| NON_CHAT_TAGS.iter().any(|b| t.contains(b)));
    ep_ok && !blocked
}

/// 价目表里的一行(= 一个模型)。字段名与站点一致,便于对着站点核。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelRow {
    pub id: String,
    pub description: String,
    /// tags 串拆成的数组(1M / 多模态 / 文本 …),原样保留站点用词
    pub tags: Vec<String>,
    /// 上下文长度标签(1M / 256K),从 tags 里挑;没有就是 None
    pub context: Option<String>,
    pub vendor: String,
    /// 0 = 按 token 计费;1 = 按次计费(走 model_price)
    pub quota_type: i64,
    /// 输入倍率
    pub ratio: f64,
    pub completion_ratio: f64,
    pub cache_ratio: Option<f64>,
    /// 按次计费单价(站点原值,元/次)。**取不到就是 None** ——
    /// 以前缺字段落成 0.0,按次渠道会被显示成"¥0.000 / 次"(与 [`Price`] 的契约同一条)。
    pub model_price: Option<f64>,
    /// 阶梯计价(billing_expr 能落成表达式)
    pub tiered: bool,
    pub billing_expr: Option<String>,
    /// 哪些分组能用它
    pub groups: Vec<String>,
    pub endpoints: Vec<String>,
    /// API protocol for providers whose model catalog does not include it.
    #[serde(default)]
    pub api_protocol: Option<ApiProtocol>,
    /// 这把 key 能不能调。None = 没取到 /v1/models(没配密钥或请求失败)—— 界面据此
    /// 从「可用」退成「价目表浏览」,不把 160 个都说成可用。
    pub usable: Option<bool>,
    pub chat: bool,
    pub multimodal: bool,
}

/// 站点的一个分组:名称 + 倍率 + 站点自己写的说明。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub name: String,
    pub ratio: f64,
    pub note: String,
}

/// 解析后的价目表。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    /// 整份价目表的指纹(pricing_version)。变了就说明站点改过价/加过模型。
    pub version: String,
    /// 站点声明的「一key」等自动分组,首个作为默认口径
    pub auto_groups: Vec<String>,
    pub groups: Vec<Group>,
    pub rows: Vec<ModelRow>,
}

impl Catalog {
    pub fn group_ratio(&self, name: &str) -> Option<f64> {
        self.groups.iter().find(|g| g.name == name).map(|g| g.ratio)
    }

    /// 默认分组:站点 auto_groups 的首个;没有就用第一个分组;再没有就空串。
    pub fn default_group(&self) -> String {
        self.auto_groups
            .first()
            .cloned()
            .or_else(|| self.groups.first().map(|g| g.name.clone()))
            .unwrap_or_default()
    }

    pub fn chat_count(&self) -> usize {
        self.rows.iter().filter(|r| r.chat).count()
    }

    /// 打上「这把 key 能调」的标记。ids 来自 /v1/models。
    pub fn mark_usable(&mut self, ids: &[String]) {
        let set: std::collections::HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
        for r in self.rows.iter_mut() {
            r.usable = Some(set.contains(r.id.as_str()));
        }
    }

    pub fn find(&self, id: &str) -> Option<&ModelRow> {
        self.rows.iter().find(|r| r.id == id)
    }
}

/// 单价。`input` / `output` 的单位是 **元/百万 token**(与站点价目表同口径);
/// `per_call` 是按次计费的元/次。取不到一律 None —— 不用 0 顶替。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub per_call: Option<f64>,
    /// 实际用来折算的分组名
    pub group: String,
    pub group_ratio: f64,
    /// 所选分组不含这个模型时,这里是实际采用的分组名(界面要写明)
    pub basis_group: Option<String>,
    /// 阶梯计价:数值只是某一档,界面加星号
    pub tiered: bool,
}

fn round4(n: f64) -> f64 {
    (n * 10_000.0).round() / 10_000.0
}

/// 解析 `/api/pricing`。
pub fn parse_catalog(v: &Value) -> Result<Catalog, String> {
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or("价目表结构异常(无 data 数组)")?;

    // 厂商 id → 名称
    let vendors: HashMap<i64, String> = v
        .get("vendors")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let id = e.get("id").and_then(num)? as i64;
                    let name = e.get("name").and_then(|n| n.as_str())?.to_string();
                    Some((id, name))
                })
                .collect()
        })
        .unwrap_or_default();

    // 分组:倍率 + 站点自己的说明(usable_group)
    let notes: HashMap<String, String> = v
        .get("usable_group")
        .and_then(|g| g.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, val)| {
                    (
                        k.clone(),
                        val.as_str().unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut groups: Vec<Group> = v
        .get("group_ratio")
        .and_then(|g| g.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, val)| {
                    Some(Group {
                        name: k.clone(),
                        ratio: num(val)?,
                        note: notes.get(k).cloned().unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    groups.sort_by(|a, b| a.name.cmp(&b.name));

    let auto_groups: Vec<String> = v
        .get("auto_groups")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut rows: Vec<ModelRow> = Vec::with_capacity(data.len());
    for e in data {
        let Some(id) = e.get("model_name").and_then(|n| n.as_str()) else {
            continue; // 没名字的条目没法用,跳过而不是记一行空
        };
        let tags: Vec<String> = e
            .get("tags")
            .and_then(|t| t.as_str())
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let endpoints: Vec<String> = e
            .get("supported_endpoint_types")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let vendor_id = e.get("vendor_id").and_then(num).map(|n| n as i64);
        let context = tags.iter().find(|t| is_context_tag(t)).cloned();
        rows.push(ModelRow {
            id: id.to_string(),
            description: e
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string(),
            multimodal: tags.iter().any(|t| t == "多模态"),
            chat: is_chat_model(&tags, &endpoints),
            tags,
            context,
            vendor: vendor_id
                .and_then(|v| vendors.get(&v).cloned())
                .unwrap_or_default(),
            quota_type: e.get("quota_type").and_then(num).unwrap_or(0.0) as i64,
            ratio: e.get("model_ratio").and_then(num).unwrap_or(0.0),
            completion_ratio: e.get("completion_ratio").and_then(num).unwrap_or(0.0),
            cache_ratio: e.get("cache_ratio").and_then(num).filter(|v| *v > 0.0),
            model_price: e.get("model_price").and_then(num),
            tiered: e
                .get("billing_mode")
                .and_then(|m| m.as_str())
                .map(|m| m == "tiered_expr")
                .unwrap_or(false),
            billing_expr: e
                .get("billing_expr")
                .and_then(|m| m.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(String::from),
            groups: e
                .get("enable_groups")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            endpoints,
            api_protocol: None,
            usable: None,
        });
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(Catalog {
        version: v
            .get("pricing_version")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        auto_groups,
        groups,
        rows,
    })
}

/// 解析 `/v1/models`(OpenAI 兼容列表)里的 id。
/// 结构不对时返回空 Vec —— 调用方据此把「可用」退成「未知」,不当成「一个都不能用」。
pub fn parse_usable(v: &Value) -> Vec<String> {
    v.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    e.get("id")
                        .and_then(|i| i.as_str())
                        .or_else(|| e.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 单价换算:输入 = 2 × 模型倍率 × 分组倍率(元/百万 token);输出 = 输入 × completion_ratio。
///
/// 所选分组不含这个模型时,退回「含有它的、倍率最低的那个分组」并**在结果里标明**
/// (界面要写成“价按「国产全模型」分组”)—— 绝不显示一个本分组用不上的价。
pub fn price_of(row: &ModelRow, groups: &[Group], want: &str) -> Price {
    let mut out = Price {
        group: want.to_string(),
        tiered: row.tiered,
        ..Default::default()
    };

    if row.quota_type != 0 {
        // 按次计费:model_price 原值就是元/次,没有输入输出之分。
        // 站点没给这个字段时给 None(界面显示"未标价"),绝不用 0 顶替
        out.per_call = row.model_price.map(round4);
        out.group_ratio = groups.iter().find(|g| g.name == want).map(|g| g.ratio).unwrap_or(0.0);
        return out;
    }

    let ratio_of = |name: &str| groups.iter().find(|g| g.name == name).map(|g| g.ratio);
    let basis = if row.groups.is_empty() || row.groups.iter().any(|g| g == want) {
        // 站点没给分组清单(字段缺失)= 我们无从判断它不在这个分组里,
        // 就按所选分组折算 —— 缺数据不当作「不可用」,也不谎报一个回退分组。
        ratio_of(want).map(|r| (want.to_string(), r))
    } else {
        // 所选分组明确不含它:挑一个含有它的、倍率最低的分组(能用且最便宜)
        row.groups
            .iter()
            .filter_map(|g| ratio_of(g).map(|r| (g.clone(), r)))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    };

    let Some((basis_group, basis_ratio)) = basis else {
        // 分组表里找不到任何可用分组(站点加新分组但价目表还没跟上):不给价
        out.basis_group = Some(String::new());
        return out;
    };
    if basis_group != want {
        out.basis_group = Some(basis_group.clone());
    }
    out.group = basis_group;
    out.group_ratio = basis_ratio;

    let input = 2.0 * row.ratio * basis_ratio;
    if input > 0.0 {
        out.input = Some(round4(input));
        if row.completion_ratio > 0.0 {
            out.output = Some(round4(input * row.completion_ratio));
        }
    }
    out
}

// ───────────────────────── 取数(两个元数据接口) ─────────────────────────

/// 单次请求预算:与渠道取数同一档(8 秒)。价目表 100 KB 上下,正常很快。
const TIMEOUT_SEC: u64 = 8;

/// 响应体上限。价目表正常 100 KB 上下;入口被网关换成大 HTML / 超大响应时,
/// 不把整包读进内存再解析(与渠道取数 fetch.rs 同一口径、同一个分块累加器)。
const MAX_BODY: usize = 2_000_000;

/// 分块读响应体,超过上限立刻停手。
async fn read_body_capped(mut resp: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(c)) => crate::fetch::push_chunk(&mut out, &c, max)?,
            Ok(None) => return Ok(out),
            Err(e) => return Err(format!("读取响应失败: {e}")),
        }
    }
}

async fn get_json(client: &reqwest::Client, url: &str, key: Option<&str>) -> Result<Value, String> {
    let mut req = client
        .get(url)
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(TIMEOUT_SEC));
    if let Some(k) = key {
        req = req.header("Authorization", format!("Bearer {}", k.trim()));
    }
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            "请求超时".to_string()
        } else if e.is_connect() {
            "无法连接(检查网络或代理)".to_string()
        } else {
            format!("请求失败: {e}")
        }
    })?;
    let status = resp.status();
    // 先看 Content-Length,超了在读之前就拒;没有长度(分块传输)才边收边判
    if let Some(len) = resp.content_length() {
        if len > MAX_BODY as u64 {
            return Err(format!("响应过大({len} 字节),已丢弃"));
        }
    }
    let raw = read_body_capped(resp, MAX_BODY).await?;
    let body: Value = serde_json::from_slice(&raw)
        .map_err(|_| format!("响应不是合法 JSON (HTTP {})", status.as_u16()))?;
    if !status.is_success() {
        // 网关的错误体里带的是最有用的一句(如 "Invalid token"),原样带给用户
        let msg = body
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .or_else(|| body.get("message").and_then(|m| m.as_str()))
            .unwrap_or("");
        return Err(if msg.is_empty() {
            format!("HTTP {}", status.as_u16())
        } else {
            format!("HTTP {} · {msg}", status.as_u16())
        });
    }
    Ok(body)
}

/// 取回价目表的结果:解析好的目录 + **响应原文**(缓存要存原文,重启后能重新解析)+ 成功的域名。
pub struct FetchedCatalog {
    pub catalog: Catalog,
    pub raw: Value,
    pub host: String,
}

/// 取价目表(公开,无需密钥)。按 hosts 顺序试。
pub async fn fetch_catalog(
    client: &reqwest::Client,
    hosts: &[String],
) -> Result<FetchedCatalog, String> {
    let mut last = String::from("没有可用的入口地址");
    for host in hosts {
        // 路径取自 SourceDef.pricing_path:以前这里又把 "/api/pricing" 抄了一遍,
        // 改字段不生效、界面只会说"价目表取回失败"(报告 P3-16)
        let def = SOURCES.iter().find(|d| {
            source_hosts(d.id)
                .iter()
                .any(|h| h.eq_ignore_ascii_case(host))
        });
        let Some(path) = def.map(|d| d.pricing_path).unwrap_or(None) else {
            last = format!("{host} 这个来源没有公开价目表");
            continue;
        };
        let url = format!("https://{host}{path}");
        match get_json(client, &url, None).await {
            Ok(v) => match parse_catalog(&v) {
                Ok(catalog) => {
                    return Ok(FetchedCatalog {
                        catalog,
                        raw: v,
                        host: host.clone(),
                    })
                }
                Err(e) => last = format!("{host}: {e}"),
            },
            Err(e) => last = format!("{host}: {e}"),
        }
    }
    Err(format!("价目表取回失败 · {last}"))
}

/// 取「这把 key 能调哪些模型」(网关按分组过滤)。同样按 hosts 顺序试。
pub async fn fetch_usable(
    client: &reqwest::Client,
    hosts: &[String],
    key: &str,
) -> Result<(Vec<String>, String), String> {
    let mut last = String::from("没有可用的入口地址");
    for host in hosts {
        let url = format!("https://{host}/v1/models");
        match get_json(client, &url, Some(key)).await {
            Ok(v) => {
                let ids = parse_usable(&v);
                if ids.is_empty() {
                    last = format!("{host}: 列表是空的");
                    continue;
                }
                return Ok((ids, host.clone()));
            }
            Err(e) => last = format!("{host}: {e}"),
        }
    }
    Err(format!("可用模型取回失败 · {last}"))
}

/// 取"只有 id 列表"的来源(Hapi / OpenCode Go)。
/// 公开列表不带密钥;需要密钥的来源没配密钥时直接报错(界面据此提示先去配)。
pub async fn fetch_source_ids(
    client: &reqwest::Client,
    def: &SourceDef,
    key: Option<&str>,
) -> Result<(Vec<String>, String), String> {
    if !def.list_public && key.is_none() {
        return Err(format!("{} 的模型列表需要密钥:先在「渠道」页签填一次", def.name));
    }
    let url = format!("https://{}{}", def.api_host, def.list_path);
    let v = get_json(client, &url, key).await?;
    let ids = parse_usable(&v);
    if ids.is_empty() {
        return Err(format!("{}:列表是空的", def.name));
    }
    Ok((ids, def.api_host.to_string()))
}

/// 由 id 列表造模型行。**没有的字段一律留空,不编** ——
/// 列表接口只给 id/object/created/owned_by,那就只有 id 是真的。
/// `chat = true`:拿不到类型信息 ≠ 不是对话模型,不能因此把它们挡在「只看编码/对话」之外。
pub fn rows_from_ids(ids: &[String], usable: &[String], usable_known: bool) -> Vec<ModelRow> {
    let set: std::collections::HashSet<&str> = usable.iter().map(|s| s.as_str()).collect();
    let mut rows: Vec<ModelRow> = ids
        .iter()
        .map(|id| ModelRow {
            id: id.clone(),
            chat: true,
            usable: if usable_known {
                Some(set.contains(id.as_str()))
            } else {
                None
            },
            ..Default::default()
        })
        .collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

/// Same ID-only list, enriched with the protocol documented for that source.
pub fn rows_from_source_ids(
    source: &str,
    ids: &[String],
    usable: &[String],
    usable_known: bool,
) -> Vec<ModelRow> {
    let mut rows = rows_from_ids(ids, usable, usable_known);
    for row in &mut rows {
        row.api_protocol = api_protocol(source, &row.id);
    }
    rows
}

// ───────────────────────── 缓存与视图 ─────────────────────────

/// 缓存落在 SQLite 的 settings 里(单独一行,不进 Config)。
///
/// 为什么不进 Config:Config 每次设置变更都会被前端整份往返一次,塞进 100 KB 的
/// 价目表等于每次改个开关都搬运一遍,而且前端那份缓存会过期打架。
pub const CACHE_KEY: &str = "model_cache";

/// 价目表缓存的自动刷新间隔:一天一次。这是元数据不是余额,没有理由高频拉。
pub const CACHE_TTL_SEC: i64 = 24 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelCache {
    pub version: String,
    pub fetched_at: i64,
    /// `/api/pricing` 的响应原文(解析后的价目表由 catalog() 现算)
    pub pricing: String,
    /// 这把 key 能调的模型 id;`usable_at` = 0 表示**从没成功取到过**
    /// (没配密钥 / 请求失败),界面据此把「可用」退成「未知」而不是「都不可用」
    pub usable: Vec<String>,
    pub usable_at: i64,
    /// 最近一次成功的域名(价目表 / 可用集合各记一个,写进客户端时优先用后者)
    pub host: String,
    pub usable_host: String,
    /// 只有 id 列表的来源(Hapi / OpenCode Go)。4SAPI 有自己的价目表字段,不进这里。
    #[serde(default)]
    pub sources: HashMap<String, SourceCache>,
}

/// 只有 id 列表的来源的缓存。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SourceCache {
    pub ids: Vec<String>,
    pub fetched_at: i64,
    pub host: String,
    /// Which endpoint produced this list. A changed endpoint invalidates the snapshot.
    #[serde(default)]
    pub list_path: String,
    /// 该来源这把 key 能调的模型;`usable_at` = 0 表示从没成功取到过
    pub usable: Vec<String>,
    pub usable_at: i64,
}

impl SourceCache {
    pub fn usable_known(&self) -> bool {
        self.usable_at > 0
    }
    pub fn stale(&self, now: i64) -> bool {
        self.fetched_at <= 0 || now - self.fetched_at > CACHE_TTL_SEC
    }
}

impl ModelCache {
    pub fn load(store: &crate::store::Store) -> Option<Self> {
        store
            .get_setting(CACHE_KEY).ok().flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn save(&self, store: &crate::store::Store) -> Result<(), String> {
        let json = serde_json::to_string(self).map_err(|e| e.to_string())?;
        store.set_setting(CACHE_KEY, &json)
    }

    pub fn catalog(&self) -> Result<Catalog, String> {
        let v: Value = serde_json::from_str(&self.pricing)
            .map_err(|e| format!("缓存的价目表解析失败: {e}"))?;
        parse_catalog(&v)
    }

    /// 有没有成功取到过可用集合(决定界面是「可用」还是「未知」)
    pub fn usable_known(&self) -> bool {
        self.usable_at > 0
    }

    pub fn stale(&self, now: i64) -> bool {
        self.fetched_at <= 0 || now - self.fetched_at > CACHE_TTL_SEC
    }

    /// 写进客户端时用的入口域名:优先「/v1/models 成功过的域名」(那才是这份密钥
    /// 真被接受的地方),其次价目表成功过的,最后兜底主域名。
    ///
    /// 认过密钥的域名要求 `usable_at > 0`:只记了域名、却没成功取到过列表,
    /// 说明那次请求本身失败了,不能拿它当「这个域名收这把密钥」的证据。
    pub fn best_host(&self) -> String {
        if self.usable_at > 0 && !self.usable_host.is_empty() {
            return self.usable_host.clone();
        }
        if !self.host.is_empty() {
            return self.host.clone();
        }
        "4sapi.com".to_string()
    }

    pub fn base_url(&self) -> String {
        format!("https://{}/v1", self.best_host())
    }
}

/// 给界面的一行:价目表字段 + 按当前分组算出来的单价。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowView {
    #[serde(flatten)]
    pub row: ModelRow,
    pub price: Price,
}

/// 按分组把所有行组装成视图(单价在这里一次算好,前端不做算术)。
pub fn row_views(cat: &Catalog, group: &str) -> Vec<RowView> {
    row_views_of(cat.rows.clone(), &cat.groups, group)
}

/// 同一件事,但收现成的行与分组表 —— 只有 id 列表的来源没有分组表,价自然是空的。
pub fn row_views_of(rows: Vec<ModelRow>, groups: &[Group], group: &str) -> Vec<RowView> {
    rows.into_iter()
        .map(|r| RowView {
            price: price_of(&r, groups, group),
            row: r,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog_fixture() -> Catalog {
        parse_catalog(&json!({
            "pricing_version": "abc123",
            "auto_groups": ["一key官方中转全模型", "企业推荐全模型分组"],
            "group_ratio": {
                "一key官方中转全模型": 2,
                "企业推荐全模型分组": 5,
                "claude-code": 1,
                "国产全模型": 0.8
            },
            "usable_group": {"一key官方中转全模型": "一key 全模型"},
            "vendors": [{"id": 3, "name": "OpenAI"}, {"id": 1, "name": "Anthropic"}],
            "data": [
                {   // 普通按 token 计价
                    "model_name": "claude-sonnet-5",
                    "description": "Sonnet 5 是 Anthropic 目前能力最强的 Sonnet…",
                    "tags": "1M,多模态,文本",
                    "vendor_id": 1, "quota_type": 0,
                    "model_ratio": 1.0, "completion_ratio": 5.0, "model_price": 0,
                    "enable_groups": ["一key官方中转全模型", "claude-code", "企业推荐全模型分组"],
                    "supported_endpoint_types": ["anthropic", "openai"]
                },
                {   // 所选分组不含它 → 价按含有它的分组算
                    "model_name": "glm-5.3",
                    "tags": "1M,文本",
                    "vendor_id": 1, "quota_type": 0,
                    "model_ratio": 4.0, "completion_ratio": 3.5,
                    "enable_groups": ["国产全模型"],
                    "supported_endpoint_types": ["openai"]
                },
                {   // 按次计费
                    "model_name": "gpt-image-2.5",
                    "tags": "图像,2K",
                    "vendor_id": 3, "quota_type": 1,
                    "model_ratio": 0, "completion_ratio": 0, "model_price": 0.1,
                    "enable_groups": ["一key官方中转全模型"],
                    "supported_endpoint_types": ["image-generation", "openai"]
                },
                {   // 阶梯计价
                    "model_name": "grok-4.7",
                    "tags": "文本,500K",
                    "vendor_id": 3, "quota_type": 0,
                    "model_ratio": 37.5, "completion_ratio": 1.0,
                    "billing_mode": "tiered_expr",
                    "billing_expr": "len < 200000 ? tier(\"Short context\", p * 2 + c * 6) : tier(\"Long context\", p * 4 + c * 12)",
                    "enable_groups": ["一key官方中转全模型"],
                    "supported_endpoint_types": ["openai"]
                },
                {   // 向量模型:不该出现在默认列表里
                    "model_name": "text-embedding-3-small",
                    "tags": "向量",
                    "vendor_id": 3, "quota_type": 0,
                    "model_ratio": 0.01, "completion_ratio": 0,
                    "enable_groups": ["一key官方中转全模型"],
                    "supported_endpoint_types": ["openai"]
                }
            ]
        }))
        .unwrap()
    }

    #[test]
    fn parses_catalog_fields() {
        let c = catalog_fixture();
        assert_eq!(c.version, "abc123");
        assert_eq!(c.default_group(), "一key官方中转全模型");
        assert_eq!(c.rows.len(), 5);
        assert_eq!(c.groups.len(), 4);
        let s = c.find("claude-sonnet-5").unwrap();
        assert_eq!(s.vendor, "Anthropic");
        assert_eq!(s.context.as_deref(), Some("1M"));
        assert_eq!(context_tokens(s.context.as_deref().unwrap()), Some(1_000_000));
        assert!(s.chat && s.multimodal);
        assert!(s.usable.is_none(), "还没取 /v1/models 时是「未知」,不是「不可用」");
    }

    /// 图像/向量模型不算「能配到编辑器里用」的,但**不删除**,开关一开还在。
    #[test]
    fn chat_filter_keeps_text_models_only_by_default() {
        let c = catalog_fixture();
        assert!(c.find("gpt-image-2.5").unwrap().chat == false);
        assert!(c.find("text-embedding-3-small").unwrap().chat == false);
        assert!(c.find("claude-sonnet-5").unwrap().chat);
        assert_eq!(c.chat_count(), 3);
    }

    /// 单价:站点的公式 2 × 倍率 × 分组倍率;输出再乘 completion_ratio。
    #[test]
    fn price_uses_site_formula() {
        let c = catalog_fixture();
        let s = c.find("claude-sonnet-5").unwrap();
        let p = price_of(s, &c.groups, "一key官方中转全模型");
        assert_eq!(p.input, Some(4.0)); // 2 × 1.0 × 2
        assert_eq!(p.output, Some(20.0)); // × 5.0
        assert_eq!(p.group_ratio, 2.0);
        assert!(p.basis_group.is_none(), "所选分组含它,不该有回退分组");
        assert!(!p.tiered);

        // 换到倍率 1 的 claude-code 分组
        let p = price_of(s, &c.groups, "claude-code");
        assert_eq!(p.input, Some(2.0));
        assert_eq!(p.output, Some(10.0));
    }

    /// 所选分组不含这个模型:退回含有它的最便宜分组,并标明用的是哪个分组。
    #[test]
    fn price_falls_back_to_a_group_that_has_it() {
        let c = catalog_fixture();
        let glm = c.find("glm-5.3").unwrap();
        let p = price_of(glm, &c.groups, "一key官方中转全模型");
        assert_eq!(p.basis_group.as_deref(), Some("国产全模型"));
        assert_eq!(p.group, "国产全模型");
        assert_eq!(p.group_ratio, 0.8);
        assert_eq!(p.input, Some(6.4)); // 2 × 4.0 × 0.8
        assert_eq!(p.output, Some(22.4));
    }

    /// 按次计费与阶梯计价:各自的标识要出来,不给「确定价」的假象。
    #[test]
    fn per_call_and_tiered_are_marked() {
        let c = catalog_fixture();
        let img = c.find("gpt-image-2.5").unwrap();
        let p = price_of(img, &c.groups, "一key官方中转全模型");
        assert_eq!(p.per_call, Some(0.1));
        assert!(p.input.is_none(), "按次计费没有输入单价");

        let grok = c.find("grok-4.7").unwrap();
        let p = price_of(grok, &c.groups, "一key官方中转全模型");
        assert!(p.tiered);
        assert_eq!(p.input, Some(150.0)); // 2 × 37.5 × 2
        assert!(grok.billing_expr.as_deref().unwrap().contains("tier(\"Long context\""));
    }

    /// F7:按次计费但站点没给 `model_price` → 是"没有数据",不是"¥0.000 / 次"。
    /// 契约见 [`Price`]:"取不到一律 None —— 不用 0 顶替"。
    #[test]
    fn per_call_is_none_when_the_site_gives_no_price() {
        let c = parse_catalog(&json!({
            "data": [
                { "model_name": "按次-没价", "quota_type": 1, "tags": "图像",
                  "supported_endpoint_types": ["openai"] },
                { "model_name": "按次-有价", "quota_type": 1, "model_price": 0.5, "tags": "图像",
                  "supported_endpoint_types": ["openai"] }
            ]
        }))
        .unwrap();
        let p = price_of(c.find("按次-没价").unwrap(), &c.groups, "");
        assert!(p.per_call.is_none(), "缺字段必须给 None,拿 0 顶替会把「没数据」显示成「¥0.000 / 次」");

        let p = price_of(c.find("按次-有价").unwrap(), &c.groups, "");
        assert_eq!(p.per_call, Some(0.5));
    }

    /// 分组表里找不到任何可用分组 → 不给价(返回 None),不用 0 顶替。
    #[test]
    fn price_is_none_when_no_group_is_known() {
        let c = catalog_fixture();
        let mut row = c.find("glm-5.3").unwrap().clone();
        row.groups = vec!["站点新分组".into()];
        let p = price_of(&row, &c.groups, "一key官方中转全模型");
        assert!(p.input.is_none() && p.output.is_none());
    }

    #[test]
    fn usable_marks_only_listed_ids() {
        let mut c = catalog_fixture();
        assert!(c.find("glm-5.3").unwrap().usable.is_none());
        c.mark_usable(&["claude-sonnet-5".into(), "glm-5.3".into()]);
        assert_eq!(c.find("claude-sonnet-5").unwrap().usable, Some(true));
        assert_eq!(c.find("grok-4.7").unwrap().usable, Some(false));
    }

    /// /v1/models 的标准响应;结构不对 → 空列表(调用方据此退成「未知」)。
    #[test]
    fn parses_openai_model_list() {
        let ids = parse_usable(&json!({
            "object": "list",
            "data": [{"id": "claude-sonnet-5", "object": "model"},
                     {"id": "glm-5.3", "object": "model"}]
        }));
        assert_eq!(ids, vec!["claude-sonnet-5", "glm-5.3"]);
        assert!(parse_usable(&json!({"error": {"message": "Invalid token"}})).is_empty());
        assert!(parse_usable(&json!({})).is_empty());
    }

    /// 结构不对必须报错,而不是给一份「0 个模型」的空目录。
    #[test]
    fn bad_catalog_shape_is_an_error() {
        assert!(parse_catalog(&json!({"success": false, "message": "Unauthorized"})).is_err());
    }

    /// 缺字段的条目要能容错(站点字段会变),脏数字不能穿进价格。
    #[test]
    fn tolerates_missing_and_dirty_fields() {
        let c = parse_catalog(&json!({
            "data": [
                {"model_name": "a"},                                   // 几乎什么都没有
                {"description": "没有 model_name"},                     // 没名字 → 跳过
                {"model_name": "b", "model_ratio": "0.5", "completion_ratio": "NaN",
                 "tags": "1M, 文本", "supported_endpoint_types": ["openai"]}
            ]
        }))
        .unwrap();
        assert_eq!(c.rows.len(), 2);
        let b = c.find("b").unwrap();
        assert_eq!(b.ratio, 0.5, "字符串数字要认");
        assert_eq!(b.completion_ratio, 0.0, "NaN 不能穿进去");
        assert_eq!(b.tags, vec!["1M", "文本"]);
        let p = price_of(b, &[Group { name: "g".into(), ratio: 2.0, note: String::new() }], "g");
        assert_eq!(p.input, Some(2.0));
        assert!(p.output.is_none(), "completion_ratio 是脏值时不给输出价");
    }

    /// 来源判定:靠客户端条目的 url 认它属于哪个来源(同名模型可能存在于多个来源)。
    #[test]
    fn source_of_url_maps_domains_to_sources() {
        assert_eq!(source_of_url("https://4sapi.com/v1"), Some("4sapi"));
        assert_eq!(source_of_url("https://4sapi.cn/v1"), Some("4sapi"), "备用域名也要认");
        assert_eq!(source_of_url("https://ai.yuchuantest.com/v1"), Some("hapi"));
        assert_eq!(source_of_url("https://opencode.ai/zen/go/v1"), Some("opencode-go"));
        // 别家的地址不能误认
        assert_eq!(source_of_url("https://api.deepseek.com/v1"), None);
        assert_eq!(source_of_url(""), None);
        // Hapi 的域名不许被 4SAPI 抢走(4SAPI 的域名是精确串,不含 yuchuantest)
        assert_ne!(source_of_url("https://ai.yuchuantest.com/v1"), Some("4sapi"));
    }

    /// 只有 id 的来源:行里除了 id 全是空的 —— 没有的字段不编。
    #[test]
    fn rows_from_ids_stay_honest() {
        let ids: Vec<String> = vec!["b-model".into(), "a-model".into()];
        let rows = rows_from_ids(&ids, &["a-model".into()], true);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "a-model", "按 id 排序");
        assert_eq!(rows[0].usable, Some(true));
        assert_eq!(rows[1].usable, Some(false));
        assert!(rows.iter().all(|r| r.chat && r.tags.is_empty() && r.context.is_none()));
        assert!(rows.iter().all(|r| r.ratio == 0.0 && r.groups.is_empty()));
        // 可用集合未知时是 None,不是"都不可用"
        let rows = rows_from_ids(&ids, &[], false);
        assert!(rows.iter().all(|r| r.usable.is_none()));
    }

    /// 来源表本身:三个来源,只有 4SAPI 有价目表;Go 的列表公开。
    #[test]
    fn source_table_shape() {
        assert_eq!(SOURCES.len(), 3);
        assert_eq!(find_source("hapi").unwrap().name, "Hapi");
        assert!(find_source("nope").is_none());
        assert!(find_source("opencode-go").unwrap().list_public);
        assert!(!find_source("hapi").unwrap().list_public);
        assert_eq!(
            SOURCES.iter().filter(|s| s.pricing_path.is_some()).count(),
            1,
            "只有 4SAPI 有公开价目表"
        );
        // 域名转发:4SAPI 的四个域名来自渠道定义
        assert_eq!(source_hosts("4sapi").len(), 4);
        assert_eq!(source_hosts("hapi"), vec!["ai.yuchuantest.com".to_string()]);
    }

    #[test]
    fn context_tokens_reads_k_and_m() {
        assert_eq!(context_tokens("1M"), Some(1_000_000));
        assert_eq!(context_tokens("256K"), Some(256_000));
        assert_eq!(context_tokens("128k"), Some(128_000));
        assert_eq!(context_tokens("多模态"), None);
        assert_eq!(context_tokens(""), None);
        // 纯解析器照直算;「2K 算不算上下文」由 is_context_tag 决定(分辨率不算)
        assert_eq!(context_tokens("2K"), Some(2_000));
        assert!(is_context_tag("256K") && is_context_tag("1M"));
        assert!(!is_context_tag("2K") && !is_context_tag("4K"), "分辨率不是上下文长度");
        assert!(!is_context_tag("图像"));
    }

    /// 图像模型的分辨率 tag 不能被当成上下文长度。
    #[test]
    fn image_resolution_is_not_a_context() {
        let c = parse_catalog(&json!({
            "data": [{"model_name": "gpt-image-1", "tags": "图像,4K",
                      "supported_endpoint_types": ["image-generation"]}]
        }))
        .unwrap();
        assert_eq!(c.find("gpt-image-1").unwrap().context, None);
    }

    /// 行视图要同时带上价目表字段(摊平)与单价 —— 前端只渲染,不做算术。
    #[test]
    fn row_views_flatten_row_and_attach_price() {
        let c = catalog_fixture();
        let views = row_views(&c, "一key官方中转全模型");
        let v = serde_json::to_value(&views).unwrap();
        let one = v
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["id"] == "claude-sonnet-5")
            .expect("应有这一行");
        assert_eq!(one["vendor"], "Anthropic");
        assert_eq!(one["price"]["input"], 4.0);
        assert_eq!(one["price"]["output"], 20.0);
        assert_eq!(one["price"]["groupRatio"], 2.0);
        assert!(one.get("price").is_some() && one.get("row").is_none(), "字段要摊平,不要嵌一层 row");
    }

    /// 缓存:往返一致;入口域名的优先级是「可用集合成功过的」>「价目表成功过的」> 主域名。
    #[test]
    fn cache_roundtrip_and_host_priority() {
        let store = crate::store::Store::open_memory().unwrap();
        assert!(ModelCache::load(&store).is_none());

        let pricing = serde_json::to_string(&json!({
            "pricing_version": "v1",
            "group_ratio": {"g": 1},
            "data": [{"model_name": "m", "supported_endpoint_types": ["openai"]}]
        }))
        .unwrap();
        let c = ModelCache {
            version: "v1".into(),
            fetched_at: 1_700_000_000,
            pricing,
            usable: vec!["m".into()],
            usable_at: 1_700_000_100,
            host: "4sapi.cn".into(),
            usable_host: "4sapi.com".into(),
            sources: Default::default(),
        };
        c.save(&store).unwrap();
        let back = ModelCache::load(&store).unwrap();
        assert_eq!(back.version, "v1");
        assert_eq!(back.usable, vec!["m".to_string()]);
        assert!(back.usable_known());
        assert_eq!(back.best_host(), "4sapi.com", "优先用它自己接受过密钥的域名");
        assert_eq!(back.base_url(), "https://4sapi.com/v1");
        assert_eq!(back.catalog().unwrap().rows.len(), 1);

        assert!(!back.stale(1_700_000_000 + 3600), "一小时内不算过期");
        assert!(back.stale(1_700_000_000 + CACHE_TTL_SEC + 1));

        // 从没取到过可用集合:usable_known 为 false(界面「未知」而不是「不可用」),
        // 入口也退回价目表成功过的域名
        let c2 = ModelCache {
            usable_at: 0,
            ..back
        };
        assert!(!c2.usable_known());
        assert_eq!(c2.best_host(), "4sapi.cn", "退到价目表成功过的域名");
        assert_eq!(ModelCache::default().best_host(), "4sapi.com", "再退到主域名");
    }
}
