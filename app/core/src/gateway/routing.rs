//! 虚拟路由组、配额/临期/峰谷感知排序、熔断冷却与 Sticky 会话粘性。

use super::{effective_models, Gateway, Published};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const DEFAULT_STICKY_TTL_SECS: u64 = 1800;
pub const DEFAULT_COOLDOWN_SECS: u64 = 60;
const MAX_STICKY_BINDINGS: usize = 512;

fn default_sticky() -> bool {
    true
}
fn default_sticky_ttl() -> u64 {
    DEFAULT_STICKY_TTL_SECS
}
fn default_cooldown_secs() -> u64 {
    DEFAULT_COOLDOWN_SECS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoutingStrategy {
    #[default]
    Order,
    Rotate,
    Expiring,
    Pace,
}

impl RoutingStrategy {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "rotate" => Self::Rotate,
            "expiring" => Self::Expiring,
            "pace" => Self::Pace,
            _ => Self::Order,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteTarget {
    pub channel_id: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteGroup {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub strategy: RoutingStrategy,
    #[serde(default = "default_sticky")]
    pub sticky: bool,
    #[serde(default = "default_sticky_ttl")]
    pub sticky_ttl_secs: u64,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    #[serde(default)]
    pub targets: Vec<RouteTarget>,
}

/// 渠道只读账务信号（由余额/配额刷新推送到网关内存，用于 expiring / pace 策略）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSignal {
    pub provider_id: String,
    pub remaining_ratio: Option<f64>,
    pub has_expiring_7d: bool,
    pub soonest_expiry_ts: Option<i64>,
    pub is_deepseek: bool,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct StickyBinding {
    pub channel_id: String,
    pub model: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CooldownSnapshot {
    pub channel_id: String,
    pub model: String,
    pub until_ts: i64,
    pub remaining_secs: i64,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct RoutingRuntime {
    pub cooldowns: BTreeMap<String, (i64, String)>,
    pub sticky: BTreeMap<String, StickyBinding>,
    pub rr_cursors: BTreeMap<String, usize>,
    pub signals: BTreeMap<String, ChannelSignal>,
}

impl RoutingRuntime {
    fn target_key(channel_id: &str, model: &str) -> String {
        format!("{channel_id}\0{model}")
    }

    fn sticky_key(group_id: &str, session_hash: &str) -> String {
        format!("{group_id}\0{session_hash}")
    }

    pub fn cooldown_until(&self, channel_id: &str, model: &str, now_ts: i64) -> Option<i64> {
        self.cooldowns
            .get(&Self::target_key(channel_id, model))
            .and_then(|(until, _)| (*until > now_ts).then_some(*until))
    }

    pub fn set_cooldown(&mut self, channel_id: &str, model: &str, until_ts: i64, reason: String) {
        self.cooldowns
            .insert(Self::target_key(channel_id, model), (until_ts, reason));
    }

    pub fn clear_expired(&mut self, now_ts: i64) {
        self.cooldowns.retain(|_, (until, _)| *until > now_ts);
        self.sticky.retain(|_, b| b.expires_at > now_ts);
    }

    pub fn set_sticky(
        &mut self,
        group_id: &str,
        session_hash: &str,
        channel_id: &str,
        model: &str,
        ttl_secs: u64,
        now_ts: i64,
    ) {
        self.sticky.retain(|_, b| b.expires_at > now_ts);
        while self.sticky.len() >= MAX_STICKY_BINDINGS {
            if let Some(oldest) = self
                .sticky
                .iter()
                .min_by_key(|(_, b)| b.expires_at)
                .map(|(k, _)| k.clone())
            {
                self.sticky.remove(&oldest);
            } else {
                break;
            }
        }
        let ttl = ttl_secs.clamp(30, 86_400) as i64;
        self.sticky.insert(
            Self::sticky_key(group_id, session_hash),
            StickyBinding {
                channel_id: channel_id.to_string(),
                model: model.to_string(),
                expires_at: now_ts.saturating_add(ttl),
            },
        );
    }

    pub fn get_sticky(&self, group_id: &str, session_hash: &str, now_ts: i64) -> Option<&StickyBinding> {
        self.sticky
            .get(&Self::sticky_key(group_id, session_hash))
            .filter(|b| b.expires_at > now_ts)
    }

    pub fn snapshots(&self, now_ts: i64) -> Vec<CooldownSnapshot> {
        self.cooldowns
            .iter()
            .filter(|(_, (until, _))| *until > now_ts)
            .filter_map(|(k, (until, reason))| {
                let (channel_id, model) = k.split_once('\0')?;
                Some(CooldownSnapshot {
                    channel_id: channel_id.to_string(),
                    model: model.to_string(),
                    until_ts: *until,
                    remaining_secs: (*until - now_ts).max(0),
                    reason: reason.clone(),
                })
            })
            .collect()
    }
}

pub fn normalize_group_id(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    let id = trimmed.strip_prefix("group/").unwrap_or(&trimmed).trim();
    if id.is_empty()
        || id.len() > 32
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err("路由组 ID 只能包含小写英文、数字、连字符和下划线（1 到 32 字）".into());
    }
    Ok(id.to_string())
}

pub fn group_model_id(id: &str) -> String {
    format!("group/{id}")
}

/// 生成纯摘要会话指纹（绝不保存原文）。优先取显式会话头，否则对 system + 首条 user 文本取 SHA-256。
pub fn derive_session_hash(headers: &HeaderMap, openai_body: &Value) -> Option<String> {
    for name in ["x-session-id", "x-conversation-id", "anthropic-session-id"] {
        if let Some(val) = headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let mut hasher = Sha256::new();
            hasher.update(b"hdr:");
            hasher.update(val.as_bytes());
            return Some(hex_prefix(&hasher.finalize()));
        }
    }
    let msgs = openai_body.get("messages").and_then(Value::as_array)?;
    let mut sys_text = String::new();
    let mut first_user = String::new();
    for m in msgs {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");
        let txt = extract_msg_text(m);
        if txt.is_empty() {
            continue;
        }
        if role == "system" && sys_text.is_empty() {
            sys_text = txt.chars().take(512).collect();
        } else if role == "user" && first_user.is_empty() {
            first_user = txt.chars().take(512).collect();
            break;
        }
    }
    if sys_text.is_empty() && first_user.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(sys_text.as_bytes());
    hasher.update(b"\x1e");
    hasher.update(first_user.as_bytes());
    Some(hex_prefix(&hasher.finalize()))
}

fn hex_prefix(digest: &[u8]) -> String {
    digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

fn extract_msg_text(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string(),
        _ => String::new(),
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedCandidate {
    pub channel: Published,
    pub upstream_model: String,
    pub in_cooldown: bool,
}

fn match_signal<'a>(
    ch: &Published,
    vault_platforms: &BTreeMap<String, String>,
    signals: &'a BTreeMap<String, ChannelSignal>,
) -> Option<&'a ChannelSignal> {
    let tpl = ch.template.to_ascii_lowercase();
    if let Some(sig) = signals.get(&tpl) {
        return Some(sig);
    }
    let host = ch.host.to_ascii_lowercase();
    if host.contains("deepseek.com") {
        if let Some(sig) = signals.get("deepseek") {
            return Some(sig);
        }
    }
    if host.contains("yuchuantest.com") {
        if let Some(sig) = signals.get("hapi") {
            return Some(sig);
        }
    }
    if host.contains("opencode.ai") {
        if let Some(sig) = signals.get("opencode-go") {
            return Some(sig);
        }
    }
    if crate::providers::four_s_api_hosts()
        .iter()
        .any(|h| host == h.to_ascii_lowercase())
    {
        if let Some(sig) = signals.get("4sapi") {
            return Some(sig);
        }
    }
    if let Some(platform) = vault_platforms.get(&ch.vault_id) {
        let slug = platform.trim().to_ascii_lowercase().replace(' ', "-");
        if let Some(sig) = signals.get(&slug) {
            return Some(sig);
        }
    }
    None
}

fn is_deepseek_channel(ch: &Published, sig: Option<&ChannelSignal>) -> bool {
    sig.is_some_and(|s| s.is_deepseek)
        || ch.template.eq_ignore_ascii_case("deepseek")
        || ch.host.to_ascii_lowercase().contains("deepseek.com")
        || ch.alias.eq_ignore_ascii_case("deepseek")
}

pub fn select_candidates(
    group: &RouteGroup,
    live_channels: &[Published],
    vault_platforms: &BTreeMap<String, String>,
    runtime: &mut RoutingRuntime,
    session_hash: Option<&str>,
    now_ts: i64,
) -> Vec<ResolvedCandidate> {
    runtime.clear_expired(now_ts);
    let mut healthy: Vec<(usize, Published, String, Option<ChannelSignal>)> = Vec::new();
    let mut cooling: Vec<(i64, Published, String)> = Vec::new();

    for (idx, target) in group.targets.iter().enumerate() {
        let Some(ch) = live_channels
            .iter()
            .find(|c| c.channel_id == target.channel_id)
        else {
            continue;
        };
        let eff = effective_models(ch);
        if !eff.iter().any(|m| m == &target.model) {
            continue;
        }
        if let Some(until) = runtime.cooldown_until(&ch.channel_id, &target.model, now_ts) {
            cooling.push((until, ch.clone(), target.model.clone()));
        } else {
            let sig = match_signal(ch, vault_platforms, &runtime.signals).cloned();
            healthy.push((idx, ch.clone(), target.model.clone(), sig));
        }
    }

    let sticky_binding = if group.sticky {
        session_hash
            .and_then(|sh| runtime.get_sticky(&group.id, sh, now_ts))
            .filter(|b| {
                healthy
                    .iter()
                    .any(|(_, c, m, _)| c.channel_id == b.channel_id && m == &b.model)
            })
            .map(|b| (b.channel_id.clone(), b.model.clone()))
    } else {
        None
    };

    match group.strategy {
        RoutingStrategy::Order => {}
        RoutingStrategy::Rotate => {
            if !healthy.is_empty() && sticky_binding.is_none() {
                let cursor = runtime.rr_cursors.entry(group.id.clone()).or_insert(0);
                let shift = *cursor % healthy.len();
                healthy.rotate_left(shift);
                *cursor = cursor.wrapping_add(1);
            }
        }
        RoutingStrategy::Expiring => {
            let off_peak = !crate::peak::info_at(now_ts).peak;
            healthy.sort_by(|a, b| {
                let a_exp = a.3.as_ref().is_some_and(|s| s.has_expiring_7d);
                let b_exp = b.3.as_ref().is_some_and(|s| s.has_expiring_7d);
                let a_soon = a.3.as_ref().and_then(|s| s.soonest_expiry_ts).unwrap_or(i64::MAX);
                let b_soon = b.3.as_ref().and_then(|s| s.soonest_expiry_ts).unwrap_or(i64::MAX);
                let a_ds = off_peak && is_deepseek_channel(&a.1, a.3.as_ref());
                let b_ds = off_peak && is_deepseek_channel(&b.1, b.3.as_ref());
                b_exp
                    .cmp(&a_exp)
                    .then_with(|| a_soon.cmp(&b_soon))
                    .then_with(|| b_ds.cmp(&a_ds))
                    .then_with(|| a.0.cmp(&b.0))
            });
        }
        RoutingStrategy::Pace => {
            healthy.sort_by(|a, b| {
                let ar = a.3.as_ref().and_then(|s| s.remaining_ratio);
                let br = b.3.as_ref().and_then(|s| s.remaining_ratio);
                match (ar, br) {
                    (Some(x), Some(y)) => y
                        .partial_cmp(&x)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.0.cmp(&b.0)),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => a.0.cmp(&b.0),
                }
            });
        }
    }

    // Sticky 会话粘性：若命中未过期且健康的候选，提升至首位（不消耗 Rotate 游标，不影响冷却降级）
    if let Some((cid, model)) = sticky_binding {
        if let Some(pos) = healthy
            .iter()
            .position(|(_, c, m, _)| c.channel_id == cid && m == &model)
        {
            if pos > 0 {
                let pinned = healthy.remove(pos);
                healthy.insert(0, pinned);
            }
        }
    }

    cooling.sort_by_key(|(until, _, _)| *until);
    let mut out: Vec<ResolvedCandidate> = healthy
        .into_iter()
        .map(|(_, channel, upstream_model, _)| ResolvedCandidate {
            channel,
            upstream_model,
            in_cooldown: false,
        })
        .collect();
    out.extend(
        cooling
            .into_iter()
            .map(|(_, channel, upstream_model)| ResolvedCandidate {
                channel,
                upstream_model,
                in_cooldown: true,
            }),
    );
    out
}

pub fn is_failover_status(status: StatusCode, code: Option<&str>) -> bool {
    if matches!(code, Some("responses_conversion")) {
        return false;
    }
    if status == StatusCode::BAD_REQUEST
        && !matches!(code, Some("invalid_upstream_header"))
    {
        return false;
    }
    status.as_u16() >= 400
        && matches!(
            status.as_u16(),
            401 | 403 | 404 | 408 | 429 | 500..=599
        )
        || matches!(
            code,
            Some("missing_upstream_key" | "proxy_init_failed" | "upstream_unreachable" | "upstream_timeout" | "gateway_protocol")
        )
}

pub fn compute_cooldown_secs(
    group: &RouteGroup,
    status: StatusCode,
    retry_after: Option<&HeaderValue>,
) -> u64 {
    if let Some(secs) = retry_after
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
    {
        return secs.clamp(5, 600);
    }
    let base = group.cooldown_secs.clamp(5, 600);
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        base.max(120)
    } else {
        base
    }
}

pub fn group_is_live(group: &RouteGroup, live_channels: &[Published]) -> bool {
    group.targets.iter().any(|t| {
        live_channels
            .iter()
            .find(|c| c.channel_id == t.channel_id)
            .is_some_and(|ch| effective_models(ch).iter().any(|m| m == &t.model))
    })
}

pub fn snapshot_vault_platforms(g: &Gateway) -> BTreeMap<String, String> {
    super::lock(&g.store)
        .vault_list()
        .unwrap_or_default()
        .into_iter()
        .map(|v| (v.id, v.platform))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mk_channel(id: &str, alias: &str, template: &str, host: &str, model: &str) -> Published {
        serde_json::from_value(json!({
            "channelId": id,
            "vaultId": format!("v_{id}"),
            "noUpstreamKey": true,
            "source": "openai_compat",
            "template": template,
            "baseUrl": format!("https://{host}/v1"),
            "alias": alias,
            "host": host,
            "models": [model],
        }))
        .unwrap()
    }

    #[test]
    fn order_rotate_cooldown_and_sticky_work_as_designed() {
        let ch_a = mk_channel("ch_a", "a", "custom", "a.example", "m1");
        let ch_b = mk_channel("ch_b", "b", "custom", "b.example", "m2");
        let live = vec![ch_a, ch_b];
        let mut group = RouteGroup {
            id: "coding".into(),
            name: "Coding".into(),
            strategy: RoutingStrategy::Rotate,
            sticky: true,
            sticky_ttl_secs: 600,
            cooldown_secs: 60,
            targets: vec![
                RouteTarget { channel_id: "ch_a".into(), model: "m1".into() },
                RouteTarget { channel_id: "ch_b".into(), model: "m2".into() },
            ],
        };
        let mut rt = RoutingRuntime::default();
        let vp = BTreeMap::new();
        let now = 1_795_000_000;

        // Rotate shifts 0 -> 1 across calls without sticky
        let c1 = select_candidates(&group, &live, &vp, &mut rt, None, now);
        assert_eq!(c1[0].channel.channel_id, "ch_a");
        let c2 = select_candidates(&group, &live, &vp, &mut rt, None, now);
        assert_eq!(c2[0].channel.channel_id, "ch_b");

        // Sticky pins ch_b for session_1
        rt.set_sticky("coding", "sess_1", "ch_b", "m2", 600, now);
        group.strategy = RoutingStrategy::Order;
        let cs = select_candidates(&group, &live, &vp, &mut rt, Some("sess_1"), now + 10);
        assert_eq!(cs[0].channel.channel_id, "ch_b");

        // Cooling ch_b moves it after healthy ch_a even when sticky
        rt.set_cooldown("ch_b", "m2", now + 100, "429".into());
        let cc = select_candidates(&group, &live, &vp, &mut rt, Some("sess_1"), now + 20);
        assert_eq!(cc[0].channel.channel_id, "ch_a");
        assert!(!cc[0].in_cooldown);
        assert_eq!(cc[1].channel.channel_id, "ch_b");
        assert!(cc[1].in_cooldown);
    }

    #[test]
    fn expiring_and_pace_use_real_signals_and_deepseek_peak_hours() {
        let ch_custom = mk_channel("ch_c", "c", "custom", "c.example", "m1");
        let ch_ds = mk_channel("ch_ds", "ds", "deepseek", "api.deepseek.com", "deepseek-chat");
        let ch_exp = mk_channel("ch_exp", "wb", "4sapi", "4sapi.com", "claude");
        let live = vec![ch_custom, ch_ds, ch_exp];
        let mut group = RouteGroup {
            id: "smart".into(),
            name: "Smart".into(),
            strategy: RoutingStrategy::Expiring,
            sticky: false,
            sticky_ttl_secs: 600,
            cooldown_secs: 60,
            targets: vec![
                RouteTarget { channel_id: "ch_c".into(), model: "m1".into() },
                RouteTarget { channel_id: "ch_ds".into(), model: "deepseek-chat".into() },
                RouteTarget { channel_id: "ch_exp".into(), model: "claude".into() },
            ],
        };
        let mut rt = RoutingRuntime::default();
        // 2026-11-23 22:00 UTC+8 (valley hours): 2026-11-23 14:00:00 UTC
        let valley_ts = chrono::DateTime::parse_from_rfc3339("2026-11-23T22:00:00+08:00")
            .unwrap()
            .timestamp();
        rt.signals.insert(
            "4sapi".into(),
            ChannelSignal {
                provider_id: "4sapi".into(),
                remaining_ratio: Some(0.25),
                has_expiring_7d: true,
                soonest_expiry_ts: Some(valley_ts + 3600),
                is_deepseek: false,
                updated_at: valley_ts,
            },
        );
        rt.signals.insert(
            "deepseek".into(),
            ChannelSignal {
                provider_id: "deepseek".into(),
                remaining_ratio: None,
                has_expiring_7d: false,
                soonest_expiry_ts: None,
                is_deepseek: true,
                updated_at: valley_ts,
            },
        );
        rt.signals.insert(
            "custom".into(),
            ChannelSignal {
                provider_id: "custom".into(),
                remaining_ratio: Some(0.90),
                has_expiring_7d: false,
                soonest_expiry_ts: None,
                is_deepseek: false,
                updated_at: valley_ts,
            },
        );

        let vp = BTreeMap::new();
        let ord = select_candidates(&group, &live, &vp, &mut rt, None, valley_ts);
        assert_eq!(ord[0].channel.channel_id, "ch_exp", "expiring 7d first");
        assert_eq!(ord[1].channel.channel_id, "ch_ds", "deepseek valley second");
        assert_eq!(ord[2].channel.channel_id, "ch_c");

        group.strategy = RoutingStrategy::Pace;
        let paced = select_candidates(&group, &live, &vp, &mut rt, None, valley_ts);
        assert_eq!(paced[0].channel.channel_id, "ch_c", "90% remaining beats 25%");
        assert_eq!(paced[1].channel.channel_id, "ch_exp");
        assert_eq!(paced[2].channel.channel_id, "ch_ds", "unknown ratio last");
    }
}
