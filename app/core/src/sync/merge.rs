//! 跨来源合并：按 platformId 取最新整条记录，不跨来源拼接、不求和。
//!
//! 规则（与 R1 修订一致）：
//! - 同一 platformId：`updatedAt` 最大者胜；同刻按 `source` 字典序确定性选择。
//! - 选中的整条记录原样保留，不把 A 来源的余额和 B 来源的消耗拼在一起。
//! - 最新一条是 stale 就带着 stale 标记展示，不退回更旧的观测。
//! - 历史按 (platformId, periodStart, periodEnd, unit) 去重，同窗口取最新修订。
//! - 签到按 itemId 去重，取最新观测；done/total 由客户端重算。
//! - 总额从合并后的渠道行重算，绝不累加各来源的 totals。

use std::collections::BTreeMap;

use serde::Serialize;

use super::builder::build_totals;
use super::error::{SyncError, CONFLICT};
use super::model::{
    ChannelSnapshot, CheckinItem, CheckinSnapshot, CheckinStatus, HistoryPoint, SyncSnapshot,
    TotalsSnapshot, MAX_HISTORY,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedSnapshot {
    pub channels: Vec<ChannelSnapshot>,
    pub checkin: Option<CheckinSnapshot>,
    pub totals: TotalsSnapshot,
    pub sources: usize,
    /// 解密失败或载荷不合法、被跳过的文件（手机端诊断用）。
    pub failed_sources: Vec<String>,
    pub generated_at: i64,
    pub stale_channels: usize,
}

struct Picked<'a> {
    source: &'a str,
    channel: &'a ChannelSnapshot,
}

#[derive(Clone, Copy)]
struct HistPick<'a> {
    revised_at: i64,
    source: &'a str,
    point: &'a HistoryPoint,
}

pub fn merge_snapshots(
    snapshots: &[SyncSnapshot],
    failed_sources: Vec<String>,
) -> Result<MergedSnapshot, SyncError> {
    let deduped = dedupe_by_source(snapshots)?;

    let mut channels: BTreeMap<String, Picked<'_>> = BTreeMap::new();
    for snap in &deduped {
        for ch in &snap.channels {
            let candidate = Picked {
                source: snap.source.as_str(),
                channel: ch,
            };
            match channels.get(&ch.platform_id) {
                None => {
                    channels.insert(ch.platform_id.clone(), candidate);
                }
                Some(cur) => {
                    if wins(candidate.channel.updated_at, candidate.source, cur.channel.updated_at, cur.source) {
                        channels.insert(ch.platform_id.clone(), candidate);
                    }
                }
            }
        }
    }

    let mut history: BTreeMap<(String, i64, i64, String), HistPick<'_>> = BTreeMap::new();
    for snap in &deduped {
        for ch in &snap.channels {
            for point in &ch.history {
                let key = (
                    ch.platform_id.clone(),
                    point.period_start,
                    point.period_end,
                    point.unit.clone(),
                );
                let candidate = HistPick {
                    revised_at: point.revised_at,
                    source: snap.source.as_str(),
                    point,
                };
                match history.get(&key) {
                    None => {
                        history.insert(key, candidate);
                    }
                    Some(cur) => {
                        if candidate.revised_at > cur.revised_at
                            || (candidate.revised_at == cur.revised_at
                                && candidate.source < cur.source)
                        {
                            history.insert(key, candidate);
                        }
                    }
                }
            }
        }
    }

    let mut merged_channels = Vec::with_capacity(channels.len());
    for (platform_id, picked) in channels {
        let mut ch = picked.channel.clone();
        let mut points: Vec<HistoryPoint> = history
            .iter()
            .filter(|((pid, _, _, _), _)| pid == &platform_id)
            .map(|(_, pick)| pick.point.clone())
            .collect();
        points.sort_by_key(|p| p.period_start);
        if points.len() > MAX_HISTORY {
            points = points.split_off(points.len() - MAX_HISTORY);
        }
        ch.history = points;
        merged_channels.push(ch);
    }
    merged_channels.sort_by(|a, b| a.platform_id.cmp(&b.platform_id));

    let checkin = merge_checkin(&deduped);
    let totals = build_totals(&merged_channels);
    let generated_at = deduped.iter().map(|s| s.at).max().unwrap_or(0);
    let stale_channels = merged_channels.iter().filter(|c| c.stale).count();

    Ok(MergedSnapshot {
        channels: merged_channels,
        checkin,
        totals,
        sources: deduped.len(),
        failed_sources,
        generated_at,
        stale_channels,
    })
}

fn wins(cand_at: i64, cand_src: &str, cur_at: i64, cur_src: &str) -> bool {
    cand_at > cur_at || (cand_at == cur_at && cand_src < cur_src)
}

fn dedupe_by_source<'a>(snapshots: &'a [SyncSnapshot]) -> Result<Vec<&'a SyncSnapshot>, SyncError> {
    let mut by_source: BTreeMap<&str, &SyncSnapshot> = BTreeMap::new();
    for snap in snapshots {
        snap.validate()?;
        match by_source.get(snap.source.as_str()) {
            None => {
                by_source.insert(snap.source.as_str(), snap);
            }
            Some(old) => {
                if snap.seq > old.seq {
                    by_source.insert(snap.source.as_str(), snap);
                } else if snap.seq == old.seq && snap != *old {
                    return Err(SyncError::new(
                        CONFLICT,
                        format!("来源 {} 的 seq={} 出现不同内容", snap.source, snap.seq),
                    ));
                }
            }
        }
    }
    Ok(by_source.into_values().collect())
}

fn merge_checkin(snapshots: &[&SyncSnapshot]) -> Option<CheckinSnapshot> {
    let date = snapshots
        .iter()
        .filter_map(|s| s.checkin.as_ref().map(|c| c.date.clone()))
        .max()?;

    let mut items: BTreeMap<String, (i64, &str, &CheckinItem)> = BTreeMap::new();
    for snap in snapshots {
        let Some(ck) = &snap.checkin else { continue };
        if ck.date != date {
            continue;
        }
        for item in &ck.items {
            match items.get(&item.item_id) {
                None => {
                    items.insert(item.item_id.clone(), (item.observed_at, snap.source.as_str(), item));
                }
                Some((at, src, _)) => {
                    if item.observed_at > *at
                        || (item.observed_at == *at && snap.source.as_str() < *src)
                    {
                        items.insert(item.item_id.clone(), (item.observed_at, snap.source.as_str(), item));
                    }
                }
            }
        }
    }

    if items.is_empty() {
        return None;
    }
    let mut merged: Vec<CheckinItem> = items.into_values().map(|(_, _, i)| i.clone()).collect();
    merged.sort_by(|a, b| a.item_id.cmp(&b.item_id));
    let done = merged
        .iter()
        .filter(|i| i.status == CheckinStatus::Done)
        .count() as u32;
    let pending = merged
        .iter()
        .filter(|i| i.status == CheckinStatus::Pending)
        .count() as u32;
    let unsupported = merged
        .iter()
        .filter(|i| i.status == CheckinStatus::Unsupported)
        .count() as u32;
    let unknown = merged
        .iter()
        .filter(|i| i.status == CheckinStatus::Unknown)
        .count() as u32;
    Some(CheckinSnapshot {
        date,
        time_zone: "Asia/Shanghai".into(),
        items: merged,
        done,
        total: done + pending,
        unsupported,
        unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::model::{
        ChannelKind, TotalsSnapshot,
    };

    fn channel(platform: &str, updated: i64, remaining: &str) -> ChannelSnapshot {
        ChannelSnapshot {
            platform_id: platform.into(),
            id: platform.into(),
            name: platform.into(),
            kind: ChannelKind::Money,
            unit: "CNY".into(),
            remaining: Some(remaining.into()),
            total: None,
            percent: None,
            windows: vec![],
            expiring: vec![],
            day: None,
            week: None,
            month: None,
            estimated: false,
            stale: false,
            error_code: None,
            updated_at: updated,
            history: vec![],
        }
    }

    fn snap(source: &str, seq: u64, channels: Vec<ChannelSnapshot>) -> SyncSnapshot {
        SyncSnapshot {
            v: 1,
            source: source.into(),
            seq,
            at: 1_793_500_000,
            app: "0.3.2".into(),
            channels,
            checkin: None,
            totals: TotalsSnapshot::default(),
        }
    }

    #[test]
    fn newest_whole_record_wins() {
        let a = snap(
            "s_0123456789abcdef0123456789abcdef",
            1,
            vec![channel("deepseek", 100, "10")],
        );
        let b = snap(
            "s_0123456789abcdef0123456789abcdee",
            1,
            vec![channel("deepseek", 200, "20")],
        );
        let m = merge_snapshots(&[a, b], vec![]).unwrap();
        assert_eq!(m.channels.len(), 1);
        assert_eq!(m.channels[0].remaining.as_deref(), Some("20"));
        assert_eq!(m.totals.cny.as_deref(), Some("20"));
    }

    #[test]
    fn same_time_uses_source_lexicographic() {
        let a = snap(
            "s_0123456789abcdef0123456789abcdef",
            1,
            vec![channel("deepseek", 100, "10")],
        );
        let b = snap(
            "s_0123456789abcdef0123456789abcdee",
            1,
            vec![channel("deepseek", 100, "20")],
        );
        let m = merge_snapshots(&[a, b], vec![]).unwrap();
        // `...abcdee` 的字典序小于 `...abcdef`，所以 b 的整条记录胜出。
        assert_eq!(m.channels[0].remaining.as_deref(), Some("20"));
    }

    #[test]
    fn duplicate_source_conflict_is_rejected() {
        let a = snap(
            "s_0123456789abcdef0123456789abcdef",
            1,
            vec![channel("deepseek", 100, "10")],
        );
        let b = snap(
            "s_0123456789abcdef0123456789abcdef",
            1,
            vec![channel("deepseek", 100, "20")],
        );
        let err = merge_snapshots(&[a, b], vec![]).unwrap_err();
        assert_eq!(err.code, CONFLICT);
    }

    #[test]
    fn stale_newest_is_not_replaced_by_older_fresh() {
        let mut fresh_old = channel("deepseek", 100, "10");
        fresh_old.stale = false;
        let mut stale_new = channel("deepseek", 200, "20");
        stale_new.stale = true;
        let a = snap(
            "s_0123456789abcdef0123456789abcdef",
            1,
            vec![fresh_old],
        );
        let b = snap(
            "s_0123456789abcdef0123456789abcdee",
            1,
            vec![stale_new],
        );
        let m = merge_snapshots(&[a, b], vec![]).unwrap();
        assert_eq!(m.channels[0].remaining.as_deref(), Some("20"));
        assert!(m.channels[0].stale);
    }
}
