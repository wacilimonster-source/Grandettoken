//! 取数与视图组装。
//!
//! 核心原则:**失败不降级成 0**。取数失败时继续显示上次成功快照并标注时间,
//! 因为余额类工具最危险的 bug 就是把请求失败渲染成"余额归零",用户会以为欠费了。

use crate::providers::{self, FetchResult, Kind};
use crate::store::Store;
use crate::secrets;
use chrono::{Datelike, Duration, Local, TimeZone, Weekday};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 传给界面的一行。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelView {
    pub id: String,
    pub name: String,
    pub short: String,
    pub color: String,
    pub kind: Kind,
    /// 未公开接口,失败时应静默降级而非报警
    pub unstable: bool,
    pub has_key: bool,
    pub valid: bool,
    pub remaining: Option<f64>,
    pub used: Option<f64>,
    pub total: Option<f64>,
    pub unit: String,
    pub windows: Vec<providers::Window>,
    pub extra: Vec<(String, String)>,
    pub error: Option<String>,
    pub limited: bool,
    /// 今日 / 本周 / 本月消耗。None 表示数据窗口有空洞,不能报 0
    pub day: Option<f64>,
    pub week: Option<f64>,
    pub month: Option<f64>,
    /// 消耗是否为本地快照推算值(充值型渠道接口不给累计消耗)
    pub estimated: bool,
    /// 本次取数失败,以上数值来自上次成功快照
    pub stale: bool,
    pub updated_at: i64,
}

pub fn now_ts() -> i64 {
    Local::now().timestamp()
}

/// 本地零点。按用户本地时区切,不按 UTC —— 否则跨时区时"今日消耗"会指错日子。
fn local_date_start(y: i32, m: u32, d: u32) -> i64 {
    Local
        .with_ymd_and_hms(y, m, d, 0, 0, 0)
        .single()
        .map(|x| x.timestamp())
        .unwrap_or_else(now_ts)
}

pub fn local_midnight_today() -> i64 {
    let n = Local::now();
    local_date_start(n.year(), n.month(), n.day())
}

pub fn local_week_start() -> i64 {
    let n = Local::now();
    let back = match n.weekday() {
        Weekday::Mon => 0,
        Weekday::Tue => 1,
        Weekday::Wed => 2,
        Weekday::Thu => 3,
        Weekday::Fri => 4,
        Weekday::Sat => 5,
        Weekday::Sun => 6,
    };
    let d = n.date_naive() - Duration::days(back);
    local_date_start(d.year(), d.month(), d.day())
}

pub fn local_month_start() -> i64 {
    let n = Local::now();
    local_date_start(n.year(), n.month(), 1)
}

pub async fn fetch_channel(
    client: &reqwest::Client,
    def: &providers::ProviderDef,
    key: &str,
) -> FetchResult {
    let mut req = client
        .get(def.url)
        .header("Authorization", format!("Bearer {}", key.trim()))
        .header("Accept", "application/json");

    for (k, v) in def.extra_headers {
        req = req.header(*k, *v);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = if e.is_timeout() {
                "请求超时".to_string()
            } else if e.is_connect() {
                "无法连接(检查网络或代理)".to_string()
            } else {
                format!("请求失败: {e}")
            };
            return FetchResult {
                valid: false,
                error: Some(msg),
                ..Default::default()
            };
        }
    };

    let status = resp.status();
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => {
            return FetchResult {
                valid: false,
                error: Some(format!("响应不是合法 JSON (HTTP {})", status.as_u16())),
                ..Default::default()
            }
        }
    };

    let mut result = (def.extract)(&body);

    // 非 2xx 却解析成功,说明接口用 HTTP 状态码表达业务错误 —— 以状态码为准。
    // OpenCode 的 401/403 带 type:error 体,已被 extractor 处理,不覆盖它的文案。
    if !status.is_success() && result.valid {
        result.valid = false;
        result.error = Some(format!("HTTP {}", status.as_u16()));
    }

    result
}

fn build_view(
    def: &providers::ProviderDef,
    result: FetchResult,
    stale: bool,
    updated_at: i64,
    store: &Store,
) -> ChannelView {
    // 只有金额型才有"消耗"这个概念可算;配额型的用量由接口直接给出
    let (day, week, month) = if result.valid && result.kind == Kind::Amount {
        let now = now_ts();
        (
            store
                .consumption_since(def.id, local_midnight_today(), now)
                .ok()
                .flatten(),
            store
                .consumption_since(def.id, local_week_start(), now)
                .ok()
                .flatten(),
            store
                .consumption_since(def.id, local_month_start(), now)
                .ok()
                .flatten(),
        )
    } else {
        (None, None, None)
    };

    ChannelView {
        id: def.id.into(),
        name: def.name.into(),
        short: def.short.into(),
        color: def.color.into(),
        kind: result.kind,
        unstable: def.unstable,
        has_key: secrets::exists(def.id),
        valid: result.valid,
        remaining: result.remaining,
        used: result.used,
        total: result.total,
        unit: result.unit,
        windows: result.windows,
        extra: result.extra,
        error: result.error,
        limited: result.limited,
        day,
        week,
        month,
        estimated: result.kind == Kind::Amount,
        stale,
        updated_at,
    }
}

/// 未配置密钥的渠道占位。这不是故障,只是没启用。
fn placeholder(def: &providers::ProviderDef) -> ChannelView {
    ChannelView {
        id: def.id.into(),
        name: def.name.into(),
        short: def.short.into(),
        color: def.color.into(),
        kind: Kind::Amount,
        unstable: def.unstable,
        has_key: false,
        valid: false,
        remaining: None,
        used: None,
        total: None,
        unit: "CNY".into(),
        windows: vec![],
        extra: vec![],
        error: None,
        limited: false,
        day: None,
        week: None,
        month: None,
        estimated: false,
        stale: false,
        updated_at: 0,
    }
}

/// 取数器。持有 HTTP 客户端和上次成功结果的缓存,失败时用于降级显示。
pub struct Fetcher {
    pub http: reqwest::Client,
    cache: Mutex<HashMap<String, (FetchResult, i64)>>,
}

impl Default for Fetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .user_agent("TokenScope/0.1")
                .build()
                .expect("HTTP 客户端初始化失败"),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// 取全部渠道。
    ///
    /// 结构上刻意分成两段:**await 段**只做网络请求,**同步段**才锁库读写。
    /// 因为 `std::sync::MutexGuard` 不是 Send,一旦跨越 await 就会让整个
    /// future 失去 Send,而 Tauri 的命令要求 future 是 Send。
    pub async fn fetch_all(&self, store: &Arc<Mutex<Store>>) -> Vec<ChannelView> {
        let n = providers::PROVIDERS.len();
        let mut out: Vec<Option<ChannelView>> = (0..n).map(|_| None).collect();
        let mut pending: Vec<(usize, &'static providers::ProviderDef, FetchResult, bool, i64)> =
            Vec::new();
        let mut successful: Vec<(&'static str, FetchResult, i64)> = Vec::new();

        // ── await 段:纯网络,不碰数据库 ──
        for (i, def) in providers::PROVIDERS.iter().enumerate() {
            let Some(key) = secrets::get(def.id) else {
                out[i] = Some(placeholder(def));
                continue;
            };

            let result = fetch_channel(&self.http, def, &key).await;
            let ts = now_ts();

            if result.valid {
                successful.push((def.id, result.clone(), ts));
                pending.push((i, def, result, false, ts));
            } else {
                // 失败降级:沿用上次成功值,标注时间,绝不显示成 0
                let cached = self.cache.lock().ok().and_then(|c| c.get(def.id).cloned());
                match cached {
                    Some((mut prev, prev_ts)) => {
                        prev.error = result.error;
                        prev.valid = false;
                        pending.push((i, def, prev, true, prev_ts));
                    }
                    None => pending.push((i, def, result, false, ts)),
                }
            }
        }

        // ── 同步段:一次性锁库完成快照写入与消耗推算,期间不跨 await ──
        {
            let s = store.lock().unwrap();
            for (id, result, ts) in &successful {
                let kind = match result.kind {
                    Kind::Amount => "amount",
                    Kind::Percent => "percent",
                };
                let _ = s.record(id, *ts, result.remaining, result.used, result.total, kind);
            }
            for (i, def, result, stale, ts) in pending {
                out[i] = Some(build_view(def, result, stale, ts, &s));
            }
        }

        for (id, result, ts) in successful {
            if let Ok(mut c) = self.cache.lock() {
                c.insert(id.to_string(), (result, ts));
            }
        }

        out.into_iter().flatten().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn month_start_is_first_day() {
        let ts = local_month_start();
        let d = Local.timestamp_opt(ts, 0).unwrap();
        assert_eq!(d.day(), 1);
        assert_eq!(d.hour(), 0);
        assert_eq!(d.minute(), 0);
    }

    #[test]
    fn week_start_is_monday() {
        let ts = local_week_start();
        let d = Local.timestamp_opt(ts, 0).unwrap();
        assert_eq!(d.weekday(), Weekday::Mon);
        assert_eq!(d.hour(), 0);
    }

    #[test]
    fn today_start_is_before_now_and_same_day() {
        let ts = local_midnight_today();
        let now = Local::now();
        assert!(ts <= now.timestamp());
        let d = Local.timestamp_opt(ts, 0).unwrap();
        assert_eq!(d.day(), now.day());
        assert_eq!(d.hour(), 0);
    }

    #[test]
    fn week_start_not_after_today_start() {
        assert!(local_week_start() <= local_midnight_today());
    }
}
