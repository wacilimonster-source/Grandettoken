use super::{params, Connection, Store};
use std::path::Path;

pub(super) const SCHEMA_VERSION: i64 = 10;

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建数据目录失败: {e}"))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("打开数据库失败: {e}"))?;
        // 双开或 WAL 写者竞争时,SQLite 默认立刻回 SQLITE_BUSY —— 之前快照就是
        // 这样被静默吞掉的;等 5 秒锁再报错,把可恢复的冲突变成可恢复的等待(O-7)。
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        let s = Self::from_conn(conn);
        s.init_schema()?;
        s.migrate()?;
        Ok(s)
    }

    pub fn open_readonly(path: &Path) -> Result<Self, String> {
        use rusqlite::OpenFlags;
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("打开只读数据库失败: {e}"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "query_only", "ON")
            .map_err(|e| e.to_string())?;
        Ok(Self::from_conn(conn))
    }

    /// 测试用:内存库,不落盘,给跨模块的组装逻辑(如申请制状态)当输入。
    #[cfg(test)]
    pub fn open_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("打开数据库失败: {e}"))?;
        let s = Self::from_conn(conn);
        s.init_schema()?;
        s.migrate()?;
        Ok(s)
    }
}

impl Store {
    /// 版本化迁移。`CREATE TABLE IF NOT EXISTS` 对**已存在**的表零作用 —— 既不补列
    /// 也不校验形状,老库要等运行期报 "no such column" 才发现(报告 P2-17)。
    pub(super) fn migrate(&self) -> Result<(), String> {
        // 2 = vault_items 按 (platform,label) 去重 + 唯一索引(下面那段),老库也补跑
        // 3 = snapshots 加 key_fp(密钥指纹分段,换 key 后两本账不互相推算消耗)
        // 4 = credit_grants(积分到账事件,WorkBuddy AI 的每日活跃 +30 等)
        // 5 = snapshots 的覆盖索引 idx_snap_fp_cover(P1b:批量历史读取不回表); 7 = gateway_usage_log; 8 = 台账 agent 列
        // 9 = 快照覆盖索引; 10 = 移除无查询使用的台账 (day, channel_id) 索引。
        let cur: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|e| format!("读取数据库版本号失败: {e}"))?;
        if cur >= SCHEMA_VERSION {
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
                    ("api_endpoint", "TEXT NOT NULL DEFAULT ''"),
                    ("website", "TEXT NOT NULL DEFAULT ''"),
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
        // 分组可以包含多个账号：保留所有旧数据，移除旧的「平台+标签」唯一约束。
        if self.table_exists("vault_items")? {
            self.conn.execute_batch("DROP INDEX IF EXISTS idx_vault_uniq;")
                .map_err(|e| format!("更新密钥箱分组失败: {e}"))?;
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
        self.migrate_gateway_ledger()?;
        self.add_column_if_missing("gateway_usage_log", "agent", "TEXT")?;
        // 索引替换和版本号同事务，失败后仍可用旧索引并在下次打开时重试。
        let tx = self.conn.unchecked_transaction()
            .map_err(|e| format!("开始快照索引迁移失败: {e}"))?;
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_snap_fp_cover
                 ON snapshots (provider_id, key_fp, ts, remaining);
             DROP INDEX IF EXISTS idx_snap_fp;
             DROP INDEX IF EXISTS idx_gw_usage_day_ch;",
        ).map_err(|e| format!("更新快照指纹索引失败: {e}"))?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|e| format!("写数据库版本号失败: {e}"))?;
        tx.commit().map_err(|e| format!("提交快照索引迁移失败: {e}"))
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


    pub(super) fn init_schema(&self) -> Result<(), String> {
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
                api_endpoint TEXT  NOT NULL DEFAULT '',
                website    TEXT    NOT NULL DEFAULT '',
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

}
