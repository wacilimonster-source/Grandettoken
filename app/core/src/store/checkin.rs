use super::{params, Store};

/// 一条签到流水(界面直接用)。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinRow {
    pub ts: i64,
    pub day: String,
    /// 结果短码(claimed / already / retry:… / auth / shape:… / failed:…)
    pub outcome: String,
    pub credits: Option<f64>,
    pub attempts: i64,
    pub detail: Option<String>,
}

/// 一天聚合出的一条签到结论(签到中心的日历带直接用)。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinDayAgg {
    pub day: String,
    /// 当天最后一行的时刻
    pub ts: i64,
    /// 当天最后一行的结果短码
    pub outcome: String,
    pub credits: Option<f64>,
    /// 当天的流水条数(尝试次数)
    pub count: i64,
}

impl Store {
    /// 记一条签到流水。失败只打日志不打断 —— 签到本身已经发生,记不上账不该回滚事实。
    pub fn record_checkin(
        &self,
        channel: &str,
        ts: i64,
        day: &str,
        outcome: &str,
        credits: Option<f64>,
        attempts: i64,
        detail: &str,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO checkin_log (channel, ts, day, outcome, credits, attempts, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![channel, ts, day, outcome, credits, attempts, detail],
            )
            .map_err(|e| format!("写签到流水失败: {e}"))?;
        Ok(())
    }

    /// 某渠道最近的签到流水(新的在前)。`limit` 由调用方定:徽标只要 7 天,
    /// 签到中心的明细窗口是 90 天,失败重试的一天可能有多条,400 不够用。
    pub fn recent_checkins(
        &self,
        channel: &str,
        since_ts: i64,
        limit: usize,
    ) -> Result<Vec<CheckinRow>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ts, day, outcome, credits, attempts, detail FROM checkin_log
                 WHERE channel = ?1 AND ts >= ?2 ORDER BY ts DESC LIMIT ?3",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![channel, since_ts, limit as i64], row_to_checkin)
            .map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 某渠道**按天聚合**的签到流水:每天取最后一行,新的在前。
    ///
    /// 裸列(outcome/credits)配 `MAX(ts)` 是 SQLite 的文档化行为:查询里恰有一个
    /// min/max 聚合时,裸列取自 max 所在的那一行 —— 节拍器失败重试会让一天有多条,
    /// 日的结论以当天最后一次为准,与 [`Self::checkin_of_day`] 同一口径。
    /// `COUNT(*)` 是当天的流水条数(尝试次数),不受该语义影响。
    pub fn checkin_day_rows(&self, channel: &str, since_day: &str) -> Result<Vec<CheckinDayAgg>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT day, MAX(ts), outcome, credits, COUNT(*) FROM checkin_log
                 WHERE channel = ?1 AND day >= ?2 GROUP BY day ORDER BY day DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![channel, since_day], |r| {
                Ok(CheckinDayAgg {
                    day: r.get(0)?,
                    ts: r.get(1)?,
                    outcome: r.get(2)?,
                    credits: r.get(3)?,
                    count: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 该渠道**有记录以来**最早的一条流水时刻。签到中心给老配置兜底:
    /// `enabled_since` 还没来得及记录(升级前就开着)时,用它当"该签了"的起点 ——
    /// 它之前的日子本来就没有记录,只能如实说"无数据",不能画成"没签"。
    pub fn first_checkin_ts(&self, channel: &str) -> Result<Option<i64>, String> {
        let v: Option<i64> = self
            .conn
            .query_row(
                "SELECT MIN(ts) FROM checkin_log WHERE channel = ?1",
                params![channel],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(v)
    }

    /// 某天该渠道的最后一条流水(判"今天处理过了吗")。
    pub fn checkin_of_day(&self, channel: &str, day: &str) -> Result<Option<CheckinRow>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ts, day, outcome, credits, attempts, detail FROM checkin_log
                 WHERE channel = ?1 AND day = ?2 ORDER BY ts DESC LIMIT 1",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map(params![channel, day], row_to_checkin)
            .map_err(|e| e.to_string())?;
        Ok(rows.next().and_then(|r| r.ok()))
    }

    /// 本月的签到合计(界面上的"这个月自动领了多少分")。只算真领到的。
    pub fn checkin_month_credits(&self, channel: &str, month_prefix: &str) -> Result<f64, String> {
        let v: Option<f64> = self
            .conn
            .query_row(
                "SELECT SUM(credits) FROM checkin_log
                 WHERE channel = ?1 AND day LIKE ?2 AND outcome LIKE 'claimed%'",
                params![channel, format!("{month_prefix}%")],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(v.unwrap_or(0.0))
    }
}

fn row_to_checkin(r: &rusqlite::Row<'_>) -> rusqlite::Result<CheckinRow> {
    Ok(CheckinRow {
        ts: r.get(0)?,
        day: r.get(1)?,
        outcome: r.get(2)?,
        credits: r.get(3)?,
        attempts: r.get(4)?,
        detail: r.get(5)?,
    })
}
