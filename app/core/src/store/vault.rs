use super::{params, Store};
use std::sync::atomic::Ordering;

/// 密钥箱的一条索引元数据。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultItem {
    pub id: String,
    pub platform: String,
    pub label: String,
    /// 尾 4 位掩码(存下来,列表不需要解密)
    pub hint: String,
    pub note: String,
    #[serde(default)]
    pub api_endpoint: String,
    #[serde(default)]
    pub website: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Store {
    pub fn vault_list(&self) -> Result<Vec<VaultItem>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, platform, label, hint, note, created_at, updated_at, api_endpoint, website
                 FROM vault_items ORDER BY platform COLLATE NOCASE, created_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], row_to_vault)
            .map_err(|e| e.to_string())?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 写索引(新建或改名/改备注)。密钥本身由 `secrets::vault_set` 负责。
    pub fn vault_upsert(&self, it: &VaultItem) -> Result<(), String> {
        self.write_vault_metadata(it)?;
        self.vault_revision.fetch_add(1, Ordering::Release);
        Ok(())
    }

    fn write_vault_metadata(&self, it: &VaultItem) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO vault_items (id, platform, label, hint, note, created_at, updated_at, api_endpoint, website)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                   platform = excluded.platform, label = excluded.label,
                   hint = excluded.hint, note = excluded.note, updated_at = excluded.updated_at,
                   api_endpoint = excluded.api_endpoint, website = excluded.website",
                params![
                    it.id,
                    it.platform,
                    it.label,
                    it.hint,
                    it.note,
                    it.created_at,
                    it.updated_at,
                    it.api_endpoint,
                    it.website
                ],
            )
            .map_err(|e| format!("写密钥箱索引失败: {e}"))?;
        Ok(())
    }

    /// 默认条目和映射属于同一次索引提交；失败时两者都保留旧值。
    pub fn vault_upsert_default(&self, item: &VaultItem, mapping: &str) -> Result<(), String> {
        let transaction = self
            .conn
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        self.write_vault_metadata(item)?;
        self.set_setting(mapping, &item.id)?;
        transaction
            .commit()
            .map_err(|e| format!("保存默认密钥映射失败: {e}"))?;
        self.vault_revision.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// 发布端点和网关设置共享提交边界；只改端点元数据，避免旧快照覆盖备注或密钥提示。
    pub fn vault_metadata_and_setting(
        &self,
        key: &str,
        value: &str,
        vault_version: Option<(&str, i64)>,
        endpoint: Option<(&str, i64)>,
    ) -> Result<(), String> {
        let transaction = self
            .conn
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        if let Some((id, expected)) = vault_version {
            if self.vault_get(id)?.map(|item| item.updated_at) != Some(expected) {
                return Err("关联密钥已变化或删除，请重新上架".into());
            }
            if let Some((url, updated_at)) = endpoint {
                self.conn.execute(
                    "UPDATE vault_items SET api_endpoint = ?1, updated_at = ?2 WHERE id = ?3 AND updated_at = ?4",
                    params![url, updated_at, id, expected],
                ).map_err(|e| format!("保存密钥箱端点失败: {e}"))?;
            }
        } else if endpoint.is_some() {
            return Err("保存端点需要关联密钥箱条目".into());
        }
        self.set_setting(key, value)?;
        transaction
            .commit()
            .map_err(|e| format!("保存网关及端点失败: {e}"))?;
        if endpoint.is_some() {
            self.vault_revision.fetch_add(1, Ordering::Release);
        }
        Ok(())
    }

    pub fn vault_delete(&self, id: &str) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM vault_items WHERE id = ?1", params![id])
            .map_err(|e| format!("删密钥箱索引失败: {e}"))?;
        self.vault_revision.fetch_add(1, Ordering::Release);
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
            .execute(
                "DELETE FROM vault_refs WHERE channel_id = ?1",
                params![channel_id],
            )
            .map_err(|e| format!("清除密钥引用失败: {e}"))?;
        Ok(())
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
                "SELECT id, platform, label, hint, note, created_at, updated_at, api_endpoint, website
                 FROM vault_items WHERE id = ?1",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map(params![id], row_to_vault)
            .map_err(|e| e.to_string())?;
        Ok(rows.next().and_then(|r| r.ok()))
    }

    /// 把 WAL 日志刷回主库并截断到 0 字节，防止 `-wal` 长期膨胀。
    pub fn wal_checkpoint(&self) -> Result<(i64, i64, i64), String> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(|e| format!("WAL checkpoint 失败: {e}"))
    }
}

fn row_to_vault(r: &rusqlite::Row<'_>) -> rusqlite::Result<VaultItem> {
    Ok(VaultItem {
        id: r.get(0)?,
        platform: r.get(1)?,
        label: r.get(2)?,
        hint: r.get(3)?,
        note: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        api_endpoint: r.get(7)?,
        website: r.get(8)?,
    })
}
