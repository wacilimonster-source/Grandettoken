//! 从本机 `ChannelView` + SQLite 快照构建待发布的 SyncSnapshot。
//!
//! 这是“只上传查询结果”的核心闸门：只读 `ChannelView` 的展示字段，
//! 原始错误文本、authLabel、accounts、extra、本机路径一概不碰。

use std::collections::BTreeMap;

use crate::checkin;
use crate::config::Config;
use crate::fetch::{local_tz_offset, ChannelView};
use crate::providers::{self, Kind};
use crate::store::Store;

use super::model::{
    decimal_string, format_micros, parse_decimal_micros, safe_unit, sanitize_name, sum_decimal_strings,
    ChannelKind, ChannelSnapshot, CheckinItem, CheckinSnapshot, CheckinStatus, ExpiringSnapshot,
    HistoryPoint, OtherMoney, SyncSnapshot, TotalsSnapshot, MAX_HISTORY, MAX_WINDOWS,
};
use super::platform::platform_id_for;
use super::{crypto, error::SyncError};

pub struct BuildInput<'a> {
    pub cfg: &'a Config,
    pub views: &'a [ChannelView],
    pub store: &'a Store,
    pub source: &'a str,
    pub seq: u64,
    pub now: i64,
    pub app: &'a str,
}

pub fn build_snapshot(input: BuildInput<'_>) -> Result<SyncSnapshot, SyncError> {
    let source = crypto::normalize_source(input.source).ok_or_else(|| {
        SyncError::new(super::error::SOURCE_INVALID, "写入者 ID 格式不正确")
    })?;

    let published: Vec<&ChannelView> = input
        .views
        .iter()
        .filter(|v| !v.hidden && !v.auto_hidden)
        .filter(|v| {
            input.cfg.sync_channels.is_empty() || input.cfg.sync_channels.iter().any(|id| id == &v.id)
        })
        .collect();

    let mut channels = Vec::with_capacity(published.len());
    for view in &published {
        channels.push(build_channel(view, input.cfg, input.store, input.now));
    }

    let checkin = build_checkin(input.cfg, &published, input.now);
    let totals = build_totals(&channels);

    let snapshot = SyncSnapshot {
        v: super::model::SNAPSHOT_VERSION,
        source,
        seq: input.seq,
        at: input.now,
        app: input.app.chars().take(40).collect(),
        channels,
        checkin,
        totals,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

fn build_channel(view: &ChannelView, cfg: &Config, store: &Store, now: i64) -> ChannelSnapshot {
    let kind = match view.kind {
        Kind::Amount => ChannelKind::Money,
        Kind::Percent => ChannelKind::Quota,
        Kind::Points => ChannelKind::Points,
    };
    let unit_fallback = match kind {
        ChannelKind::Money => "CNY",
        ChannelKind::Quota => "%",
        ChannelKind::Points => "credits",
    };
    let unit = safe_unit(&view.unit, unit_fallback);
    let platform_id = platform_id_for(&view.id, cfg);
    let name = sanitize_name(&view.name, default_name(&view.id));

    let (remaining, percent) = match kind {
        ChannelKind::Quota => (None, view.remaining.and_then(decimal_string)),
        _ => (view.remaining.and_then(decimal_string), None),
    };
    let total = match kind {
        ChannelKind::Quota => None,
        _ => view.total.and_then(decimal_string),
    };

    let windows = view
        .windows
        .iter()
        .take(MAX_WINDOWS)
        .map(|w| super::model::WindowSnapshot {
            label: sanitize_name(&w.label, "窗口"),
            remain_percent: decimal_string(w.remain_percent),
            reset_at: None,
            reset_text: w.resets_at.as_deref().map(short_reset_text),
        })
        .collect();

    let expiring = view
        .expiring
        .iter()
        .filter_map(|e| {
            decimal_string(e.amount).map(|amount| ExpiringSnapshot {
                amount,
                at: e.at,
            })
        })
        .take(super::model::MAX_EXPIRING)
        .collect();

    let error_code = if view.stale {
        Some("SYNC_STALE".to_string())
    } else if view.error.is_some() {
        Some("SYNC_UPSTREAM".to_string())
    } else {
        None
    };

    ChannelSnapshot {
        platform_id,
        id: view.id.clone(),
        name,
        kind,
        unit: unit.clone(),
        remaining,
        total,
        percent,
        windows,
        expiring,
        day: decimal_string_opt(view.day),
        week: decimal_string_opt(view.week),
        month: decimal_string_opt(view.month),
        estimated: view.estimated,
        stale: view.stale,
        error_code,
        updated_at: view.updated_at.max(1),
        history: build_history(view, store, now, &unit),
    }
}

fn default_name(channel_id: &str) -> &str {
    providers::PROVIDERS
        .iter()
        .find(|p| p.id == channel_id)
        .map(|p| p.name)
        .unwrap_or("自定义渠道")
}

fn decimal_string_opt(v: Option<f64>) -> Option<String> {
    v.and_then(decimal_string)
}

fn short_reset_text(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect::<String>()
        .trim()
        .to_string()
}

fn build_history(view: &ChannelView, store: &Store, now: i64, unit: &str) -> Vec<HistoryPoint> {
    let tz = local_tz_offset();
    let day_start = ((now + tz).div_euclid(86_400)) * 86_400 - tz;
    let from = day_start - (MAX_HISTORY as i64 - 1) * 86_400;
    let summaries = store.daily_summaries(&view.id, None, from).unwrap_or_default();
    let mut by_day = BTreeMap::new();
    for s in summaries {
        by_day.insert(s.day_start, s);
    }
    let tz_label = offset_label(tz);
    (0..MAX_HISTORY)
        .map(|i| {
            let start = from + i as i64 * 86_400;
            let summary = by_day.get(&start);
            HistoryPoint {
                period_start: start,
                period_end: start + 86_400,
                time_zone: tz_label.clone(),
                unit: unit.to_string(),
                balance: None,
                spent: summary.and_then(|s| decimal_string(s.consumed)),
                estimated: true,
                revised_at: summary.map(|s| s.last_ts.min(now).max(1)).unwrap_or(now),
            }
        })
        .collect()
}

fn offset_label(offset_sec: i64) -> String {
    let sign = if offset_sec < 0 { '-' } else { '+' };
    let abs = offset_sec.abs();
    format!("{}{:02}:{:02}", sign, abs / 3600, (abs % 3600) / 60)
}

fn build_checkin(cfg: &Config, published: &[&ChannelView], now: i64) -> Option<CheckinSnapshot> {
    let published_ids: Vec<&str> = published.iter().map(|v| v.id.as_str()).collect();
    let mut items = Vec::new();
    for def in checkin::CHECKINS {
        if !published_ids.iter().any(|id| *id == def.id) {
            continue;
        }
        let Some(cc) = cfg.checkin.get(def.id) else {
            continue;
        };
        if !cc.enabled {
            continue;
        }
        let day = checkin::cycle_day(now, def.day_roll_hour);
        let status = checkin_status(cc, &day);
        let observed_at = cc.last_try_at.unwrap_or(now).clamp(1, now);
        items.push(CheckinItem {
            item_id: format!("{}:main", def.id),
            platform_id: def.id.to_string(),
            status,
            observed_at,
        });
    }
    if items.is_empty() {
        return None;
    }
    let done = items
        .iter()
        .filter(|i| i.status == CheckinStatus::Done)
        .count() as u32;
    let pending = items
        .iter()
        .filter(|i| i.status == CheckinStatus::Pending)
        .count() as u32;
    let unsupported = items
        .iter()
        .filter(|i| i.status == CheckinStatus::Unsupported)
        .count() as u32;
    let unknown = items
        .iter()
        .filter(|i| i.status == CheckinStatus::Unknown)
        .count() as u32;
    Some(CheckinSnapshot {
        date: checkin::beijing_day(now),
        time_zone: "Asia/Shanghai".into(),
        items,
        done,
        total: done + pending,
        unsupported,
        unknown,
    })
}

fn checkin_status(cc: &checkin::CheckinCfg, day: &str) -> CheckinStatus {
    // 不能直接用 done_for_day：它把 auth / shape / manual / inactive 也算“今天
    // 别试了”，但手机汇总要的是“完成 / 待办 / 不支持 / 未知”四种口径。
    if cc.last_day.as_deref() != Some(day) {
        return CheckinStatus::Pending;
    }
    match cc.last_code.as_deref() {
        Some(code) if code.starts_with("claimed") || code.starts_with("already") => {
            CheckinStatus::Done
        }
        Some(code) if code.starts_with("inactive") || code.starts_with("manual") => {
            CheckinStatus::Unsupported
        }
        Some(code) if code.starts_with("auth") || code.starts_with("cred") || code.starts_with("shape") => {
            CheckinStatus::Unknown
        }
        _ => CheckinStatus::Pending,
    }
}

pub(crate) fn build_totals(channels: &[ChannelSnapshot]) -> TotalsSnapshot {
    let mut cny_remaining: Option<i128> = None;
    let mut cny_today: Option<i128> = None;
    let mut others: BTreeMap<String, i128> = BTreeMap::new();

    for ch in channels {
        if ch.kind != ChannelKind::Money {
            continue;
        }
        if ch.unit == "CNY" {
            if let Some(v) = ch.remaining.as_deref().and_then(parse_decimal_micros) {
                cny_remaining = Some(cny_remaining.unwrap_or(0).saturating_add(v));
            }
            if let Some(v) = ch.day.as_deref().and_then(parse_decimal_micros) {
                cny_today = Some(cny_today.unwrap_or(0).saturating_add(v));
            }
        } else if let Some(v) = ch.remaining.as_deref().and_then(parse_decimal_micros) {
            let entry = others.entry(ch.unit.clone()).or_insert(0);
            *entry = entry.saturating_add(v);
        }
    }

    TotalsSnapshot {
        cny: cny_remaining.map(format_micros),
        others: others
            .into_iter()
            .map(|(unit, amount)| OtherMoney {
                unit,
                amount: format_micros(amount),
            })
            .collect(),
        today: cny_today.map(format_micros),
    }
}

/// 给手机端做合计重算用：同一组十进制字符串求和的公开入口。
pub fn sum_amounts<'a, I>(values: I) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    sum_decimal_strings(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_label_formats_utc8() {
        assert_eq!(offset_label(8 * 3600), "+08:00");
        assert_eq!(offset_label(-5 * 3600 - 30 * 60), "-05:30");
    }

    #[test]
    fn totals_are_exact_and_grouped() {
        let ch = |unit: &str, remaining: &str, day: &str| ChannelSnapshot {
            platform_id: "x".into(),
            id: "x".into(),
            name: "X".into(),
            kind: ChannelKind::Money,
            unit: unit.into(),
            remaining: Some(remaining.into()),
            total: None,
            percent: None,
            windows: vec![],
            expiring: vec![],
            day: Some(day.into()),
            week: None,
            month: None,
            estimated: false,
            stale: false,
            error_code: None,
            updated_at: 1,
            history: vec![],
        };
        let totals = build_totals(&[
            ch("CNY", "0.1", "0.2"),
            ch("CNY", "0.2", "0.1"),
            ch("USD", "1.25", "0"),
        ]);
        assert_eq!(totals.cny.as_deref(), Some("0.3"));
        assert_eq!(totals.today.as_deref(), Some("0.3"));
        assert_eq!(totals.others.len(), 1);
        assert_eq!(totals.others[0].unit, "USD");
        assert_eq!(totals.others[0].amount, "1.25");
    }
}
