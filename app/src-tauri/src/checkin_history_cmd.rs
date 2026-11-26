use super::*;

/// 签到中心的一天(按天聚合,`day` 已是该渠道的周期日;设计见
/// docs/product/product-checkin-center-2026-10-29.md)。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckinDayCell {
    pub(crate) day: String,
    /// 当天最后一行的时刻;空槽是 None
    pub(crate) ts: Option<i64>,
    /// claimed / already / inactive / manual / failed / pending / missed / no_data
    pub(crate) status: String,
    pub(crate) credits: Option<f64>,
    /// 当天流水条数(尝试次数)
    pub(crate) attempts: i64,
}

/// 签到中心的单渠道历史。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckinHistoryChannel {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) auto: bool,
    /// 本次开启的时刻;None = 老配置(升级前就开着),查询侧已用首条流水时刻兜底
    pub(crate) enabled_since: Option<i64>,
    /// 本月真领到的合计(只算 claimed)
    pub(crate) month_credits: f64,
    /// 该渠道的周期日"今天"(Qoder 从 10:00 起算 —— 前端零日期运算)
    pub(crate) today: String,
    /// 窗口内逐天,时间正序,末格是今天
    pub(crate) days: Vec<CheckinDayCell>,
}

/// 明细表的一行:流水 + 它属于哪个渠道(recent_checkins 的行不带渠道)。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckinHistoryRow {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(flatten)]
    pub(crate) row: tokenscope_core::store::CheckinRow,
}

/// `checkin_history` 的返回:逐天日历 + 逐条明细,一次给全(本地 SQLite,便宜)。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckinHistory {
    pub(crate) days: u32,
    pub(crate) channels: Vec<CheckinHistoryChannel>,
    /// 所有渠道合在一起,新的在前
    pub(crate) rows: Vec<CheckinHistoryRow>,
}

/// 签到中心的数据源:每渠道近 `days` 天的逐天结论 + 逐条明细。
/// 数据全在 `checkin_log`(清理策略留 400 天),这里只是把它取出来;
/// `checkin_state`(首页徽标用)不动 —— 那边只要 7 天。
#[tauri::command]
pub(crate) async fn checkin_history(
    state: State<'_, AppState>,
    days: Option<u32>,
) -> Result<CheckinHistory, String> {
    let days = days.unwrap_or(90).clamp(7, 365) as i64;
    let now = now_ts();
    let cfg = lock_rw(&state.config).clone();
    let reader = state.read_store.clone();
    tokio::task::spawn_blocking(move || {
    let mut channels = Vec::new();
    let mut rows = Vec::new();
    {
        let store = lock_rw(&reader);
        for d in checkin::CHECKINS.iter() {
            let id = d.id;
            let c = cfg.checkin.get(id).cloned().unwrap_or_default();
            let today = cycle_day_of(id, now);
            // 老配置没有 enabled_since:回退到首条流水的时刻 —— 它之前的日子
            // 本来就没有记录,只能如实说"无数据",不能画成"没签"。
            let enabled_since = c
                .enabled_since
                .or_else(|| store.first_checkin_ts(id).unwrap_or(None));
            let since_ts = now - (days - 1) * 86_400;
            let aggs = store.checkin_day_rows(id, &cycle_day_of(id, since_ts)).unwrap_or_default();
            let by_day: std::collections::HashMap<&str, &tokenscope_core::store::CheckinDayAgg> =
                aggs.iter().map(|a| (a.day.as_str(), a)).collect();
            let expect_from = enabled_since
                .map(|t| checkin::cycle_day(t, d.day_roll_hour))
                .unwrap_or_default();
            let mut cells = Vec::with_capacity(days as usize);
            for i in 0..days {
                let day = cycle_day_of(id, since_ts + i * 86_400);
                // 北京无夏令时,ts 每 86_400 秒一步,cycle_day 严格逐日递增 —— 不会重复
                let agg = by_day.get(day.as_str());
                // 该不该签:现在开着,且这一天不早于"本次开启日"
                let expected = c.enabled && !expect_from.is_empty() && expect_from.as_str() <= day.as_str();
                let (status, ts, credits, attempts) = match agg {
                    Some(a) => (
                        checkin::day_status(Some(&a.outcome), expected, day == today),
                        Some(a.ts),
                        a.credits,
                        a.count,
                    ),
                    None => (checkin::day_status(None, expected, day == today), None, None, 0),
                };
                cells.push(CheckinDayCell {
                    day,
                    ts,
                    status: status.to_string(),
                    credits,
                    attempts,
                });
            }
            for r in store.recent_checkins(id, since_ts, 1000).unwrap_or_default() {
                rows.push(CheckinHistoryRow {
                    id: id.to_string(),
                    name: d.name.to_string(),
                    row: r,
                });
            }
            channels.push(CheckinHistoryChannel {
                id: id.to_string(),
                name: d.name.to_string(),
                enabled: c.enabled,
                auto: c.auto,
                enabled_since,
                month_credits: store
                    .checkin_month_credits(id, today.get(..7).unwrap_or(today.as_str()))
                    .unwrap_or(0.0),
                today,
                days: cells,
            });
        }
    }
    rows.sort_by(|a, b| b.row.ts.cmp(&a.row.ts));
    Ok(CheckinHistory {
        days: days as u32,
        channels,
        rows,
    })
    }).await.map_err(|e| format!("读取签到历史失败: {e}"))?
}
