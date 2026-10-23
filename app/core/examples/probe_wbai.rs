//! WorkBuddy AI(国际版)真机探针 —— **只读**,不写任何东西。
//!
//! 目的:回答产品设计(design-workbuddy-ai-channel-2026-10-23.html)里三个"待取证"的问题:
//!   ① 国际版的计费接口是不是同一套结构(`get-user-resource` 的 Accounts 数组)?
//!   ② 有没有"发放记录"类接口?有的话"今日是否已获取"就不用再靠余额增量推算;
//!   ③ 签到接口在国际版是不是真的 10001「活动未开启」?
//!
//! 纪律(与 `diaglog` 同一条):**令牌只记长度与前 8 位**,响应里的账号/余额照实记 ——
//! 那是本机用户自己的数据,报告是给他看的。
//!
//! 用法:
//!   cd app/core && cargo run --example probe_wbai
//! 需要本机装着国际版桌面端并登录过(凭据在 `…/CodeBuddyExtension/Data/Public/auth`)。
//! 出网走系统代理时带上 `HTTPS_PROXY=127.0.0.1:7897`。

use std::time::Duration;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// 把响应体压成"结构摘要":键名 + 数组长度 + 标量值(数字/短字符串)。
/// 长字符串只报长度 —— 那大概率是令牌或签名,不该进报告。
fn shape(v: &serde_json::Value, depth: usize) -> String {
    if depth > 4 {
        return "…".into();
    }
    match v {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => {
            if s.len() > 40 {
                format!("<str {} chars>", s.len())
            } else {
                format!("{:?}", s)
            }
        }
        serde_json::Value::Array(a) => {
            let inner: Vec<String> = a.iter().take(3).map(|x| shape(x, depth + 1)).collect();
            format!("[{} 项: {}]", a.len(), inner.join(" , "))
        }
        serde_json::Value::Object(m) => {
            let inner: Vec<String> = m
                .iter()
                .take(60)
                .map(|(k, x)| format!("{}={}", k, shape(x, depth + 1)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// 打一个 GET/POST,只回报状态与结构摘要(响应体本身也打印,但截断)。
async fn hit(client: &reqwest::Client, method: &str, url: &str, token: &str, body: Option<serde_json::Value>) {
    let mut req = match method {
        "GET" => client.get(url),
        _ => client.post(url),
    };
    req = req
        .bearer_auth(token)
        .header("Accept", "application/json")
        .header("X-Client-Platform", "web")
        .header("Origin", "https://www.workbuddy.ai")
        .header("Referer", "https://www.workbuddy.ai/profile/plans-usage");
    if let Some(b) = body {
        req = req.json(&b);
    }
    match req.send().await {
        Ok(r) => {
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            let brief = text.chars().take(220).collect::<String>();
            let parsed = serde_json::from_str::<serde_json::Value>(&text).ok();
            println!("  {method} {url}\n    HTTP {} · {} 字节", status.as_u16(), text.len());
            if let Some(j) = parsed {
                println!("    结构: {}", shape(&j, 0));
            } else {
                println!("    非 JSON: {}", brief);
            }
        }
        Err(e) => println!("  {method} {url}\n    请求失败: {e}"),
    }
}

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(probe());
}

async fn probe() {
    // 探针输出单独落一个目录,不污染应用的诊断日志
    tokenscope_core::diaglog::set_dir(std::path::PathBuf::from("target/probe-diag"));

    println!("── 1. 本机凭据(只报区域与形态,不打印令牌)──");
    let toks = tokenscope_core::workbuddy::desktop_tokens_region(now_ms(), Some(true));
    if toks.is_empty() {
        println!("没读到国际版凭据 —— 装过并登录过国际版桌面端再跑");
        return;
    }
    for (i, t) in toks.iter().enumerate() {
        println!(
            "  第 {} 个令牌:len={} head={}…",
            i + 1,
            t.len(),
            t.chars().take(8).collect::<String>()
        );
    }
    let token = toks[0].clone();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap_or_default();

    let body = tokenscope_core::workbuddy::billing_body(chrono::Local::now());

    println!("\n── 2. 计费接口(与国内版同路径,换域)──");
    let bases = ["https://www.workbuddy.ai", "https://www.codebuddy.ai"];
    match tokenscope_core::workbuddy::fetch_billing(&client, &tokenscope_core::workbuddy::FetchAuth::Bearer(token.clone()), &body, bases).await {
        Ok((url, text)) => {
            println!("  成功:{url}");
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(j) => println!("  结构:{}", shape(&j, 0)),
                Err(_) => println!("  非 JSON,前 220 字:{}", text.chars().take(220).collect::<String>()),
            }
        }
        Err(e) => println!("  失败:{e}"),
    }

    println!("
── 2b. Accounts 逐笔字段(区分充值 / 活跃奖励的关键)──");
    {
        let url = "https://www.workbuddy.ai/v2/billing/meter/get-user-resource";
        if let Ok(r) = client.post(url).bearer_auth(&token).json(&body).send().await {
            let text = r.text().await.unwrap_or_default();
            if let Ok(j) = serde_json::from_str::<serde_json::Value>(&text) {
                let accs = j.pointer("/data/Response/Data/Accounts")
                    .and_then(|a| a.as_array()).cloned().unwrap_or_default();
                println!("  Accounts {} 笔,逐笔全部字段(长字符串只报长度):", accs.len());
                for (i, a) in accs.iter().enumerate() {
                    println!("  [{}] {}", i, shape(a, 0));
                }
            }
        }
    }

    println!("
── 2c. 每笔额度/用量横向对比(看是不是每笔都 30 分)──");
    {
        let url = "https://www.workbuddy.ai/v2/billing/meter/get-user-resource";
        if let Ok(r) = client.post(url).bearer_auth(&token).json(&body).send().await {
            let text = r.text().await.unwrap_or_default();
            if let Ok(j) = serde_json::from_str::<serde_json::Value>(&text) {
                let accs = j.pointer("/data/Response/Data/Accounts").and_then(|a| a.as_array()).cloned().unwrap_or_default();
                for (i, a) in accs.iter().enumerate() {
                    let pick = |k: &str| a.get(k).cloned().unwrap_or(serde_json::Value::Null);
                    let ct = pick("CreateTime").as_i64().unwrap_or(0);
                    let day = chrono::DateTime::from_timestamp_millis(ct).map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
                    println!("  [{}] {} · FeeType={} · CapacityType={} · Unit={} · CycleSize={} · Remain={} · Slice={}",
                        i, day,
                        pick("FeeType"), pick("CapacityType"), pick("CapacityUnit"),
                        pick("CycleCapacitySizePrecise"), pick("CycleCapacityRemainPrecise"),
                        pick("SlicePeriodUsageDetails"));
                }
            }
        }
    }

    println!("
── 2d. Status / Expiring 字段(决定老包过期后还能不能查到)──");
    {
        let url = "https://www.workbuddy.ai/v2/billing/meter/get-user-resource";
        if let Ok(r) = client.post(url).bearer_auth(&token).json(&body).send().await {
            let text = r.text().await.unwrap_or_default();
            if let Ok(j) = serde_json::from_str::<serde_json::Value>(&text) {
                let accs = j.pointer("/data/Response/Data/Accounts").and_then(|a| a.as_array()).cloned().unwrap_or_default();
                for (i, a) in accs.iter().enumerate() {
                    let pick = |k: &str| a.get(k).cloned().unwrap_or(serde_json::Value::Null);
                    println!("  [{}] Status={} · Create={} · CycleStart={} · CycleEnd={} · DeductEnd={} · DealName={}",
                        i, pick("Status"), pick("CreateTime"), pick("CycleStartTime"), pick("CycleEndTime"), pick("DeductionEndTime"),
                        pick("DealName").as_str().map(|s| if s.chars().count() > 18 { s.chars().take(18).collect::<String>() } else { s.to_string() }).unwrap_or_default());
                }
            }
        }
    }

    println!("\n── 3. 试探:有没有「发放记录」类接口(这些路径都是猜的)──");
    for p in [
        "/v2/billing/meter/credit-records",
        "/v2/billing/meter/credit-history",
        "/v2/billing/meter/get-credit-records",
        "/v2/billing/meter/credit-log",
        "/v2/billing/meter/get-credit-detail",
        "/v2/billing/meter/checkin-activity-status",
    ] {
        hit(&client, "POST", &format!("https://www.workbuddy.ai{p}"), &token, Some(serde_json::json!({}))).await;
    }

    println!("\n── 4. 试探:同一份 token 打国区域名(预期 401,验证分区路由是对的)──");
    hit(&client, "POST", "https://www.codebuddy.cn/v2/billing/meter/get-user-resource", &token, Some(body)).await;

    println!("\n完。响应里的结构摘要已按「长字符串只报长度」处理。");
}
