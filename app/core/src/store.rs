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

/// 变动台账里"同方向合并"的时间窗:轮询是分钟级的,一次连续消费会攒成几十条。
/// 20 分钟能把最长的退避轮询(15 分钟)也并进同一条,又不会把"隔小时的两笔"误并。
const MERGE_GAP: i64 = 20 * 60;

/// 判"数据有缺口"的空档线:窗口起点/一天的两头离开观测时刻超过它,才算缺口。
/// 正常轮询是分钟级的,不会被误伤。定在 store 这一层,因为**归档(日级摘要)、
/// 曲线、界面文案三处都用它**,必须只有一个数字。
pub const OBSERVE_GAP_SEC: i64 = 10 * 60;

/// 一个窗口里的快照覆盖情况(缺口标注用)。
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowCoverage {
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    pub count: i64,
}

/// 某一自然日的快照覆盖(长期趋势柱图的"缺口日"判据)。
#[derive(Debug, Clone, Copy)]
pub struct DayCoverage {
    pub day_start: i64,
    pub first_ts: i64,
    pub last_ts: i64,
    pub count: i64,
}

/// 变动台账的一条(同方向的连续变动已聚合;`delta` 负 = 消费、正 = 入账)。
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeEntry {
    pub start_ts: i64,
    pub end_ts: i64,
    pub delta: f64,
    pub remaining: f64,
    /// 参与聚合的轮询条数(1 = 单笔)
    pub samples: i64,
    /// 与上一次变动之间的空档秒数(>0 = 中间有段时间没观测);窗口内第一条为 0
    pub gap_secs: i64,
}

/// 长期趋势的一条日级摘要(90 天外的明细归档出来的)。
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaySummary {    pub day_start: i64,
    pub consumed: f64,
    pub first_ts: i64,
    pub last_ts: i64,
    pub count: i64,
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
        s.migrate()?;
        Ok(s)
    }

    /// 版本化迁移。`CREATE TABLE IF NOT EXISTS` 对**已存在**的表零作用 —— 既不补列
    /// 也不校验形状,老库要等运行期报 "no such column" 才发现(报告 P2-17)。
    fn migrate(&self) -> Result<(), String> {
        // 2 = vault_items 按 (platform,label) 去重 + 唯一索引(下面那段),老库也补跑
        // 3 = snapshots 加 key_fp(密钥指纹分段,换 key 后两本账不互相推算消耗)
        // 4 = credit_grants(积分到账事件,WorkBuddy AI 的每日活跃 +30 等)
        const WANT: i64 = 4;
        let cur: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        if cur >= WANT {
            return Ok(());
        }
        for (table, cols) in [
            (
                "checkin_log",
                [
                    ("attempts", "INTEGER NOT NULL DEFAULT 0"),
                    ("detail", "TEXT"),
                ]
                .as_slice(),
            ),
            (
                "vault_items",
                [
                    ("hint", "TEXT NOT NULL DEFAULT ''"),
                    ("note", "TEXT NOT NULL DEFAULT ''"),
                ]
                .as_slice(),
            ),
            (
                "snapshots",
                [
                    ("kind", "TEXT NOT NULL DEFAULT 'amount'"),
                    // 密钥指纹(见 `adopt_orphan_snapshots`):老库已有行的值留空 ——
                    // 空串由"首次带指纹的那一轮"认领,不在这里猜。
                    ("key_fp", "TEXT NOT NULL DEFAULT ''"),
                ]
                .as_slice(),
            ),
        ] {
            if !self.table_exists(table)? {
                continue;
            }
            for (col, decl) in cols {
                self.add_column_if_missing(table, col, decl)?;
            }
        }
        // 按指纹分段查快照是新读路径的主力,单独给一条索引(provider + 指纹 + 时间)。
        if self.table_exists("snapshots")? {
            self.conn
                .execute_batch(
                    "CREATE INDEX IF NOT EXISTS idx_snap_fp
                        ON snapshots (provider_id, key_fp, ts);",
                )
                .map_err(|e| format!("建立快照指纹索引失败: {e}"))?;
        }
        // vault_items 里两条同 (platform,label) 的行是允许的 —— 唯一性只由应用层
        // "先查后插"把关,而查与插之间会放开锁:双击「保存」能插进两条一模一样的
        // 索引行。落成库级约束:先在**一个事务**里按大小写不敏感去重(留最早那一行
        // —— earliest 是用户最初那条,后插的副本跟着它;顺序按 created_at,同刻按 rowid),
        // 再建唯一索引。两段都幂等,user_version 到 2 之后不再跑。
        if self.table_exists("vault_items")? {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("开启密钥箱迁移事务失败: {e}"))?;
            tx.execute_batch(
                r#"
                DELETE FROM vault_items WHERE rowid NOT IN (
                    SELECT rowid FROM vault_items v WHERE NOT EXISTS (
                        SELECT 1 FROM vault_items o
                        WHERE o.platform = v.platform COLLATE NOCASE
                          AND o.label = v.label COLLATE NOCASE
                          AND (o.created_at < v.created_at
                               OR (o.created_at = v.created_at AND o.rowid < v.rowid))
                    )
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_vault_uniq
                    ON vault_items (platform COLLATE NOCASE, label COLLATE NOCASE);
                "#,
            )
            .map_err(|e| format!("整理密钥箱索引失败: {e}"))?;
            tx.commit()
                .map_err(|e| format!("提交密钥箱迁移失败: {e}"))?;
        }
        // 积分到账事件。**纯新增表**,不动任何老表 —— 老库升级上来只会多这一张空表。
        // UNIQUE(channel, ts):同一秒的重放(重启补跑、时钟回拨)不产生第二条。
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS credit_grants (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    channel TEXT NOT NULL,
                    day TEXT NOT NULL,
                    ts INTEGER NOT NULL,
                    amount REAL NOT NULL,
                    source TEXT NOT NULL,
                    from_bal REAL,
                    to_bal REAL,
                    UNIQUE(channel, ts)
                 );
                 CREATE INDEX IF NOT EXISTS idx_credit_grants_day
                    ON credit_grants (channel, day);",
            )
            .map_err(|e| format!("建立积分到账事件表失败: {e}"))?;
        self.conn
            .execute_batch(&format!("PRAGMA user_version = {WANT};"))
            .map_err(|e| format!("写数据库版本号失败: {e}"))?;
        Ok(())
    }

    fn table_exists(&self, table: &str) -> Result<bool, String> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
                params![table],
                |r| r.get(0),
            )
            .map_err(|e| format!("查表 {table} 失败: {e}"))?;
        Ok(n > 0)
    }

    fn add_column_if_missing(&self, table: &str, col: &str, decl: &str) -> Result<(), String> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|e| format!("读 {table} 列信息失败: {e}"))?;
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(|e| format!("读 {table} 列信息失败: {e}"))?
            .filter_map(|r| r.ok())
            .collect();
        if names.iter().any(|n| n == col) {
            return Ok(());
        }
        // 表名/列名都是本文件里的字面量,不是外部输入
        self.conn
            .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl};"))
            .map_err(|e| format!("给 {table} 补列 {col} 失败: {e}"))?;
        Ok(())
    }

    /// 测试用:内存库,不落盘,给跨模块的组装逻辑(如申请制状态)当输入。
    #[cfg(test)]
    pub fn open_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("打开数据库失败: {e}"))?;
        let s = Self { conn };
        s.init_schema()?;
        s.migrate()?;
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

            -- 长期趋势的底料:90 天外的明细在 `prune` 时**先归档再删**,
            -- 这里按 (渠道 + 密钥指纹 + 自然日) 留一条日级摘要,保留 365 天。
            -- consumed 只累加相邻明细的下降(与 consumption_since 同一口径),
            -- 所以它是"已观测消费"而不是接口给的数。
            CREATE TABLE IF NOT EXISTS daily_summary (
                provider_id TEXT    NOT NULL,
                key_fp      TEXT    NOT NULL,
                day_start   INTEGER NOT NULL,
                consumed    REAL    NOT NULL DEFAULT 0,
                first_ts    INTEGER NOT NULL DEFAULT 0,
                last_ts     INTEGER NOT NULL DEFAULT 0,
                count       INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (provider_id, key_fp, day_start)
            );

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

            -- 渠道 → 密钥箱条目的**引用关系**(设计稿 `product-vault-channel-bridge`)。
            --
            -- 这张表里**没有任何密钥值**,只有 id 之间的关系 —— 密文始终在凭据管理器里。
            -- 为什么需要它:密钥值不落库,就没法靠"把所有密钥扫一遍比对"来回答
            -- 「这把被谁用了」;而反查是删除前的拦截与列表标注的必需品。
            --
            -- 一个渠道最多一条引用(裁决 5);一个箱子条目可以被多个渠道引用(裁决 6)。
            -- 断链**故意保留**:箱子条目被删后这行还在,界面才能说"引用的条目已删除"(裁决 4),
            -- 而不是含糊地退回"未配置密钥"。
            CREATE TABLE IF NOT EXISTS vault_refs (
                channel_id TEXT PRIMARY KEY,
                vault_id   TEXT    NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_vault_refs_vault
                ON vault_refs (vault_id);
            "#,
            )
            .map_err(|e| format!("初始化数据库失败: {e}"))?;
        Ok(())
    }

    /// 写一条快照(不带密钥指纹,落成空串)。旧签名保留给"指纹未知"的场合与既有测试。
    pub fn record(
        &self,
        provider_id: &str,
        ts: i64,
        remaining: Option<f64>,
        used: Option<f64>,
        total: Option<f64>,
        kind: &str,
    ) -> Result<(), String> {
        self.record_fp(provider_id, "", ts, remaining, used, total, kind)
    }

    /// 写一条快照,连**这次实际用的凭据指纹**一起记下。
    ///
    /// 为什么要有指纹:同渠道换一次 key 就是换一个余额账户,两个账户的余额差
    /// 不是消费。按指纹分段后,`*_fp` 系列查询只在同一本账里推算消耗;
    /// 历史账户的数据不删,标注后照样能看(照小鲸鱼 issue #163 的教训:
    /// "数据没丢,只是没有任何入口能再看到它")。
    ///
    /// 只对**稳定的 API key** 型渠道有意义:网页会话/本机登录态的凭据会轮换,
    /// 拿它当指纹等于每轮开一本新账(调用方不要给那两类传指纹)。
    pub fn record_fp(
        &self,
        provider_id: &str,
        key_fp: &str,
        ts: i64,
        remaining: Option<f64>,
        used: Option<f64>,
        total: Option<f64>,
        kind: &str,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO snapshots (provider_id, ts, remaining, used, total, kind, key_fp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![provider_id, ts, remaining, used, total, kind, key_fp],
            )
            .map_err(|e| format!("写入快照失败: {e}"))?;
        Ok(())
    }

    /// 把某渠道**还没有指纹**的历史行认领给当前指纹。
    ///
    /// 升级上来的库里旧行 key_fp 是空串。空串既不等于"某个旧 key"也不等于"新 key",
    /// 只有"这一轮实际用的凭据"能回答它属于谁 —— 所以首次拿到稳定指纹时认领一次;
    /// 之后换 key 写进来的都是别的指纹,不会再被认领。返回认领行数(0 = 无需认领)。
    pub fn adopt_orphan_snapshots(&self, provider_id: &str, key_fp: &str) -> Result<usize, String> {
        if key_fp.is_empty() {
            return Ok(0);
        }
        self.conn
            .execute(
                "UPDATE snapshots SET key_fp = ?2 WHERE provider_id = ?1 AND key_fp = ''",
                params![provider_id, key_fp],
            )
            .map_err(|e| format!("认领无指纹的历史快照失败: {e}"))
    }

    /// 该渠道历史上用过几个不同的密钥指纹(含空串),用来判断"有没有历史账户"。
    /// 只回答数量,不暴露指纹本身。
    pub fn fingerprint_count(&self, provider_id: &str) -> Result<i64, String> {
        self.conn
            .query_row(
                "SELECT COUNT(DISTINCT key_fp) FROM snapshots WHERE provider_id = ?1",
                params![provider_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("统计快照指纹失败: {e}"))
    }

    /// 指定时间窗口内的消耗推算值。
    /// 窗口起点前有基线 → 精确到窗口(跨起点的基线对按时间折算);
    /// 没有基线(软件那天还没装上/机器整段没开)→ 退化为「窗口内首快照起算」
    /// 的**下限估算** —— 界面本来就标"推算",报一个偏小的真值比整格「——」有用
    /// (踩过:装当周周一/月首没有快照,本周消耗一直显示数据不足)。
    /// 窗口内不足两条快照才算真的无从推算,返回 None;窗口内**一条都没有**
    /// (只剩窗口起点之前的基线)同样返回 None —— 那是"这一期还没数据",
    /// 不是"这期消耗 0"(以前回 Some(0.0),环比会凭空多一个 0 当分母)。
    /// 读窗口内的 (ts, remaining) 升序行。
    ///
    /// `key_fp`:`Some(指纹)` 只看那一本账;`None` = 不限(升级前的旧行为)。
    /// 一条 SQL 覆盖两种口径 —— `?4 IS NULL` 时整条指纹条件失效。
    fn rows_between(
        &self,
        provider_id: &str,
        key_fp: Option<&str>,
        from_ts: i64,
        to_ts: i64,
    ) -> Result<Vec<(i64, f64)>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ts, remaining FROM snapshots
                 WHERE provider_id = ?1 AND remaining IS NOT NULL
                   AND ts >= ?2 AND ts <= ?3
                   AND (?4 IS NULL OR key_fp = ?4)
                 ORDER BY ts ASC",
            )
            .map_err(|e| format!("查询失败: {e}"))?;

        let rows: Vec<(i64, f64)> = stmt
            .query_map(params![provider_id, from_ts, to_ts, key_fp], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|e| format!("读取快照失败: {e}"))?
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    // 静默丢行 = 数字悄悄偏小且无从排查(与写库失败同一条纪律,O-7)
                    eprintln!("读取快照行失败(该行已跳过): {e}");
                    None
                }
            })
            .collect();
        Ok(rows)
    }

    /// 某渠道最新一条**成功**快照的 (ts, remaining)。
    ///
    /// 签到的到账核对用(checkin::ArrivalInfo):claim 前读一次当基线,
    /// claim + 取数之后再读一次比增量 —— 纯读库,零网络请求。
    /// 读不到(该渠道从未成功取数)返回 None,调用方退化为"不核对"。
    pub fn latest_snapshot(&self, provider_id: &str) -> Result<Option<(i64, f64)>, String> {
        self.conn
            .query_row(
                "SELECT ts, remaining FROM snapshots
                 WHERE provider_id = ?1 AND remaining IS NOT NULL
                 ORDER BY ts DESC LIMIT 1",
                params![provider_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(format!("读取最新快照失败: {e}")),
            })
    }

    // ───────────── 积分到账(credit_grants)─────────────
    // 设计见 docs/design/design-workbuddy-ai-channel-2026-10-23.html。
    // 与 checkin_log 分开:那张表记的是"签到尝试"(退避、幂等、连签),这张记的是
    // "到账事件"(可能只是余额正增量推算出来的,没有"尝试"这回事),保留期与查询口径都不同。

    /// 记一条到账事件。`UNIQUE(channel, ts)` 让同一秒的重放落空(进程重启 / 补跑)。
    /// 返回 true = 真的写进去了 —— 调用方据此打日志,不靠猜。
    ///
    /// 守卫:秒级 ts + 这个唯一键意味着**同秒两笔不同数额会被静默吞掉一条**。
    /// 现在每轮取数至多记一笔(余额正增量推算),碰不到;将来若接平台逐笔台账
    /// (真机探针:Accounts[] 的 CreateTime 是毫秒),必须把 ts 改存毫秒,
    /// 否则同秒的两笔充值/发放会丢记录。
    pub fn insert_grant(&self, g: &crate::grants::Grant) -> Result<bool, String> {
        let n = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO credit_grants
                   (channel, day, ts, amount, source, from_bal, to_bal)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![g.channel, g.day, g.ts, g.amount, g.source, g.from_bal, g.to_bal],
            )
            .map_err(|e| format!("写入积分到账事件失败: {e}"))?;
        Ok(n > 0)
    }

    /// 某渠道从 `since_ts` 起的到账事件,**按时间升序**(界面按天分组要顺着看)。
    pub fn grants_since(
        &self,
        channel: &str,
        since_ts: i64,
    ) -> Result<Vec<crate::grants::Grant>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT channel, day, ts, amount, source, from_bal, to_bal
                 FROM credit_grants
                 WHERE channel = ?1 AND ts >= ?2
                 ORDER BY ts ASC",
            )
            .map_err(|e| format!("查询积分到账事件失败: {e}"))?;
        let rows = stmt
            .query_map(params![channel, since_ts], |r| {
                Ok(crate::grants::Grant {
                    channel: r.get(0)?,
                    day: r.get(1)?,
                    ts: r.get(2)?,
                    amount: r.get(3)?,
                    source: r.get(4)?,
                    from_bal: r.get(5)?,
                    to_bal: r.get(6)?,
                })
            })
            .map_err(|e| format!("读取积分到账事件失败: {e}"))?
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    // 静默丢行 = 数字悄悄偏小且无从排查(与快照读行同一条纪律)
                    eprintln!("读取积分到账事件失败(该行已跳过): {e}");
                    None
                }
            })
            .collect();
        Ok(rows)
    }

    /// 最后一次成功快照的余额 —— 算正增量要用它当基线。
    ///
    /// `ts` 用**严格小于**:本轮快照还没写(调用方在 `record_fp` 之前调),
    /// 万一同秒写入两条,也不能拿自己当自己的基线。
    /// `key_fp` 传空串 = 不分段(本机登录态渠道没有稳定指纹,见 `record_fp` 的注释)。
    pub fn prev_remaining(
        &self,
        provider_id: &str,
        key_fp: &str,
        ts: i64,
    ) -> Result<Option<f64>, String> {
        match self.conn.query_row(
            "SELECT remaining FROM snapshots
             WHERE provider_id = ?1 AND remaining IS NOT NULL AND ts < ?2
               AND (?3 = '' OR key_fp = ?3)
             ORDER BY ts DESC LIMIT 1",
            params![provider_id, ts, key_fp],
            |r| r.get::<_, f64>(0),
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("读取上次余额失败: {e}")),
        }
    }

    /// 清理过期事件。保留期由调用方给(现在 30 天),这里不猜。
    pub fn prune_grants(&self, before_ts: i64) -> Result<usize, String> {
        self.conn
            .execute("DELETE FROM credit_grants WHERE ts < ?1", params![before_ts])
            .map_err(|e| format!("清理积分到账事件失败: {e}"))
    }

    pub fn consumption_since(
        &self,
        provider_id: &str,
        from_ts: i64,
        now_ts: i64,
    ) -> Result<Option<f64>, String> {
        self.consumption_since_scoped(provider_id, None, from_ts, now_ts)
    }

    /// 同 [`Self::consumption_since`],但只在**同一把密钥**的账里推算(换 key 不清算旧账)。
    pub fn consumption_since_fp(
        &self,
        provider_id: &str,
        key_fp: &str,
        from_ts: i64,
        now_ts: i64,
    ) -> Result<Option<f64>, String> {
        self.consumption_since_scoped(provider_id, Some(key_fp), from_ts, now_ts)
    }

    fn consumption_since_scoped(
        &self,
        provider_id: &str,
        key_fp: Option<&str>,
        from_ts: i64,
        now_ts: i64,
    ) -> Result<Option<f64>, String> {
        // 多读一天:窗口起点前的基线要在窗口外(按时间折算进来),这是原口径
        let rows = self.rows_between(provider_id, key_fp, from_ts - 86400, now_ts)?;

        // 窗口内一条都没有:只有窗口起点之前的基线。这是"这一期还没数据",
        // 不是"这期消耗 0" —— 报 0 会把汇总条的环比百分比凭空抬高。
        // (与"不足两条快照返回 None"同一条纪律;doc 早写明窗口要两条快照才算得出。)
        if !rows.iter().any(|(ts, _)| *ts >= from_ts) {
            return Ok(None);
        }

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
        self.consumed_in_window_scoped(provider_id, None, from_ts, to_ts)
    }

    /// 同 [`Self::consumed_in_window`],但只看同一把密钥的账。
    pub fn consumed_in_window_fp(
        &self,
        provider_id: &str,
        key_fp: &str,
        from_ts: i64,
        to_ts: i64,
    ) -> Result<f64, String> {
        self.consumed_in_window_scoped(provider_id, Some(key_fp), from_ts, to_ts)
    }

    fn consumed_in_window_scoped(
        &self,
        provider_id: &str,
        key_fp: Option<&str>,
        from_ts: i64,
        to_ts: i64,
    ) -> Result<f64, String> {
        let vals: Vec<f64> = self
            .rows_between(provider_id, key_fp, from_ts, to_ts)?
            .into_iter()
            .map(|(_, r)| r)
            .collect();

        let mut sum = 0.0;
        for pair in vals.windows(2) {
            sum += (pair[0] - pair[1]).max(0.0);
        }
        Ok((sum * 100.0).round() / 100.0)
    }

    /// 窗口内快照的 MIN/MAX 时间(不限指纹的旧口径)。
    /// 注意:这里算的是**快照覆盖**(应用在不在跑),不看 `remaining` 是否为空。
    pub fn first_ts_since(&self, provider_id: &str, from_ts: i64) -> Result<Option<i64>, String> {
        self.edge_ts(provider_id, None, from_ts, None, "MIN")
    }

    /// 同 [`Self::first_ts_since`],但只看同一把密钥的账。
    pub fn first_ts_since_fp(
        &self,
        provider_id: &str,
        key_fp: &str,
        from_ts: i64,
    ) -> Result<Option<i64>, String> {
        self.edge_ts(provider_id, Some(key_fp), from_ts, None, "MIN")
    }

    /// 窗口内最晚一条快照的时间(缺口标注用:尾部缺口 = now - last_ts)。
    pub fn last_ts_since(&self, provider_id: &str, key_fp: Option<&str>, from_ts: i64) -> Result<Option<i64>, String> {
        self.edge_ts(provider_id, key_fp, from_ts, None, "MAX")
    }

    fn edge_ts(
        &self,
        provider_id: &str,
        key_fp: Option<&str>,
        from_ts: i64,
        to_ts: Option<i64>,
        agg: &str,
    ) -> Result<Option<i64>, String> {
        // agg 只能是本文件里写死的 "MIN"/"MAX",不是外部输入
        let sql = format!(
            "SELECT {agg}(ts) FROM snapshots
              WHERE provider_id = ?1 AND ts >= ?2
                AND (?3 IS NULL OR key_fp = ?3)
                AND (?4 IS NULL OR ts <= ?4)"
        );
        self.conn
            .query_row(&sql, params![provider_id, from_ts, key_fp, to_ts], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .map_err(|e| format!("查询失败: {e}"))
    }

    /// 窗口内的覆盖情况(最早/最晚快照 + 条数)。缺口标注的原料:
    /// 界面拿它和窗口起止一比,就知道"数据从几点才开始有 / 已经多久没更新"。
    pub fn window_coverage(
        &self,
        provider_id: &str,
        key_fp: Option<&str>,
        from_ts: i64,
        to_ts: i64,
    ) -> Result<WindowCoverage, String> {
        self.conn
            .query_row(
                "SELECT MIN(ts), MAX(ts), COUNT(*) FROM snapshots
                  WHERE provider_id = ?1 AND ts >= ?2 AND ts <= ?3
                    AND (?4 IS NULL OR key_fp = ?4)",
                params![provider_id, from_ts, to_ts, key_fp],
                |r| {
                    Ok(WindowCoverage {
                        first_ts: r.get(0)?,
                        last_ts: r.get(1)?,
                        count: r.get(2)?,
                    })
                },
            )
            .map_err(|e| format!("查询失败: {e}"))
    }

    /// 逐日覆盖(长期趋势柱图上的"缺口日"判据)。分桶按调用方给的时区偏移,
    /// 不用 SQLite 的 'localtime'(那玩意儿在不同环境行为不一致,踩过)。
    ///
    /// 只对 `ts + tz_offset` 为正的现代时间做整除 —— 1970 年前的数据不在考虑范围。
    pub fn daily_coverage(
        &self,
        provider_id: &str,
        key_fp: Option<&str>,
        from_ts: i64,
        to_ts: i64,
        tz_offset: i64,
    ) -> Result<Vec<DayCoverage>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ((ts + ?4) / 86400) AS b, MIN(ts), MAX(ts), COUNT(*) FROM snapshots
                  WHERE provider_id = ?1 AND ts >= ?2 AND ts <= ?3
                    AND (?5 IS NULL OR key_fp = ?5)
                  GROUP BY b ORDER BY b ASC",
            )
            .map_err(|e| format!("查询失败: {e}"))?;
        let rows = stmt
            .query_map(
                params![provider_id, from_ts, to_ts, tz_offset, key_fp],
                |r| {
                    let b: i64 = r.get(0)?;
                    Ok(DayCoverage {
                        day_start: b * 86_400 - tz_offset,
                        first_ts: r.get(1)?,
                        last_ts: r.get(2)?,
                        count: r.get(3)?,
                    })
                },
            )
            .map_err(|e| format!("读取快照失败: {e}"))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 余额变动台账(行详情里那张"最近变动")。
    ///
    /// 聚合口径:同一方向的连续变动在 `MERGE_GAP` 内并成一条(轮询是分钟级的,
    /// 一次连续消费会攒成几十条 —— 逐条列出来是噪音);方向一变(充值/申请)
    /// 立刻另起一条。`samples` 记参与聚合的轮询条数,`gap_secs` 记这条与
    /// 上一次变动之间的空档(>0 说明中间有段时间没观测)。
    pub fn recent_changes(
        &self,
        provider_id: &str,
        key_fp: Option<&str>,
        from_ts: i64,
        limit: usize,
    ) -> Result<Vec<ChangeEntry>, String> {
        let rows = self.rows_between(provider_id, key_fp, from_ts, i64::MAX / 2)?;
        let mut out: Vec<ChangeEntry> = Vec::new();
        for pair in rows.windows(2) {
            let (t0, r0) = pair[0];
            let (t1, r1) = pair[1];
            let delta = r1 - r0;
            if delta.abs() < 0.005 {
                continue; // 四舍五入级别的抖动不算一次变动
            }
            let same_sign = out
                .last()
                .map(|e| (e.delta > 0.0) == (delta > 0.0))
                .unwrap_or(false);
            let within_merge = out
                .last()
                .map(|e| t0 - e.end_ts <= MERGE_GAP)
                .unwrap_or(false);
            if same_sign && within_merge {
                let e = out.last_mut().expect("刚判过非空");
                e.end_ts = t1;
                e.delta += delta;
                e.samples += 1;
                e.remaining = r1;
            } else {
                // 空档 = 从"上一次观测到的时点"到"这次观测到时点"之间有多久没看:
                // 它说明这条变动的金额里可能揉进了好几段时间的消耗,界面据此标注。
                let gap_secs = out.last().map(|e| t1 - e.end_ts).unwrap_or(0);
                out.push(ChangeEntry {
                    start_ts: t0,
                    end_ts: t1,
                    delta,
                    remaining: r1,
                    samples: 1,
                    gap_secs,
                });
            }
        }
        let tail: Vec<ChangeEntry> = out.split_off(out.len().saturating_sub(limit));
        Ok(tail.into_iter().rev().collect())
    }

    /// 最近一次"余额跳升"的时间(申请额度 / 充值到账)。
    /// 只有跳升幅度 >= min_delta 才算,避免四舍五入的小抖动被误判成一次申请。
    pub fn last_jump_since(
        &self,
        provider_id: &str,
        from_ts: i64,
        min_delta: f64,
        target: f64,
        max_gap: i64,
    ) -> Result<Option<i64>, String> {
        self.last_jump_since_scoped(provider_id, None, from_ts, min_delta, target, max_gap)
    }

    /// 同 [`Self::last_jump_since`],但只看同一把密钥的账(换 key 后的补账不该被算成申请)。
    pub fn last_jump_since_fp(
        &self,
        provider_id: &str,
        key_fp: &str,
        from_ts: i64,
        min_delta: f64,
        target: f64,
        max_gap: i64,
    ) -> Result<Option<i64>, String> {
        self.last_jump_since_scoped(provider_id, Some(key_fp), from_ts, min_delta, target, max_gap)
    }

    fn last_jump_since_scoped(
        &self,
        provider_id: &str,
        key_fp: Option<&str>,
        from_ts: i64,
        min_delta: f64,
        target: f64,
        max_gap: i64,
    ) -> Result<Option<i64>, String> {
        let rows = self.rows_between(provider_id, key_fp, from_ts, i64::MAX / 2)?;

        // 顺序扫描,取最后一次**像申请的**跳升。
        // 只看涨幅会把平台的预扣退还(一次能涨几十元)记成"用户申请过了",
        // 于是申请倒计时被顶回去、用户白等(P2-1)。
        let mut last = None;
        for pair in rows.windows(2) {
            if pair[1].1 - pair[0].1 < min_delta {
                continue;
            }
            // 落在额度线附近才算申请到账(target<=0 = 调用方没给额度,退回旧口径)
            if target > 0.0 && (pair[1].1 - target).abs() > target * 0.15 {
                continue;
            }
            // 相邻两条隔太久(合盖多日后醒来第一轮)的差值不是一次跳升
            if max_gap > 0 && pair[1].0 - pair[0].0 > max_gap {
                continue;
            }
            last = Some(pair[1].0);
        }
        Ok(last)
    }

    /// 用于展开区的迷你柱图。按小时分桶取窗口内最低余额,再转成差值曲线。
    pub fn series(
        &self,
        provider_id: &str,
        from_ts: i64,
        to_ts: i64,
        buckets: usize,
    ) -> Result<Vec<f64>, String> {
        self.series_scoped(provider_id, None, from_ts, to_ts, buckets)
    }

    /// 同 [`Self::series`],但只看同一把密钥的账。
    pub fn series_fp(
        &self,
        provider_id: &str,
        key_fp: &str,
        from_ts: i64,
        to_ts: i64,
        buckets: usize,
    ) -> Result<Vec<f64>, String> {
        self.series_scoped(provider_id, Some(key_fp), from_ts, to_ts, buckets)
    }

    fn series_scoped(
        &self,
        provider_id: &str,
        key_fp: Option<&str>,
        from_ts: i64,
        to_ts: i64,
        buckets: usize,
    ) -> Result<Vec<f64>, String> {
        let rows = self.rows_between(provider_id, key_fp, from_ts, i64::MAX / 2)?;

        // 少于两条推不出任何差值:返回空数组,让界面画"数据不足"。
        // 以前回落到全 0,画出来与"真实零消耗"一模一样,分不出来(P2-18)
        if rows.len() < 2 {
            return Ok(Vec::new());
        }

        // 归一基准是**请求窗口的终点**,不是最后一条快照:否则数据停在 3 小时前,
        // 这 3 小时的消耗会被拉伸铺满 24 格,趋势看着像"一直在花"(P2-18)
        let span = (to_ts - from_ts).max(1) as f64;
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

    /// Ok(None) = 库里真没有这一行;Err = 存储出问题(损坏 / IO 错)。
    /// 以前用 `.ok()` 把两者折叠成 None,于是 `Config::try_load` 把"读坏了"当成
    /// "没配过"而回落默认值 —— 隐藏渠道被静默解除隐藏、恢复对外发请求(P2-5;
    /// 同一条红线 JSON 解析那一支早在 P2-11 修过)。
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, String> {
        match self.conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |r| r.get::<_, String>(0),
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("读取设置 {key} 失败: {e}")),
        }
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

    /// 归档时往分界线**之后**多看多久,专门用来接住跨边界的那一笔(见 `archive_before` 的注)。
    /// 取一天:比任何渠道的轮询间隔都宽得多,笔记本睡眠/挂件暂停留出的空档也盖得住。
    const ARCHIVE_LOOKBACK_SEC: i64 = 86_400;

    /// 清理旧数据 —— **删明细之前先把它们压成日级摘要**(长期趋势的底料):
    /// 快照明细按 90 天,归档出的日级摘要保留 365 天;签到流水按 400 天。
    ///
    /// `day_offset_secs` 是调用方的**时区偏移**(本地零点 = 日桶边界)。归档必须按
    /// 和界面同一个日历分桶,否则趋势里的"日"和明细里的"日"对不上。
    pub fn prune(&self, before_ts: i64, day_offset_secs: i64) -> Result<usize, String> {
        self.archive_before(before_ts, day_offset_secs)?;
        let n = self
            .conn
            .execute("DELETE FROM snapshots WHERE ts < ?1", params![before_ts])
            .map_err(|e| format!("清理旧快照失败: {e}"))?;
        // 日级摘要留到明细之外再一年
        self.conn
            .execute(
                "DELETE FROM daily_summary WHERE day_start < ?1",
                params![before_ts - 275 * 86_400],
            )
            .map_err(|e| format!("清理旧日级摘要失败: {e}"))?;
        let log_before = before_ts - 310 * 86_400; // 快照的 90 天之外再多留 310 天(合计 400 天,与上面的说明对齐)
        self.conn
            .execute(
                "DELETE FROM checkin_log WHERE ts < ?1",
                params![log_before],
            )
            .map_err(|e| format!("清理旧签到流水失败: {e}"))?;
        Ok(n)
    }

    /// 把 `before_ts` 之前的明细压成日级摘要。可重入:行随后就被删,不会被算两次;
    /// 跨 `before_ts` 的那一天如果被劈成两半,两半靠 ON CONFLICT **累加**并回同一条。
    ///
    /// 覆盖(首/末/条数)按**全部行**算,消耗只累加下降(与 `consumption_since` 同口径),
    /// 且只归档 `kind='amount'` —— 百分比渠道没有"余额消耗"这个概念,进来是噪声。
    fn archive_before(&self, before_ts: i64, day_offset_secs: i64) -> Result<(), String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("开启归档事务失败: {e}"))?;
        // ① 覆盖:全部行,按天桶(MIN/MAX/COUNT 累加 —— 分界日两半会并回同一条)
        tx.execute(
            "INSERT INTO daily_summary (provider_id, key_fp, day_start, consumed, first_ts, last_ts, count)
             SELECT provider_id, key_fp, b * 86400 - ?2, 0, MIN(ts), MAX(ts), COUNT(*)
               FROM (
                 SELECT provider_id, key_fp, ts, ((ts + ?2) / 86400) AS b
                   FROM snapshots WHERE ts < ?1
               )
              GROUP BY provider_id, key_fp, b
             ON CONFLICT(provider_id, key_fp, day_start) DO UPDATE SET
               first_ts = MIN(daily_summary.first_ts, excluded.first_ts),
               last_ts  = MAX(daily_summary.last_ts,  excluded.last_ts),
               count    = daily_summary.count + excluded.count",
            params![before_ts, day_offset_secs],
        )
        .map_err(|e| format!("归档日覆盖失败: {e}"))?;
        // ② 消耗:相邻下降之和,归属到**前一行**所在的那天(与界面柱图同一口径:
        //    差值按起点分桶)。先在一条 SQL 里聚合成少量行,再逐行累加进摘要 ——
        //    不要在 UPDATE 里写相关子查询(那是 O(摘要数 × 明细数))。
        //
        //    读的时候要多往后看 `ARCHIVE_LOOKBACK_SEC`:跨在分界线上的那一跳,它的"后一行"
        //    落在 `ts < before_ts` 之外,而下一轮归档时它的前一行已经被删掉了 —— LAG 永远取不到
        //    参照,于是每轮 prune 都永久丢一笔消耗(第三轮体检 P2-④)。现在收"前一行在窗口内"
        //    的所有下降(`pts < before_ts`),跨边界那一跳归到前一行那天。
        //    不会重复计:下一轮里前一行已经不在表里,同一对相邻行不可能再被算第二次。
        let horizon = before_ts + Self::ARCHIVE_LOOKBACK_SEC;
        let deltas: Vec<(String, String, i64, f64)> = {
            let mut stmt = tx
                .prepare(
                    "SELECT provider_id, key_fp, b * 86400 - ?2 AS day_start, SUM(delta)
                       FROM (
                         SELECT provider_id, key_fp,
                                ((pts + ?2) / 86400) AS b,
                                prem - remaining AS delta
                           FROM (
                             SELECT provider_id, key_fp, ts,
                                    LAG(ts)        OVER win AS pts,
                                    LAG(remaining) OVER win AS prem,
                                    remaining
                               FROM snapshots
                              WHERE ts < ?3 AND kind = 'amount' AND remaining IS NOT NULL
                             WINDOW win AS (PARTITION BY provider_id, key_fp ORDER BY ts ASC)
                           )
                          WHERE delta > 0 AND pts IS NOT NULL AND pts < ?1
                       )
                      GROUP BY provider_id, key_fp, b",
                )
                .map_err(|e| format!("聚合归档消耗失败: {e}"))?;
            let rows = stmt
                .query_map(params![before_ts, day_offset_secs, horizon], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .map_err(|e| format!("聚合归档消耗失败: {e}"))?;
            // 坏行如实记一行再丢(第二轮刚把四处同类 `filter_map(Result::ok)` 改成这样,
            // 别让同一个静默吞数据的问题又回来)
            let mut out: Vec<(String, String, i64, f64)> = Vec::new();
            for r in rows {
                match r {
                    Ok(v) => out.push(v),
                    Err(e) => eprintln!("归档消耗聚合里丢了一行: {e}"),
                }
            }
            out
        };
        for (provider_id, key_fp, day_start, delta) in deltas {
            tx.execute(
                "UPDATE daily_summary SET consumed = consumed + ?4
                  WHERE provider_id = ?1 AND key_fp = ?2 AND day_start = ?3",
                params![provider_id, key_fp, day_start, delta],
            )
            .map_err(|e| format!("累加归档消耗失败: {e}"))?;
        }
        tx.commit().map_err(|e| format!("提交归档失败: {e}"))?;
        Ok(())
    }

    /// 长期趋势曲线:老的部分吃**日级摘要**(90 天外的明细已归档),新的部分吃明细,
    /// 统一装进 `buckets` 个桶。返回 (每桶消耗, 有缺口的分桶下标)。
    ///
    /// 与 [`Self::series`] 的差别:那个只看明细、也不报缺口,是"近 30 天"用的;
    /// 这个能一路画到一年前,并且把"那段没观测"的日子标出来(柱图上打斜纹)——
    /// 否则长曲线会把空洞画成一串 0,看着像"那些天真的没花钱"。
    #[allow(clippy::too_many_arguments)]
    pub fn trend_series(
        &self,
        provider_id: &str,
        key_fp: Option<&str>,
        from_ts: i64,
        to_ts: i64,
        buckets: usize,
        tz_offset: i64,
    ) -> Result<(Vec<f64>, Vec<usize>), String> {
        if buckets == 0 {
            return Ok((Vec::new(), Vec::new()));
        }
        let span = (to_ts - from_ts).max(1) as f64;
        let bucket_of = |t: i64| -> usize {
            let idx = (((t - from_ts) as f64 / span) * buckets as f64) as usize;
            idx.min(buckets - 1)
        };
        let mut out = vec![0.0f64; buckets];
        let mut gaps: Vec<usize> = Vec::new();

        // ① 归档部分:日级摘要(完全落在窗口起点之前的那天不算)
        for d in self.daily_summaries(provider_id, key_fp, from_ts)? {
            if d.day_start + 86_400 <= from_ts {
                continue;
            }
            let idx = bucket_of(d.day_start);
            out[idx] += d.consumed;
            if day_summary_partial(&d, to_ts) {
                gaps.push(idx);
            }
        }

        // ② 明细部分:与 series() 同一套"下降才累加"的差值分桶(按差值起点归桶)
        let rows = self.rows_between(provider_id, key_fp, from_ts, to_ts)?;
        for pair in rows.windows(2) {
            let (t0, r0) = pair[0];
            let drop = (r0 - pair[1].1).max(0.0);
            out[bucket_of(t0)] += drop;
        }

        // ③ 缺口:明细窗口内按天看覆盖(日级摘要那边已经在①里判过)
        for d in self.daily_coverage(provider_id, key_fp, from_ts, to_ts, tz_offset)? {
            let leading = d.first_ts - d.day_start;
            let finished = d.day_start + 86_400 <= to_ts;
            let trailing = if finished {
                (d.day_start + 86_400 - d.last_ts).max(0)
            } else {
                0
            };
            if leading > OBSERVE_GAP_SEC || trailing > OBSERVE_GAP_SEC {
                // 归到这天中点所在的桶,比归到零点更贴近"看起来缺的是哪一段"
                gaps.push(bucket_of(d.day_start + 43_200));
            }
        }
        gaps.sort_unstable();
        gaps.dedup();
        Ok((out, gaps))
    }

    /// 长期趋势:某渠道(某一本账)的日级摘要,`from_ts` 之后的都在。
    pub fn daily_summaries(
        &self,
        provider_id: &str,
        key_fp: Option<&str>,
        from_ts: i64,
    ) -> Result<Vec<DaySummary>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT day_start, consumed, first_ts, last_ts, count FROM daily_summary
                  WHERE provider_id = ?1 AND day_start >= ?2
                    AND (?3 IS NULL OR key_fp = ?3)
                  ORDER BY day_start ASC",
            )
            .map_err(|e| format!("查询失败: {e}"))?;
        let rows = stmt
            .query_map(params![provider_id, from_ts, key_fp], |r| {
                Ok(DaySummary {
                    day_start: r.get(0)?,
                    consumed: r.get(1)?,
                    first_ts: r.get(2)?,
                    last_ts: r.get(3)?,
                    count: r.get(4)?,
                })
            })
            .map_err(|e| format!("读取日级摘要失败: {e}"))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }
}

/// 一条日级摘要算不算"部分数据"(归档侧的判据,与明细侧 [`OBSERVE_GAP_SEC`] 同源)。
fn day_summary_partial(d: &DaySummary, now: i64) -> bool {
    let leading = d.first_ts - d.day_start;
    let finished = d.day_start + 86_400 <= now;
    let trailing = if finished {
        (d.day_start + 86_400 - d.last_ts).max(0)
    } else {
        0
    };
    leading > OBSERVE_GAP_SEC || trailing > OBSERVE_GAP_SEC
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

    // ── 渠道 → 箱子条目的引用关系 ──────────────────────────────────────────
    //
    // 只在 SQLite 里记 id 关系,不碰密文。详见 init_schema 里 vault_refs 的注释。

    /// 让 `channel_id` 引用箱子里的 `vault_id`(已有引用则覆盖 —— 裁决 5 只允许一把)。
    pub fn vault_ref_set(&self, channel_id: &str, vault_id: &str) -> Result<(), String> {
        // store.rs 里没有时间工具函数(created_at 一律由调用方给):
        // 这里的 created_at 只用于排查"这引用什么时候建的",不需要可注入的时钟。
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn
            .execute(
                "INSERT INTO vault_refs (channel_id, vault_id, created_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(channel_id) DO UPDATE SET vault_id = excluded.vault_id",
                params![channel_id, vault_id, created_at],
            )
            .map_err(|e| format!("写密钥引用失败: {e}"))?;
        Ok(())
    }

    /// 用一份"按真实指针算出来的关系"**整表替换**索引。
    ///
    /// 这是对账的唯一动作(设计稿 `product-vault-ref-reconcile-2026-10-21`)。
    /// 一句话规矩:**书架说了算,索引只是副本,随时可以重抄。**
    ///
    /// 为什么是整表替换而不是逐条 upsert:逐条改的话,"索引里有、指针里没有"的行
    /// **永远清不掉** —— 而那恰恰是走偏的主要形态(取消引用时索引没删干净)。
    /// 整表替换让索引只可能等于"这一次扫描到的真相",不可能残留幽灵行。
    pub fn vault_ref_rebuild(&self, pairs: &[(String, String)]) -> Result<(), String> {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("开启索引重建事务失败: {e}"))?;
        tx.execute("DELETE FROM vault_refs", [])
            .map_err(|e| format!("清空引用索引失败: {e}"))?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT OR REPLACE INTO vault_refs (channel_id, vault_id, created_at)
                     VALUES (?1, ?2, ?3)",
                )
                .map_err(|e| format!("准备写引用索引失败: {e}"))?;
            for (c, v) in pairs {
                stmt.execute(rusqlite::params![c, v, created_at])
                    .map_err(|e| format!("写引用索引失败: {e}"))?;
            }
        }
        tx.commit().map_err(|e| format!("提交索引重建失败: {e}"))?;
        Ok(())
    }

    // 原 `vault_ref_of` 已删除(N30):它读的是**索引**,而索引是派生数据。
    // 需要"某渠道引用了谁"时一律走 `secrets::link_get` —— 书架才是权威。
    /// 取消引用(改回"直接填"或清除密钥时用)。本来没有引用也算成功。
    pub fn vault_ref_clear(&self, channel_id: &str) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM vault_refs WHERE channel_id = ?1", params![channel_id])
            .map_err(|e| format!("清除密钥引用失败: {e}"))?;
        Ok(())
    }

    /// 反查:有哪些渠道正引用这个箱子条目。删前拦截与列表标注都靠它。
    pub fn vault_ref_channels(&self, vault_id: &str) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT channel_id FROM vault_refs WHERE vault_id = ?1 ORDER BY channel_id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![vault_id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 全部引用关系,一次读出来给界面批量渲染(避免逐条箱子条目各查一次)。
    pub fn vault_ref_pairs(&self) -> Result<Vec<(String, String)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT channel_id, vault_id FROM vault_refs")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
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
                ts INTEGER NOT NULL, remaining REAL, used REAL, total REAL, kind TEXT NOT NULL,
                key_fp TEXT NOT NULL DEFAULT '');
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        Store { conn }
    }

    /// 走完整建表 + 迁移的库(验新表/迁移用)。`mem()` 只搭了最小的两张表。
    fn mem_migrated() -> Store {
        let conn = Connection::open_in_memory().unwrap();
        let s = Store { conn };
        s.init_schema().unwrap();
        s.migrate().unwrap();
        s
    }

    #[test]
    fn grant_rows_roundtrip_and_dedupe_by_second() {
        let s = mem_migrated();
        let g = |ts: i64, amount: f64| crate::grants::Grant {
            channel: "workbuddy-ai".into(),
            day: "2026-10-23".into(),
            ts,
            amount,
            source: "delta".into(),
            from_bal: Some(1000.0),
            to_bal: Some(1000.0 + amount),
        };
        assert!(s.insert_grant(&g(200, 30.0)).unwrap());
        assert!(!s.insert_grant(&g(200, 30.0)).unwrap(), "同一秒重放不该记第二条");
        assert!(s.insert_grant(&g(100, 500.0)).unwrap());

        let rows = s.grants_since("workbuddy-ai", 0).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ts, 100, "按时间升序,界面按天分组要顺着看");
        assert_eq!(rows[1].amount, 30.0);
        // 别的渠道不串
        assert!(s.grants_since("workbuddy", 0).unwrap().is_empty());
    }

    #[test]
    fn prev_remaining_is_strictly_earlier() {
        let s = mem_migrated();
        s.record_fp("workbuddy-ai", "", 100, Some(1000.0), None, None, "points").unwrap();
        s.record_fp("workbuddy-ai", "", 200, Some(1030.0), None, None, "points").unwrap();
        assert_eq!(s.prev_remaining("workbuddy-ai", "", 200).unwrap(), Some(1000.0));
        assert_eq!(s.prev_remaining("workbuddy-ai", "", 300).unwrap(), Some(1030.0));
        assert_eq!(s.prev_remaining("workbuddy-ai", "", 100).unwrap(), None, "首条没有基线");
        assert_eq!(s.prev_remaining("workbuddy", "", 300).unwrap(), None, "别的渠道不串");
    }

    #[test]
    fn prune_grants_keeps_the_window() {
        let s = mem_migrated();
        for ts in [100, 200, 300] {
            let g = crate::grants::Grant {
                channel: "workbuddy-ai".into(),
                day: "2026-10-23".into(),
                ts,
                amount: 30.0,
                source: "delta".into(),
                from_bal: None,
                to_bal: None,
            };
            s.insert_grant(&g).unwrap();
        }
        assert_eq!(s.prune_grants(250).unwrap(), 2);
        assert_eq!(s.grants_since("workbuddy-ai", 0).unwrap().len(), 1);
    }

    /// 迁移前的现场:只有 vault_items(且里面已经有重复行),`user_version` 还是 0。
    /// 用 (id, platform, label, created_at)。
    fn mem_with_vault(rows: &[(&str, &str, &str, i64)]) -> Store {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE vault_items (
                id         TEXT PRIMARY KEY,
                platform   TEXT    NOT NULL,
                label      TEXT    NOT NULL,
                hint       TEXT    NOT NULL DEFAULT '',
                note       TEXT    NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );",
        )
        .unwrap();
        for (id, platform, label, created_at) in rows {
            conn.execute(
                "INSERT INTO vault_items (id, platform, label, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![id, platform, label, created_at],
            )
            .unwrap();
        }
        Store { conn }
    }

        /// 索引是**派生数据**:整表重抄之后,它必须完全等于"这一次扫描到的真相"。
    ///
    /// 这条测试盯的是最容易漏的一种走偏:逐条 upsert 只能"加和改",**清不掉**
    /// "索引里有、指针里没有"的行 —— 而那正是取消引用时索引没删干净的形态。
    #[test]
    fn vault_ref_rebuild_replaces_the_whole_index() {
        // 用 open_memory()(会跑 init_schema),不能用 mem():后者只有裸连接,
        // vault_refs 表根本没建出来 —— 那样测的是表不存在,不是对账逻辑。
        let s = Store::open_memory().unwrap();

        // 第一次对账:两个渠道各引用一条
        s.vault_ref_rebuild(&[
            ("4sapi".to_string(), "uuid-a".to_string()),
            ("hapi".to_string(), "uuid-b".to_string()),
        ])
        .unwrap();
        assert_eq!(s.vault_ref_channels("uuid-a").unwrap(), vec!["4sapi".to_string()]);
        assert_eq!(s.vault_ref_channels("uuid-b").unwrap(), vec!["hapi".to_string()]);

        // 第二次对账:4sapi 改引用 uuid-c,hapi 取消引用 —— 旧行必须消失
        s.vault_ref_rebuild(&[
            ("4sapi".to_string(), "uuid-c".to_string()),
            ("hapi".to_string(), "uuid-b".to_string()),
        ])
        .unwrap();
        assert!(
            s.vault_ref_channels("uuid-a").unwrap().is_empty(),
            "旧的 uuid-a 行必须被清掉,否则列表会说谎"
        );
        assert_eq!(s.vault_ref_channels("uuid-c").unwrap(), vec!["4sapi".to_string()]);

        // 第三次对账:什么都不剩(全部取消引用)
        s.vault_ref_rebuild(&[]).unwrap();
        assert!(s.vault_ref_pairs().unwrap().is_empty(), "空真相 -> 空索引");

        // 一个条目可以被多个渠道引用(裁决 6)
        s.vault_ref_rebuild(&[
            ("4sapi".to_string(), "uuid-shared".to_string()),
            ("hapi".to_string(), "uuid-shared".to_string()),
        ])
        .unwrap();
        let mut got = s.vault_ref_channels("uuid-shared").unwrap();
        got.sort();
        assert_eq!(got, vec!["4sapi".to_string(), "hapi".to_string()]);
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

    /// F8:有基线但**窗口内一条都没有** → None,不是 Some(0.0)。
    /// 报 0 等于说"这一期消耗 0",环比百分比会凭空被抬高(doc 早写明要两条快照)。
    #[test]
    fn consumption_is_none_when_the_window_itself_has_no_row() {
        let s = mem();
        // 只有窗口起点之前的那一条快照(基线),窗口里什么都没有
        s.record("p", 1000, Some(100.0), None, None, "amount").unwrap();
        assert_eq!(
            s.consumption_since("p", 2000, 3000).unwrap(),
            None,
            "窗口内没有快照时不能编一个 0 出来"
        );
        // 窗口内一旦有行,原有的"跨起点按时间折算"行为一字不变:
        // 基线 100(ts=1000)→ 窗口首行 80(ts=2500),起点 2000 处按线性折算余额
        // = 100 - 20×(1000/1500) = 86.67,差额 6.67
        s.record("p", 2500, Some(80.0), None, None, "amount").unwrap();
        assert_eq!(s.consumption_since("p", 2000, 3000).unwrap(), Some(6.67));
    }

    /// F10:老库里已经有重复的 (platform,label)(大小写不同也算同一对)时,
    /// 迁移要去重到最早那一行,并落下大小写不敏感的唯一索引。
    #[test]
    fn migration_dedupes_existing_vault_duplicates() {
        let s = mem_with_vault(&[
            ("a", "4SAPI", "主key", 100),
            ("b", "4sapi", "主KEY", 200), // 双击「保存」留下的副本
            ("c", "Hapi", "别的", 150),
        ]);
        s.migrate().unwrap();
        let list = s.vault_list().unwrap();
        assert_eq!(list.len(), 2, "同 (platform,label) 只留一条: {list:?}");
        let kept = list
            .iter()
            .find(|i| i.platform.eq_ignore_ascii_case("4sapi"))
            .expect("4SAPI 那条还在");
        assert_eq!(kept.id, "a", "留最早那一行(用户最初建的)");
        s.migrate().unwrap(); // 幂等:再跑一次不动数据也不报错
        // 把版本号退回去重跑一遍:去重 + CREATE INDEX IF NOT EXISTS 都能重入
        s.conn.execute_batch("PRAGMA user_version = 0;").unwrap();
        s.migrate().unwrap();
        assert_eq!(s.vault_list().unwrap().len(), 2);
    }

    /// F10:唯一索引在库这一层拒绝重复插入(应用层的"先查后插"隔着一次放开锁,
    /// 双击保存时挡不住第二个)。
    #[test]
    fn vault_unique_index_rejects_a_duplicate_insert() {
        let s = Store::open_memory().unwrap();
        let mk = |id: &str, platform: &str, label: &str, ts: i64| VaultItem {
            id: id.into(),
            platform: platform.into(),
            label: label.into(),
            hint: String::new(),
            note: String::new(),
            created_at: ts,
            updated_at: ts,
        };
        s.vault_upsert(&mk("a", "4SAPI", "主key", 100)).unwrap();
        let e = s
            .vault_upsert(&mk("b", "4sapi", "主KEY", 200))
            .unwrap_err();
        assert!(e.contains("UNIQUE"), "库级约束要真的拦住: {e}");
        assert_eq!(s.vault_list().unwrap().len(), 1);
        // 不同标签/不同平台照旧能插
        s.vault_upsert(&mk("c", "4sapi", "备用", 300)).unwrap();
        s.vault_upsert(&mk("d", "Hapi", "主KEY", 300)).unwrap();
        assert_eq!(s.vault_list().unwrap().len(), 3);
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
        assert_eq!(s.get_setting("interval").unwrap().as_deref(), Some("300"));
        s.set_setting("interval", "60").unwrap();
        assert_eq!(s.get_setting("interval").unwrap().as_deref(), Some("60"));
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
        assert_eq!(s.last_jump_since("p", 0, 20.0, 200.0, 0).unwrap(), Some(5000));
        // 阈值高于实际跳升(45)-> 只剩第一次(181.01)
        assert_eq!(s.last_jump_since("p", 0, 182.0, 200.0, 0).unwrap(), None);
        // 窗口把第一次跳升切掉
        assert_eq!(s.last_jump_since("p", 2500, 20.0, 200.0, 0).unwrap(), Some(5000));
    }

    #[test]
    fn small_wobbles_are_not_applications() {
        let s = mem();
        for (ts, rem) in [(1000, 100.0), (2000, 100.4), (3000, 99.8), (4000, 105.0)] {
            s.record("p", ts, Some(rem), None, None, "amount").unwrap();
        }
        // 5 元的抖动在 20 元阈值下不算申请
        assert_eq!(s.last_jump_since("p", 0, 20.0, 0.0, 0).unwrap(), None);
        assert_eq!(s.last_jump_since("p", 0, 4.0, 0.0, 0).unwrap(), Some(4000));
    }

    /// 平台对失败请求退还预扣,一次能涨几十元 —— 只看涨幅会把它记成"用户申请过",
    /// 于是申请倒计时被顶回去、用户白等(报告 P2-1)。
    #[test]
    fn refunds_far_below_the_quota_line_are_not_applications() {
        let s = mem();
        for (ts, rem) in [
            (1000, 12.0),
            (2000, 60.0), // 涨 48 元,但离 200 的额度线很远 -> 是退还,不是申请
            (3000, 40.0),
            (4000, 198.0), // 这才是一次申请
        ] {
            s.record("p", ts, Some(rem), None, None, "amount").unwrap();
        }
        assert_eq!(s.last_jump_since("p", 0, 20.0, 200.0, 0).unwrap(), Some(4000));
        // 隔太久的两条(休眠后第一跳)也不算跳升
        assert_eq!(s.last_jump_since("p", 0, 20.0, 200.0, 500).unwrap(), None);
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

    // ───────────────── 密钥指纹分段(换 key 账目不互相推算) ─────────────────

    /// 老库(没有 key_fp 列)升级后,列要真的补上,历史行落在空串里。
    #[test]
    fn migration_adds_key_fp_to_old_snapshots_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT, provider_id TEXT NOT NULL,
                ts INTEGER NOT NULL, remaining REAL, used REAL, total REAL, kind TEXT NOT NULL);
             INSERT INTO snapshots (provider_id, ts, remaining, kind) VALUES ('p', 1000, 80.0, 'amount');",
        )
        .unwrap();
        let s = Store { conn };
        s.migrate().unwrap();
        // 补出来的列能写能读:给历史行认领指纹
        assert_eq!(s.adopt_orphan_snapshots("p", "fpA").unwrap(), 1);
        assert_eq!(s.consumption_since_fp("p", "fpA", 500, 2000).unwrap(), None, "只有一条,推不出消耗");
        s.record_fp("p", "fpA", 2000, Some(70.0), None, None, "amount").unwrap();
        assert_eq!(s.consumption_since_fp("p", "fpA", 500, 3000).unwrap(), Some(10.0));
        // 幂等:再跑一次迁移不动数据
        s.migrate().unwrap();
        assert_eq!(s.fingerprint_count("p").unwrap(), 1);
    }

    /// 两本账互不推算;不带指纹的旧口径会把中间那笔跨账户跳变算成消耗 —— 这正是
    /// 分段要挡住的东西。
    #[test]
    fn fingerprint_scoping_keeps_accounts_apart() {
        let s = Store::open_memory().unwrap();
        s.record_fp("p", "A", 1000, Some(100.0), None, None, "amount").unwrap();
        s.record_fp("p", "A", 2000, Some(90.0), None, None, "amount").unwrap();
        s.record_fp("p", "B", 3000, Some(500.0), None, None, "amount").unwrap();
        s.record_fp("p", "B", 4000, Some(400.0), None, None, "amount").unwrap();
        assert_eq!(s.consumption_since_fp("p", "A", 500, 5000).unwrap(), Some(10.0));
        assert_eq!(s.consumption_since_fp("p", "B", 500, 5000).unwrap(), Some(100.0));
        // 旧口径(不分段):A→B 的 410 元跳升被 clamp 掉,但两本账的下降被加在一起
        assert_eq!(
            s.consumption_since("p", 500, 5000).unwrap(),
            Some(110.0),
            "不分段 = 跨账户求和,这就是升级后要避免的旧行为"
        );
        assert_eq!(s.fingerprint_count("p").unwrap(), 2, "两本账");
    }

    /// 认领只发生一次:换 key 写进来的新指纹不会被再认领走。
    #[test]
    fn adopt_orphans_claims_legacy_rows_only_once() {
        let s = Store::open_memory().unwrap();
        s.record("p", 1000, Some(50.0), None, None, "amount").unwrap(); // 升级前的行(空指纹)
        assert_eq!(s.adopt_orphan_snapshots("p", "fpA").unwrap(), 1);
        assert_eq!(s.adopt_orphan_snapshots("p", "fpA").unwrap(), 0, "再认领没有新行");
        s.record_fp("p", "fpB", 2000, Some(20.0), None, None, "amount").unwrap();
        assert_eq!(s.adopt_orphan_snapshots("p", "fpB").unwrap(), 0, "别把 A 的账抢过来");
        assert_eq!(s.fingerprint_count("p").unwrap(), 2);
        // 空指纹不认领(调用方没拿到稳定凭据时不该动数据)
        assert_eq!(s.adopt_orphan_snapshots("p", "").unwrap(), 0);
    }

    // ───────────────── 缺口覆盖 / 变动台账 / 长期趋势 ─────────────────

    #[test]
    fn window_and_daily_coverage_report_gaps() {
        let s = Store::open_memory().unwrap();
        let tz = 8 * 3600;
        // 北京 10-16:09:00 开机,23:00 关机;10-17 只观测了 14:00 之后两条
        let d16 = 16 * 86_400 - tz; // 10-16 00:00(北京)
        let d17 = 17 * 86_400 - tz;
        s.record("p", d16 + 9 * 3600, Some(100.0), None, None, "amount").unwrap();
        s.record("p", d16 + 23 * 3600, Some(80.0), None, None, "amount").unwrap();
        s.record("p", d17 + 14 * 3600, Some(70.0), None, None, "amount").unwrap();
        s.record("p", d17 + 15 * 3600, Some(66.0), None, None, "amount").unwrap();

        let cov = s.window_coverage("p", None, d16, d17 + 86_400).unwrap();
        assert_eq!(cov.count, 4);
        assert_eq!(cov.first_ts, Some(d16 + 9 * 3600));
        assert_eq!(cov.last_ts, Some(d17 + 15 * 3600));

        let days = s.daily_coverage("p", None, d16, d17 + 86_400, tz).unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].day_start, d16);
        assert_eq!(days[0].first_ts, d16 + 9 * 3600, "16 日 09:00 才开始有观测");
        assert_eq!(days[0].count, 2);
        assert_eq!(days[1].day_start, d17);
        assert_eq!(days[1].first_ts, d17 + 14 * 3600);
        assert_eq!(days[1].count, 2);
    }

    /// 台账:同方向的连续轮询并成一条,方向一变立刻另起;空档秒数如实带上。
    #[test]
    fn change_ledger_merges_consecutive_drops_and_splits_on_topup() {
        let s = Store::open_memory().unwrap();
        s.record("p", 9 * 3600, Some(100.0), None, None, "amount").unwrap();
        s.record("p", 9 * 3600 + 60, Some(99.5), None, None, "amount").unwrap();
        s.record("p", 9 * 3600 + 120, Some(99.0), None, None, "amount").unwrap();
        // 一小时后充值 +50
        s.record("p", 10 * 3600, Some(149.0), None, None, "amount").unwrap();
        // 紧接着又花掉 1 元
        s.record("p", 10 * 3600 + 60, Some(148.0), None, None, "amount").unwrap();

        let ch = s.recent_changes("p", None, 0, 20).unwrap();
        assert_eq!(ch.len(), 3, "掉→充→掉,三次变动:{ch:?}");
        // 最新在前
        assert!((ch[0].delta + 1.0).abs() < 1e-9 && ch[0].samples == 1);
        assert_eq!(ch[0].gap_secs, 60, "与上一笔间隔 1 分钟");
        assert!((ch[1].delta - 50.0).abs() < 1e-9, "充值那条是 +50");
        assert_eq!(ch[1].gap_secs, 10 * 3600 - (9 * 3600 + 120), "含一小时的空白");
        assert!((ch[2].delta + 1.0).abs() < 1e-9, "两笔 0.5 合并成 -1.0");
        assert_eq!(ch[2].samples, 2, "两次轮询参与了聚合");
        assert_eq!(ch[2].start_ts, 9 * 3600);
        assert_eq!(ch[2].end_ts, 9 * 3600 + 120);

        // limit 只留最近的几条(一条变动记的是 [起点, 终点点] 这一段)
        let one = s.recent_changes("p", None, 0, 1).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].start_ts, 10 * 3600);
        assert_eq!(one[0].end_ts, 10 * 3600 + 60);
    }

    /// 台账按指纹分段:另一本账的变动不该出现在这本账里。
    #[test]
    fn change_ledger_respects_fingerprint() {
        let s = Store::open_memory().unwrap();
        s.record_fp("p", "A", 1000, Some(100.0), None, None, "amount").unwrap();
        s.record_fp("p", "A", 1060, Some(95.0), None, None, "amount").unwrap();
        s.record_fp("p", "B", 1200, Some(500.0), None, None, "amount").unwrap();
        s.record_fp("p", "B", 1260, Some(400.0), None, None, "amount").unwrap();
        let a = s.recent_changes("p", Some("A"), 0, 20).unwrap();
        assert_eq!(a.len(), 1);
        assert!((a[0].delta + 5.0).abs() < 1e-9, "A 账只掉了 5");
    }

    /// 明细归档成日级摘要:删之前先压,摘要保留;再跑一次不重复累加。
    #[test]
    fn prune_archives_details_into_daily_summaries() {
        let s = Store::open_memory().unwrap();
        let tz = 8 * 3600;
        let now = 100 * 86_400;
        let day = |d: i64| d * 86_400 - tz; // 那天的本地零点
        // 三天明细,每天掉 10 元(相邻下降)
        for d in [10_i64, 11, 12] {
            s.record("p", day(d) + 10 * 3600, Some(100.0), None, None, "amount").unwrap();
            s.record("p", day(d) + 11 * 3600, Some(90.0), None, None, "amount").unwrap();
        }
        // 保留窗口:now - 20 天,只有 10~12 号那三天被清
        let cutoff = now - 20 * 86_400;
        let removed = s.prune(cutoff, tz).unwrap();
        assert_eq!(removed, 6, "6 条明细被清掉");

        let sums = s.daily_summaries("p", None, 0).unwrap();
        assert_eq!(sums.len(), 3, "三天都有摘要");
        assert_eq!(sums[0].day_start, day(10));
        assert!((sums[0].consumed - 10.0).abs() < 1e-9, "每天 10 元: {sums:?}");
        assert!((sums[2].consumed - 10.0).abs() < 1e-9, "每天 10 元: {sums:?}");
        assert_eq!(sums[0].first_ts, day(10) + 10 * 3600);
        assert_eq!(sums[0].last_ts, day(10) + 11 * 3600);
        assert_eq!(sums[0].count, 2);

        // 幂等:再清一次,摘要不会翻倍
        let again = s.prune(cutoff, tz).unwrap();
        assert_eq!(again, 0);
        let sums2 = s.daily_summaries("p", None, 0).unwrap();
        assert!((sums2[0].consumed - 10.0).abs() < 1e-9, "重跑不重复累加");
        assert_eq!(sums2[0].count, 2);
    }

    /// 摘要的保留期是"明细之外再一年":太老的日直接清掉,一年内的留着。
    #[test]
    fn daily_summary_retention_drops_ancient_days() {
        let s = Store::open_memory().unwrap();
        let tz = 8 * 3600;
        let now = 500 * 86_400;
        let old = (now - 400 * 86_400) / 86_400; // 400 天前
        let mid = (now - 100 * 86_400) / 86_400; // 100 天前
        for d in [old, mid] {
            s.record("p", d * 86_400 - tz + 10 * 3600, Some(100.0), None, None, "amount").unwrap();
            s.record("p", d * 86_400 - tz + 11 * 3600, Some(95.0), None, None, "amount").unwrap();
        }
        s.prune(now - 90 * 86_400, tz).unwrap();
        let sums = s.daily_summaries("p", None, 0).unwrap();
        assert_eq!(sums.len(), 1, "只留 100 天前那条(400 天前超出一年保留期): {sums:?}");
        assert_eq!(sums[0].day_start, mid * 86_400 - tz);
        assert!((sums[0].consumed - 5.0).abs() < 1e-9, "{sums:?}");
    }

    /// 百分比渠道不进归档:daily_summary 只收 kind='amount' 的消耗。
    #[test]
    fn archive_ignores_non_amount_kinds() {
        let s = Store::open_memory().unwrap();
        let tz = 0;
        let d = 10 * 86_400;
        s.record("p", d + 3600, Some(50.0), None, None, "percent").unwrap();
        s.record("p", d + 7200, Some(40.0), None, None, "percent").unwrap();
        s.prune(100 * 86_400, tz).unwrap();
        let sums = s.daily_summaries("p", None, 0).unwrap();
        assert_eq!(sums.len(), 1, "覆盖仍记(它证明应用在跑)");
        assert_eq!(sums[0].count, 2);
        assert!((sums[0].consumed - 0.0).abs() < 1e-9, "百分比渠道没有余额消耗");
    }

    /// 跨归档边界的那一笔不能丢(第三轮体检 P2-④)。
    /// 构造:10 号两条明细(100→90,掉 10),11 号两条(80→70),清理线压在 11 号零点 ——
    /// 于是"90→80"这一跳的**前一行在窗口内、后一行在窗口外**,修复前它两头都不沾:
    /// 这一轮读不到后一行,下一轮前一行已被删除 → 永久少记一笔。
    #[test]
    fn archive_keeps_the_drop_that_straddles_the_cutoff() {
        let s = Store::open_memory().unwrap();
        let tz = 0i64;
        let day = |d: i64| d * 86_400;
        s.record("p", day(10) + 10 * 3600, Some(100.0), None, None, "amount").unwrap();
        s.record("p", day(10) + 11 * 3600, Some(90.0), None, None, "amount").unwrap();
        s.record("p", day(11) + 3600, Some(80.0), None, None, "amount").unwrap();
        s.record("p", day(11) + 2 * 3600, Some(70.0), None, None, "amount").unwrap();

        s.prune(day(11), tz).unwrap();
        let sums = s.daily_summaries("p", None, 0).unwrap();
        assert_eq!(sums.len(), 1, "只有 10 号那天进了摘要: {sums:?}");
        assert!(
            (sums[0].consumed - 20.0).abs() < 1e-9,
            "10 号 = 当天 10 + 跨边界的 10: {sums:?}"
        );

        // 再清一轮:11 号那两条归档。同一对相邻行不能被算第二次(前一行已不在表里)。
        s.prune(day(12), tz).unwrap();
        let sums2 = s.daily_summaries("p", None, 0).unwrap();
        let d10 = sums2.iter().find(|x| x.day_start == day(10)).unwrap();
        let d11 = sums2.iter().find(|x| x.day_start == day(11)).unwrap();
        assert!((d10.consumed - 20.0).abs() < 1e-9, "10 号不该被再算一次: {sums2:?}");
        assert!((d11.consumed - 10.0).abs() < 1e-9, "11 号只有 80→70: {sums2:?}");
    }

    /// 长期趋势曲线:明细与日级摘要拼在一条线上;有缺口的日子标出来。
    #[test]
    fn trend_series_merges_archive_and_details_and_marks_gaps() {
        let s = Store::open_memory().unwrap();
        let tz = 0;
        let now = 100 * 86_400;
        let from = now - 10 * 86_400; // 10 天窗口
        // 8 天前:一天的明细(掉 10 元)——会被归档
        let old = now - 8 * 86_400;
        s.record("p", old + 3600, Some(100.0), None, None, "amount").unwrap();
        s.record("p", old + 7200, Some(90.0), None, None, "amount").unwrap();
        // 今天:明细(掉 3 元)
        s.record("p", now - 3600, Some(50.0), None, None, "amount").unwrap();
        s.record("p", now - 1800, Some(47.0), None, None, "amount").unwrap();
        // 归档(把 8 天前那天压进摘要并清掉明细;90 天清理线设在 5 天前)
        s.prune(now - 5 * 86_400, tz).unwrap();
        assert_eq!(s.daily_summaries("p", None, from).unwrap().len(), 1, "归档那天在摘要里");

        let (vals, gaps) = s.trend_series("p", None, from, now, 10, tz).unwrap();
        assert_eq!(vals.len(), 10);
        let total: f64 = vals.iter().sum();
        assert!((total - 13.0).abs() < 1e-9, "归档的 10 元 + 今天的 3 元 = 13:{vals:?}");
        // 8 天前那天:明细起点 01:00(离零点 1 小时 > 10 分钟)且末尾 02:00 收工 → 有缺口
        assert!(!gaps.is_empty(), "那天只观测了两小时,必须标成缺口");
        assert_eq!(vals[2], 10.0, "归档那天落在第 3 个桶");
    }

    /// 窗口里一天都没数据的那种"空",不算缺口(那是没数据,不是有缺口)。
    #[test]
    fn trend_series_ignores_days_without_any_observation() {
        let s = Store::open_memory().unwrap();
        let now = 100 * 86_400;
        let from = now - 10 * 86_400;
        s.record("p", now - 3600, Some(10.0), None, None, "amount").unwrap();
        s.record("p", now - 1800, Some(9.0), None, None, "amount").unwrap();
        let (vals, gaps) = s.trend_series("p", None, from, now, 10, 0).unwrap();
        let total: f64 = vals.iter().sum();
        assert!((total - 1.0).abs() < 1e-9);
        // 只有"今天"这一天有观测,其余 9 天是"没数据" —— 只有今天可能算缺口
        assert!(gaps.len() <= 1, "没数据的日子不该被标成缺口:{gaps:?}");
    }
}
