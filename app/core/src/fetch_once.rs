use super::*;

/// 一次尝试的失败类型 —— 它决定"要不要换个域名再试"。
/// 分错会让排障变难:密钥错(4xx)换域名只是白等,网络故障不换就白白瞎掉一个渠道。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    /// 连不上 / DNS / 超时:换域名
    Transport,
    RateLimited(u64),
    /// 5xx、429、响应不是 JSON:换域名(可能是入口自身的问题或限流)
    Server,
    /// 401/403/407:服务器明确拒绝这份凭据 —— 换下一份凭据的信号
    Auth,
    /// 其余 4xx(404/400):路径或参数问题,换域名、换凭据都是同一答案
    Client,
    /// 业务失败(HTTP 200 但 code:false 之类):服务器明确答复了,不换
    Business,
}

impl FailKind {
    /// 值得换域名重试吗
    pub fn retryable(self) -> bool {
        matches!(self, FailKind::Transport | FailKind::Server | FailKind::RateLimited(_))
    }
}

/// 值得换"下一份凭据"再试吗?
///
/// 只有服务器明确答复"这份凭据不行"(401/403)才是换凭据的信号:本机可能留着
/// 几份登录态(Trae 装了多个版本),其中一份可能已作废。
/// 网络不通、5xx、业务失败都跟身份无关,换凭据只是白等。
pub fn another_credential_may_help(kind: Option<FailKind>) -> bool {
    kind == Some(FailKind::Auth)
}

/// 尝试顺序:上次成功的入口优先(避免每轮都先撞那个挂掉的),
/// 其余按定义顺序补上,去重。
pub fn attempt_order(primary: &str, fallbacks: &[&str], last_good: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for u in last_good
        .into_iter()
        .chain(std::iter::once(primary))
        .chain(fallbacks.iter().copied())
    {
        if !out.iter().any(|x| x == u) {
            out.push(u.to_string());
        }
    }
    out
}

/// 取不到数时的统一失败结果:带上渠道本来的类型,别让 Default 的 Amount 把
/// 百分比渠道渲染成金额卡(扫描报告 P1-2)。
pub(super) fn err_result(
    def: &providers::ProviderDef,
    msg: String,
    fail: Option<FailKind>,
) -> (FetchResult, Option<FailKind>) {
    (
        FetchResult {
            valid: false,
            kind: def.default_kind,
            unit: unit_for(def.default_kind).to_string(),
            error: Some(msg),
            ..Default::default()
        },
        fail,
    )
}

/// kind → 单位只在这里定义一份(以前在三个地方各写一遍,漏了 Percent)。
pub fn unit_for(k: Kind) -> &'static str {
    match k {
        Kind::Amount => "CNY",
        Kind::Percent => "%",
        Kind::Points => "credits",
    }
}

/// 把 Origin/Referer 改写成与实际请求入口同域(报告 P2-3)。
/// 备用入口的网关看到别家的 Origin 会直接 403,而 403 会被界面说成"网页登录已过期"。
pub(super) fn same_origin_header(url: &str, header: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split('/').next()?;
    if host.is_empty() {
        return None;
    }
    Some(if header.eq_ignore_ascii_case("origin") {
        format!("https://{host}")
    } else {
        format!("https://{host}/")
    })
}

/// 单次尝试:一个地址、一次请求。返回结果与"这次失败值不值得换域名"。
///
/// **这里是 9 个内置渠道取余额的唯一出口** —— 排查用的 L2 日志埋在这一层,
/// 一次覆盖全部渠道,不用去改 9 个 extractor(见 docs/product/product-logging-observability)。
/// 改写 `extra` 里某个键的值;没有就追加(逐包到期那行在 extract 里先占位,这里改写)。
pub(super) fn extra_set(extra: &mut Vec<(String, String)>, key: &str, val: &str) {
    match extra.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = val.to_string(),
        None => extra.push((key.to_string(), val.to_string())),
    }
}

#[cfg(test)]
pub(super) async fn fetch_once(
    client: &reqwest::Client,
    def: &providers::ProviderDef,
    url: &str,
    key: &str,
) -> (FetchResult, Option<FailKind>) {
    fetch_once_scoped(client, def, url, key, None).await
}

pub(super) async fn fetch_once_scoped(
    client: &reqwest::Client,
    def: &providers::ProviderDef,
    url: &str,
    key: &str,
    account_id: Option<&str>,
) -> (FetchResult, Option<FailKind>) {
    let started = std::time::Instant::now();
    let (res, kind, info) = fetch_once_inner(client, def, url, key, account_id).await;
    log_wire(def, url, started.elapsed().as_millis(), &info, &res);
    (res, kind)
}

/// 一次请求的"线上事实" —— 只给日志用。
///
/// 排查"接口怎么了"最需要的就是这几样:状态码、字节数、响应的**结构**。
/// 只记结构不记值 —— 响应体里可能有余额、可能有令牌。
#[derive(Default, Clone)]
pub(super) struct WireInfo {
    pub(super) status: Option<u16>,
    pub(super) bytes: usize,
    /// 顶层键名列表(JSON 时)或响应片段(非 JSON 时)。**只记结构,不记值**。
    pub(super) shape: String,
    /// 这一层的错误码(传输层 / 状态码 / 业务失败)。
    pub(super) code: Option<&'static str>,
}

pub(super) fn antigravity_attempt(o: crate::antigravity::AgyOutcome, mut info: WireInfo) -> (FetchResult, Option<FailKind>, WireInfo) {
    info.bytes = o.bytes;
    info.shape = o.shape;
    info.code = o.err_code;
    let kind = o.fail_kind.map(|k| match k {
        "auth" => FailKind::Auth,
        "transport" => FailKind::Transport,
        "server" => FailKind::Server,
        _ => FailKind::Business,
    });
    (o.result, kind, info)
}

/// 真正发请求的那一段(与 [`fetch_once`] 分开,是为了"每个请求只记一行 L2")。
pub(super) async fn fetch_once_inner(
    client: &reqwest::Client,
    def: &providers::ProviderDef,
    url: &str,
    key: &str,
    account_id: Option<&str>,
) -> (FetchResult, Option<FailKind>, WireInfo) {
    let mut info = WireInfo::default();
    if def.id == "antigravity" {
        return antigravity_attempt(crate::antigravity::fetch_usage(key).await, info);
    }
    // 豆包风控码的本地静默期:冷却期内连请求都不发,避免重试加深标记。
    if def.id == "doubao" {
        if let Some(msg) = crate::doubao::risk_cooldown_message() {
            let (r, k) = err_result(def, msg, Some(FailKind::Business));
            return (r, k, info);
        }
    }
    // MiniMax:账户端点要官方客户端签名(x-signature/yy),查询串参与签名且绑定
    // URL 与时间戳 —— 每次尝试都要现算,静态 extra_headers 表达不了(见 core::minimax)。
    // 凭据键形如 mm:<region>:<token>,Authorization 必须用解出来的裸令牌,不能带前缀。
    let mm = if def.id == "minimax" {
        match crate::minimax::sign_for_fetch(url, def.method, key, account_id) {
            Some(s) => Some(s),
            None => {
                let (r, k) = err_result(
                    def,
                    "MiniMax 凭据格式异常(缺 mm: 区域前缀)".into(),
                    Some(FailKind::Business),
                );
                return (r, k, info);
            }
        }
    } else {
        None
    };
    // 豆包:活动接口按客户端环境放行,查询串要带桌面版平台参数与派生设备号
    // (core::doubao;UA/Referer/Origin 由 def.extra_headers 静态携带)
    let db_url = if def.id == "doubao" {
        Some(crate::doubao::decorate_url(url, key))
    } else {
        None
    };
    let url_signed: &str = mm
        .as_ref()
        .map(|s| s.url.as_str())
        .or(db_url.as_deref())
        .unwrap_or(url);
    let mut req = match def.method {
        Method::Get => client.get(url_signed),
        Method::PostJson(body) => client
            .post(url_signed)
            .header("Content-Type", "application/json")
            .body(body()),
    }
    .header("Accept", "application/json")
    // 单次尝试的预算:4 个入口最坏 32 秒,还收在 60 秒轮询周期内。
    // 用客户端的 20 秒默认值会让"某个域名挂着"直接拖垮整轮取数。
    .timeout(std::time::Duration::from_secs(8));

    // 网页登录型的凭据有两种形态(见 core::weblogin):
    // - `cookie:session=…; session_2=…` —— 宿主浏览器登录后留下的会话
    // - `desktop:<accessToken>` —— 从 WorkBuddy **桌面端**凭据里解出来的令牌
    // - 其余一律当令牌,走 `Authorization: Bearer …`(手动粘贴的访问密钥)
    //
    // Cookie 这条路**必须冒充浏览器 UA**:网关(openresty)按 UA 分流,同一批 Cookie
    // 用浏览器 UA 200、用客户端 UA 401(2026-10-08 实测,两种各打一次)。
    // 客户端默认那个 `TokenScope/0.1` 恰好是被拒的一类,所以这台机器上要靠逐请求覆盖。
    let desktop_token = key.trim().strip_prefix(DESKTOP_TOKEN_PREFIX);
    if mm.is_some() {
        let token = crate::minimax::parse_key(key).map(|(_, t)| t.trim()).unwrap_or("");
        req = req.header("Authorization", format!("Bearer {}", token));
    } else if def.id == "doubao" {
        // 豆包只有 Cookie 一条路:粘贴值允许不带 cookie: 前缀(手动自救路径)。
        // 形状不对就如实报错,绝不拿去当 Bearer 撞 —— 那只会换来一句看不懂的 401。
        match crate::doubao::cookie_header_of(key) {
            Some(c) => req = req.header("Cookie", c),
            None => {
                let (r, k) = err_result(
                    def,
                    "豆包凭据不是合法的 Cookie 字符串 · 点「登录豆包账号」重新捕获".into(),
                    Some(FailKind::Auth),
                );
                return (r, k, info);
            }
        }
    } else if let Some(token) = desktop_token {
        // 桌面端令牌走 Bearer,且**不能**带 Cookie;UA 用桌面端自己的形态
        req = req
            .header("Authorization", format!("Bearer {}", token.trim()))
            .header("User-Agent", DESKTOP_UA);
    } else {
        match weblogin::cookie_header(key) {
            Some(cookie) => {
                req = req.header("Cookie", cookie).header("User-Agent", BROWSER_UA);
            }
            None => {
                req = req.header(
                    "Authorization",
                    format!("{}{}", def.auth_prefix, key.trim()),
                );
            }
        }
    }

    for (k, v) in def.extra_headers {
        // 跨域名回退时 Origin/Referer 必须跟着走,别把主域的头发给备用域(P2-3)
        let val = match k.as_ref() {
            "Origin" | "Referer" => same_origin_header(url, k).unwrap_or_else(|| v.to_string()),
            "User-Agent" if def.id == "doubao" => crate::doubao::app_ua().to_string(), // 豆包 UA 版本运行时探测替换(钉死已腐烂,见 checkin.rs / core::doubao_client)
            _ => v.to_string(),
        };
        req = req.header(*k, val);
    }
    // MiniMax 的三个签名头最后追加(查询串已在上面的签名 URL 里,与头同源)
    if let Some(s) = &mm {
        for (k, v) in &s.headers {
            req = req.header(*k, v);
        }
    }
    if def.id == "codex" {
        if let Some(id) = account_id {
            req = req.header("ChatGPT-Account-Id", id);
        }
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let text = e.to_string().to_lowercase();
            let (msg, kind, code) = if text.contains("certificate") || text.contains("cert") {
                // 证书/时钟问题换域名是同一个答案,别把四个入口各撞 8 秒(P2-24)
                (
                    "HTTPS 证书校验失败(检查代理拦截或系统时间)".to_string(),
                    FailKind::Business,
                    crate::errcodes::TLS,
                )
            } else if e.is_timeout() {
                ("请求超时".to_string(), FailKind::Transport, crate::errcodes::TIMEOUT)
            } else if e.is_connect() {
                (
                    "无法连接(检查网络或代理)".to_string(),
                    FailKind::Transport,
                    crate::errcodes::CONNECT,
                )
            } else {
                // 原始 reqwest 错误只进日志,用户面给一句可读的(报障时日志里搜 reqwest)
                crate::diaglog::write_keyed(def.id, "reqwest", &format!("请求失败: {e}"));
                (
                    "请求没发出去(网络或代理问题)".to_string(),
                    FailKind::Transport,
                    crate::errcodes::CONNECT,
                )
            };
            info.code = Some(code);
            let (r, k) = err_result(def, msg, Some(kind));
            return (r, k, info);
        }
    };

    let status = resp.status();
    info.status = Some(status.as_u16());
    if status.as_u16() == 429 {
        let wait = retry_after_seconds(resp.headers(), now_ts());
        let (r, k) = err_result(def, "请求过于频繁，稍后自动重试".into(), Some(FailKind::RateLimited(wait)));
        return (r, k, info);
    }
    info.status = Some(status.as_u16());
    // 上限 2 MB:入口被网关换成大 HTML 时不要整包读进内存再解析(P2-24)。
    // 先看 Content-Length,超了**在读之前**就拒;没有长度(分块传输)才按块累加,
    // 一超上限立刻停手 —— 以前 `resp.bytes()` 会先把整包缓进内存再判大小(P2)。
    const MAX_BODY: usize = 2_000_000;
    if let Some(len) = resp.content_length() {
        if len > MAX_BODY as u64 {
            info.code = Some(crate::errcodes::BODY_SHAPE);
            let (r, k) = err_result(def, format!("响应过大({len} 字节),已丢弃"), Some(FailKind::Server));
            return (r, k, info);
        }
    }
    let raw = match read_body_capped(resp, MAX_BODY).await {
        Ok(b) => {
            info.bytes = b.len();
            b
        }
        Err((msg, kind)) => {
            info.code = Some(crate::errcodes::CONNECT);
            let (r, k) = err_result(def, msg, Some(kind));
            return (r, k, info);
        }
    };
    let body: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(v) => {
            // 顶层键名列表:**只记结构,不记值** —— 值里可能有余额,可能有令牌。
            // 接口改字段名时,这是唯一能看出"原来叫 X 现在叫 Y"的线索。
            info.shape = shape_of(&v);
            v
        }
        Err(_) => {
            // 响应体不是 JSON 时,**失败类别要跟着状态码走**,不能一律当成 Server。
            //
            // 2026-10-20 真机踩到(用户 diag.log):WorkBuddy 的 401 由网关回了一页 HTML,
            // 这里判成 `FailKind::Server` → 只换域名、**不换下一份凭据** → 明明已经解出了
            // 桌面端令牌(fp=345578fe),循环却停在那份 401 的手动令牌上,
            // 日志写的是"凭据 [手动令牌→不通]",用户永远取不到数。
            // 401/403 说的是"这份凭据不行",与"响应长什么样"无关 —— 语义由状态码定。
            let code = status.as_u16();
            // 2xx 但不是 JSON = **接口结构变了**(网关挡了 / 返回了 HTML),
            // 不是"业务失败" —— 两者要分开,否则 grep BODY_NOT_JSON 找不到这里。
            info.code = Some(if status.is_success() {
                crate::errcodes::BODY_NOT_JSON
            } else {
                crate::errcodes::for_status(code)
            });
            // 非 JSON 时把响应**开头一小段**记下来(脱敏后)—— 网关回的是 HTML 还是
            // JSON 错误体,看这一段就知道。
            info.shape = snippet_of(&raw);
            let (r, k) = err_result(
                def,
                format!("响应不是合法 JSON (HTTP {code})"),
                Some(kind_for_status(code)),
            );
            return (r, k, info);
        }
    };

    // 风控码只记业务码与静默期,不保存响应正文;冷却期内后续轮询不再发请求。
    if def.id == "doubao" {
        crate::doubao::note_risk_if_present(&body);
    }
    let mut result = (def.extract)(&body);

    // 非 2xx 却解析成功,说明接口用 HTTP 状态码表达业务错误 —— 以状态码为准。
    // OpenCode 的 401/403 带 type:error 体,已被 extractor 处理,不覆盖它的文案。
    if !status.is_success() && result.valid {
        result.valid = false;
        result.error = Some(format!("HTTP {}", status.as_u16()));
    }
    // 复用登录态的渠道:401/403 不是"配置错了",而是那份凭据过期了 ——
    // 用户能做的只有重新登录一次,直接把话说清楚,并且要说对方向:
    // 本机登录型渠道(Trae/Codex)去开客户端;网页登录型(WorkBuddy)回挂件重新登录。
    if !status.is_success() && (status.as_u16() == 401 || status.as_u16() == 403) {
        if let Some(msg) = expired_credential_msg(def) {
            result.error = Some(msg);
        }
    }

    let kind = if result.valid {
        None
    } else if !status.is_success() {
        // 凭据被明确拒绝 → 换下一份登录态值得一试;404/400 → 路径/参数问题,换谁都没用。
        Some(kind_for_status(status.as_u16()))
    } else {
        Some(FailKind::Business) // HTTP 200 但业务失败
    };

    // 码:状态码优先(HTTP 200 但业务失败是另一回事)
    if info.code.is_none() {
        info.code = if !status.is_success() {
            Some(crate::errcodes::for_status(status.as_u16()))
        } else if !result.valid {
            Some(crate::errcodes::BIZ_FAIL)
        } else {
            None
        };
    }

    (result, kind, info)
}

/// 响应的**结构**摘要:顶层键名(最多 8 个)。**只记键名,不记值。**
pub(super) fn shape_of(body: &serde_json::Value) -> String {
    let Some(obj) = body.as_object() else {
        return match body {
            serde_json::Value::Array(a) => format!("[{} 项]", a.len()),
            serde_json::Value::Null => "null".into(),
            other => format!("{}", type_name_of(other)),
        };
    };
    let keys: Vec<&str> = obj.keys().take(8).map(|s| s.as_str()).collect();
    if obj.len() > 8 {
        format!("{{{}+{}…}}", keys.join(","), obj.len() - 8)
    } else {
        format!("{{{}}}", keys.join(","))
    }
}

/// JSON 值的类型名(不记值本身)。
pub(super) fn type_name_of(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// 非 JSON 响应的开头一小段(给日志看,脱敏由 [`crate::diaglog::redact`] 负责)。
///
/// ⚠ 这是**唯一一处原始第三方响应内容**进日志字段的地方,所以要额外收紧:
/// 只留可打印 ASCII、遇到 HTML 标签就截断、限长 80 字节。
/// 网关回的错误页有时会把请求行原样带回来,那里面可能有令牌。
pub(super) fn snippet_of(raw: &[u8]) -> String {
    let head = String::from_utf8_lossy(&raw[..raw.len().min(200)]);
    let mut out = String::new();
    for ch in head.chars() {
        if ch == '<' {
            break; // HTML 开始 —— 后面是页面正文,没有排查价值
        }
        if ch.is_ascii_graphic() || ch == ' ' {
            out.push(ch);
        } else {
            out.push(' '); // 控制字符/非 ASCII 一律换成空格
        }
        if out.len() >= 80 {
            break;
        }
    }
    out.trim().to_string()
}

/// 把一次请求记成一行 L2。
///
/// **成功的事件不落盘**(只进内存环形缓冲);失败的落盘,并连带写出前后的上下文。
/// 这是"日志量"和"信息量"不矛盾的关键 —— 平时不占盘,出事才有现场。
pub(super) fn log_wire(
    def: &providers::ProviderDef,
    url: &str,
    ms: u128,
    info: &WireInfo,
    res: &FetchResult,
) {
    let host = providers::url_host(url).unwrap_or_default();
    let path = url
        .split_once("://")
        .map(|(_, rest)| rest.find('/').map(|i| &rest[i..]).unwrap_or("/"))
        .unwrap_or("/")
        .to_string();
    let method = match def.method {
        Method::Get => "GET",
        Method::PostJson(_) => "POST",
    };
    let status_s = info.status.map(|s| s.to_string()).unwrap_or_default();
    let ms_s = ms.to_string();
    let bytes_s = info.bytes.to_string();
    let mut fields: Vec<(&str, &str)> = vec![
        ("url", url),
        ("host", &host),
        ("path", &path),
        ("method", method),
        ("ms", &ms_s),
    ];
    // 状态码只在真的拿到响应时才有(连接失败/超时时根本没有 HTTP 状态) ——
    // 写一个空的状态码会让人以为"服务器回了个空状态"。
    if info.status.is_some() {
        fields.push(("status", &status_s));
    }
    if info.bytes > 0 {
        fields.push(("bytes", &bytes_s));
    }
    if !info.shape.is_empty() {
        fields.push(("shape", &info.shape));
    }
    crate::diaglog::wire("fetch", Some(def.id), "http", info.code, &fields);
    // 成功的那次:把解析出的值也记下来(只进内存,除非开了详细日志)。
    // "余额显示不对"这类问题必须看成功响应,这是唯一能看到"接口到底返了什么"的地方。
    if res.valid {
        let rem = res
            .remaining
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into());
        let unit = res.unit.clone();
        crate::diaglog::result(
            "fetch",
            Some(def.id),
            "parse",
            &[("remaining", &rem), ("unit", &unit), ("kind", kind_str(res.kind))],
        );
    }
}

/// kind → 日志里用的短名(与 serde 的 lowercase 表示一致)。
pub(super) fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::Amount => "amount",
        Kind::Percent => "percent",
        Kind::Points => "points",
    }
}

/// 非 2xx 的状态码 → 失败类别。**唯一口径**:正常分支与"响应体不是 JSON"那条
/// 错误页分支都必须走它,否则会出现"同一个 401 在两处被判成不同类型"的漂移
/// —— 2026-10-20 真机日志就是这么被坑的(HTML 错误页让 401 变成了 Server,
/// 于是只换域名、不换凭据,桌面端令牌永远轮不到)。
pub(crate) fn retry_after_seconds(headers: &reqwest::header::HeaderMap, now: i64) -> u64 {
    headers.get("retry-after").and_then(|h| h.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok().or_else(|| chrono::DateTime::parse_from_rfc2822(v).ok().map(|d| (d.timestamp() - now).max(0) as u64)))
        .unwrap_or(60).min(86_400)
}

pub(super) fn custom_scope(ch: &custom::CustomChannel, fp: &str) -> String {
    key_fp(&format!("{fp}:{}", serde_json::to_string(ch).unwrap_or_default()))
}

pub(crate) fn kind_for_status(code: u16) -> FailKind {
    if matches!(code, 401 | 403 | 407) {
        FailKind::Auth
    } else if code == 429 || code >= 500 {
        FailKind::Server
    } else {
        FailKind::Client
    }
}

/// 把下一块拼进缓冲,一旦超过 `max` 就报错(**纯函数**:上限逻辑靠它单测,
/// 真起 HTTP 服务端才能造出的 2 MB 响应不该进单元测试)。
/// `pub(crate)`:模型价目表那条取数路径(models.rs)用同一个上限口径,不另写一份。
pub(crate) fn push_chunk(out: &mut Vec<u8>, c: &[u8], max: usize) -> Result<(), String> {
    if out.len() + c.len() > max {
        return Err(format!("响应过大(超过 {max} 字节),已丢弃"));
    }
    out.extend_from_slice(c);
    Ok(())
}

/// 分块读响应体,超限**立刻停手** —— 不再把超大的包整个读进内存。
/// 返回 (缓冲, 失败类别),失败类别沿用 fetch_once 的口径。
pub(super) async fn read_body_capped(
    mut resp: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, (String, FailKind)> {
    let mut out: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(c)) => {
                if let Err(msg) = push_chunk(&mut out, &c, max) {
                    return Err((msg, FailKind::Server));
                }
            }
            Ok(None) => return Ok(out),
            // 连接中断属于传输类 → 值得换下一个入口(以前 None 会让整条回退链就地停下)
            Err(e) => {
                // 原始错误进日志,用户面给一句可读的
                crate::diaglog::write_keyed("fetch", "read-body", &format!("读取响应失败: {e}"));
                return Err(("响应没读完(连接中断)".to_string(), FailKind::Transport));
            }
        }
    }
}

/// 要不要把"上次成功地址"的备忘清掉:**这次失败、且确实发过请求**时清。
/// 这个入口已经不行了却还被记着,每轮都要先等满 8 秒超时才切下一个(P2-22);
/// 成功那一支不清(备忘正要写进去),没发过请求(没凭据)也无从谈起。
pub(super) fn should_clear_memo(result_valid: bool, attempted: bool) -> bool {
    !result_valid && attempted
}

/// 返回 (结果, **最后一次实际尝试的地址**, 最后一次失败的类别)。调用方据此记住
/// "哪个入口是通的",失败时也要知道"刚撞的是哪一个"好把那条备忘清掉。
pub async fn fetch_channel(
    client: &reqwest::Client,
    def: &providers::ProviderDef,
    key: &str,
    last_good: Option<&str>,
) -> (FetchResult, Option<String>, Option<FailKind>) {
    fetch_channel_scoped(client, def, key, last_good, None).await
}

pub(super) async fn enrich_fetched_result(
    client: &reqwest::Client,
    def: &providers::ProviderDef,
    key: &str,
    used_url: &str,
    account_id: Option<&str>,
    result: &mut FetchResult,
) {
    if !result.valid { return; }
    match def.id {
        "codex" => {
            let url = reqwest::Url::parse(crate::codex::RESET_CREDITS_URL).and_then(|endpoint| {
                reqwest::Url::parse(used_url)?.join(endpoint.path())
            });
            if let Ok(url) = url {
                enrich_codex(client, key, account_id, url.as_str(), result).await;
            }
        }
        "minimax" => crate::minimax::merge_expiry(client, key, used_url, account_id, result).await,
        _ => {}
    }
}

pub(super) async fn fetch_enriched_attempt(
    client: &reqwest::Client,
    def: &providers::ProviderDef,
    url: &str,
    key: &str,
    account_id: Option<&str>,
) -> (FetchResult, Option<FailKind>) {
    let (mut result, kind) = fetch_once_scoped(client, def, url, key, account_id).await;
    enrich_fetched_result(client, def, key, url, account_id, &mut result).await;
    (result, kind)
}

pub(super) async fn enrich_codex(
    client: &reqwest::Client, key: &str, account_id: Option<&str>,
    reset_url: &str, result: &mut FetchResult,
) {
    result.reset_credits = Some(match fetch_codex_reset_credits(client, key, account_id, reset_url).await {
        Ok(cards) => cards,
        Err(message) => crate::codex::unavailable(result.reset_credits.take(), message),
    });
}

pub(super) async fn fetch_channel_scoped(
    client: &reqwest::Client,
    def: &providers::ProviderDef,
    key: &str,
    last_good: Option<&str>,
    account_id: Option<&str>,
) -> (FetchResult, Option<String>, Option<FailKind>) {
    let order = attempt_order(def.url, def.fallback_urls, last_good);
    let mut last: Option<FetchResult> = None;
    let mut last_kind: Option<FailKind> = None;

    for (i, url) in order.iter().enumerate() {
        let (result, kind) = fetch_enriched_attempt(client, def, url, key, account_id).await;
        // 成功,或者失败类型不值得换域名(密钥错/业务失败)—— 就此打住
        let stop = match kind {
            None => true,
            Some(k) => !k.retryable(),
        };
        if stop || matches!(kind, Some(FailKind::RateLimited(_))) || i + 1 == order.len() {
            // 失败也要把**刚试过的地址**报上去:调用方要靠它把"上次成功地址"清掉。
            // 以前只在成功时给 Some,于是失败分支永远不会清备忘 ——
            // 一个挂掉的备用镜像能把渠道永久钉在上面,每轮白等满超时(P2)。
            let used = Some(url.clone());
            return (result, used, kind);
        }
        // L1 决策:换下一个入口了 —— 记下"从哪换到哪、为什么换"。
        // 没有这一行,日志里只会看到"401"和"连不上"混在一起,看不出是哪个域名在坏。
        let why = match kind {
            Some(FailKind::Auth) => "凭据被拒",
            Some(FailKind::Transport) => "传输失败",
            Some(FailKind::Server) => "服务端错误",
            Some(FailKind::RateLimited(_)) => "请求过于频繁",
            Some(FailKind::Client) => "请求有问题",
            Some(FailKind::Business) => "业务失败",
            None => "",
        };
        let from = providers::url_host(url).unwrap_or_default();
        let to = order
            .get(i + 1)
            .and_then(|u| providers::url_host(u))
            .unwrap_or_default();
        crate::diaglog::decision(
            "fetch",
            Some(def.id),
            "attempt",
            &format!("换下一个入口:{from} → {to}(原因:{why})"),
        );
        last_kind = kind;
        last = Some(result);
    }
    // order 至少含主地址,这里到不了;给个兜底避免 unwrap
    (
        last.unwrap_or(FetchResult {
            valid: false,
            kind: def.default_kind,
            error: Some("没有可用的接口地址".into()),
            ..Default::default()
        }),
        None,
        last_kind,
    )
}

pub(super) async fn fetch_codex_reset_credits(
    client: &reqwest::Client,
    key: &str,
    account_id: Option<&str>,
    url: &str,
) -> Result<crate::codex::ResetCredits, String> {
    let mut req = client.get(url)
        .bearer_auth(key.trim()).header("Accept", "application/json")
        .header("OpenAI-Beta", "codex-1").header("originator", "Codex Desktop")
        .timeout(std::time::Duration::from_secs(4));
    if let Some(id) = account_id {
        req = req.header("ChatGPT-Account-Id", id);
    }
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() { "重置卡查询超时".to_string() } else { "重置卡查询连接失败".to_string() }
    })?;
    if !resp.status().is_success() {
        return Err(format!("重置卡查询 HTTP {}", resp.status().as_u16()));
    }
    let raw = read_body_capped(resp, 512_000).await.map_err(|(msg, _)| msg)?;
    let body = serde_json::from_slice(&raw).map_err(|_| "重置卡响应不是 JSON".to_string())?;
    crate::codex::inventory(&body, now_ts()).map_err(str::to_string)
}

/// 读桌面端凭据候选。`region`:`Some(true)` 只国际版 / `Some(false)` 只国内版 /
/// 自定义渠道批量取数(P2):与内置渠道**同时**跑,内部最多 4 个并发。
///
/// 返回顺序按配置里的原始下标恢复 —— 界面顺序不因并发而变;隐藏渠道连凭据都不读。
/// 凭据走同一个凭据管理器(账户名 = 渠道 id),密钥纪律与内置渠道完全相同。
#[cfg(test)]
pub(super) async fn fetch_custom_all<F>(
    http: &reqwest::Client,
    channels: &[custom::CustomChannel],
    hidden: &[String],
    key_for: F,
) -> Vec<(custom::CustomChannel, Option<custom::CustomResult>, String)>
where
    F: Fn(&str) -> Option<String> + Clone + Send + 'static,
{
    fetch_custom_all_observed(http, channels, hidden, key_for, None).await
}

pub(super) type CustomObserver<'a> = &'a (dyn Fn(&custom::CustomChannel, Option<&custom::CustomResult>, &str) + Send + Sync);

pub(super) async fn fetch_custom_all_observed<F>(
    http: &reqwest::Client,
    channels: &[custom::CustomChannel],
    hidden: &[String],
    key_for: F,
    observer: Option<CustomObserver<'_>>,
) -> Vec<(custom::CustomChannel, Option<custom::CustomResult>, String)>
where
    F: Fn(&str) -> Option<String> + Clone + Send + 'static,
{
    // 先把任务收成**拥有所有权**的 (下标, 渠道):直接用 `channels.iter().filter().map()`
    // 会把 `&CustomChannel` 和异步块绑在一起,泛型 async 在 Tauri 的 `spawn` 下会
    // 因高阶生命周期推断失败而报 "FnOnce is not general enough"。
    let jobs: Vec<(usize, custom::CustomChannel)> = channels
        .iter()
        .enumerate()
        .filter(|(_, ch)| !hidden.iter().any(|h| h == &ch.id))
        .map(|(i, ch)| (i, ch.clone()))
        .collect();

    let futures = jobs.into_iter().map(|(i, ch)| {
        let key_for = key_for.clone();
        async move {
            let cid = ch.id.clone();
            let key = tokio::task::spawn_blocking(move || key_for(&cid))
                .await
                .unwrap_or(None);
            match key {
                None => (i, ch, None, String::new()),
                Some(k) => {
                    let fp = key_fp(&k);
                    if let Some(f) = observer { f(&ch, None, &fp); }
                    let r = custom::fetch_custom(http, &ch, &k).await;
                    if let Some(f) = observer { f(&ch, Some(&r), &fp); }
                    (i, ch, Some(r), fp)
                }
            }
        }
    });

    let mut out: Vec<(usize, custom::CustomChannel, Option<custom::CustomResult>, String)> =
        futures::stream::iter(futures).buffer_unordered(4).collect().await;
    // 并发完成顺序 ≠ 配置顺序:按下标排回来,下游按 id 查找/界面按配置排序都不变。
    out.sort_by_key(|(i, _, _, _)| *i);
    out.into_iter()
        .map(|(_, ch, r, fp)| (ch, r, fp))
        .collect()
}

