//! SQLite 快照库。
//!
//! 每次成功取数都落一条快照。充值型渠道(DeepSeek / Hapi)的接口只给当前余额、
//! 不给累计消耗,所以"今日/本周/本月消耗"只能靠相邻快照的余额差值推算:
//!
//! ```text
//! consumption(t0,t1) = sum of max(0, remaining[i-1] - remaining[i])
//! ```
//!
//! 负差值(充值、配额重置)被 clamp 成 0,不会被误算成负消耗。
//! 代价是程序未运行的时段形成空洞,数字偏小,界面需标注为推算值。

use rusqlite::{params, Connection};
use std::path::Path;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建数据目录失败: {e}"))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("打开数据库失败: {e}"))?;
        let s = Self { conn };
        s.init_schema()?;
        Ok(s)
    }

    /// 测试用:内存库,不落盘,给跨模块的组装逻辑(如申请制状态)当输入。
    #[cfg(test)]
    pub fn open_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("打开数据库失败: {e}"))?;
        let s = Self { conn };
        s.init_schema()?;
        Ok(s)
    }

    fn init_schema(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;

            CREATE TABLE IF NOT EXISTS snapshots (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                provider_id TEXT    NOT NULL,
                ts          INTEGER NOT NULL,
                remaining   REAL,
                used        REAL,
                total       REAL,
                kind        TEXT    NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_snap_lookup
                ON snapshots (provider_id, ts DESC);

            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
            )
            .map_err(|e| format!("初始化数据库失败: {e}"))?;
        Ok(())
    }

    pub fn record(
        &self,
        provider_id: &str,
        ts: i64,
        remaining: Option<f64>,
        used: Option<f64>,
        total: Option<f64>,
        kind: &str,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO snapshots (provider_id, ts, remaining, used, total, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![provider_id, ts, remaining, used, total, kind],
            )
            .map_err(|e| format!("写入快照失败: {e}"))?;
        Ok(())
    }

    /// 指定时间窗口内的消耗推算值。窗口起点没有前置快照时返回 None,
    /// 因为那时段的数据是空洞,报 0 会让人以为"没花钱"。
    pub fn consumption_since(
        &self,
        provider_id: &str,
        from_ts: i64,
        now_ts: i64,
    ) -> Result<Option<f64>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ts, remaining FROM snapshots
                 WHERE provider_id = ?1 AND remaining IS NOT NULL
                   AND ts >= ?2 - 86400 AND ts <= ?3
                 ORDER BY ts ASC",
            )
            .map_err(|e| format!("查询失败: {e}"))?;

        let rows: Vec<(i64, f64)> = stmt
            .query_map(params![provider_id, from_ts, now_ts], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|e| format!("读取快照失败: {e}"))?
            .filter_map(Result::ok)
            .collect();

        // 窗口起点之前要有基线,否则无法判断窗口内的变化量
        let has_baseline = rows.iter().any(|(ts, _)| *ts <= from_ts);
        if !has_baseline {
            return Ok(None);
        }

        let mut sum = 0.0;
        let mut prev: Option<(i64, f64)> = None;
        for (ts, remaining) in rows {
            if let Some((pts, prem)) = prev {
                if pts < from_ts {
                    if ts < from_ts {
                        prev = Some((ts, remaining));
                        continue;
                    }
                    // 跨窗口起点的一对:基线→起点的下降发生在窗口外,不该记账。
                    // 按时间线性折算基线余额到 from_ts,再与窗口首行求差。
                    // (之前整段计入,"今日消耗"会把昨晚的花销背进今天)
                    let span = ts - pts;
                    let base = if span > 0 {
                        let k = (from_ts - pts) as f64 / span as f64;
                        prem - (prem - remaining) * k
                    } else {
                        prem
                    };
                    sum += (base - remaining).max(0.0);
                } else {
                    sum += (prem - remaining).max(0.0);
                }
            }
            prev = Some((ts, remaining));
        }
        Ok(Some((sum * 100.0).round() / 100.0))
    }

    /// 窗口内的消耗合计:只累加相邻快照之间的下降(上升 = 充值/申请,不计)。
    /// 与 consumption_since 的差别:不要求窗口起点之前有基线 —— 窗口内第一行
    /// 就是基准,因为"近 7 天日均"这类推算只关心窗口自身。
    pub fn consumed_in_window(&self, provider_id: &str, from_ts: i64, to_ts: i64) -> Result<f64, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT remaining FROM snapshots
                 WHERE provider_id = ?1 AND remaining IS NOT NULL
                   AND ts >= ?2 AND ts <= ?3
                 ORDER BY ts ASC",
            )
            .map_err(|e| format!("查询失败: {e}"))?;

        let vals: Vec<f64> = stmt
            .query_map(params![provider_id, from_ts, to_ts], |r| r.get(0))
            .map_err(|e| format!("读取快照失败: {e}"))?
            .filter_map(Result::ok)
            .collect();

        let mut sum = 0.0;
        for pair in vals.windows(2) {
            sum += (pair[0] - pair[1]).max(0.0);
        }
        Ok((sum * 100.0).round() / 100.0)
    }

    /// 窗口内最早一条快照的时间 —— 用来判断数据覆盖了多长,太短就不做推算。
    pub fn first_ts_since(&self, provider_id: &str, from_ts: i64) -> Result<Option<i64>, String> {
        self.conn
            .query_row(
                "SELECT MIN(ts) FROM snapshots WHERE provider_id = ?1 AND ts >= ?2",
                params![provider_id, from_ts],
                |r| r.get::<_, Option<i64>>(0),
            )
            .map_err(|e| format!("查询失败: {e}"))
    }

    /// 最近一次"余额跳升"的时间(申请额度 / 充值到账)。
    /// 只有跳升幅度 >= min_delta 才算,避免四舍五入的小抖动被误判成一次申请。
    pub fn last_jump_since(
        &self,
        provider_id: &str,
        from_ts: i64,
        min_delta: f64,
    ) -> Result<Option<i64>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ts, remaining FROM snapshots
                 WHERE provider_id = ?1 AND remaining IS NOT NULL AND ts >= ?2
                 ORDER BY ts ASC",
            )
            .map_err(|e| format!("查询失败: {e}"))?;

        let rows: Vec<(i64, f64)> = stmt
            .query_map(params![provider_id, from_ts], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| format!("读取快照失败: {e}"))?
            .filter_map(Result::ok)
            .collect();

        // 顺序扫描,取最后一次跳升
        let mut last = None;
        for pair in rows.windows(2) {
            if pair[1].1 - pair[0].1 >= min_delta {
                last = Some(pair[1].0);
            }
        }
        Ok(last)
    }

    /// 用于展开区的迷你柱图。按小时分桶取窗口内最低余额,再转成差值曲线。
    pub fn series(&self, provider_id: &str, from_ts: i64, buckets: usize) -> Result<Vec<f64>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ts, remaining FROM snapshots
                 WHERE provider_id = ?1 AND remaining IS NOT NULL AND ts >= ?2
                 ORDER BY ts ASC",
            )
            .map_err(|e| format!("查询失败: {e}"))?;

        let rows: Vec<(i64, f64)> = stmt
            .query_map(params![provider_id, from_ts], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| format!("读取快照失败: {e}"))?
            .filter_map(Result::ok)
            .collect();

        if rows.len() < 2 {
            return Ok(vec![0.0; buckets]);
        }

        let now = rows.last().unwrap().0;
        let span = (now - from_ts).max(1) as f64;
        let mut out = vec![0.0f64; buckets];
        for pair in rows.windows(2) {
            let (t0, r0) = (pair[0].0, pair[0].1);
            let delta = (r0 - pair[1].1).max(0.0);
            let idx = (((t0 - from_ts) as f64 / span) * buckets as f64) as usize;
            let idx = idx.min(buckets - 1);
            out[idx] += delta;
        }
        Ok(out)
    }

    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .ok()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(|e| format!("保存设置失败: {e}"))?;
        Ok(())
    }

    /// 清理 90 天前的快照,避免库无限增长。
    pub fn prune(&self, before_ts: i64) -> Result<usize, String> {
        self.conn
            .execute("DELETE FROM snapshots WHERE ts < ?1", params![before_ts])
            .map_err(|e| format!("清理旧快照失败: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Store {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT, provider_id TEXT NOT NULL,
                ts INTEGER NOT NULL, remaining REAL, used REAL, total REAL, kind TEXT NOT NULL);
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        Store { conn }
    }

    #[test]
    fn consumption_sums_decreases_and_ignores_topups() {
        let s = mem();
        // 余额 100 -> 90 -> 85 -> 充值回 200 -> 195
        for (ts, rem) in [(1000, 100.0), (2000, 90.0), (3000, 85.0), (4000, 200.0), (5000, 195.0)] {
            s.record("p", ts, Some(rem), None, None, "amount").unwrap();
        }
        // 消耗 = 10 + 5 + 0(充值不算) + 5 = 20
        assert_eq!(s.consumption_since("p", 1000, 5000).unwrap(), Some(20.0));
    }

    #[test]
    fn consumption_is_none_without_baseline() {
        let s = mem();
        s.record("p", 5000, Some(10.0), None, None, "amount").unwrap();
        // 窗口起点 4000 之前没有快照 → 数据空洞,不能报 0
        assert_eq!(s.consumption_since("p", 4000, 6000).unwrap(), None);
    }

    /// 跨窗口起点的基线对:窗口外那段时间的下降不能记进窗口
    /// (修复"今日消耗把昨晚花销背进今天"的越界计数)。
    #[test]
    fn baseline_pair_is_prorated_to_window_start() {
        let s = mem();
        // 0 时点余额 100,100 时点余额 80:线性掉 20。窗口从 50 起 → 只算后一半 10。
        s.record("p", 0, Some(100.0), None, None, "amount").unwrap();
        s.record("p", 100, Some(80.0), None, None, "amount").unwrap();
        assert_eq!(s.consumption_since("p", 50, 200).unwrap(), Some(10.0));
        // 窗口起点恰好等于基线时刻:退化为整段计入
        assert_eq!(s.consumption_since("p", 0, 200).unwrap(), Some(20.0));
        // 起点贴着窗口首行:只剩 0 段
        assert_eq!(s.consumption_since("p", 99, 200).unwrap(), Some(0.2));
    }

    #[test]
    fn settings_roundtrip() {
        let s = mem();
        s.set_setting("interval", "300").unwrap();
        assert_eq!(s.get_setting("interval").as_deref(), Some("300"));
        s.set_setting("interval", "60").unwrap();
        assert_eq!(s.get_setting("interval").as_deref(), Some("60"));
    }

    #[test]
    fn last_jump_finds_most_recent_application() {
        let s = mem();
        // 18.99 -> 申请补到 200 -> 慢慢花 -> 再申请一次 -> 又花一点
        for (ts, rem) in [
            (1000, 18.99),
            (2000, 200.0), // 第一次申请
            (3000, 180.0),
            (4000, 150.0),
            (5000, 195.0), // 第二次申请(补到 200 后花掉 5)
            (6000, 190.0),
        ] {
            s.record("p", ts, Some(rem), None, None, "amount").unwrap();
        }
        // 只认最近的跳升
        assert_eq!(s.last_jump_since("p", 0, 20.0).unwrap(), Some(5000));
        // 阈值高于实际跳升(45)-> 只剩第一次(181.01)
        assert_eq!(s.last_jump_since("p", 0, 182.0).unwrap(), None);
        // 窗口把第一次跳升切掉
        assert_eq!(s.last_jump_since("p", 2500, 20.0).unwrap(), Some(5000));
    }

    #[test]
    fn small_wobbles_are_not_applications() {
        let s = mem();
        for (ts, rem) in [(1000, 100.0), (2000, 100.4), (3000, 99.8), (4000, 105.0)] {
            s.record("p", ts, Some(rem), None, None, "amount").unwrap();
        }
        // 5 元的抖动在 20 元阈值下不算申请
        assert_eq!(s.last_jump_since("p", 0, 20.0).unwrap(), None);
        assert_eq!(s.last_jump_since("p", 0, 4.0).unwrap(), Some(4000));
    }

    #[test]
    fn consumed_in_window_ignores_topups_and_needs_no_baseline() {
        let s = mem();
        for (ts, rem) in [(3000, 100.0), (4000, 90.0), (5000, 200.0), (6000, 195.0)] {
            s.record("p", ts, Some(rem), None, None, "amount").unwrap();
        }
        // 10 + 0(申请补到 200,不算消耗) + 5 = 15
        assert_eq!(s.consumed_in_window("p", 1000, 6000).unwrap(), 15.0);
        // 窗口从历史中间开始:只用窗口内的相邻行,得到 0 + 5
        assert_eq!(s.consumed_in_window("p", 3500, 6000).unwrap(), 5.0);
        assert_eq!(s.first_ts_since("p", 3500).unwrap(), Some(4000));

        // 对照:consumption_since 需要窗口起点之前有基线,这里是数据空洞 → None
        assert_eq!(s.consumption_since("p", 1000, 6000).unwrap(), None);
    }
}
