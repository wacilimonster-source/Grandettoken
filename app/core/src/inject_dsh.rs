use super::*;

// ───────────────────────── DeepSeek Harness ─────────────────────────
//
// 两份文件、一套写入:
//   <harness home>/profiles/<profile>/cordis.patch.yml → llm-pi-ai 的 config.providers.<route>
//   <harness home>/.credentials.yaml                   → refs.<ROUTE>_API_KEY = 明文密钥
//
// 与其它四家最大的不同:**边跑边写是它认的用法**。DSH 的 profile 按请求解析,而且它自己的
// settings / credentials 两层都收敛外部编辑(配置编辑器在派生候选之前先采纳外部改动;
// 凭据存储在同一把写锁下合并行编辑)。四条共同前提里的另外三条一条不少 ——
// 先备份 / 只增改自己写的那条 / 读回校验不过就回滚。
//
// 结构取自 2026-10-17 在本机 DSH 安装包里四个包的取证(dsh-llm-pi-ai / dsh-settings /
// dsh-config-editor / dsh-credentials-local 的 README 与 lib/index.js),不是猜的:
//   · 路由字段:`api`(`openai-completions` / `openai-responses` / `anthropic-messages`)、
//     `baseURL`、`apiKeyEnv`、`models[]`(`id`/`name`/`contextWindow`/`maxTokens`/`input`);
//   · 目录里已有的提供方(如 `opencode-go`)**不给** api / baseURL —— 端点与协议由目录给;
//     但**只有目录里真有这个模型时**才成立:目录没有的模型必须按官网协议写自建 route,
//     否则 pi-ai 会直接报 `model "…" needs an api` 而把 route 判成不可服务(2026-10-20 实测:
//     用户点 `minimax-m3.1-flash-free` 就是这条,而它不在 pi-ai 目录里);
//   · 自建 route 按协议拆分(`opencode-go-custom` / `-custom-responses` / `-custom-anthropic`),
//     路由名不能与目录 route 重名,否则 pi-ai 会拿目录的协议覆盖掉我们写的;
//   · pi-ai 不提供的路由必须给 `api` + `baseURL` + **非空** models,所以移除最后一个模型时
//     连 route 一起收走(留一个空 models 列表会让 pi-ai 把它判成"不可服务");
//   · `apiKeyEnv` 是**凭据引用**,值落在 `.credentials.yaml` 的 `refs:` 下(名字按
//     `<ROUTE>_API_KEY`、非字母数字换成下划线)。但引用名必须匹配 `^[A-Za-z_][A-Za-z0-9_]*$`
//     (dsh-credentials 的 REF_PATTERN):照这个公式,来源 `4sapi` 会派生出 `4SAPI_API_KEY` ——
//     数字开头,**整个 refs 段连同 pi-ai 的全部 route 一起被拒**(实测)。所以 route 键必须
//     以字母开头(`4sapi` → `foursapi`),引用名跟着变成 `FOURSAPI_API_KEY`。
//   · 模型显示名按来源区分(`4S-` / `HP-` / `OG `):三家各有一个前缀,同一份列表里
//     一眼看得出这条是哪个渠道的(用户口径:4s / opencodego 要显示成不同的名字)。

/// patch 层里那条 pi-ai 条目:认 id,也认 name(用户手写的那份两个都有)。
pub(super) const DSH_PI_AI_ID: &str = "llm-pi-ai";
pub(super) const DSH_PI_AI_NAME: &str = "@deepseek-ai/dsh-llm-pi-ai";
/// 桌面端的默认 profile 名。
pub(super) const DSH_DESKTOP_PROFILE: &str = "desktop";
/// 默认选中的模型写在 `agent-default-model` 这条里(只读它,不替用户改)。
pub(super) const DSH_DEFAULT_MODEL_ID: &str = "agent-default-model";

/// DSH 的凭据引用名规则(dsh-credentials 的 `REF_PATTERN`)。照它校验自己派生的名字 ——
/// 不合格的名字会让凭据文件整份被拒,连带 pi-ai 的所有 route 一起失效。
pub(super) fn dsh_ref_legal(name: &str) -> bool {
    let mut it = name.chars();
    match it.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    it.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// pi-ai **已安装目录**里的一个提供方:它自带的模型 id / 协议 / 上下文 / 能力。
///
/// 取自 DSH 安装包 `@earendil-works/pi-ai/dist/providers/data/<route>.json`
/// (2026-10-20,本机 v1.1.0 的 `opencode-go.json`,30 个 chat 模型)。
/// 为什么要抄一份:① 目录里没有的模型**不能**写进目录 route(会报 `needs an api`);
/// ② 目录里有的模型不该被我们改窄(用户那条 `input` 是 `text+image`,以前被写成只有 `text`)。
pub struct CatalogRoute {
    /// 目录里这个 route 的模型:`(id, 协议, 上下文, 是否支持图片)`。
    pub(super) models: &'static [(&'static str, &'static str, i64, bool)],
}

/// 目录 route `opencode-go`(协议三家混着:`openai-completions` / `openai-responses` /
/// `anthropic-messages`,所以不写 route 级 `api`)。
pub(super) const DSH_CATALOG: &[(&str, CatalogRoute)] = &[
    (
        "opencode-go",
        CatalogRoute {
            models: &[
                ("claude-haiku-5-5", "anthropic-messages", 1_000_000, true),
                ("minimax-m3", "anthropic-messages", 1_000_000, true),
                ("qwen3.7-plus", "anthropic-messages", 1_000_000, true),
                ("qwen3.8-flash", "anthropic-messages", 1_000_000, true),
                ("qwen3.8-max", "anthropic-messages", 1_000_000, true),
                ("deepseek-v4-flash", "openai-completions", 1_000_000, false),
                ("deepseek-v4-flash-vision-exp", "openai-completions", 1_000_000, true),
                ("deepseek-v4-pro", "openai-completions", 1_000_000, false),
                ("deepseek-v4.1-flash", "openai-completions", 1_000_000, true),
                ("glm-5.2", "openai-completions", 1_000_000, false),
                ("glm-5.3", "openai-completions", 1_000_000, false),
                ("glm-5.3-flash", "openai-completions", 1_000_000, true),
                ("hy3", "openai-completions", 256_000, false),
                ("hy4-preview", "openai-completions", 1_024_000, false),
                ("kimi-k2.7-code", "openai-completions", 262_144, true),
                ("kimi-k3", "openai-completions", 1_048_576, true),
                ("longcat-2.0", "openai-completions", 1_000_000, false),
                ("longcat-2.5-preview-free", "openai-completions", 1_000_000, true),
                ("mimo-v2.5", "openai-completions", 1_000_000, true),
                ("mimo-v2.5-pro", "openai-completions", 1_048_576, false),
                ("mimo-v2.6-flash", "openai-completions", 1_048_576, true),
                ("mimo-v2.6-pro", "openai-completions", 1_048_576, true),
                ("minimax-m2.7", "openai-completions", 204_800, false),
                ("space-bunny", "openai-completions", 1_048_576, true),
                ("gpt-5.6-luna", "openai-responses", 1_050_000, true),
                ("gpt-6-luna", "openai-responses", 1_050_000, true),
                ("grok-4.6", "openai-responses", 500_000, true),
                ("grok-4.7", "openai-responses", 500_000, true),
                ("muse-spark-1.2-contributor", "openai-responses", 1_048_576, true),
                ("muse-spark-1.3-contributor", "openai-responses", 1_048_576, true),
            ],
        },
    ),
];

/// 目录里这个 route 的模型表(没有这个 route 就是空)。
pub(super) fn dsh_catalog(route: &str) -> Option<&'static CatalogRoute> {
    DSH_CATALOG.iter().find(|(k, _)| *k == route).map(|(_, v)| v)
}

/// 目录里这个 route 的这个模型。
pub(super) fn dsh_catalog_model(
    route: &str,
    id: &str,
) -> Option<&'static (&'static str, &'static str, i64, bool)> {
    dsh_catalog(route)?.models.iter().find(|(m, ..)| *m == id)
}

/// harness home:`$DSH_HOME`,否则 `~/.dsh`(与 dsh-credentials-local 的默认一致)。
pub(super) fn dsh_home() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("DSH_HOME") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    home().map(|h| h.join(".dsh"))
}

pub(super) fn dsh_patch_of(home: &Path, profile: &str) -> PathBuf {
    home.join("profiles").join(profile).join("cordis.patch.yml")
}

pub(super) fn dsh_creds_of(home: &Path) -> PathBuf {
    home.join(".credentials.yaml")
}

/// 挑一个 profile 写:桌面端默认 `desktop`;没有就挑**已经配了 llm-pi-ai** 的那个
/// (说明用户正在用它);再没有就按名字第一个。挑不出来就没装。
pub(super) fn dsh_profile(home: &Path) -> Option<String> {
    let dir = home.join("profiles");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| {
            let d = dir.join(n);
            d.join("cordis.patch.yml").exists() || d.join("cordis.yml").exists()
        })
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort();
    if names.iter().any(|n| n == DSH_DESKTOP_PROFILE) {
        return Some(DSH_DESKTOP_PROFILE.to_string());
    }
    for n in &names {
        if let Ok(t) = std::fs::read_to_string(dir.join(n).join("cordis.patch.yml")) {
            if t.contains(DSH_PI_AI_ID) {
                return Some(n.clone());
            }
        }
    }
    names.into_iter().next()
}

/// 站点的两份文件。约定:`files[0]` = patch,`files[1]` = 凭据(名字反查,不靠顺序)。
pub(super) fn dsh_paths(site: &Site) -> Result<(PathBuf, PathBuf), String> {
    let patch = site
        .files
        .iter()
        .find(|f| {
            f.file_name()
                .map(|n| n.to_string_lossy().eq_ignore_ascii_case("cordis.patch.yml"))
                .unwrap_or(false)
        })
        .cloned()
        .ok_or("站点里没有 cordis.patch.yml")?;
    let cred = site
        .files
        .iter()
        .find(|f| {
            f.file_name()
                .map(|n| n.to_string_lossy().eq_ignore_ascii_case(".credentials.yaml"))
                .unwrap_or(false)
        })
        .cloned()
        .ok_or("站点里没有 .credentials.yaml")?;
    Ok((patch, cred))
}

/// 这个来源写进 DSH 用哪个 route 键:一个来源一条 route(与 ZCode「每个来源一个 provider」
/// 同口径),互不覆盖。
///
/// 键不能直接照抄来源 id:route 键会派生凭据引用名,而引用名必须以字母开头
/// (`4sapi` → `4SAPI_API_KEY` 非法,整份凭据文件连带 pi-ai 全部 route 一起失效)。
pub(super) fn dsh_route(source: &str) -> String {
    match source {
        "4sapi" => "foursapi".to_string(),
        other => other.to_string(),
    }
}

/// 目录里没有、但我们仍要写的模型落到哪条自建 route 上(不与目录 route 重名,
/// 否则 pi-ai 会拿目录协议覆盖我们写的 `api`)。
pub(super) fn dsh_custom_route(source: &str) -> String {
    format!("{}-custom", dsh_route(source))
}

/// OpenCode Go's custom models need separate routes because pi-ai selects one
/// wire protocol per route. Keep the old `-custom` route for Chat Completions.
pub(super) fn dsh_custom_route_for(source: &str, protocol: Option<crate::models::ApiProtocol>) -> String {
    let root = dsh_route(source);
    match protocol {
        Some(crate::models::ApiProtocol::OpenAiResponses) => {
            format!("{root}-custom-responses")
        }
        Some(crate::models::ApiProtocol::AnthropicMessages) => {
            format!("{root}-custom-anthropic")
        }
        _ => dsh_custom_route(source),
    }
}

/// 这个模型进 DSH 时走哪条 route:
/// · 来源本身就不在目录里(4SAPI / Hapi) → 只有一条 route,一直都是自建;
/// · 来源在目录里:目录**真有**这个模型 → 目录 route(协议/端点/目录名都留给 pi-ai,
///   我们不写 `api`);目录里没有 → 按协议写入专用自建 route,并给出 `api` + `baseURL`。
/// 只剩测试在用:生产路径全部改走 `dsh_route_for_protocol`(要带协议)。
/// 留着不标 `#[cfg(test)]` 的话,lib 编译会报 dead_code。
#[cfg(test)]
pub(super) fn dsh_route_for(spec_source: &str, model: &str) -> String {
    dsh_route_for_protocol(
        spec_source,
        model,
        Some(crate::models::ApiProtocol::OpenAiChatCompletions),
    )
}

pub(super) fn dsh_route_for_protocol(
    spec_source: &str,
    model: &str,
    protocol: Option<crate::models::ApiProtocol>,
) -> String {
    let route = dsh_route(spec_source);
    if dsh_catalog(&route).is_none() {
        return route;
    }
    if dsh_catalog_model(&route, model).is_some() {
        route
    } else {
        dsh_custom_route_for(spec_source, protocol)
    }
}

/// 这条 route 是不是 pi-ai 已安装目录里就有的、且这个模型目录里也有。
/// 是 → 端点与协议由目录给,写它时**不给** api / baseURL(给了反而与目录打架)。
pub(super) fn dsh_catalog_route(route: &str, model: &str) -> bool {
    dsh_catalog_model(route, model).is_some()
}

/// 凭据引用名:DSH 自己的 Models 页派生的是 `<ROUTE>_API_KEY`(大写、非字母数字换下划线)。
/// 只要 route 以字母开头(见 `dsh_route`),这个名字一定满足 REF_PATTERN。
pub(super) fn dsh_key_ref(route: &str) -> String {
    let mut s = String::new();
    for c in route.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_uppercase());
        } else {
            s.push('_');
        }
    }
    format!("{s}_API_KEY")
}

/// 我们会写下去的 route 显示名全集(= 各来源的界面名)。盘上的 displayName 不在这份里,
/// 就说明是用户自己起的名字 —— 那就不覆盖。
pub(super) fn dsh_source_names() -> Vec<&'static str> {
    crate::models::SOURCES.iter().map(|d| d.name).collect()
}

/// 某条 route 的凭据引用名(唯一口径:写入、校验、移除、预告都用它)。
pub(super) fn dsh_ref_for(route: &str) -> String {
    dsh_key_ref(route)
}

/// 文件相对 harness home 的落点。备份清单要用它 —— 这两份文件不在同一个目录里,
/// 而其它四家的文件都直接躺在 root 下(所以那份清单只对 DSH 写)。
pub(super) fn dsh_rel(home: &Path, f: &Path) -> String {
    f.strip_prefix(home)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| {
            f.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        })
}

/// 读文本文件。不存在或读不了 → 空串(新建是合法初态)。
pub(super) fn dsh_read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// 原子写文本:先写同目录临时文件再改名 —— 半截 YAML 比不写更糟(DSH 会当场解析失败)。
pub(super) fn dsh_write_text(path: &Path, text: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| format!("文件名非法: {}", pretty(path)))?;
    let tmp = path.with_file_name(format!("{name}.ts-tmp"));
    std::fs::write(&tmp, text).map_err(|e| format!("写 {} 失败: {e}", pretty(&tmp)))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换 {} 失败: {e}", pretty(path))
    })
}

/// patch 文件里那条 pi-ai 条目。
pub(super) fn dsh_entry(root: &crate::yamledge::Node) -> Option<&crate::yamledge::Node> {
    root.children.iter().find(|e| {
        e.get("id").and_then(|n| n.scalar()) == Some(DSH_PI_AI_ID)
            || e.get("name").and_then(|n| n.scalar()) == Some(DSH_PI_AI_NAME)
    })
}

pub(super) fn dsh_providers(root: &crate::yamledge::Node) -> Option<&crate::yamledge::Node> {
    dsh_entry(root)?.get("config")?.get("providers")
}

/// 这条 route 属于哪个来源(认不出来的 route 不是我们写的)。
/// 除来源 route 本身,还要认自建 route(`<route>-custom`)与旧的 `4sapi`(改名前的写法,
/// 若用户机器上还留着就仍然算我们的,便于取消写入)。
pub(super) fn dsh_route_source(route: &str) -> Option<String> {
    if route == "4sapi" {
        return Some("4sapi".to_string());
    }
    if route == "gateway" || route == "gateway-custom" {
        return Some("gateway".to_string());
    }
    crate::models::SOURCES
        .iter()
        .find(|d| {
            dsh_route(d.id) == route
                || dsh_custom_route(d.id) == route
                || (d.id == "opencode-go"
                    && (dsh_custom_route_for(
                        d.id,
                        Some(crate::models::ApiProtocol::OpenAiResponses),
                    ) == route
                        || dsh_custom_route_for(
                            d.id,
                            Some(crate::models::ApiProtocol::AnthropicMessages),
                        ) == route))
        })
        .map(|d| d.id.to_string())
}

/// 我们写进 DSH 那条 route 的完整块(不带缩进,插入时统一缩进)。
pub(super) fn dsh_route_block(spec: &ModelSpec) -> Result<Vec<String>, String> {
    let route = dsh_route_for_protocol(&spec.source, &spec.id, spec.api_protocol);
    let catalog = dsh_catalog_route(&route, &spec.id);
    let mut out = vec![format!("{route}:")];
    // 目录 route 的显示名也给我们写的来源名(目录默认是 `opencode-go`,人看不出来是什么)
    out.push(format!(
        "  displayName: {}",
        crate::yamledge::scalar_text(&spec.source_name)
    ));
    if !catalog {
        let api = spec
            .api_protocol
            .and_then(crate::models::ApiProtocol::dsh_api)
            .ok_or_else(|| format!("{} 的接口协议不能写入 DeepSeek Harness", spec.id))?;
        if spec.base_url.trim().is_empty() {
            return Err(format!("{} 没有可用的接口地址", spec.id));
        }
        out.push(format!("  api: {api}"));
        out.push(format!(
            "  baseURL: {}",
            crate::yamledge::scalar_text(&spec.base_url)
        ));
    }
    out.push(format!(
        "  apiKeyEnv: {}",
        crate::yamledge::scalar_text(&dsh_ref_for(&route))
    ));
    out.push("  models:".to_string());
    // 列表项比 `models:` 再深 2 列(与用户那份手写文件、以及 DSH 自己的写法一致)
    out.extend(crate::yamledge::indented(&dsh_model_block(spec, &route), 4));
    Ok(out)
}

/// 一条模型条目(`- id: …`,不带缩进)。
///
/// 上下文与图片能力**只在知道时才写**:编一个能力上限会让客户端按不存在的上限发请求。
/// 目录里有的模型,直接照抄目录的能力 —— 否则会把目录的 `text+image` 写窄成 `text`;
/// 目录里没有的模型,只写我们确实知道的(`context_tokens` / `multimodal` 为空就不写,
/// 让 pi-ai 用 route 的默认能力,而不是由我们替它编一个)。
pub(super) fn dsh_model_block(spec: &ModelSpec, route: &str) -> Vec<String> {
    let cat = dsh_catalog_model(route, &spec.id);
    let mut out = vec![format!(
        "- id: {}",
        crate::yamledge::scalar_text(&spec.id)
    )];
    out.push(format!(
        "  name: {}",
        crate::yamledge::scalar_text(&spec.display_name)
    ));
    let ctx = cat.map(|c| c.2).or(spec.context_tokens);
    if let Some(ctx) = ctx {
        out.push(format!("  contextWindow: {ctx}"));
    }
    let (text_only, image) = match cat {
        // 目录里的模型:能力照目录写全(目录 route 不写 api,能力只能落在这里)
        Some(c) => (true, c.3),
        // 目录外的模型:只有确认多模态才写 input,不确认就整段省掉(默认 text)
        None if spec.multimodal => (true, true),
        None => (false, false),
    };
    if text_only {
        out.push("  input:".to_string());
        out.push("    - text".to_string());
        if image {
            out.push("    - image".to_string());
        }
    }
    out
}

/// 更新已有条目时,用户自己加过的字段(maxTokens / reasoningEfforts / compat …)原样留着 ——
/// 整条替换会把它们抹掉(与 ZCode 那条 P2-10 同一个教训)。
///
/// `name` 也留着:**用户已经改过的名字**(不是我们那几种前缀,或已经不是我们算出来的那个)
/// 一律不覆盖;只有名字还是我们起的(或原本没有名字)时才按新口径更新。
pub(super) fn dsh_model_merged(lines: &[String], old: &crate::yamledge::Node, ours: &[String]) -> Vec<String> {
    let old_name = old.get("name").and_then(|n| n.scalar()).unwrap_or("");
    let ours_name = ours
        .iter()
        .find_map(|l| l.trim().strip_prefix("name: "))
        .map(|v| v.trim_matches('"').to_string())
        .unwrap_or_default();
    let keep_user_name = !old_name.is_empty() && old_name != ours_name && !name_is_ours(old_name);
    let mut out: Vec<String> = Vec::with_capacity(ours.len());
    for l in ours {
        if keep_user_name && l.trim_start().starts_with("name: ") {
            continue;
        }
        out.push(l.clone());
    }
    for c in &old.children {
        let k = c.key.as_deref().unwrap_or("");
        if matches!(k, "id" | "name" | "contextWindow" | "input") {
            if keep_user_name && k == "name" {
                for i in c.start..c.end {
                    out.push(dsh_strip_lead(&lines[i], old.indent));
                }
            }
            continue;
        }
        for i in c.start..c.end {
            out.push(dsh_strip_lead(&lines[i], old.indent));
        }
    }
    out
}

/// 去掉行首 `n` 个缩进空格(行尾先剥掉)。
pub(super) fn dsh_strip_lead(raw: &str, n: usize) -> String {
    let c = raw.trim_end_matches('\n').trim_end_matches('\r');
    let cut = c.len().min(n);
    c[cut..].to_string()
}

/// route 那一段必须是块式 mapping:空值(null)替它补一个冒号行,标量值一律放弃。
pub(super) fn dsh_route_fix(lines: &mut Vec<String>, route: &str) -> Result<(), String> {
    let root = crate::yamledge::parse(lines)?;
    let r = dsh_providers(&root)
        .and_then(|p| p.get(route))
        .ok_or_else(|| format!("route「{route}」不见了"))?;
    if r.kind != crate::yamledge::Kind::Scalar {
        return Ok(());
    }
    if r.value.is_some() {
        return Err(format!(
            "route「{route}」的值是标量({:?}),不是块,本次不写",
            r.value.as_deref().unwrap_or("")
        ));
    }
    let line = format!("{}{route}:", " ".repeat(r.indent));
    crate::yamledge::splice(lines, r.start..r.end, &[line]);
    Ok(())
}

/// route 下设置一个标量键:有就换那一行,没有就插在它末尾。
pub(super) fn dsh_set_scalar(
    lines: &mut Vec<String>,
    route: &str,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let root = crate::yamledge::parse(lines)?;
    let r = dsh_providers(&root)
        .and_then(|p| p.get(route))
        .ok_or_else(|| format!("route「{route}」不见了"))?;
    let pad = " ".repeat(r.indent + 2);
    let line = format!("{pad}{key}: {}", crate::yamledge::scalar_text(value));
    match r.get(key) {
        Some(c) if c.kind == crate::yamledge::Kind::Scalar => {
            crate::yamledge::splice(lines, c.start..c.end, &[line]);
        }
        Some(c) => {
            return Err(format!(
                "route「{route}」下的 {key} 不是标量({}),本次不写",
                c.what()
            ))
        }
        None => crate::yamledge::splice(lines, r.end..r.end, &[line]),
    }
    Ok(())
}

/// 把这条模型放进 route 的 models 列表。返回"是不是新增的"。
pub(super) fn dsh_put_model(
    lines: &mut Vec<String>,
    route: &str,
    spec: &ModelSpec,
) -> Result<bool, String> {
    let item = dsh_model_block(spec, route);
    let root = crate::yamledge::parse(lines)?;
    let r = dsh_providers(&root)
        .and_then(|p| p.get(route))
        .ok_or_else(|| format!("route「{route}」不见了"))?;
    match r.get("models") {
        Some(m) => {
            let m = m.as_sequence()?;
            let ind = m.items().first().map(|x| x.indent).unwrap_or(m.indent + 2);
            if let Some(old) = m
                .items()
                .iter()
                .find(|x| x.get("id").and_then(|n| n.scalar()) == Some(spec.id.as_str()))
            {
                let merged = dsh_model_merged(lines, old, &item);
                let block = crate::yamledge::indented(&merged, ind);
                crate::yamledge::splice(lines, old.start..old.end, &block);
                Ok(false)
            } else {
                let block = crate::yamledge::indented(&item, ind);
                crate::yamledge::splice(lines, m.end..m.end, &block);
                Ok(true)
            }
        }
        None => {
            let pad = r.indent + 2;
            let mut block = vec![format!("{}models:", " ".repeat(pad))];
            block.extend(crate::yamledge::indented(&item, pad + 2));
            crate::yamledge::splice(lines, r.end..r.end, &block);
            Ok(true)
        }
    }
}

/// 写 patch:保证 pi-ai 条目 → config.providers → route → models 这条链都在。
/// 返回(新文本, 动作)。
pub(super) fn dsh_patch_apply(text: &str, spec: &ModelSpec) -> Result<(String, PatchAction), String> {
    let route = dsh_route_for_protocol(&spec.source, &spec.id, spec.api_protocol);
    let working_text = if spec.source == "opencode-go" {
        // Migrate only this model from old protocol-specific custom routes. Keep
        // native catalog routes and any other models/settings untouched.
        let old_custom_routes = [
            dsh_custom_route_for(
                &spec.source,
                Some(crate::models::ApiProtocol::OpenAiChatCompletions),
            ),
            dsh_custom_route_for(
                &spec.source,
                Some(crate::models::ApiProtocol::OpenAiResponses),
            ),
            dsh_custom_route_for(
                &spec.source,
                Some(crate::models::ApiProtocol::AnthropicMessages),
            ),
        ]
        .into_iter()
        .filter(|old| old != &route)
        .collect::<Vec<_>>();
        dsh_patch_remove_routes(text, spec, &old_custom_routes)?.0
    } else {
        text.to_string()
    };
    let mut lines = crate::yamledge::split(&working_text);

    // ① 条目/接口层:缺哪层补哪层,已有的一个字不动
    {
        let root = crate::yamledge::parse(&lines)?;
        if dsh_entry(&root).is_none() {
            let pad = lines.len();
            let block = vec![
                format!("- id: {DSH_PI_AI_ID}"),
                format!(
                    "  name: {}",
                    crate::yamledge::scalar_text(DSH_PI_AI_NAME)
                ),
                "  config:".to_string(),
                "    providers:".to_string(),
            ];
            crate::yamledge::splice(&mut lines, pad..pad, &block);
        }
    }
    {
        let root = crate::yamledge::parse(&lines)?;
        let entry = dsh_entry(&root).ok_or("刚补进去的条目又不见了")?;
        if entry.get("config").is_none() {
            let at = entry.end;
            let block = vec!["  config:".to_string(), "    providers:".to_string()];
            crate::yamledge::splice(&mut lines, at..at, &block);
        }
    }
    {
        let root = crate::yamledge::parse(&lines)?;
        let entry = dsh_entry(&root).ok_or("条目不见了")?;
        let config = entry.get("config").ok_or("config 不见了")?;
        if config.get("providers").is_none() {
            let at = config.end;
            let pad = " ".repeat(config.indent + 2);
            let block = vec![format!("{pad}providers:")];
            crate::yamledge::splice(&mut lines, at..at, &block);
        }
    }

    // ② route:没有就整块插进 providers 末尾
    let existed = {
        let root = crate::yamledge::parse(&lines)?;
        dsh_providers(&root)
            .and_then(|p| p.get(&route))
            .map(|r| {
                r.get("models")
                    .map(|m| {
                        m.items()
                            .iter()
                            .any(|x| x.get("id").and_then(|n| n.scalar()) == Some(spec.id.as_str()))
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    };
    {
        let root = crate::yamledge::parse(&lines)?;
        let providers = dsh_providers(&root).ok_or("providers 不见了")?;
        if providers.get(&route).is_none() {
            let at = providers.end;
            let pad = providers.indent + 2;
            let block = crate::yamledge::indented(&dsh_route_block(spec)?, pad);
            crate::yamledge::splice(&mut lines, at..at, &block);
        }
    }
    dsh_route_fix(&mut lines, &route)?;

    // ③ 标量键(幂等:值一样就是换一行同样的内容,最终文本不变 → 不写盘)
    dsh_set_scalar(&mut lines, &route, "apiKeyEnv", &dsh_ref_for(&route))?;
    // `displayName` 三条 route 都写:目录 route 的默认名是 `opencode-go`,人认不出是哪个渠道。
    // 但**用户自己起的 route 名**不覆盖(他可能把这条 route 改了名,和模型的 name 同一条纪律)。
    let user_named = {
        let root = crate::yamledge::parse(&lines)?;
        dsh_providers(&root)
            .and_then(|p| p.get(&route))
            .and_then(|r| r.get("displayName"))
            .and_then(|n| n.scalar())
            .map(|v| !dsh_source_names().contains(&v))
            .unwrap_or(false)
    };
    if !user_named {
        dsh_set_scalar(&mut lines, &route, "displayName", &spec.source_name)?;
    }
    if !dsh_catalog_route(&route, &spec.id) {
        let api = spec
            .api_protocol
            .and_then(crate::models::ApiProtocol::dsh_api)
            .ok_or_else(|| format!("{} 的接口协议不能写入 DeepSeek Harness", spec.id))?;
        dsh_set_scalar(&mut lines, &route, "api", api)?;
        dsh_set_scalar(&mut lines, &route, "baseURL", &spec.base_url)?;
    }

    // ④ 模型条目
    dsh_put_model(&mut lines, &route, spec)?;

    let out = crate::yamledge::join(&lines);
    let action = if existed {
        PatchAction::Updated
    } else {
        PatchAction::Added
    };
    Ok((out, action))
}

/// 从 patch 里把这条模型摘掉:最后一个模型连 route 一起收走,route 空了连 providers 一起收,
/// 免得留一个空 models 列表(pi-ai 会把它判成不可服务)。
///
/// 会依次试**两条** route:这次该写的那条,以及旧的 `4sapi`(改名前的写法)——
/// 老版本写进去的条目,新版本也要能取消掉。
pub(super) fn dsh_patch_remove(text: &str, spec: &ModelSpec) -> Result<(String, usize), String> {
    let routes = dsh_routes_for_removal(&spec.source, &spec.id, spec.api_protocol);
    dsh_patch_remove_routes(text, spec, &routes)
}

/// Remove one model from selected routes, retaining any routes that still hold
/// other models. This lets protocol upgrades migrate old custom routes safely.
pub(super) fn dsh_patch_remove_routes(
    text: &str,
    spec: &ModelSpec,
    routes: &[String],
) -> Result<(String, usize), String> {
    let mut removed = 0;
    let mut lines = crate::yamledge::split(text);
    for route in routes {
        let span = {
            let root = crate::yamledge::parse(&lines)?;
            let Some(r) = dsh_providers(&root).and_then(|p| p.get(&route)) else {
                continue;
            };
            let Some(m) = r.get("models") else { continue };
            let Some(item) = m
                .items()
                .iter()
                .find(|x| x.get("id").and_then(|n| n.scalar()) == Some(spec.id.as_str()))
            else {
                continue;
            };
            if m.items().len() == 1 {
                (r.start, r.end)
            } else {
                (item.start, item.end)
            }
        };
        let (start, end) = span;
        crate::yamledge::splice(&mut lines, start..end, &[]);
        removed += 1;
    }
    if removed == 0 {
        return Ok((text.to_string(), 0));
    }
    // 收空壳:providers 空了连它一起收,config 空了也收(空 mapping 在 YAML 里是 null,
    // 会让 pi-ai 的 schema 直接报"配置不可用")
    dsh_prune_empty_patch_containers(&mut lines)?;
    Ok((crate::yamledge::join(&lines), removed))
}

pub(super) fn dsh_prune_empty_patch_containers(lines: &mut Vec<String>) -> Result<(), String> {
    for _ in 0..2 {
        let root = crate::yamledge::parse(&lines)?;
        let Some(entry) = dsh_entry(&root) else { break };
        let Some(config) = entry.get("config") else { break };
        if let Some(p) = config.get("providers") {
            if p.children.is_empty() {
                crate::yamledge::splice(lines, p.start..p.end, &[]);
                continue;
            }
        }
        if config.children.is_empty() {
            crate::yamledge::splice(lines, config.start..config.end, &[]);
        }
        break;
    }
    Ok(())
}

/// 取消写入时按这个顺序找:这次写的那条 route → 老版本写的那条 → 目录内有/无两种落点。
/// 只收**这个来源自己的**键,绝不顺着别人的 route 找 —— 同名模型在别家 route 上时,
/// 那不是我们这次要删的东西。
pub(super) fn dsh_routes_for_removal(
    source: &str,
    model: &str,
    protocol: Option<crate::models::ApiProtocol>,
) -> Vec<String> {
    let mut out = vec![dsh_route_for_protocol(source, model, protocol)];
    let mut cand = vec![dsh_route(source), dsh_custom_route(source)];
    if source == "opencode-go" {
        cand.push(dsh_custom_route_for(
            source,
            Some(crate::models::ApiProtocol::OpenAiResponses),
        ));
        cand.push(dsh_custom_route_for(
            source,
            Some(crate::models::ApiProtocol::AnthropicMessages),
        ));
    }
    if source == "4sapi" {
        cand.push("4sapi".to_string()); // 改名前的写法
    }
    for c in cand {
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// 凭据文件里写一个引用。返回(新文本, 动作)。
pub(super) fn dsh_cred_apply(
    text: &str,
    name: &str,
    key: &str,
) -> Result<(String, CredAction), String> {
    let mut lines = crate::yamledge::split(text);
    let root = crate::yamledge::parse(&lines)?;
    if root.kind != crate::yamledge::Kind::Mapping {
        return Err("凭据文件的根不是 mapping,认不出来(本次不写)".to_string());
    }
    match root.get("refs") {
        Some(refs) if refs.kind == crate::yamledge::Kind::Mapping => {
            let pad = refs.indent + 2;
            let line = format!("{}{name}: {}", " ".repeat(pad), crate::yamledge::scalar_text(key));
            match refs.get(name) {
                Some(c) if c.kind == crate::yamledge::Kind::Scalar => {
                    if c.scalar() == Some(key) {
                        Ok((text.to_string(), CredAction::Same))
                    } else {
                        crate::yamledge::splice(&mut lines, c.start..c.end, &[line]);
                        Ok((crate::yamledge::join(&lines), CredAction::Updated))
                    }
                }
                Some(c) => Err(format!(
                    "refs 下的 {name} 不是标量({}),本次不写",
                    c.what()
                )),
                None => {
                    crate::yamledge::splice(&mut lines, refs.end..refs.end, &[line]);
                    Ok((crate::yamledge::join(&lines), CredAction::Added))
                }
            }
        }
        // `refs:` 后面什么都没有:补成块
        Some(refs) => {
            let pad = refs.indent + 2;
            let block = vec![
                format!("{}refs:", " ".repeat(refs.indent)),
                format!("{}{name}: {}", " ".repeat(pad), crate::yamledge::scalar_text(key)),
            ];
            crate::yamledge::splice(&mut lines, refs.start..refs.end, &block);
            Ok((crate::yamledge::join(&lines), CredAction::Added))
        }
        None => {
            let mut block: Vec<String> = Vec::new();
            if root.get("version").is_none() {
                // 带版本的文档是这个存储的形式要求(裸 refs 的扁平形态只在启动时被迁移)
                block.push("version: 1".to_string());
            }
            block.push("refs:".to_string());
            block.push(format!("  {name}: {}", crate::yamledge::scalar_text(key)));
            let at = lines.len();
            crate::yamledge::splice(&mut lines, at..at, &block);
            Ok((crate::yamledge::join(&lines), CredAction::Added))
        }
    }
}

/// 凭据文件里去掉一个引用。`refs` 空了连它一起收走 —— 空 mapping 在存储里是类型错误。
pub(super) fn dsh_cred_remove(text: &str, name: &str) -> Result<String, String> {
    let mut lines = crate::yamledge::split(text);
    let root = crate::yamledge::parse(&lines)?;
    let Some(refs) = root.get("refs") else {
        return Ok(text.to_string());
    };
    if refs.kind != crate::yamledge::Kind::Mapping {
        return Ok(text.to_string());
    }
    let Some(c) = refs.get(name) else {
        return Ok(text.to_string());
    };
    if c.kind != crate::yamledge::Kind::Scalar {
        return Err(format!("refs 下的 {name} 不是标量,本次不写"));
    }
    crate::yamledge::splice(&mut lines, c.start..c.end, &[]);
    let root2 = crate::yamledge::parse(&lines)?;
    if let Some(r) = root2.get("refs") {
        if r.children.is_empty() {
            crate::yamledge::splice(&mut lines, r.start..r.end, &[]);
        }
    }
    Ok(crate::yamledge::join(&lines))
}

/// 读回校验:route 在、模型在、引用对、基地址对(目录路由不看 baseURL)、密钥落到了凭据文件。
pub(super) fn dsh_verify(patch: &Path, cred: &Path, spec: &ModelSpec) -> bool {
    let text = dsh_read_text(patch);
    let Ok(root) = crate::yamledge::parse(&crate::yamledge::split(&text)) else {
        return false;
    };
    let route = dsh_route_for_protocol(&spec.source, &spec.id, spec.api_protocol);
    let Some(r) = dsh_providers(&root).and_then(|p| p.get(&route)) else {
        return false;
    };
    let model_ok = r
        .get("models")
        .map(|m| {
            m.items()
                .iter()
                .any(|x| x.get("id").and_then(|n| n.scalar()) == Some(spec.id.as_str()))
        })
        .unwrap_or(false);
    let name = dsh_ref_for(&route);
    let key_ok = r.get("apiKeyEnv").and_then(|n| n.scalar()) == Some(name.as_str());
    let url_ok = dsh_catalog_route(&route, &spec.id)
        || r.get("baseURL").and_then(|n| n.scalar()) == Some(spec.base_url.as_str());
    let cred_ok = {
        let t = dsh_read_text(cred);
        crate::yamledge::parse(&crate::yamledge::split(&t))
            .ok()
            .and_then(|root| {
                root.get("refs")
                    .and_then(|r| r.get(&name))
                    .and_then(|n| n.scalar())
                    .map(|v| v == spec.key)
            })
            .unwrap_or(false)
    };
    model_ok && key_ok && url_ok && cred_ok
}

/// 写入 DSH 的一次「方案」:两份文件的现状与目标形态。纯函数(除读两个文件外不碰盘),
/// 预告与真正写盘共用它 —— 两边口径就不可能分叉。
pub(super) struct DshPlan {
    pub(super) patch: PathBuf,
    pub(super) cred: PathBuf,
    pub(super) next_patch: String,
    pub(super) next_cred: String,
    pub(super) same: bool,
    pub(super) patch_action: PatchAction,
    pub(super) cred_action: CredAction,
}

pub(super) fn dsh_plan(site: &Site, spec: &ModelSpec) -> Result<DshPlan, String> {
    let (patch, cred) = dsh_paths(site)?;
    let patch_text = dsh_read_text(&patch);
    let cred_text = dsh_read_text(&cred);
    let (next_patch, patch_action) = dsh_patch_apply(&patch_text, spec)?;
    let name = dsh_ref_for(&dsh_route_for_protocol(
        &spec.source,
        &spec.id,
        spec.api_protocol,
    ));
    // 兜底:引用名不合法会让**整份**凭据文件被拒(连带 pi-ai 全部 route 失效),
    // 所以这里宁可拒写,也不写一个注定把 DSH 弄坏的名字。
    if !dsh_ref_legal(&name) {
        return Err(format!(
            "凭据引用名「{name}」不合法(DSH 要求字母或下划线开头)—— 这条 route 需要换一个以字母开头的键,本次不写"
        ));
    }
    let (next_cred, cred_action) = dsh_cred_apply(&cred_text, &name, &spec.key)?;
    let same = next_patch == patch_text && next_cred == cred_text;
    Ok(DshPlan {
        patch,
        cred,
        next_patch,
        next_cred,
        same,
        patch_action,
        cred_action,
    })
}

/// 写入 DSH:两份文件一起改,一次备份、一起校验。
pub(super) fn write_dsh_site(site: &Site, spec: &ModelSpec) -> Result<SiteOutcome, String> {
    let plan = dsh_plan(site, spec)?;
    let mut outcome = SiteOutcome::default();
    if plan.same {
        // 内容和盘上一致:不写盘,连备份都不必做
        outcome.same = true;
        // **"没变化所以没写"也要记**:用户说"我什么都没干它自己坏了"时,
        // 日志里只有这一行才能说清"这一轮我们其实什么都没动"。
        crate::diaglog::write_keyed(
            "inject",
            &format!("{}:same", site.label),
            "内容与盘上一致 —— 没有改动,没写盘也没建备份",
        );
        return Ok(outcome);
    }
    match plan.patch_action {
        PatchAction::Added => outcome.added = 1,
        PatchAction::Updated => outcome.updated = 1,
    }
    let name = dsh_ref_for(&dsh_route_for_protocol(
        &spec.source,
        &spec.id,
        spec.api_protocol,
    ));
    match plan.cred_action {
        CredAction::Added => outcome
            .notes
            .push(format!("凭据文件里新增引用 {name}(只写密钥本身,不碰别的引用)")),
        CredAction::Updated => outcome
            .notes
            .push(format!("引用 {name} 原有的值与这次不同,已按这次的内容覆盖")),
        CredAction::Same => {}
    }

    // L1:这次**具体改了什么**。只说"写了"不够 —— 要知道是新增还是覆盖、
    // 覆盖的是哪条(模型 id)。键名走白名单,值走脱敏。
    crate::diaglog::write_keyed(
        "inject",
        &format!("{}:what", site.label),
        &format!(
            "改动内容:新增 {} 条 / 更新 {} 条 · patch 动作 {:?} · 凭据动作 {:?}",
            outcome.added, outcome.updated, plan.patch_action, plan.cred_action
        ),
    );

    let stamp = stamp_now();
    let (patch, cred) = (plan.patch.clone(), plan.cred.clone());
    let (pw, cw) = (patch.clone(), cred.clone());
    let (pc, cc) = (patch.clone(), cred.clone());
    let rels = vec![dsh_rel(&site.root, &patch), dsh_rel(&site.root, &cred)];
    let (next_patch, next_cred) = (plan.next_patch, plan.next_cred);
    outcome.backup = commit_layered(
        site,
        &[patch.clone(), cred.clone()],
        Some(&rels),
        &stamp,
        move || {
            dsh_write_text(&pw, &next_patch)?;
            dsh_write_text(&cw, &next_cred)
        },
        move || dsh_verify(&pc, &cc, spec),
    )?;
    outcome
        .notes
        .push(format!("备份:{}", pretty(&outcome.backup)));
    outcome.notes.push(
        "DSH 不用退出也能写:它的 profile 按请求解析,配置改动在下一个请求生效。".to_string(),
    );
    Ok(outcome)
}

/// 取消写入 DSH:摘掉这一条模型;最后一个模型连 route 一起收;没人再用这个引用了才删密钥。
pub(super) fn remove_dsh_site(site: &Site, spec: &ModelSpec) -> Result<SiteOutcome, String> {
    let (patch, cred) = dsh_paths(site)?;
    let patch_text = dsh_read_text(&patch);
    let cred_text = dsh_read_text(&cred);
    let (mid_patch, removed) = dsh_patch_remove(&patch_text, spec)?;

    // 还有别的 route 在用这些引用名吗?没有才删密钥(别人在用的引用一个都不动)。
    // 名字可能不止一个:旧写法 `4sapi` 与新写法 `foursapi` 的引用名不同,一起收拾。
    let names: Vec<String> = dsh_routes_for_removal(&spec.source, &spec.id, spec.api_protocol)
        .iter()
        .map(|r| dsh_ref_for(r))
        .collect();
    let still_used = |name: &str| -> Result<bool, String> {
        let root = crate::yamledge::parse(&crate::yamledge::split(&mid_patch))?;
        Ok(dsh_providers(&root)
            .map(|p| {
                p.children.iter().any(|r| {
                    r.get("apiKeyEnv").and_then(|n| n.scalar()) == Some(name)
                })
            })
            .unwrap_or(false))
    };
    let mut dropped: Vec<String> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    let mut next_cred = cred_text.clone();
    if removed > 0 {
        for name in &names {
            if still_used(name)? {
                kept.push(name.clone());
            } else if crate::yamledge::parse(&crate::yamledge::split(&next_cred))
                .ok()
                .and_then(|r| {
                    r.get("refs")
                        .and_then(|refs| refs.get(name))
                        .map(|_| ())
                })
                .is_some()
            {
                next_cred = dsh_cred_remove(&next_cred, name)?;
                dropped.push(name.clone());
            }
        }
    }
    let mut outcome = SiteOutcome::default();
    outcome.removed = removed;
    if mid_patch == patch_text && next_cred == cred_text {
        return Ok(outcome);
    }
    if removed > 0 {
        if !kept.is_empty() {
            outcome.notes.push(format!(
                "引用 {} 还有别的 route 在用,密钥留着不动",
                kept.join(" / ")
            ));
        }
        if !dropped.is_empty() {
            outcome.notes.push(format!(
                "引用 {} 已经没有 route 再用,密钥一并从凭据文件里撤掉",
                dropped.join(" / ")
            ));
        }
    }
    // 默认模型还指着这条吗?指着就说明白,让用户去 DSH 里另选一个(不替他改)
    if removed > 0 && dsh_default_points_at(&patch_text, &spec.source, &spec.id) {
        outcome.notes.push(format!(
            "注意:DSH 的默认模型(agent-default-model)正指向 {} / {} —— 挂件不动它,\
             请回 DSH 的设置里另选一个,否则新会话会报模型不可用。",
            spec.source, spec.id
        ));
    }

    let stamp = stamp_now();
    let (pw, cw) = (patch.clone(), cred.clone());
    let (pc, cc) = (patch.clone(), cred.clone());
    let rels = vec![dsh_rel(&site.root, &patch), dsh_rel(&site.root, &cred)];
    let check: Vec<(String, bool)> = names
        .iter()
        .map(|n| (n.clone(), dropped.contains(n)))
        .collect();
    outcome.backup = commit_layered(
        site,
        &[patch.clone(), cred.clone()],
        Some(&rels),
        &stamp,
        move || {
            dsh_write_text(&pw, &mid_patch)?;
            dsh_write_text(&cw, &next_cred)
        },
        move || {
            // 读回校验:这条模型在盘上读不到了;密钥按**这次实际写的**口径核对 ——
            // 撤了就必须没有,留着就必须原样还在(不然读回校验会把成功的删除当失败回滚)
            let t = dsh_read_text(&pc);
            let gone = dsh_model_absent(&t, &spec.source, &spec.id);
            let c = dsh_read_text(&cc);
            let cred_ok = crate::yamledge::parse(&crate::yamledge::split(&c))
                .map(|root| {
                    // 只核对**这次真撤掉的**引用:撤了就必须没有。
                    // 没撤的那些(别人的 route 还在用、或者本来就没有这条引用)一律不碰 ——
                    // 以前按"必须还在"核对,会把「本来就不存在的引用」误判成校验失败。
                    check.iter().all(|(name, was_dropped)| {
                        if !*was_dropped {
                            return true;
                        }
                        root.get("refs")
                            .and_then(|r| r.get(name))
                            .and_then(|n| n.scalar())
                            .is_none()
                    })
                })
                .unwrap_or(true);
            gone && cred_ok
        },
    )?;
    Ok(outcome)
}

/// 盘上这条模型已经不在我们写的那条 route 里了(或连 route 都没了)。
/// 新旧两条 route 都要看:老版本写进 `4sapi` 的条目,新版本也得认得出「已经删掉了」。
pub(super) fn dsh_model_absent(text: &str, source: &str, model: &str) -> bool {
    let Ok(root) = crate::yamledge::parse(&crate::yamledge::split(text)) else {
        return false;
    };
    dsh_routes_for_removal(source, model, crate::models::api_protocol(source, model))
        .iter()
        .all(|route| {
        dsh_providers(&root)
            .and_then(|p| p.get(route))
            .and_then(|r| r.get("models"))
            .map(|m| {
                !m.items()
                    .iter()
                    .any(|x| x.get("id").and_then(|n| n.scalar()) == Some(model))
            })
            .unwrap_or(true)
        })
}

/// DSH 的默认模型是不是指着这条(只读,不改)。
/// 旧版 route 键(`4sapi`)也算:改名之后,默认模型那条里可能还是老键。
pub(super) fn dsh_default_points_at(patch_text: &str, source: &str, model: &str) -> bool {
    let Ok(root) = crate::yamledge::parse(&crate::yamledge::split(patch_text)) else {
        return false;
    };
    let routes = dsh_routes_for_removal(source, model, crate::models::api_protocol(source, model));
    root.children
        .iter()
        .find(|e| e.get("id").and_then(|n| n.scalar()) == Some(DSH_DEFAULT_MODEL_ID))
        .and_then(|e| e.get("config"))
        .map(|c| {
            let provider = c.get("provider").and_then(|n| n.scalar()).unwrap_or("");
            routes.iter().any(|r| r == provider)
                && c.get("model").and_then(|n| n.scalar()) == Some(model)
        })
        .unwrap_or(false)
}

/// 从 patch 文本里读「我们写进去的模型(带来源)」。
pub(super) fn dsh_read_configured(text: &str) -> Vec<ConfiguredModel> {
    let mut out: Vec<ConfiguredModel> = Vec::new();
    let Ok(root) = crate::yamledge::parse(&crate::yamledge::split(text)) else {
        return out;
    };
    let Some(providers) = dsh_providers(&root) else {
        return out;
    };
    for r in &providers.children {
        let Some(route) = r.key.as_deref() else { continue };
        let Some(source) = dsh_route_source(route) else {
            continue;
        };
        let Some(m) = r.get("models") else { continue };
        for item in m.items() {
            let Some(id) = item.get("id").and_then(|n| n.scalar()) else {
                continue;
            };
            if !out.iter().any(|x| x.id == id && x.source == source) {
                out.push(ConfiguredModel {
                    id: id.to_string(),
                    source: source.clone(),
                });
            }
        }
    }
    out
}

/// DSH 当前的默认模型(`agent-default-model` 里那条),读得到才给。
pub(super) fn dsh_read_selected(text: &str) -> Option<String> {
    let root = crate::yamledge::parse(&crate::yamledge::split(text)).ok()?;
    let cfg = root
        .children
        .iter()
        .find(|e| e.get("id").and_then(|n| n.scalar()) == Some(DSH_DEFAULT_MODEL_ID))?
        .get("config")?;
    let provider = cfg.get("provider").and_then(|n| n.scalar())?;
    if dsh_route_source(provider).is_none() {
        return None;
    }
    cfg.get("model").and_then(|n| n.scalar()).map(String::from)
}

/// patch 动作(回执用)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PatchAction {
    Added,
    Updated,
}

/// 凭据动作(回执用)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CredAction {
    Added,
    Updated,
    Same,
}

/// DSH:这条模型在不在我们写的那条 route 里,以及移除后会剩下什么。
pub(super) fn dsh_removal(site: &Site, spec: &ModelSpec) -> (bool, Option<Removal>) {
    let Ok((patch, _cred)) = dsh_paths(site) else {
        return (false, None);
    };
    let text = dsh_read_text(&patch);
    let Ok(root) = crate::yamledge::parse(&crate::yamledge::split(&text)) else {
        return (false, None);
    };
    let Some((route, m)) =
        dsh_routes_for_removal(&spec.source, &spec.id, spec.api_protocol)
        .into_iter()
        .find_map(|route| {
            let m = dsh_providers(&root)
                .and_then(|p| p.get(&route))
                .and_then(|r| r.get("models"))?;
            let hit = m
                .items()
                .iter()
                .any(|x| x.get("id").and_then(|n| n.scalar()) == Some(spec.id.as_str()));
            hit.then_some((route, m))
        })
    else {
        return (false, None);
    };
    let left = m.items().len().saturating_sub(1);
    let what = if left == 0 {
        format!(
            "route「{route}」下的「{}」—— 这是它最后一个模型,连 route 一起移除\
             (留一个空 models 列表会让 pi-ai 判它不可服务)",
            spec.id
        )
    } else {
        format!(
            "route「{route}」下的「{}」(route 留下,还剩 {left} 个模型)",
            spec.id
        )
    };
    (
        true,
        Some(Removal {
            what,
            // DSH 的「选中项」是 agent-default-model 那条,挂件只读不写 ——
            // 移除时不替用户改,只在回执里点明(见 remove_dsh_site)。
            clears_selection: false,
            count: 1,
        }),
    )
}

// 取消写入(移除条目)的公共描述 + 各客户端实现
