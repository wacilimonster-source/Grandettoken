//! 本地网关 SQLite 使用台账（只记脱敏元数据，绝不存 Prompt / 响应正文 / 密钥）。

use crate::models::{price_of, ModelCache, Price};
use crate::providers::four_s_api_hosts;
use crate::store::Store;
use chrono::{Local, TimeZone};
use rusqlite::params;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// 台账明细保留天数（与 snapshots 90 天口径对齐）。
pub const GATEWAY_LEDGER_KEEP_DAYS: i64 = 90;

#[derive(Debug, Clone)]
pub struct GatewayUsageRow {
    pub ts: i64,
    pub channel_id: String,
    pub channel_alias: String,
    pub route_group: Option<String>,
    pub model: String,
    pub protocol: String,
    pub stream: bool,
    pub status: u16,
    pub latency_ms: u64,
    pub first_byte_ms: Option<u64>,
    pub prompt_tokens: Option<u64>,
    pub cached_prompt_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub failover_count: u8,
    pub masked_count: usize,
    pub estimated_cost_cny: Option<f64>,
    pub error_code: Option<String>,
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GatewayUsageByKey {
    pub key: String,
    pub label: String,
    pub requests: u64,
    pub errors: u64,
    pub prompt_tokens: u64,
    pub cached_prompt_tokens: u64,
    pub cache_creation_tokens: u64,
    pub completion_tokens: u64,
    pub reasoning_tokens: u64,
    pub estimated_cost_cny: Option<f64>,
    pub priced_requests: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayUsageRecent {
    pub id: i64,
    pub ts: i64,
    pub channel_id: String,
    pub channel_alias: String,
    pub route_group: Option<String>,
    pub model: String,
    pub protocol: String,
    pub stream: bool,
    pub status: u16,
    pub latency_ms: u64,
    pub first_byte_ms: Option<u64>,
    pub prompt_tokens: Option<u64>,
    pub cached_prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub failover_count: u8,
    pub masked_count: usize,
    pub estimated_cost_cny: Option<f64>,
    pub error_code: Option<String>,
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GatewayUsageSummary {
    pub since_ts: i64,
    pub total_requests: u64,
    pub ok_requests: u64,
    pub error_requests: u64,
    pub failover_requests: u64,
    pub prompt_tokens: u64,
    pub cached_prompt_tokens: u64,
    pub cache_creation_tokens: u64,
    pub completion_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_hit_rate: Option<f64>,
    pub ttft_p50_ms: Option<u64>,
    pub ttft_p95_ms: Option<u64>,
    pub estimated_cost_cny: Option<f64>,
    pub priced_requests: u64,
    pub by_channel: Vec<GatewayUsageByKey>,
    pub by_model: Vec<GatewayUsageByKey>,
    pub recent: Vec<GatewayUsageRecent>,
}

fn day_string_of(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

fn percentile_sorted(sorted: &[u64], pct_num: usize, pct_den: usize) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((sorted.len() - 1) * pct_num) / pct_den;
    sorted.get(idx).copied()
}

/// 仅当渠道指向已知价目表来源（当前为 4SAPI）且能查到确定单价时估算人民币费用；
/// 未知单价一律返回 None，绝不编造 0 元。
pub fn estimate_channel_cost_cny(
    store: &Store,
    template: &str,
    base_url: &str,
    upstream_model: &str,
    prompt_tokens: Option<u64>,
    cached_prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
) -> Option<f64> {
    if !uses_4sapi_pricing(template, base_url) {
        return None;
    }
    PricingLookup::load(store)?.estimate(upstream_model, prompt_tokens, cached_prompt_tokens, completion_tokens)
}

pub(crate) fn uses_4sapi_pricing(template: &str, base_url: &str) -> bool {
    if template.eq_ignore_ascii_case("4sapi") { return true; }
    let lower_url = base_url.to_ascii_lowercase();
    four_s_api_hosts().iter().any(|h| lower_url.contains(&h.to_ascii_lowercase()))
}

pub(crate) struct PricingLookup(HashMap<String, (Price, f64)>);

impl PricingLookup {
    pub(crate) fn load(store: &Store) -> Option<Self> {
        let cat = ModelCache::load(store)?.catalog().ok()?;
        let group = cat.default_group();
        let mut models = HashMap::with_capacity(cat.rows.len());
        for row in &cat.rows {
            models.entry(row.id.clone()).or_insert_with(|| {
                (price_of(row, &cat.groups, &group), row.cache_ratio.unwrap_or(1.0))
            });
        }
        Some(Self(models))
    }

    pub(crate) fn estimate(
        &self, upstream_model: &str, prompt_tokens: Option<u64>,
        cached_prompt_tokens: Option<u64>, completion_tokens: Option<u64>,
    ) -> Option<f64> {
    let (price, cache_mult) = self.0.get(upstream_model)?;
    if let Some(per_call) = price.per_call {
        return Some((per_call * 1_000_000.0).round() / 1_000_000.0);
    }
    let input_price = price.input?;
    let output_price = price.output?;
    let p = prompt_tokens?;
    let c = completion_tokens?;
    let cached = cached_prompt_tokens.unwrap_or(0).min(p);
    let uncached = p - cached;
    let cost = ((uncached as f64) * input_price
        + (cached as f64) * input_price * cache_mult
        + (c as f64) * output_price)
        / 1_000_000.0;
    Some((cost * 1_000_000.0).round() / 1_000_000.0)
    }
}

impl Store {
    pub(crate) fn migrate_gateway_ledger(&self) -> Result<(), String> {
        self.conn()
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS gateway_usage_log (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts INTEGER NOT NULL,
                    day TEXT NOT NULL,
                    channel_id TEXT NOT NULL,
                    channel_alias TEXT NOT NULL,
                    route_group TEXT,
                    model TEXT NOT NULL,
                    protocol TEXT NOT NULL DEFAULT 'openai_chat',
                    stream INTEGER NOT NULL DEFAULT 0,
                    status INTEGER NOT NULL,
                    latency_ms INTEGER NOT NULL,
                    first_byte_ms INTEGER,
                    prompt_tokens INTEGER,
                    cached_prompt_tokens INTEGER,
                    cache_creation_tokens INTEGER,
                    completion_tokens INTEGER,
                    reasoning_tokens INTEGER,
                    failover_count INTEGER NOT NULL DEFAULT 0,
                    masked_count INTEGER NOT NULL DEFAULT 0,
                    estimated_cost_cny REAL,
                    error_code TEXT,
                    agent TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_gw_usage_ts ON gateway_usage_log (ts DESC);
                CREATE INDEX IF NOT EXISTS idx_gw_usage_day_ch ON gateway_usage_log (day, channel_id);",
            )
            .map_err(|e| format!("建立网关台账表失败: {e}"))
    }

    pub fn gateway_usage_insert(&self, row: &GatewayUsageRow) -> Result<(), String> {
        let day = day_string_of(row.ts);
        self.conn()
            .execute(
                "INSERT INTO gateway_usage_log (
                    ts, day, channel_id, channel_alias, route_group, model, protocol,
                    stream, status, latency_ms, first_byte_ms,
                    prompt_tokens, cached_prompt_tokens, cache_creation_tokens,
                    completion_tokens, reasoning_tokens,
                    failover_count, masked_count, estimated_cost_cny, error_code, agent
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                params![
                    row.ts,
                    day,
                    row.channel_id,
                    row.channel_alias,
                    row.route_group,
                    row.model,
                    row.protocol,
                    i64::from(row.stream),
                    i64::from(row.status),
                    row.latency_ms as i64,
                    row.first_byte_ms.map(|v| v as i64),
                    row.prompt_tokens.map(|v| v as i64),
                    row.cached_prompt_tokens.map(|v| v as i64),
                    row.cache_creation_tokens.map(|v| v as i64),
                    row.completion_tokens.map(|v| v as i64),
                    row.reasoning_tokens.map(|v| v as i64),
                    i64::from(row.failover_count),
                    row.masked_count as i64,
                    row.estimated_cost_cny,
                    row.error_code,
                    row.agent,
                ],
            )
            .map_err(|e| format!("写入网关台账失败: {e}"))?;
        Ok(())
    }

    pub fn gateway_usage_prune(&self, before_ts: i64) -> Result<usize, String> {
        self.conn()
            .execute("DELETE FROM gateway_usage_log WHERE ts < ?1", params![before_ts])
            .map_err(|e| format!("清理网关旧台账失败: {e}"))
    }

    pub fn gateway_usage_clear(&self) -> Result<(), String> {
        self.conn()
            .execute("DELETE FROM gateway_usage_log", [])
            .map_err(|e| format!("清空网关台账失败: {e}"))?;
        Ok(())
    }

    pub fn gateway_usage_summary(
        &self,
        since_ts: i64,
        recent_limit: usize,
    ) -> Result<GatewayUsageSummary, String> {
        let mut stmt = self
            .conn()
            .prepare(
                "SELECT id, ts, channel_id, channel_alias, route_group, model, protocol,
                        stream, status, latency_ms, first_byte_ms,
                        prompt_tokens, cached_prompt_tokens, cache_creation_tokens,
                        completion_tokens, reasoning_tokens,
                        failover_count, masked_count, estimated_cost_cny, error_code, agent
                 FROM gateway_usage_log
                 WHERE ts >= ?1
                 ORDER BY ts DESC, id DESC",
            )
            .map_err(|e| format!("读取网关台账失败: {e}"))?;

        let rows = stmt
            .query_map(params![since_ts], |r| {
                Ok(GatewayUsageRowWithId {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    channel_id: r.get(2)?,
                    channel_alias: r.get(3)?,
                    route_group: r.get(4)?,
                    model: r.get(5)?,
                    protocol: r.get(6)?,
                    stream: r.get::<_, i64>(7)? != 0,
                    status: r.get::<_, i64>(8)?.clamp(0, u16::MAX as i64) as u16,
                    latency_ms: r.get::<_, i64>(9)?.max(0) as u64,
                    first_byte_ms: r.get::<_, Option<i64>>(10)?.map(|v| v.max(0) as u64),
                    prompt_tokens: r.get::<_, Option<i64>>(11)?.map(|v| v.max(0) as u64),
                    cached_prompt_tokens: r.get::<_, Option<i64>>(12)?.map(|v| v.max(0) as u64),
                    cache_creation_tokens: r.get::<_, Option<i64>>(13)?.map(|v| v.max(0) as u64),
                    completion_tokens: r.get::<_, Option<i64>>(14)?.map(|v| v.max(0) as u64),
                    reasoning_tokens: r.get::<_, Option<i64>>(15)?.map(|v| v.max(0) as u64),
                    failover_count: r.get::<_, i64>(16)?.clamp(0, u8::MAX as i64) as u8,
                    masked_count: r.get::<_, i64>(17)?.max(0) as usize,
                    estimated_cost_cny: r.get(18)?,
                    error_code: r.get(19)?,
                    agent: r.get(20)?,
                })
            })
            .map_err(|e| format!("查询网关台账失败: {e}"))?;

        let mut summary = GatewayUsageSummary {
            since_ts,
            ..Default::default()
        };
        let mut ttfts: Vec<u64> = Vec::new();
        let mut by_channel: BTreeMap<String, GatewayUsageByKey> = BTreeMap::new();
        let mut by_model: BTreeMap<String, GatewayUsageByKey> = BTreeMap::new();
        let mut total_cost = 0.0_f64;

        for item in rows {
            let r = match item {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("跳过损坏的网关台账行: {e}");
                    continue;
                }
            };
            summary.total_requests += 1;
            let is_ok = (200..400).contains(&r.status);
            if is_ok {
                summary.ok_requests += 1;
            } else {
                summary.error_requests += 1;
            }
            if r.failover_count > 0 {
                summary.failover_requests += 1;
            }
            let p = r.prompt_tokens.unwrap_or(0);
            let cp = r.cached_prompt_tokens.unwrap_or(0);
            let cc = r.cache_creation_tokens.unwrap_or(0);
            let comp = r.completion_tokens.unwrap_or(0);
            let reas = r.reasoning_tokens.unwrap_or(0);
            summary.prompt_tokens = summary.prompt_tokens.saturating_add(p);
            summary.cached_prompt_tokens = summary.cached_prompt_tokens.saturating_add(cp);
            summary.cache_creation_tokens = summary.cache_creation_tokens.saturating_add(cc);
            summary.completion_tokens = summary.completion_tokens.saturating_add(comp);
            summary.reasoning_tokens = summary.reasoning_tokens.saturating_add(reas);
            if let Some(ttft) = r.first_byte_ms {
                ttfts.push(ttft);
            }
            if let Some(cost) = r.estimated_cost_cny {
                total_cost += cost;
                summary.priced_requests += 1;
            }

            let ch_key = if r.channel_id.is_empty() {
                r.channel_alias.clone()
            } else {
                r.channel_id.clone()
            };
            let ch_label = if r.channel_alias.is_empty() {
                ch_key.clone()
            } else {
                r.channel_alias.clone()
            };
            accumulate_bucket(&mut by_channel, &ch_key, &ch_label, &r, is_ok);
            accumulate_bucket(&mut by_model, &r.model, &r.model, &r, is_ok);

            if summary.recent.len() < recent_limit {
                summary.recent.push(GatewayUsageRecent {
                    id: r.id,
                    ts: r.ts,
                    channel_id: r.channel_id,
                    channel_alias: r.channel_alias,
                    route_group: r.route_group,
                    model: r.model,
                    protocol: r.protocol,
                    stream: r.stream,
                    status: r.status,
                    latency_ms: r.latency_ms,
                    first_byte_ms: r.first_byte_ms,
                    prompt_tokens: r.prompt_tokens,
                    cached_prompt_tokens: r.cached_prompt_tokens,
                    completion_tokens: r.completion_tokens,
                    reasoning_tokens: r.reasoning_tokens,
                    failover_count: r.failover_count,
                    masked_count: r.masked_count,
                    estimated_cost_cny: r.estimated_cost_cny,
                    error_code: r.error_code,
                    agent: r.agent,
                });
            }
        }

        if summary.priced_requests > 0 {
            summary.estimated_cost_cny = Some((total_cost * 1_000_000.0).round() / 1_000_000.0);
        }
        let cache_denom = summary.prompt_tokens.max(summary.cached_prompt_tokens);
        if cache_denom > 0 && summary.cached_prompt_tokens > 0 {
            let ratio = (summary.cached_prompt_tokens as f64) / (cache_denom as f64);
            summary.cache_hit_rate = Some((ratio.min(1.0) * 1000.0).round() / 1000.0);
        } else if cache_denom > 0 {
            summary.cache_hit_rate = Some(0.0);
        }
        ttfts.sort_unstable();
        summary.ttft_p50_ms = percentile_sorted(&ttfts, 50, 100);
        summary.ttft_p95_ms = percentile_sorted(&ttfts, 95, 100);

        let mut ch_vec: Vec<_> = by_channel.into_values().collect();
        ch_vec.sort_by(|a, b| b.requests.cmp(&a.requests).then_with(|| a.key.cmp(&b.key)));
        let mut m_vec: Vec<_> = by_model.into_values().collect();
        m_vec.sort_by(|a, b| b.requests.cmp(&a.requests).then_with(|| a.key.cmp(&b.key)));
        summary.by_channel = ch_vec;
        summary.by_model = m_vec;
        Ok(summary)
    }
}

struct GatewayUsageRowWithId {
    id: i64,
    ts: i64,
    channel_id: String,
    channel_alias: String,
    route_group: Option<String>,
    model: String,
    protocol: String,
    stream: bool,
    status: u16,
    latency_ms: u64,
    first_byte_ms: Option<u64>,
    prompt_tokens: Option<u64>,
    cached_prompt_tokens: Option<u64>,
    cache_creation_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    failover_count: u8,
    masked_count: usize,
    estimated_cost_cny: Option<f64>,
    error_code: Option<String>,
    agent: Option<String>,
}

fn accumulate_bucket(
    map: &mut BTreeMap<String, GatewayUsageByKey>,
    key: &str,
    label: &str,
    r: &GatewayUsageRowWithId,
    is_ok: bool,
) {
    let entry = map.entry(key.to_string()).or_insert_with(|| GatewayUsageByKey {
        key: key.to_string(),
        label: label.to_string(),
        ..Default::default()
    });
    entry.requests += 1;
    if !is_ok {
        entry.errors += 1;
    }
    entry.prompt_tokens = entry.prompt_tokens.saturating_add(r.prompt_tokens.unwrap_or(0));
    entry.cached_prompt_tokens = entry
        .cached_prompt_tokens
        .saturating_add(r.cached_prompt_tokens.unwrap_or(0));
    entry.cache_creation_tokens = entry
        .cache_creation_tokens
        .saturating_add(r.cache_creation_tokens.unwrap_or(0));
    entry.completion_tokens = entry
        .completion_tokens
        .saturating_add(r.completion_tokens.unwrap_or(0));
    entry.reasoning_tokens = entry
        .reasoning_tokens
        .saturating_add(r.reasoning_tokens.unwrap_or(0));
    if let Some(cost) = r.estimated_cost_cny {
        let prev = entry.estimated_cost_cny.unwrap_or(0.0);
        entry.estimated_cost_cny = Some(((prev + cost) * 1_000_000.0).round() / 1_000_000.0);
        entry.priced_requests += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ledger_migrates_inserts_summarizes_prunes_and_clears() {
        let store = Store::open_memory().unwrap();
        let base_ts = 1_795_000_000;
        store
            .gateway_usage_insert(&GatewayUsageRow {
                ts: base_ts - 10 * 86_400,
                agent: None,
                channel_id: "ch_old".into(),
                channel_alias: "old".into(),
                route_group: None,
                model: "old/m".into(),
                protocol: "openai_chat".into(),
                stream: false,
                status: 200,
                latency_ms: 500,
                first_byte_ms: Some(120),
                prompt_tokens: Some(100),
                cached_prompt_tokens: Some(50),
                cache_creation_tokens: None,
                completion_tokens: Some(20),
                reasoning_tokens: None,
                failover_count: 0,
                masked_count: 0,
                estimated_cost_cny: None,
                error_code: None,
            })
            .unwrap();

        for (i, (ttft, status, failover, cost)) in [
            (Some(100), 200, 0, Some(0.01)),
            (Some(200), 200, 1, Some(0.02)),
            (Some(900), 502, 2, None),
            (Some(300), 200, 0, Some(0.03)),
        ]
        .into_iter()
        .enumerate()
        {
            store
                .gateway_usage_insert(&GatewayUsageRow {
                    ts: base_ts + i as i64,
                    agent: if i == 2 { Some("codex".into()) } else { None },
                    channel_id: if i == 2 { "ch_b".into() } else { "ch_a".into() },
                    channel_alias: if i == 2 { "b".into() } else { "a".into() },
                    route_group: Some("group/coding".into()),
                    model: "group/coding".into(),
                    protocol: "anthropic_messages".into(),
                    stream: true,
                    status,
                    latency_ms: 1000,
                    first_byte_ms: ttft,
                    prompt_tokens: Some(1000),
                    cached_prompt_tokens: Some(600),
                    cache_creation_tokens: Some(100),
                    completion_tokens: Some(200),
                    reasoning_tokens: Some(50),
                    failover_count: failover,
                    masked_count: 1,
                    estimated_cost_cny: cost,
                    error_code: (status >= 400).then(|| "upstream_error".into()),
                })
                .unwrap();
        }

        let summary = store.gateway_usage_summary(base_ts, 2).unwrap();
        assert_eq!(summary.total_requests, 4);
        assert!(
            summary.recent.iter().any(|r| r.agent.as_deref() == Some("codex")),
            "agent 归因列要能写能读"
        );
        assert_eq!(summary.ok_requests, 3);
        assert_eq!(summary.error_requests, 1);
        assert_eq!(summary.failover_requests, 2);
        assert_eq!(summary.prompt_tokens, 4000);
        assert_eq!(summary.cached_prompt_tokens, 2400);
        assert_eq!(summary.cache_creation_tokens, 400);
        assert_eq!(summary.completion_tokens, 800);
        assert_eq!(summary.reasoning_tokens, 200);
        assert_eq!(summary.cache_hit_rate, Some(0.6));
        assert_eq!(summary.ttft_p50_ms, Some(200));
        assert_eq!(summary.ttft_p95_ms, Some(300));
        assert_eq!(summary.priced_requests, 3);
        assert!((summary.estimated_cost_cny.unwrap() - 0.06).abs() < 1e-9);
        assert_eq!(summary.recent.len(), 2);
        assert_eq!(summary.by_channel.len(), 2);

        let pruned = store.gateway_usage_prune(base_ts).unwrap();
        assert_eq!(pruned, 1);
        assert_eq!(store.gateway_usage_summary(0, 10).unwrap().total_requests, 4);

        store.gateway_usage_clear().unwrap();
        assert_eq!(store.gateway_usage_summary(0, 10).unwrap().total_requests, 0);
    }

    #[test]
    fn estimate_cost_uses_4sapi_catalog_and_returns_none_for_unknown() {
        let store = Store::open_memory().unwrap();
        let cache = ModelCache {
            version: "v1".into(),
            fetched_at: 100,
            host: "4sapi.com".into(),
            pricing: json!({
                "auto_groups": ["default"],
                "group_ratio": { "default": 1.0 },
                "data": [{
                    "model_name": "claude-sonnet-5",
                    "tags": "文本,1M",
                    "quota_type": 0,
                    "model_ratio": 1.0,
                    "completion_ratio": 5.0,
                    "cache_ratio": 0.1,
                    "enable_groups": ["default"],
                    "supported_endpoint_types": ["openai"]
                }]
            })
            .to_string(),
            usable: vec![],
            usable_at: 0,
            usable_host: String::new(),
            sources: Default::default(),
        };
        cache.save(&store).unwrap();
        // input = 2.0 / 1M, output = 10.0 / 1M, cache_mult = 0.1
        // 1000 prompt (800 cached, 200 uncached) + 100 completion:
        // (200 * 2.0 + 800 * 0.2 + 100 * 10.0) / 1e6 = (400 + 160 + 1000) / 1e6 = 0.00156
        let cost = estimate_channel_cost_cny(
            &store,
            "4sapi",
            "https://4sapi.com/v1",
            "claude-sonnet-5",
            Some(1000),
            Some(800),
            Some(100),
        );
        assert_eq!(cost, Some(0.00156));
        assert_eq!(
            estimate_channel_cost_cny(
                &store,
                "custom",
                "https://api.openai.com/v1",
                "claude-sonnet-5",
                Some(1000),
                Some(800),
                Some(100),
            ),
            None
        );
    }
}
