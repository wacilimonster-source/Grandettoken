//! Qoder 网页会话的账务**只读**查询:逐包到期(积分过期明细)。
//!
//! 排查与设计:docs/product/product-qoder-credit-expiry-2026-10-30.md。三条链路里
//! 只有 qoder.com 的 web 会话能拿到逐包数据(桌面 Bearer 的 openapi 网关只有汇总;
//! PAT 已证伪)。这里发的是浏览器形态的同源请求:Cookie + 浏览器 UA,与挂件
//! WorkBuddy 会话同一套规矩(网关按 UA 分流)。
//!
//! **个人版逐包端点还没在真会话下抓过包**(公开 SPA bundle 只能确证响应的
//! 字段形状,确证不了个人版调哪个路径),所以按候选顺序试,谁先给出"包形状"
//! 的响应用谁,并记住赢的那条(见 [`QoderPacksMemo`])。解析是**形状自适应**的:
//! 只映射能验证的字段,认不出就如实说拿不到 —— 绝不编数据。
use crate::providers::Expiring;
use crate::weblogin;

/// 会话凭据里 `cookie:` 前缀后面那段(发给 qoder.com 的 Cookie 头)。
fn cookie_of(session: &str) -> Option<&str> {
    weblogin::cookie_header(session)
}

/// 候选端点(2026-10-30 从公开 bundle qbase/qoder/0.0.743 提取):
/// ① 组织版 dedicated 包的个人镜像 —— 组织版响应形状(summary + details[],每包
///    带 expiresAt/remainingValue)已由渲染代码确证,个人版最可能同构;
/// ② v2 聚合的 web 版 —— web 端可能比桌面网关多给逐包字段;
/// ③ 计划信息 —— 计划对象有时内嵌权益包。
/// 顺序即先验优先级;登录观察者验证会话时也走这条链。
const CANDIDATE_PATHS: [&str; 3] = [
    "/api/v1/me/dedicated-resource-packages/usage/big_model_credits",
    "/api/v2/me/usages/big_model_credits",
    "/api/v1/me/userplan",
];

/// 单个积分包(已经过净化:只留界面要的三样)。
#[derive(Debug, Clone, PartialEq)]
pub struct CreditPack {
    /// 来源的人话("Purchased Credits" / 包名);解析不出来就是"积分包"。
    pub label: String,
    pub remaining: f64,
    /// 到期时刻(unix **秒**)。9999 哨兵(不过期)已过滤;
    /// **已过期但还有余量的包保留** —— 与 Trae/WorkBuddy 的 expiring[] 同一契约,
    /// 明细卡有"已过期"分区要消费它们(fetch.rs::build_view / app.js::corePoints)。
    pub expires_at: i64,
}

impl CreditPack {
    /// 转成渠道视图消费的 `Expiring`(风险带 / 明细卡同一份结构)。
    pub fn as_expiring(&self) -> Expiring {
        Expiring {
            at: self.expires_at,
            amount: self.remaining,
            label: self.label.clone(),
        }
    }
}

/// 候选端点谁赢了(下标)。只存内存:重启后退回定义顺序,不往磁盘写状态。
#[derive(Default)]
pub struct PacksMemo {
    idx: std::sync::Mutex<Option<usize>>,
}

impl PacksMemo {
    pub fn winner(&self) -> Option<usize> {
        *self.idx.lock().unwrap_or_else(|p| p.into_inner())
    }
    pub fn remember(&self, idx: usize) {
        *self.idx.lock().unwrap_or_else(|p| p.into_inner()) = Some(idx);
    }
    pub fn clear(&self) {
        *self.idx.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// 抓不到包的原因。三档的**界面指引完全不同**:未登录要去登录,结构没认出来
/// 是挂件这一侧的事(重登一百次也没用),其余才是取数坏了。
#[derive(Debug, Clone, PartialEq)]
pub enum PacksErr {
    /// 所有候选都被平台拒了(401/403 / TOKEN_*)—— 会话没登录或已过期
    Unauthorized,
    /// 会话被平台**接受**了(至少一条候选回了 200 + JSON),但响应里找不到逐包数据。
    ///
    /// 2026-11-03 真机:个人版 `/api/v2/me/usages/big_model_credits` 返回的是
    /// `{user_id, quota_key, status, plan_quota, resource_package_quota, …, total_quota,
    /// lastResetAt, nextResetAt}` 这种**汇总**结构,逐包明细在别的层级。这种失败以
    /// 前被并进 `Other`,登录轮询再把它说成"被平台拒绝",于是用户被告知去重登 ——
    /// 重登解决不了任何问题。单列一档,提示才说得对。
    Unrecognized(String),
    /// 其他失败(网络 / 根本没拿到 JSON……),明细只记结构不记值
    Other(String),
}

/// 按候选顺序抓一次逐包列表。全都不像包形状 → Err(带结构说明,只记形状不记值)。
///
/// `memo` 命中时先试赢过的那条,失败再回落全链 —— 每轮取数最多 3 个请求,
/// 命中后 1 个。
pub async fn fetch_packs(
    client: &reqwest::Client,
    session: &str,
    memo: &PacksMemo,
) -> Result<(Vec<CreditPack>, usize), PacksErr> {
    let cookie = cookie_of(session)
        .ok_or(PacksErr::Other("会话凭据不是 cookie 形态".into()))?
        .to_string();
    let mut order: Vec<usize> = Vec::with_capacity(CANDIDATE_PATHS.len());
    if let Some(w) = memo.winner() {
        if w < CANDIDATE_PATHS.len() {
            order.push(w);
        }
    }
    for i in 0..CANDIDATE_PATHS.len() {
        if !order.contains(&i) {
            order.push(i);
        }
    }
    let mut shapes: Vec<String> = Vec::new();
    let mut unauth_hits = 0usize;
    // 有没有哪条候选真的把会话认下来了(200 + JSON 且不是业务层拒绝)。
    // 这是"会话有效、只是结构没认出来"与"会话被拒"的唯一分界。
    let mut session_ok = false;
    for i in order {
        let url = format!("https://{}{}", weblogin::QODER_SITE_HOST, CANDIDATE_PATHS[i]);
        let resp = client
            .get(&url)
            .header("Cookie", &cookie)
            .header("Accept", "application/json")
            // 同源页面本身;有的网关按 Referer/UA 挑路由,与浏览器同源请求保持一致
            .header("Referer", "https://qoder.com/account/usage")
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
            )
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                shapes.push(format!("{}: 请求失败 {e}", CANDIDATE_PATHS[i]));
                continue;
            }
        };
        let st = resp.status();
        if !st.is_success() {
            // 401/403 = 平台明确拒了这份会话;body 里的 TOKEN_EXPIRE / Unauthorized
            // 也算(200 但业务层拒绝的形态见过)。全部候选都被拒才算"未登录":
            // 只要有一条是别的错,就按别的错报,别把网络问题说成登录问题。
            let code = st.as_u16();
            if code == 401 || code == 403 {
                unauth_hits += 1;
            }
            shapes.push(format!("{}: HTTP {st}", CANDIDATE_PATHS[i]));
            continue;
        }
        let body = match resp.json::<serde_json::Value>().await {
            Ok(v) => v,
            Err(e) => {
                shapes.push(format!("{}: 非 JSON({e})", CANDIDATE_PATHS[i]));
                continue;
            }
        };
        if body.get("code").and_then(|c| c.as_str()).map(|c| {
            c.eq_ignore_ascii_case("UNAUTHORIZED") || c.eq_ignore_ascii_case("TOKEN_EXPIRE")
                || c.eq_ignore_ascii_case("TOKEN_INVALID")
        }) == Some(true)
        {
            unauth_hits += 1;
            shapes.push(format!("{}: 业务层拒绝会话", CANDIDATE_PATHS[i]));
            continue;
        }
        // 走到这里 = 平台认了这份会话;后面的判断只关"结构",与登录无关。
        session_ok = true;
        match parse_packs(&body) {
            Some(packs) => {
                memo.remember(i);
                return Ok((packs, i));
            }
            None => {
                shapes.push(format!(
                    "{}: 响应里没有逐包数据 {}",
                    CANDIDATE_PATHS[i],
                    shape_sketch(&body)
                ));
            }
        }
    }
    if unauth_hits > 0 && unauth_hits == shapes.len() {
        return Err(PacksErr::Unauthorized);
    }
    if session_ok {
        // 会话是好的 —— 别让调用方把它当成"该去重新登录"。
        return Err(PacksErr::Unrecognized(shapes.join(" | ")));
    }
    Err(PacksErr::Other(format!(
        "候选端点都没给出逐包数据 · {}",
        shapes.join(" | ")
    )))
}

/// 上游时间的两种口径都见过:毫秒(本机实测 253402214400000)与秒。
/// 阈值取 10^12:秒级时间戳到那年也才 ~3.4×10^9,毫秒级至少 10^12 起 —— 分得开。
fn norm_ts(v: f64) -> i64 {
    if v > 1e12 {
        (v / 1000.0) as i64
    } else {
        v as i64
    }
}

/// 9999-12-31 的秒级哨兵(上游用它表示"不过期"),与 providers.rs 同一裁决。
const NEVER_TS: i64 = 253_402_214_400;

/// 形状自适应解析:只认"能验证的包形状"。
///
/// 认得的两种(都来自公开 bundle 的渲染代码):
/// ① `{summary:{…}, details:[{limitValue, remainingValue, expiresAt, name, …}]}`
///    (组织版 dedicated 包的响应,字段 camelCase;snake_case 一并认);
/// ② 任意数组字段(details/packs/items/list/packages/data),其元素**同时**有
///    "剩余量"键与"到期时刻"键 → 当作包列表。
/// 数字一律认 f64;`data` 信封(`{"code":…,"data":{…}}`)先剥一层。
///
/// ③ 兜底(2026-11-03):平台把明细藏在哪个层级都认 —— 在整棵树里找"成组的包"。
///    真机个人版 `/api/v2/me/usages/big_model_credits` 只回汇总,逐包数据在更深的
///    `plan_quota` / `resource_package_quota` 之类字段里;以前只认顶层与一层信封,
///    于是会话明明有效却天天报"没取到"。兜底只放宽**位置**,判据仍是
///    "剩余量 + 到期时刻同时在场"(见 [`pack_of`]),不放宽形状。
pub fn parse_packs(body: &serde_json::Value) -> Option<Vec<CreditPack>> {
    let obj = body.as_object()?;
    // 信封:顶层没有包形状、但有 data/data 字段且它是对象 → 剥一层再试
    let root = if obj.get("summary").is_none() && obj.get("details").is_none() {
        ["data", "Data"]
            .iter()
            .find_map(|k| obj.get(*k).and_then(|v| v.as_object()))
            .unwrap_or(obj)
    } else {
        obj
    };

    // ① summary + details 形状
    if let Some(details) = root
        .get("details")
        .or_else(|| root.get("Details"))
        .and_then(|v| v.as_array())
    {
        let packs = packs_from_array(details);
        if !packs.is_empty() {
            return Some(packs);
        }
    }

    // ② 任意像包列表的数组字段
    for key in ["packs", "items", "list", "packages", "records"] {
        if let Some(arr) = root.get(key).and_then(|v| v.as_array()) {
            let packs = packs_from_array(arr);
            if !packs.is_empty() {
                return Some(packs);
            }
        }
    }

    // ③ 兜底:整棵树找包(深度有上限,见 MAX_DEPTH)
    let mut found = Vec::new();
    collect_packs(body, MAX_DEPTH, &mut found);
    dedupe(&mut found);
    if found.is_empty() {
        None
    } else {
        found.sort_by_key(|p| p.expires_at);
        Some(found)
    }
}

/// 往下钻多少层。个人版汇总响应的层级不超过 3 层(`data.plan_quota.details[]`);
/// 留到 4 是为了容下"再包一层信封"的版本,同时不让一台指针乱飞的响应把 CPU 拖住。
const MAX_DEPTH: usize = 4;

/// 一次最多收多少个包。平台正常用户不会超过两位数;上限只防异常响应撑爆内存。
const MAX_PACKS: usize = 200;

/// 在整棵 JSON 树里收集"成组的包"。
///
/// 两条规则,都要求**成组**才认,避免把单个汇总对象(比如带账户级到期时刻的
/// `plan_quota`)误当成一个积分包:
/// · 数组:逐元素按 [`pack_of`] 判(这是组织版 `details[]` 的确证形状,允许多种来源混排);
/// · 对象:只有当**每个**值都是包形状对象时,才整体当成"按来源分组的包映射"
///   (`{"BONUS":{…}, "PURCHASED":{…}}`),否则继续往下钻。
fn collect_packs(v: &serde_json::Value, depth: usize, out: &mut Vec<CreditPack>) {
    if depth == 0 || out.len() >= MAX_PACKS {
        return;
    }
    match v {
        serde_json::Value::Array(arr) => {
            let mut any_pack = false;
            for item in arr {
                if let Some(p) = pack_of(item) {
                    out.push(p);
                    any_pack = true;
                    if out.len() >= MAX_PACKS {
                        return;
                    }
                }
            }
            if !any_pack {
                for item in arr {
                    collect_packs(item, depth - 1, out);
                }
            }
        }
        serde_json::Value::Object(map) => {
            if !map.is_empty() && map.values().all(|c| c.is_object() && looks_like_pack(c)) {
                for child in map.values() {
                    if let Some(p) = pack_of(child) {
                        out.push(p);
                        if out.len() >= MAX_PACKS {
                            return;
                        }
                    }
                }
                return;
            }
            for child in map.values() {
                collect_packs(child, depth - 1, out);
            }
        }
        _ => {}
    }
}

/// 同一个包可能被两条路径摸到(信封 + 原树);按 (来源, 余量, 到期) 去重。
fn dedupe(packs: &mut Vec<CreditPack>) {
    let mut seen: Vec<(String, u64, i64)> = Vec::with_capacity(packs.len());
    packs.retain(|p| {
        let key = (p.label.clone(), p.remaining.to_bits(), p.expires_at);
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
}

/// 给诊断日志用的**结构草图**:键名 + 值类型,不含任何值。
///
/// 追踪"平台换结构"时,只有键名和层级有用;值(余额、账号id、会话)一律不进日志。
/// 键名超过 31 字符会被截断:诊断日志的兜底脱敏会把「≥32 字符的连续串」当成令牌
/// 掩成 `«secret len=…»`,而这里最想看清的恰恰是那种长键名
/// (真机有个 32 字符的顶层键,正好被它吃掉)。截断一位就能保住线索。
pub fn shape_sketch(v: &serde_json::Value) -> String {
    fn walk(v: &serde_json::Value, depth: usize, out: &mut String) {
        match v {
            serde_json::Value::Object(map) => {
                out.push('{');
                for (i, (k, child)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&sketch_key(k));
                    out.push(':');
                    if depth == 0 {
                        out.push('…');
                    } else {
                        walk(child, depth - 1, out);
                    }
                }
                out.push('}');
            }
            serde_json::Value::Array(arr) => {
                out.push('[');
                match arr.first() {
                    None => out.push_str("空"),
                    Some(first) => {
                        out.push_str(&arr.len().to_string());
                        out.push(':');
                        if depth == 0 {
                            out.push('…');
                        } else {
                            walk(first, depth - 1, out);
                        }
                    }
                }
                out.push(']');
            }
            serde_json::Value::String(_) => out.push_str("str"),
            serde_json::Value::Number(_) => out.push_str("num"),
            serde_json::Value::Bool(_) => out.push_str("bool"),
            serde_json::Value::Null => out.push_str("null"),
        }
    }
    let mut out = String::new();
    walk(v, 4, &mut out);
    // 一条日志行别太长:结构再深也只留前 600 字符(截断处标出来,不假装完整)
    if out.chars().count() > 600 {
        out = out.chars().take(600).collect::<String>() + "…";
    }
    out
}

/// 结构草图里的键名:截到 31 字符(理由见 [`shape_sketch`] 的说明)。
fn sketch_key(k: &str) -> String {
    let mut s: String = k.chars().take(31).collect();
    if k.chars().count() > 31 {
        s.push('…');
    }
    s
}

/// 数字字段的别名表:camelCase(本机实测形态)与 snake_case(社区记录形态)都认。
fn num_of(v: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| {
        v.get(*k).and_then(|x| {
            x.as_f64()
                .or_else(|| x.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
        })
    })
}

fn str_of<'a>(v: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| {
        v.get(*k).and_then(|x| x.as_str()).filter(|s| {
            !s.trim().is_empty() && s.chars().all(|c| !c.is_control())
        })
    })
}

/// "剩余量"与"到期时刻"的字段别名表:camelCase(本机实测形态)与 snake_case 都认。
const REMAINING_KEYS: [&str; 3] = ["remainingValue", "remaining_value", "remaining"];
const EXPIRES_KEYS: [&str; 5] = ["expiresAt", "expires_at", "expireAt", "expire_at", "expirationAt"];

/// 一条记录算不算"包":**同时**有剩余量与到期时刻,才允许进列表。
/// 只认能验证的字段 —— 半个形状都不凑合,凑出来的"到期日"比没有更害人。
///
/// 注意它比 [`looks_like_pack`] 更严:除了"字段在",还要求值能当包用
/// (剩余 > 0、到期日是真实时刻)。判"这块数据是不是积分包集合"用那个宽松的,
/// 取数据用这个严格的 —— 否则一个"永久包"就会让整组被判成"不是包"。
fn pack_of(item: &serde_json::Value) -> Option<CreditPack> {
    let remaining = num_of(item, &REMAINING_KEYS)?;
    let at = norm_ts(num_of(item, &EXPIRES_KEYS)?);
    // 不过期哨兵 / 时刻异常(<=0):这类包没有"哪天过期"可言,不进列表。
    // 也不能因此把整个列表判为不像 —— 平台经常混发"有效期内"与"永久"两种包。
    if at <= 0 || at >= NEVER_TS {
        return None;
    }
    // 剩余 0 的包没有"还剩多少要过期"可言
    if remaining <= 0.0 {
        return None;
    }
    let label = str_of(item, &["name", "source", "title", "product_name"])
        .map(|s| s.to_string())
        .unwrap_or_else(|| "积分包".into());
    Some(CreditPack {
        label,
        remaining,
        expires_at: at,
    })
}

/// 字段形状像包即可,不看值 —— 用来判"这一组的每个元素是不是同类记录"。
fn looks_like_pack(item: &serde_json::Value) -> bool {
    num_of(item, &REMAINING_KEYS).is_some() && num_of(item, &EXPIRES_KEYS).is_some()
}

fn packs_from_array(arr: &[serde_json::Value]) -> Vec<CreditPack> {
    let mut out: Vec<CreditPack> = arr.iter().filter_map(pack_of).collect();
    // 到期早的排前面 —— 明细卡"最近 3 笔"吃的就是这个顺序
    out.sort_by_key(|p| p.expires_at);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 组织版 dedicated 包的确证形状(camelCase):details 逐包带 expiresAt。
    #[test]
    fn parses_summary_details_shape() {
        let body = json!({
            "summary": {"limitValue": 5000.0, "usedValue": 200.0, "remainingValue": 4800.0, "usagePercentage": 4.0, "unit": "credits"},
            "details": [
                {"id": "p1", "name": "Purchased Credits", "limitValue": 3000.0, "usedValue": 100.0,
                 "remainingValue": 2900.0, "unit": "credits", "isActive": true,
                 "expiresAt": 1796000000000i64, "status": "ACTIVE"},
                {"id": "p2", "name": "Bonus", "remainingValue": 0.0,
                 "expiresAt": 1797000000000i64, "status": "ACTIVE"}
            ]
        });
        let packs = parse_packs(&body).expect("应认出包列表");
        assert_eq!(packs.len(), 1, "剩余 0 的包不进列表");
        assert_eq!(packs[0].label, "Purchased Credits");
        assert_eq!(packs[0].remaining, 2900.0);
        assert_eq!(packs[0].expires_at, 1_796_000_000, "毫秒归一到秒");
    }

    /// snake_case 变体与 data 信封都认;两条按到期日升序。
    #[test]
    fn parses_snake_and_envelope() {
        let body = json!({"code": 0, "data": {
            "details": [
                {"remaining_value": 100.0, "expires_at": 1797000000i64, "source": "BONUS"},
                {"remaining_value": 50.5, "expires_at": 1795000000i64, "source": "PURCHASED"}
            ]
        }});
        let packs = parse_packs(&body).expect("信封 + snake 也该认出");
        assert_eq!(packs.len(), 2);
        assert_eq!(packs[0].expires_at, 1_795_000_000, "升序");
        assert_eq!(packs[0].label, "PURCHASED");
    }

    /// 不过期哨兵(9999)与过去的时刻不进列表;但列表本身仍算认出 ——
    /// 平台混发"有效期内"与"永久"两种包是常态。
    #[test]
    fn filters_never_and_expired() {
        let body = json!({"details": [
            {"remainingValue": 500.0, "expiresAt": 253402214400000i64},
            {"remainingValue": 300.0, "expiresAt": 1000000000i64},
            {"remainingValue": 200.0, "expiresAt": 1795000000000i64}
        ]});
        let packs = parse_packs(&body).expect("有真实包就该认出");
        assert_eq!(packs.len(), 2, "9999 哨兵滤掉;过去时刻的包保留(契约含已过期)");
        assert_eq!(packs[0].remaining, 300.0, "按到期日升序,过去的排前面");
        assert_eq!(packs[1].remaining, 200.0);
    }

    /// 不像包的响应一律 None —— 绝不把用量流水或计划信息硬掰成"包"。
    #[test]
    fn rejects_non_pack_shapes() {
        assert_eq!(parse_packs(&json!({"error": "unauthorized"})), None);
        // 只有用量、没有到期时刻的数组不算包
        assert_eq!(parse_packs(&json!({"details": [{"usedValue": 1.0}]})), None);
        assert_eq!(parse_packs(&json!([1, 2, 3])), None);
        assert_eq!(parse_packs(&serde_json::Value::Null), None);
    }

    /// 任意数组字段的兜底形状:元素同时有剩余量与到期时刻才认。
    #[test]
    fn accepts_generic_array_shape() {
        let body = json!({"packages": [
            {"title": "双 11 加量包", "remaining": 800.0, "expire_at": 1798000000000i64}
        ]});
        let packs = parse_packs(&body).expect("泛化形状该认出");
        assert_eq!(packs[0].label, "双 11 加量包");
        assert_eq!(packs[0].remaining, 800.0);
    }

    /// 2026-11-03 真机:个人版 `/api/v2/me/usages/big_model_credits` 只回汇总,
    /// 逐包明细埋在更深的层级里。这部分数据必须照样找得到 —— 否则"登录成功、
    /// 却天天取不到明细"。
    #[test]
    fn finds_packs_nested_below_summary() {
        let body = json!({
            "user_id": "u-1",
            "quota_key": "big_model_credits",
            "status": "ACTIVE",
            // 汇总对象:带 remaining 但**没有**到期时刻 → 不许被当成一个包
            "plan_quota": {"limit_value": 5000.0, "used_value": 1200.0, "remaining_value": 3800.0},
            "resource_package_quota": {
                "limit_value": 1000.0,
                "remaining_value": 700.0,
                "details": [
                    {"source": "PURCHASED", "remaining_value": 500.0, "expires_at": 1797000000000i64},
                    {"source": "BONUS", "remaining_value": 200.0, "expires_at": 1796000000000i64}
                ]
            },
            "total_quota": {"limit_value": 6000.0, "remaining_value": 4500.0},
            "lastResetAt": "2026-11-01T00:00:00Z",
            "nextResetAt": "2026-12-01T00:00:00Z"
        });
        let packs = parse_packs(&body).expect("深层的逐包数据该找到");
        assert_eq!(packs.len(), 2, "汇总对象不算包:{packs:?}");
        assert_eq!(packs[0].expires_at, 1_796_000_000, "按到期日升序");
        assert_eq!(packs[0].remaining, 200.0);
    }

    /// 平台把包按来源放成对象映射(`{"BONUS":{…}, "PURCHASED":{…}}`)时也要认;
    /// 但只要有一个值不像包,就整组不算包(继续往下钻),免得把别的东西凑成包。
    #[test]
    fn accepts_homogeneous_pack_map_only() {
        let homogeneous = json!({
            "resource_package_quota": {
                "BONUS": {"remaining_value": 30.0, "expires_at": 1797000000000i64},
                "PURCHASED": {"remaining_value": 90.0, "expires_at": 1798000000000i64}
            }
        });
        let packs = parse_packs(&homogeneous).expect("同构映射该认出");
        assert_eq!(packs.len(), 2);
        assert_eq!(packs[0].remaining, 30.0);

        // 混着别的字段 → 整组不算"按来源分组的包",里面也没有可钻的包 → None
        let mixed = json!({
            "resource_package_quota": {
                "BONUS": {"remaining_value": 30.0, "expires_at": 1797000000000i64},
                "unit": "credits"
            }
        });
        assert_eq!(parse_packs(&mixed), None, "混排的映射不凑合");
    }

    /// 永久包(9999 哨兵)与剩余 0 的包不进列表,但不能因此把同组其他包也丢掉。
    #[test]
    fn keeps_real_packs_next_to_permanent_ones() {
        let body = json!({"resource_package_quota": {"details": [
            {"source": "BONUS", "remaining_value": 100.0, "expires_at": 253402214400000i64},
            {"source": "PURCHASED", "remaining_value": 0.0, "expires_at": 1797000000000i64},
            {"source": "TRIAL", "remaining_value": 42.0, "expires_at": 1797500000000i64}
        ]}});
        let packs = parse_packs(&body).expect("同组里有真包就该认出");
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].remaining, 42.0);
    }

    /// 结构草图:只报键名与类型,绝不带值;长键名截到 31 字符(诊断日志的兜底
    /// 脱敏会把 ≥32 字符的连续串当成令牌掩掉,那正好会把要看的键名吃掉)。
    #[test]
    fn shape_sketch_hides_values_and_keeps_keys() {
        let body = json!({
            "user_id": "u-should-not-appear",
            "plan_quota": {"remaining_value": 3800.0},
            "resource_package_quota_details_list": [1, 2],
            "nested": [[{"a": null}]]
        });
        let s = shape_sketch(&body);
        assert!(!s.contains("u-should-not-appear"), "值不许进草图:{s}");
        assert!(!s.contains("3800"), "数值不许进草图:{s}");
        assert!(s.contains("user_id:str"), "{s}");
        assert!(s.contains("plan_quota:{remaining_value:num}"), "{s}");
        // 35 字符的键名截到 31 位 → 不再是"≥32 字符的令牌形态"
        assert!(s.contains("resource_package_quota_details_…"), "{s}");
        assert!(s.contains("nested:[1:[1:{a:null}]]"), "{s}");
    }
}
