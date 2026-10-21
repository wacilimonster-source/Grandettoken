//! 错误码表:**界面文案与日志共用同一套码**。
//!
//! ## 为什么要有它
//!
//! 2026-10-20 那次排查暴露了一个很具体的问题:界面上写的是「凭据失效 —— 打开一次客户端」,
//! 日志里写的是 `no-cred`。**用户报的那句话在日志里搜不到**,support 只能来回问。
//!
//! 所以定一条规矩:**界面上显示的每一句"出错原因",背后都有一个码;日志里必须有那个码。**
//!
//! 好处有两个:
//!
//! 1. 用户报一个码(比如 `E_HTTP_401`),support 直接 `grep` 就能定位;
//! 2. 界面能按码给出**不同的行动建议** —— "去登录一次" / "去密钥箱看看" /
//!    "打开客户端" / "等一会儿",而不是所有错误都说同一句话。
//!
//! ## 怎么用
//!
//! ```ignore
//! // 日志侧:码跟着事件走
//! diaglog::wire("fetch", Some(def.id), "http", Some(errcodes::HTTP_401), &[("status", "401")]);
//!
//! // 界面侧:码决定给用户什么建议
//! let (text, action) = errcodes::advice(errcodes::HTTP_401, def.name);
//! ```

/// 没有可用凭据:这个渠道压根没配过密钥。
pub const NO_CRED: &str = "E_NO_CRED";
/// 凭据读不到:凭据管理器/文件读不出来 —— **和"没配"是两回事**。
pub const CRED_UNREADABLE: &str = "E_CRED_UNREADABLE";
/// 引用的密钥箱条目被删了(断链)。去密钥箱看,不是去渠道卡填。
pub const CRED_DANGLING: &str = "E_CRED_DANGLING";
/// 凭据被平台明确拒绝(401/403)。
pub const CRED_EXPIRED: &str = "E_CRED_EXPIRED";
/// 401。
pub const HTTP_401: &str = "E_HTTP_401";
/// 403。
pub const HTTP_403: &str = "E_HTTP_403";
/// 404:路径不对(接口改地址了)。
pub const HTTP_404: &str = "E_HTTP_404";
/// 429:被限流。
pub const HTTP_429: &str = "E_HTTP_429";
/// 407:代理要求认证。和 401 一样是"这份凭据不行",但走的是代理那条路。
pub const HTTP_407: &str = "E_HTTP_407";
/// 5xx:服务端错误。
pub const HTTP_5XX: &str = "E_HTTP_5XX";
/// 其它 4xx。
pub const HTTP_4XX: &str = "E_HTTP_4xx";
/// 请求超时。
pub const TIMEOUT: &str = "E_TIMEOUT";
/// 连不上(网络/代理)。
pub const CONNECT: &str = "E_CONNECT";
/// HTTPS 证书校验失败。
pub const TLS: &str = "E_TLS";
/// 响应不是 JSON(常见于网关回了一页 HTML)。
pub const BODY_NOT_JSON: &str = "E_BODY_NOT_JSON";
/// 是 JSON,但字段对不上 —— **接口结构变了**。
pub const BODY_SHAPE: &str = "E_BODY_SHAPE";
/// HTTP 200 但业务失败。
pub const BIZ_FAIL: &str = "E_BIZ_FAIL";
/// 解析器出了意外。
pub const PARSE: &str = "E_PARSE";
/// 数据库出错。
pub const DB: &str = "E_DB";

/// HTTP 状态码 → 错误码。**唯一口径**:取数、探活、体检都走它,
/// 免得同一个 401 在两处被判成不同类型(2026-10-20 真机踩过)。
///
/// ⚠ **只对非 2xx 有意义**。2xx 但"内容不对"是另一回事
/// (不是 JSON → `BODY_NOT_JSON`;JSON 但字段对不上 → `BODY_SHAPE`;
/// 业务自己报失败 → `BIZ_FAIL`)。别把 2xx 塞进来。
pub fn for_status(code: u16) -> &'static str {
    match code {
        401 => HTTP_401,
        403 => HTTP_403,
        404 => HTTP_404,
        407 => HTTP_407,
        429 => HTTP_429,
        500..=599 => HTTP_5XX,
        400..=499 => HTTP_4XX,
        // 2xx 走到这里说明调用方用错了 —— 宁可给一个明确的码,也不要
        // 悄悄说成"业务失败"(那会把"接口结构变了"误报成"平台说不行")。
        200..=299 => BODY_SHAPE,
        _ => BIZ_FAIL,
    }
}

/// 用户该做什么。界面按这个给建议,别所有错误都说同一句话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 去渠道卡上填一把 / 重新登录一次
    Login,
    /// 去密钥箱看看(引用的条目被删了)
    Vault,
    /// 打开对应的桌面客户端一次
    OpenClient,
    /// 等一会儿再试(限流 / 服务端错)
    Wait,
    /// 没什么要做的,重试就好
    Retry,
    /// 这是我们自己的 bug,不用用户动手
    Bug,
}

/// 码 → 一句话说明(不带渠道名)。界面按这个渲染。
pub fn short(code: &str) -> &'static str {
    match code {
        NO_CRED => "还没配置密钥",
        CRED_UNREADABLE => "本机读不到这份凭据",
        CRED_DANGLING => "引用的密钥箱条目已被删除",
        CRED_EXPIRED => "凭据已失效",
        HTTP_401 => "凭据被拒绝(401)",
        HTTP_403 => "凭据被拒绝(403)",
        HTTP_404 => "接口地址变了(404)",
        HTTP_407 => "代理要求认证(407)",
        HTTP_429 => "被限流了(429)",
        HTTP_5XX => "对方服务器出错",
        HTTP_4XX => "请求被拒绝",
        TIMEOUT => "请求超时",
        CONNECT => "连不上服务器",
        TLS => "HTTPS 证书校验失败",
        BODY_NOT_JSON => "返回的不是 JSON(网关可能挡了)",
        BODY_SHAPE => "接口结构变了,读不懂返回内容",
        BIZ_FAIL => "接口返回了业务失败",
        PARSE => "解析返回内容时出错",
        DB => "本地数据库出错",
        _ => "出错了",
    }
}

/// 码 → 用户该做什么。界面按这个给建议,别所有错误都说同一句话。
pub fn action(code: &str) -> Action {
    match code {
        NO_CRED | CRED_EXPIRED | HTTP_401 | HTTP_403 | HTTP_407 => Action::Login,
        CRED_DANGLING => Action::Vault,
        CRED_UNREADABLE => Action::OpenClient,
        HTTP_429 | HTTP_5XX | HTTP_404 | HTTP_4XX | BODY_NOT_JSON | BODY_SHAPE | BIZ_FAIL => {
            Action::Wait
        }
        TIMEOUT | CONNECT | TLS => Action::Retry,
        PARSE | DB => Action::Bug,
        _ => Action::Retry,
    }
}

/// 码 → (一句话说明, 该让用户做什么)。
///
/// `name` 是渠道名,用来把话说到具体渠道上。
pub fn advice(code: &str, name: &str) -> (String, Action) {
    (format!("{name}:{}", short(code)), action(code))
}

/// 码 → 界面显示的完整一句话。
pub fn text(code: &str, name: &str) -> String {
    format!("{name}:{}", short(code))
}
