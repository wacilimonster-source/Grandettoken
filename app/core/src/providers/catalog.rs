use super::*;

pub const PROVIDERS: &[ProviderDef] = &[
    ProviderDef {
        id: "4sapi",
        name: "4SAPI",
        short: "4S",
        color: "#e8622c",
        url: "https://4sapi.com/api/usage/token",
        fallback_urls: &[
            "https://4sapi.cn/api/usage/token",
            "https://4sapi.ai/api/usage/token",
            "https://4sapi.org/api/usage/token",
        ],
        extra_headers: &[],
        auth: AuthKind::Keyring,
        auth_prefix: "Bearer ",
        method: Method::Get,
        unstable: false,
        default_kind: Kind::Amount,
        peak_pricing: false,
        extract: extract_4sapi,
    },
    ProviderDef {
        id: "opencode-go",
        name: "OpenCode Go",
        short: "OC",
        color: "#f5a623",
        url: "https://opencode.ai/zen/go/v1/usage",
        fallback_urls: &[],
        extra_headers: &[], // 只认 Bearer,x-api-key 无效
        auth: AuthKind::Keyring,
        auth_prefix: "Bearer ",
        method: Method::Get,
        unstable: true,
        default_kind: Kind::Percent,
        peak_pricing: false,
        extract: extract_opencode_go,
    },
    ProviderDef {
        id: "deepseek",
        name: "DeepSeek",
        short: "DS",
        color: "#4d6bfe",
        url: "https://api.deepseek.com/user/balance",
        fallback_urls: &[],
        extra_headers: &[],
        auth: AuthKind::Keyring,
        auth_prefix: "Bearer ",
        method: Method::Get,
        unstable: false,
        default_kind: Kind::Amount,
        peak_pricing: true,
        extract: extract_deepseek,
    },
    ProviderDef {
        id: "hapi",
        name: "Hapi",
        short: "HA",
        color: "#22a06b",
        url: "https://ai.yuchuantest.com/v1/usage",
        fallback_urls: &[],
        extra_headers: &[("User-Agent", "cc-switch/1.0")],
        auth: AuthKind::Keyring,
        auth_prefix: "Bearer ",
        method: Method::Get,
        unstable: false,
        default_kind: Kind::Amount,
        peak_pricing: false,
        extract: extract_hapi,
    },
    ProviderDef {
        id: "trae",
        name: "Trae",
        short: "TR",
        color: "#6d5ef8",
        // 两个端点返回同一份数据,互为备份(某个被摘掉时不至于整个渠道瞎掉)
        url: "https://api.trae.cn/trae/api/v2/pay/user_current_entitlement_list",
        fallback_urls: &["https://api.trae.cn/trae/api/v2/pay/ide_user_ent_usage"],
        // Origin 可以不发,但一旦发错就是 403 —— 带着正确值最稳
        extra_headers: &[
            ("Origin", "https://www.trae.cn"),
            ("Referer", "https://www.trae.cn/"),
        ],
        auth: AuthKind::App(App::Trae),
        auth_prefix: "Cloud-IDE-JWT ",
        method: Method::PostJson(trae_body),
        unstable: true,
        default_kind: Kind::Points,
        peak_pricing: false,
        extract: extract_trae,
    },
    ProviderDef {
        id: "workbuddy",
        name: "WorkBuddy",
        short: "WB",
        color: "#19b8c4",
        // 2026-10-19:**必须带 `/v2`**,主域用 codebuddy.cn(桌面端凭据的 domain 也是它)。
        // 老的 `/billing/meter/get-user-resource`(无 /v2)实测已不可用。
        // 取证自 dsh-connect-workbuddy v4.8.1 的 billingBase() + fetchCredits()。
        url: "https://www.codebuddy.cn/v2/billing/meter/get-user-resource",
        fallback_urls: &["https://www.workbuddy.cn/v2/billing/meter/get-user-resource"],
        // X-Client-Platform 是硬要求:缺了直接 403 code 10085(实测)
        extra_headers: &[
            ("X-Client-Platform", "web"),
            ("Origin", "https://www.codebuddy.cn"),
            ("Referer", "https://www.codebuddy.cn/profile/plans-usage"),
        ],
        auth: AuthKind::WebSession,
        auth_prefix: "Bearer ",
        method: Method::PostJson(workbuddy_body),
        unstable: true,
        default_kind: Kind::Points,
        peak_pricing: false,
        extract: extract_workbuddy,
    },
    // WorkBuddy AI(国际版):同一个产品的**另一套账号**(官方不支持跨区迁移),
    // 所以与 `workbuddy` 并列、不合并 —— 与 Qoder / Qoder 国内版同构。
    //
    // 三条与国内版不同的地方,改之前先看这里:
    //   ① 域名 workbuddy.ai / codebuddy.ai。用国区域名打国际版 token 一定 401
    //      (workbuddy.rs::bases_for 的注释里有真机记录)。
    //   ② 凭据只有桌面端一条路 —— 国际版被 weblogin 的裁决 6A 排除在网页登录之外,
    //      所以这里虽然沿用 `WebSession`(借它的凭据链路),界面却按 app 型渲染
    //      (见 fetch.rs 里 `wb_desktop_only` 的分支)。
    //   ③ **没有签到**。`daily-checkin` 在国际版回 10001「活动未开启」,与国区
    //      「今天已签到」同码异义,所以它不进 `checkin::CHECKINS`。
    //      "今日是否已获取积分"走 `grants` 的余额正增量,不是签到那套。
    ProviderDef {
        id: "workbuddy-ai",
        name: "WorkBuddy AI",
        short: "WBA",
        color: "#4f7cff",
        url: "https://www.workbuddy.ai/v2/billing/meter/get-user-resource",
        fallback_urls: &["https://www.codebuddy.ai/v2/billing/meter/get-user-resource"],
        extra_headers: &[
            ("X-Client-Platform", "web"),
            ("Origin", "https://www.workbuddy.ai"),
            ("Referer", "https://www.workbuddy.ai/profile/plans-usage"),
        ],
        auth: AuthKind::WebSession,
        auth_prefix: "Bearer ",
        method: Method::PostJson(workbuddy_body),
        unstable: true,
        default_kind: Kind::Points,
        peak_pricing: false,
        extract: extract_workbuddy,
    },
    ProviderDef {
        id: "codex",
        name: "Codex",
        short: "CX",
        color: "#10a37f",
        // ChatGPT 后端内部接口,Codex CLI 自身每 60s 轮询同一个地址。
        // 实测只带 Authorization 即可(account-id 头可省);无备用入口。
        url: "https://chatgpt.com/backend-api/wham/usage",
        fallback_urls: &[],
        extra_headers: &[],
        auth: AuthKind::App(App::Codex),
        auth_prefix: "Bearer ",
        method: Method::Get,
        unstable: true,
        default_kind: Kind::Percent,
        peak_pricing: false,
        extract: extract_codex,
    },
    // ───────────── Qoder(国际版 / 国内版)─────────────
    //
    // 两版是**两套账号**(官方明确不支持跨区迁移),所以是两个独立渠道 —— 余额无法合并。
    // 2026-10-20 真机取证(本机桌面端 0.4.8,region=global):
    //   ① `GET /api/v2/quota/usage` → 200,额度在 userQuota + addOnQuota 两块
    //   ② 头只需 Authorization + Cosy-ClientType + User-Agent(额度接口**不走** Cosy 签名)
    //   ③ 凭据是本机桌面端 auth.v1.dat(Electron safeStorage + DPAPI,见 core::qoder)
    ProviderDef {
        id: "qoder",
        name: "Qoder",
        short: "QD",
        color: "#00b3a4",
        url: "https://openapi.qoder.sh/api/v2/quota/usage",
        fallback_urls: &[],
        extra_headers: &[
            ("Cosy-ClientType", "10"),
            // 网关按 UA 分流;官方客户端自己发的就是 "Qoder"
            ("User-Agent", "Qoder"),
        ],
        auth: AuthKind::App(App::Qoder),
        auth_prefix: "Bearer ",
        method: Method::Get,
        unstable: true,
        default_kind: Kind::Points,
        peak_pricing: false,
        extract: extract_qoder,
    },
    ProviderDef {
        id: "qoder-cn",
        name: "Qoder 国内版",
        short: "QDC",
        color: "#e8622c",
        url: "https://openapi.qoder.com.cn/api/v2/quota/usage",
        fallback_urls: &[],
        extra_headers: &[
            ("Cosy-ClientType", "10"),
            ("User-Agent", "Qoder"),
        ],
        auth: AuthKind::App(App::QoderCn),
        auth_prefix: "Bearer ",
        method: Method::Get,
        unstable: true,
        default_kind: Kind::Points,
        peak_pricing: false,
        extract: extract_qoder,
    },
    // MiniMax(积分型):账户端点要官方客户端签名,URL 查询串与 x-signature/yy
    // 在 fetch_once_inner 的 minimax 分支现算(见 core::minimax);逐笔到期来自
    // 另一个账本端点,由 fetch_channel_scoped 在主响应成功后追加(口径同 codex 明细)。
    ProviderDef {
        id: "minimax",
        name: "MiniMax",
        short: "MM",
        color: "#f95348",
        url: "https://agent.minimax.cn/matrix/api/v1/commerce/get_membership_info",
        // 域名跟凭据区域走:先打 cn,401 后逐个回退(io / com)。签名只绑 path+query,
        // 换 host 不用重算;第一轮成功后 last_good 备忘会钉住对的那个。
        fallback_urls: &[
            "https://agent.minimax.io/matrix/api/v1/commerce/get_membership_info",
            "https://agent.minimaxi.com/matrix/api/v1/commerce/get_membership_info",
        ],
        extra_headers: &[
            // 官方客户端自己的 UA(签名请求口径,CreditDaddy 实证)
            ("User-Agent", "MiniMaxCode"),
        ],
        auth: AuthKind::App(App::MiniMax),
        auth_prefix: "Bearer ",
        method: Method::PostJson(|| "{}".to_string()),
        unstable: false,
        default_kind: Kind::Points,
        peak_pricing: false,
        extract: crate::minimax::extract_membership,
    },
    // 豆包工作(积分型):doubao.com 网页会话,读官方客户端启动时自调的订阅额度
    // 汇总接口,旧任务面板响应仍兼容解析;查询串(平台参数 + 派生设备号)在
    // fetch_once_inner 的 doubao 分支现拼;UA 运行时替换(core::doubao_client),
    // 登录窗同用一份 —— 网页 Cookie 配另一套身份即 710022004。前端按 unit 渲染。
    ProviderDef {
        id: "doubao",
        name: "豆包工作",
        short: "豆包",
        color: "#cae4ff",
        url: "https://www.doubao.com/alice/commerce/sale/subscription/quota/summary/",
        fallback_urls: &[],
        extra_headers: &[
            ("Referer", "https://www.doubao.com/pc/agent-explore"),
            ("Origin", "https://www.doubao.com"),
            ("User-Agent", crate::doubao::APP_UA),
        ],
        auth: AuthKind::WebSession,
        auth_prefix: "",
        method: Method::PostJson(|| crate::doubao::QUOTA_BODY.to_string()),
        unstable: true,
        default_kind: Kind::Points,
        peak_pricing: false,
        extract: crate::doubao::extract_doubao,
    },
    ProviderDef {
        id: "antigravity", name: "Antigravity", short: "AG", color: "#3286fc",
        url: "local://agy-cli", fallback_urls: &[], extra_headers: &[],
        auth: AuthKind::App(App::Antigravity), auth_prefix: "", method: Method::Get,
        unstable: false, default_kind: Kind::Percent, peak_pricing: false,
        extract: crate::antigravity::extract_antigravity,
    },
];

pub fn find(id: &str) -> Option<&'static ProviderDef> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// 这个渠道要不要记"积分到账"事件(见 `crate::grants`)。
///
/// 现在只有国际版:它没有签到接口,「每天对话一次 +30 积分」只在余额上留痕。
/// 国内版有签到(`checkin_log`),不需要再猜一遍;以后哪个积分渠道也变成
/// "只在余额上留痕"的,把 id 加进这里就行 —— 判定逻辑在 `grants` 里,不用改。
pub fn tracks_grants(id: &str) -> bool {
    id == "workbuddy-ai"
}

/// 没检测到凭据时,要不要把这个渠道**自动藏起来**。
///
/// 只给"没装就等于不存在"的渠道用。现在只有 WorkBuddy AI(国际版):
/// 它没有网页登录路径,没装国际版桌面端的人永远看不到数据 —— 列一行"未检测到登录"
/// 只是噪音。装上并登录后下一轮自动出现,不需要用户去设置里翻。
///
/// **不要**把 Trae / Codex / Qoder 加进来:那几条的凭据会瞬时读不到
/// (文件被客户端占着、凭据管理器 RPC 卡一下),藏起来会让人以为配置丢了。
/// 判据不是"有没有凭据",而是"没凭据是不是稳定等于没装"。
pub fn auto_hide_when_no_credential(id: &str) -> bool {
    id == "workbuddy-ai"
}

/// 订阅型套餐的**固定期费**(美元)。只有 OpenCode Go 一家:它是 $10/期的包月,
/// 接口只回百分比用量、不回金额,所以"今日/本周/本月消耗"要靠这个数按天摊出来
/// (2026-10-14 产品口径:按天算、周期按实际重置间隔自动学、汇率每月自动取)。
/// 返回 None = 该渠道是按量计费的余额型,消耗由快照差值算,不该再加一笔。
pub fn plan_usd(id: &str) -> Option<f64> {
    match id {
        "opencode-go" => Some(10.0),
        _ => None,
    }
}

/// 4SAPI 的入口域名(按渠道定义的主地址 + 备用地址推导,避免域名在两处各写一遍)。
/// 模型查询与「写进客户端」的地址都从这里取 —— 域名只有一个来源。
pub fn four_s_api_hosts() -> Vec<String> {
    let def = find("4sapi").expect("4sapi 渠道一定在表里");
    std::iter::once(def.url)
        .chain(def.fallback_urls.iter().copied())
        .filter_map(url_host)
        .collect()
}

/// "https://4sapi.com/api/usage/token" → "4sapi.com"
pub fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest
        .split('/')
        .next()?
        .rsplit('@')
        .next()?
        .split(':')
        .next()?
        .trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}
