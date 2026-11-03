//! 从本机 `ChannelView` + SQLite 快照构建待发布的 SyncSnapshot。
//!
//! 这是“只上传查询结果”的核心闸门：只读 `ChannelView` 的展示字段，
//! 原始错误文本、authLabel、accounts、extra、本机路径一概不碰。

use std::collections::BTreeMap;

use crate::checkin;
use crate::config::Config;
use crate::fetch::{current_key_fp, key_fp as credential_fp, local_tz_offset, ChannelView};
use crate::providers::{self, Kind};
use crate::secrets;
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
            key: w.key.clone(),
            label: sanitize_name(&w.label, "窗口"),
            remain_percent: decimal_string(w.remain_percent),
            reset_at: None,
            reset_text: w.resets_at.as_deref().map(short_reset_text),
            main: w.main,
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
        history: {
            let key_fp = history_key_fp(view);
            build_history(
                HistoryInput {
                    channel_id: &view.id,
                    unit: &unit,
                    accounts: view.accounts,
                    current_remaining: view.remaining,
                    updated_at: view.updated_at,
                    day_today: view.day,
                    gap_day_from: view.gap_day_from,
                    gap_days: &view.gap_days,
                    key_fp: key_fp.as_deref(),
                },
                store,
                now,
            )
        },
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

pub(crate) struct HistoryInput<'a> {
    pub channel_id: &'a str,
    pub unit: &'a str,
    /// 这个渠道历史上有几本账（不同 key 指纹）。
    pub accounts: i64,
    pub current_remaining: Option<f64>,
    pub updated_at: i64,
    /// 今天的实时消耗（`ChannelView.day`），历史日没有这个值。
    pub day_today: Option<f64>,
    pub gap_day_from: Option<i64>,
    pub gap_days: &'a [i64],
    /// 当前凭据指纹；None = 不区分/取不到。
    pub key_fp: Option<&'a str>,
}

/// 内置 Keyring 渠道用当前密钥指纹；自定义渠道解析本机密钥后取指纹。
/// App/WebSession 型凭据会轮换，返回 None（与桌面曲线同一口径）。
fn history_key_fp(view: &ChannelView) -> Option<String> {
    if let Some(def) = providers::PROVIDERS.iter().find(|p| p.id == view.id) {
        let fp = current_key_fp(def);
        return if fp.is_empty() { None } else { Some(fp) };
    }
    let fp = secrets::resolve(&view.id)
        .ok()
        .and_then(|r| r.key().map(credential_fp))
        .unwrap_or_default();
    if fp.is_empty() {
        None
    } else {
        Some(fp)
    }
}

/// 最近 7 个自然日的余额 + 消耗。
///
/// - `balance`：当天最后一条**成功**快照；没有观测就是 null。
/// - `spent`：今天用 `ChannelView.day`（今天到目前为止），历史日只用完整覆盖的
///   `daily_summary`；有缺口的历史日给 null，不拿部分数据冒充全天。
/// - `complete`：这一天有没有完整观测。缺口日/采样不足为 false，手机据此断线。
/// - `revised_at`：真实最后观测时间；没有观测时用当天零点，**绝不用 now**，
///   否则合并时一条空历史会压过另一台电脑的真实记录。
/// - 多本账但取不到当前指纹时整段返回空历史，绝不把两个账户拼成一条线。
pub(crate) fn build_history(input: HistoryInput<'_>, store: &Store, now: i64) -> Vec<HistoryPoint> {
    let tz = local_tz_offset();
    let today_start = ((now + tz).div_euclid(86_400)) * 86_400 - tz;
    let from = today_start - (MAX_HISTORY as i64 - 1) * 86_400;
    let tz_label = offset_label(tz);
    let unknown_accounts = input.accounts > 1 && input.key_fp.is_none();

    (0..MAX_HISTORY)
        .map(|i| {
            let start = from + i as i64 * 86_400;
            let end = start + 86_400;
            let is_today = start == today_start;

            let last = if !unknown_accounts && start <= now {
                let to = end.saturating_sub(1).min(now);
                store
                    .last_snapshot_between(input.channel_id, input.key_fp, start, to)
                    .ok()
                    .flatten()
                    .or_else(|| {
                        if is_today {
                            input
                                .current_remaining
                                .map(|v| (input.updated_at.max(1), v))
                        } else {
                            None
                        }
                    })
            } else {
                None
            };

            let balance = last.and_then(|(_, v)| decimal_string(v));
            let has_data = last.is_some() || (is_today && input.day_today.is_some());
            let gap = input.gap_days.contains(&start)
                || (is_today && input.gap_day_from.is_some());
            let complete = !unknown_accounts
                && has_data
                && !gap
                // 今天还没有实时 day 值 = 消耗未知，不能标成完整。
                && (!is_today || input.day_today.is_some());

            // 最近 7 天在明细快照里：用与桌面 day/week/month 相同的余额差规则。
            // daily_summary 只在 90 天归档后才有内容，不能拿来算最近一周。
            let spent = if unknown_accounts || !complete {
                None
            } else if is_today {
                input.day_today.and_then(decimal_string)
            } else {
                let consumed = match input.key_fp {
                    Some(fp) => store
                        .consumed_in_window_fp(input.channel_id, fp, start, end - 1),
                    None => store.consumed_in_window(input.channel_id, start, end - 1),
                };
                consumed.ok().and_then(decimal_string)
            };

            let revised_at = last.map(|(ts, _)| ts).unwrap_or(start).max(1);

            HistoryPoint {
                period_start: start,
                period_end: end,
                time_zone: tz_label.clone(),
                unit: input.unit.to_string(),
                balance,
                spent,
                estimated: true,
                complete,
                revised_at,
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

    #[test]
    fn last_snapshot_between_is_scoped_to_current_fingerprint() {
        let store = Store::open_memory().unwrap();
        let now = 1_793_500_800;
        let tz = local_tz_offset();
        let day = ((now + tz).div_euclid(86_400)) * 86_400 - tz;
        store.record_fp("x", "a", day + 10, Some(10.0), None, None, "amount").unwrap();
        store.record_fp("x", "a", day + 20, Some(20.0), None, None, "amount").unwrap();
        store.record_fp("x", "b", day + 30, Some(30.0), None, None, "amount").unwrap();
        assert_eq!(
            store.last_snapshot_between("x", Some("a"), day, day + 86_399).unwrap(),
            Some((day + 20, 20.0))
        );
        assert_eq!(
            store.last_snapshot_between("x", Some("b"), day, day + 86_399).unwrap(),
            Some((day + 30, 30.0))
        );
        assert_eq!(
            store.last_snapshot_between("x", Some("c"), day, day + 86_399).unwrap(),
            None
        );
    }

    #[test]
    fn history_uses_current_account_and_preserves_gaps() {
        let store = Store::open_memory().unwrap();
        let now = 1_793_500_800;
        let tz = local_tz_offset();
        let today_start = ((now + tz).div_euclid(86_400)) * 86_400 - tz;
        let yesterday = today_start - 86_400;
        let fp_a = "aaaaaaaaaaaaaaaaaaaaaaaa";
        let fp_b = "bbbbbbbbbbbbbbbbbbbbbbbb";

        // 昨天：A 110→100（花 10），B 60→40（花 20）。
        store.record_fp("deepseek", fp_a, yesterday + 3_600, Some(110.0), None, None, "amount").unwrap();
        store.record_fp("deepseek", fp_a, yesterday + 7_200, Some(100.0), None, None, "amount").unwrap();
        store.record_fp("deepseek", fp_b, yesterday + 3_600, Some(60.0), None, None, "amount").unwrap();
        store.record_fp("deepseek", fp_b, yesterday + 7_200, Some(40.0), None, None, "amount").unwrap();
        // 今天：A 记录 88。
        store.record_fp("deepseek", fp_a, today_start + 3_600, Some(88.0), None, None, "amount").unwrap();

        let input = |fp: Option<&'static str>, accounts: i64| HistoryInput {
            channel_id: "deepseek",
            unit: "CNY",
            accounts,
            current_remaining: Some(88.0),
            updated_at: today_start + 3_600,
            day_today: Some(1.2),
            gap_day_from: None,
            gap_days: &[],
            key_fp: fp,
        };

        let a = build_history(input(Some(fp_a), 2), &store, now);
        assert_eq!(a.len(), MAX_HISTORY);
        let y = a.iter().find(|p| p.period_start == yesterday).unwrap();
        assert_eq!(y.spent.as_deref(), Some("10"));
        assert_eq!(y.balance.as_deref(), Some("100"));
        assert!(y.complete);
        assert_eq!(y.revised_at, yesterday + 7_200);
        let today = a.last().unwrap();
        assert_eq!(today.spent.as_deref(), Some("1.2"));
        assert_eq!(today.balance.as_deref(), Some("88"));
        assert!(today.complete);

        let b = build_history(input(Some(fp_b), 2), &store, now);
        let yb = b.iter().find(|p| p.period_start == yesterday).unwrap();
        assert_eq!(yb.spent.as_deref(), Some("20"));
        assert_eq!(yb.balance.as_deref(), Some("40"));

        // 多本账但取不到当前指纹：整段不可信，不能把 A/B 拼起来。
        let unknown = build_history(input(None, 2), &store, now);
        assert!(unknown
            .iter()
            .all(|p| p.spent.is_none() && p.balance.is_none() && !p.complete));

        // 两天前没有观测：null + complete=false；revised_at 是当天零点，不是 now。
        let gap_day = today_start - 2 * 86_400;
        let g = a.iter().find(|p| p.period_start == gap_day).unwrap();
        assert!(g.spent.is_none() && g.balance.is_none() && !g.complete);
        assert_eq!(g.revised_at, gap_day);
    }
}
