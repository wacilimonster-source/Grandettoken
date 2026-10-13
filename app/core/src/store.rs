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
        // 双开或 WAL 写者竞争时,SQLite 默认立刻回 SQLITE_BUSY —— 之前快照就是
        // 这样被静默吞掉的;等 5 秒锁再报错,把可恢复的冲突变成可恢复的等待(O-7)。
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
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

            -- 签到流水:唯一能回答"到底哪天断了"的凭据(设计稿第 9 节)。
            -- 只记结果与数额,凭据/令牌一律不进这里。
            CREATE TABLE IF NOT EXISTS checkin_log (
                id       INTEGER PRIMARY KEY AUTOINCREMENT,
                channel  TEXT    NOT NULL,
                ts       INTEGER NOT NULL,
                day      TEXT    NOT NULL,
                outcome  TEXT    NOT NULL,
                credits  REAL,
                attempts INTEGER NOT NULL DEFAULT 0,
                detail   TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_checkin_lookup
                ON checkin_log (channel, ts DESC);

            -- 密钥箱的**索引**。注意:这张表里没有密钥字段,也永远不该有 ——
            -- 密文在 Windows 凭据管理器(服务名 TokenScope:vault),这里只存
            -- 平台/标签/尾号/备注/时间,好让列表**不需要解密**就能渲染。
            CREATE TABLE IF NOT EXISTS vault_items (
                id         TEXT PRIMARY KEY,
                platform   TEXT    NOT NULL,
                label      TEXT    NOT NULL,
                hint       TEXT    NOT NULL DEFAULT '',
                note       TEXT    NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_vault_platform
                ON vault_items (platform, created_at DESC);
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

    /// 指定时间窗口内的消耗推算值。
    /// 窗口起点前有基线 → 精确到窗口(跨起点的基线对按时间折算);
    /// 没有基线(软件那天还没装上/机器整段没开)→ 退化为「窗口内首快照起算」
    /// 的**下限估算** —— 界面本来就标"推算",报一个偏小的真值比整格「——」有用
    /// (踩过:装当周周一/月首没有快照,本周消耗一直显示数据不足)。
    /// 窗口内不足两条快照才算真的无从推算,返回 None。
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

        // 窗口起点之前要有基线;没有就退到窗口内首行起算(下限)
        let has_baseline = rows.iter().any(|(ts, _)| *ts <= from_ts);
        if !has_baseline {
            let win: Vec<f64> = rows
                .iter()
                .filter(|(ts, _)| *ts >= from_ts)
                .map(|(_, r)| *r)
                .collect();
            if win.len() < 2 {
                return Ok(None);
            }
            let mut sum = 0.0;
            for pair in win.windows(2) {
                sum += (pair[0] - pair[1]).max(0.0);
            }
            return Ok(Some((sum * 100.0).round() / 100.0));
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

    /// 某渠道最近的签到流水(新的在前)。
    pub fn recent_checkins(&self, channel: &str, since_ts: i64) -> Result<Vec<CheckinRow>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ts, day, outcome, credits, attempts, detail FROM checkin_log
                 WHERE channel = ?1 AND ts >= ?2 ORDER BY ts DESC LIMIT 400",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![channel, since_ts], |r| {
                Ok(CheckinRow {
                    ts: r.get(0)?,
                    day: r.get(1)?,
                    outcome: r.get(2)?,
                    credits: r.get(3)?,
                    attempts: r.get(4)?,
                    detail: r.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
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
            .query_map(params![channel, day], |r| {
                Ok(CheckinRow {
                    ts: r.get(0)?,
                    day: r.get(1)?,
                    outcome: r.get(2)?,
                    credits: r.get(3)?,
                    attempts: r.get(4)?,
                    detail: r.get(5)?,
                })
            })
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

/// 密钥箱的一条**索引**(不含密钥)。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultItem {
    pub id: String,
    pub platform: String,
    pub label: String,
    /// 尾 4 位掩码(存下来,列表不需要解密)
    pub hint: String,
    pub note: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Store {
    pub fn vault_list(&self) -> Result<Vec<VaultItem>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, platform, label, hint, note, created_at, updated_at
                 FROM vault_items ORDER BY platform COLLATE NOCASE, created_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(VaultItem {
                    id: r.get(0)?,
                    platform: r.get(1)?,
                    label: r.get(2)?,
                    hint: r.get(3)?,
                    note: r.get(4)?,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 写索引(新建或改名/改备注)。密钥本身由 `secrets::vault_set` 负责。
    pub fn vault_upsert(&self, it: &VaultItem) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO vault_items (id, platform, label, hint, note, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                   platform = excluded.platform, label = excluded.label,
                   hint = excluded.hint, note = excluded.note, updated_at = excluded.updated_at",
                params![
                    it.id,
                    it.platform,
                    it.label,
                    it.hint,
                    it.note,
                    it.created_at,
                    it.updated_at
                ],
            )
            .map_err(|e| format!("写密钥箱索引失败: {e}"))?;
        Ok(())
    }

    pub fn vault_delete(&self, id: &str) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM vault_items WHERE id = ?1", params![id])
            .map_err(|e| format!("删密钥箱索引失败: {e}"))?;
        Ok(())
    }

    pub fn vault_get(&self, id: &str) -> Result<Option<VaultItem>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, platform, label, hint, note, created_at, updated_at
                 FROM vault_items WHERE id = ?1",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map(params![id], |r| {
                Ok(VaultItem {
                    id: r.get(0)?,
                    platform: r.get(1)?,
                    label: r.get(2)?,
                    hint: r.get(3)?,
                    note: r.get(4)?,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        Ok(rows.next().and_then(|r| r.ok()))
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
        // 窗口起点 4000 之前没有基线,窗口内也只有一条 → 真的无从推算
        assert_eq!(s.consumption_since("p", 4000, 6000).unwrap(), None);
    }

    /// 无窗口前基线(装软件当周/当月):从窗口内首快照起算下限,而不是整格「——」。
    #[test]
    fn missing_baseline_falls_back_to_window_floor() {
        let s = mem();
        // 快照从 3000 开始,窗口从 1000 起:100→90→充值 200→195
        for (ts, rem) in [(3000, 100.0), (4000, 90.0), (5000, 200.0), (6000, 195.0)] {
            s.record("p", ts, Some(rem), None, None, "amount").unwrap();
        }
        // 下限 = 窗口内相邻下降之和 = 10 + 0 + 5(3000 之前那段看不到,不算)
        assert_eq!(s.consumption_since("p", 1000, 6000).unwrap(), Some(15.0));
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

        // 对照:consumption_since 无窗口前基线时退化为窗口内下限估算(同为 15)
        assert_eq!(s.consumption_since("p", 1000, 6000).unwrap(), Some(15.0));
    }
}
