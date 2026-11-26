use super::*;

/// 签到专用的 UA:与取数路径共用同一份(网关按 UA 分流,两处不一致会各踩各的坑)。
use crate::fetch::BROWSER_UA;

/// 签到专用的 HTTP 客户端:**强制直连,不跟随系统代理**。
///
/// 理由(设计稿裁决 4A):Trae 侧 9074("当前参与用户太多")的成因之一是按出口 IP 排队限流,
/// 而系统代理/机场 IP 正是那类高发出口。余额取数跟随代理没问题,签到这条特意绕开。
///
/// 连接池:**进程内复用一份**。这里原写「一天就几次请求,不用连接池」,但同一个客户端
/// 现在也被 5 分钟一次的签到节拍器与活动监测调用(`lib.rs` 的 `monitor_*` / `run_checkin`),
/// 一天是几百次 —— 每次重建 TLS 上下文与连接池纯属浪费。`no_proxy` 的**语义不变**,
/// 只是把「每次新建」改成「首次构造、之后复用」。
pub fn direct_client() -> reqwest::Client {
    crate::http_pool::shared_direct()
        .expect("签到 HTTP 客户端初始化失败")
        .clone()
}

/// 一次请求要带的那份凭据。**只在内存里过一趟**:不落盘、不进日志。
#[derive(Clone)]
pub enum Cred {
    /// Trae:本机登录态(令牌 + 纯数字设备号;设备号缺了会恒定 9074)
    Trae { token: String, device_id: String },
    /// WorkBuddy:网页会话。`cookie:…` 走 Cookie 头,其余当 Bearer 令牌。
    ///
    /// **也承载桌面端凭据**:`workbuddy::desktop_tokens()` 给的是裸 JWT(不带
    /// `desktop:` 前缀 —— 那是取数层的标记),走到这里正好落进"其余当 Bearer"这一支。
    /// 2026-10-20 实测:该令牌打签到接口 200,且换哪种 UA 都行。
    Web(String),
    /// Qoder:本机桌面端 `auth.v1.dat` 解出的访问令牌,一律走 `Authorization: Bearer`。
    Qoder(String),
    /// Qoder 国际版:在 Bearer 之外带上 Qoder 自己生成的设备身份头。
    QoderMachine {
        token: String,
        identity: crate::qoder::QoderMachineIdentity,
    },
    /// MiniMax:本机客户端凭据(键形如 `mm:<region>:<token>`,区域决定域名;
    /// 签名 URL 与三个签名头在 call_with_body 的 minimax 分支现算,见 `crate::minimax`)。
    MiniMax { key: String, uid: Option<String> },
}

fn apply_cred(req: reqwest::RequestBuilder, cred: &Cred) -> reqwest::RequestBuilder {
    match cred {
        Cred::Trae { token, device_id } => req
            .header("Authorization", format!("Cloud-IDE-JWT {}", token.trim()))
            .header("x-device-id", device_id.trim()),
        Cred::Web(key) => match crate::weblogin::cookie_header(key) {
            // 会话 Cookie + 浏览器 UA(网关按 UA 分流)
            Some(cookie) => req.header("Cookie", cookie).header("User-Agent", BROWSER_UA),
            None => req.header("Authorization", format!("Bearer {}", key.trim())),
        },
        Cred::Qoder(token) => req.header("Authorization", format!("Bearer {}", token.trim())),
        Cred::QoderMachine { token, identity } => {
            let mut req = req
                .header("Authorization", format!("Bearer {}", token.trim()))
                .header("Cosy-MachineToken", identity.machine_token.trim())
                .header("Cosy-MachineType", identity.machine_type.trim())
                .header("Cosy-MachineCode", identity.machine_code.trim());
            if let Some(version) = identity.client_version.as_deref() {
                req = req.header("Cosy-Version", version);
            }
            if let Some(machine_os) = identity.machine_os.as_deref() {
                req = req.header("Cosy-MachineOS", machine_os);
            }
            if let Some(hostname) = identity.machine_hostname.as_deref() {
                req = req.header("Cosy-MachineHostname", hostname);
            }
            if let Some(machine_id) = identity.machine_id.as_deref() {
                req = req.header("Cosy-MachineId", machine_id);
            }
            req
        }
        Cred::MiniMax { key, .. } => {
            // 令牌从键里解出;签名头与实际域名在 call_with_body 里处理
            let token = crate::minimax::parse_key(key)
                .map(|(_, t)| t.trim())
                .unwrap_or(key.trim());
            req.header("Authorization", format!("Bearer {}", token))
        }
    }
    .header("Content-Type", "application/json")
    .header("Accept", "application/json, text/plain, */*")
}

/// 与 [`call`] 同一条路,但**额外把原始 body 带回来**。
///
/// 为什么需要:Qoder 的连续签到天数(`streak_days`)在**顶层**,而 `Outcome` 只承载
/// 判定结果、不承载原始响应。与其为了一个展示字段再打一次接口(多一跳网络、
/// 多一次被限流的机会),不如让第一次调用就把 body 捎回来。
///
/// `call()` 保持原样转发到这里 —— 它只是丢掉了 body。
pub async fn call_with_body(
    client: &reqwest::Client,
    def: &CheckinDef,
    cred: &Cred,
    claim: bool,
    claim_id: Option<&str>,
) -> (Outcome, Option<serde_json::Value>) {
    let primary = if claim { def.claim_url } else { def.status_url };
    let fallbacks = if claim { def.fallback_claim } else { def.fallback_status };
    let urls: Vec<&str> = std::iter::once(primary).chain(fallbacks.iter().copied()).collect();
    let mut last = Outcome::Failed("没有可用地址".into());

    let subst: Vec<String> = urls
        .iter()
        .map(|u| {
            if u.contains("{id}") {
                match claim_id {
                    Some(id) => u.replace("{id}", id),
                    None => String::new(),
                }
            } else {
                (*u).to_string()
            }
        })
        .collect();
    if subst.iter().all(|u| u.is_empty()) {
        return (
            Outcome::Failed("领取地址需要活动 id,但调用方没给".into()),
            None,
        );
    }

    for url in subst.iter().filter(|u| !u.is_empty()) {
        // 方法逐渠道声明(见 CheckinMethod):Qoder 的查状态是 GET,POST 会 404
        let method = if claim { def.claim_method } else { def.status_method };
        // MiniMax:账户端点要官方客户端签名,查询串带 IANA 时区名,域名跟凭据
        // 区域走 —— 全部在发请求前现算,静态表表达不了(见 core::minimax 头注)
        let mm = match cred {
            Cred::MiniMax { key, uid } => {
                crate::minimax::signed_checkin(url, method == CheckinMethod::Post, key, uid.as_deref())
            }
            _ => None,
        };
        let url: &str = mm.as_ref().map(|s| s.url.as_str()).unwrap_or(url);
        let mut req = match method {
            CheckinMethod::Get => client.get(url),
            CheckinMethod::Post => client.post(url),
        };
        req = apply_cred(req, cred);
        for (k, v) in def.headers {
            req = req.header(*k, *v);
        }
        if let Some(s) = &mm {
            for (k, v) in &s.headers {
                req = req.header(*k, v);
            }
        }
        // GET **不发 body**(带了反而可能被网关当成非法请求)
        if method == CheckinMethod::Post {
            req = req.body(def.body.to_string());
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                let msg = if e.is_timeout() {
                    "请求超时".to_string()
                } else if e.is_connect() {
                    "无法连接(检查网络)".to_string()
                } else {
                    // 原始 reqwest 错误只进日志,用户面给一句可读的
                    crate::diaglog::write_keyed(def.id, "reqwest", &format!("请求失败: {e}"));
                    "请求没发出去(检查网络)".to_string()
                };
                let code = if e.is_timeout() {
                    crate::errcodes::TIMEOUT
                } else {
                    crate::errcodes::CONNECT
                };
                crate::diaglog::wire(
                    "checkin",
                    Some(def.id),
                    "http",
                    Some(code),
                    &[
                        ("url", url),
                        ("host", &host_of(url)),
                        ("method", if method == CheckinMethod::Get { "GET" } else { "POST" }),
                        ("action", if claim { "claim" } else { "status" }),
                    ],
                );
                last = Outcome::Failed(msg);
                continue;
            }
        };
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if matches!(status.as_u16(), 401 | 403) {
            // L2:凭据被拒 —— 用户只会说"没领到",日志要能指出是哪一环。
            crate::diaglog::wire(
                "checkin",
                Some(def.id),
                "http",
                Some(crate::errcodes::CRED_EXPIRED),
                &[
                    ("url", url),
                    ("host", &host_of(url)),
                    ("status", &status.as_u16().to_string()),
                    ("action", if claim { "claim" } else { "status" }),
                ],
            );
            return (Outcome::AuthNeeded, None);
        }
        if status.as_u16() == 404 {
            crate::diaglog::wire(
                "checkin",
                Some(def.id),
                "http",
                Some(crate::errcodes::HTTP_404),
                &[
                    ("url", url),
                    ("host", &host_of(url)),
                    ("action", if claim { "claim" } else { "status" }),
                ],
            );
            last = Outcome::Inactive(format!("HTTP 404 ({})", host_of(url)));
            continue;
        }
        let body: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                let later = status.is_server_error() || status.as_u16() == 429;
                last = if later {
                    Outcome::Retry(format!("HTTP {}", status.as_u16()))
                } else {
                    Outcome::BadShape(format!("HTTP {} · 响应不是 JSON", status.as_u16()))
                };
                crate::diaglog::wire(
                    "checkin",
                    Some(def.id),
                    "http",
                    Some(if later {
                        crate::errcodes::HTTP_5XX
                    } else {
                        crate::errcodes::BODY_NOT_JSON
                    }),
                    &[
                        ("url", url),
                        ("status", &status.as_u16().to_string()),
                        ("bytes", &text.len().to_string()),
                        ("action", if claim { "claim" } else { "status" }),
                    ],
                );
                if later {
                    continue;
                }
                return (last, None);
            }
        };
        let out = match def.id {
            "trae" => parse_trae(&body, claim),
            "minimax" => crate::minimax::parse_minimax(&body, claim),
            "qoder" | "qoder-cn" => parse_qoder(&body, claim),
            _ => parse_workbuddy_for_region(
                &body,
                claim,
                workbuddy_region_for_host(&host_of(url)),
            ),
        };
        // L1:平台给的**业务码**(不看 message 文案 —— 那条红线就是"判因只看 code")。
        // 平台不返 code 时就空着,绝不编。码也限长:它是第三方给的字符串。
        let biz = body
            .get("code")
            .map(|c| match c {
                serde_json::Value::String(s) => s.chars().take(24).collect::<String>(),
                other => other.to_string(),
            })
            .unwrap_or_default();
        crate::diaglog::write_keyed(
            "checkin",
            &format!("{}:resp", def.id),
            &format!(
                "签到响应 · 动作 {} · HTTP {} · 平台码 {} · 结论 {}",
                if claim { "claim" } else { "status" },
                status.as_u16(),
                if biz.is_empty() { "(没给)" } else { &biz },
                // ⚠ 只记**类别**,不记 `out.code()` —— 后者嵌着平台给的 message,
                // 那是第三方原样返回的任意文本,不能进要贴给 support 的日志。
                out.verdict()
            ),
        );
        if status.is_server_error() && matches!(out, Outcome::Failed(_)) {
            last = Outcome::Retry(format!("HTTP {}", status.as_u16()));
            continue;
        }
        return (out, Some(body));
    }
    (last, None)
}

/// 打一次 status 或 claim。按 URL 链逐个试:网络/5xx 才换下一个;
/// 拿到能解读的响应(含业务失败)就地返回 —— 换域名只会换出同一句答复。
///
/// `claim_id`:领取地址里 `{id}` 占位符的取值(只有 Qoder 用得上 ——
/// 它的领取路径是 `/campaigns/{campaignId}/claim`,而 id 只能从查状态那一步拿到)。
/// 占位符存在但 `claim_id` 为 None = 调用方没把 id 带过来,这是**调用方的错**,
/// 直接报出来而不是拿一个空 id 去撞平台。
pub async fn call(
    client: &reqwest::Client,
    def: &CheckinDef,
    cred: &Cred,
    claim: bool,
    claim_id: Option<&str>,
) -> Outcome {
    call_with_body(client, def, cred, claim, claim_id).await.0
}

fn host_of(url: &str) -> String {
    url.split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}
