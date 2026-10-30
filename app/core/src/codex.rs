//! Codex 重置卡只读元数据。参考 CodexBar 的 wham 接口契约;
//! 不保存卡 ID、不兑换卡,也不从配额下降推断“发生过重置”。
use serde::Serialize;
use serde_json::Value;

pub const RESET_CREDITS_URL: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetCredit {
    pub title: String,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetCredits {
    /// None = 连数量也未知;不能用 0 代替查询失败。
    pub available_count: Option<u64>,
    /// None = 未取得明细;Some([]) = 成功取得空明细。
    pub credits: Option<Vec<ResetCredit>>,
    pub last_redeemed_at: Option<i64>,
    pub updated_at: i64,
    pub error: Option<String>,
}

/// REST 明细返回 ISO 8601;兼容 usage/RPC 里的 Unix 秒/毫秒。
fn timestamp(v: &Value) -> Option<i64> {
    let n = crate::providers::num(v);
    let ts = if let Some(n) = n {
        if n > 1e11 { (n / 1000.0) as i64 } else { n as i64 }
    } else {
        chrono::DateTime::parse_from_rfc3339(v.as_str()?).ok()?.timestamp()
    };
    (ts > 0 && chrono::DateTime::from_timestamp(ts, 0).is_some()).then_some(ts)
}

pub fn summary(body: &Value, now: i64) -> Option<ResetCredits> {
    let available_count = body.get("available_count")?.as_u64()?;
    Some(ResetCredits {
        available_count: Some(available_count),
        credits: None,
        last_redeemed_at: None,
        updated_at: now,
        error: None,
    })
}

pub fn inventory(body: &Value, now: i64) -> Result<ResetCredits, &'static str> {
    let mut result = summary(body, now).ok_or("重置卡响应缺少可用数量")?;
    // 接口可能只给计数;null 与空数组的语义不同。
    let rows = match body.get("credits") {
        None | Some(Value::Null) => return Ok(result),
        Some(Value::Array(rows)) => rows,
        _ => return Err("重置卡明细格式已变化"),
    };
    let mut credits = Vec::new();
    for row in rows {
        if row.get("status").and_then(Value::as_str) == Some("available") {
            // 有日期却无法解析,整份明细报未知,不能误说成“无到期日”。
            let expires_at = match row.get("expires_at") {
                None | Some(Value::Null) => None,
                Some(v) => Some(timestamp(v).ok_or("重置卡到期时间格式已变化")?),
            };
            if expires_at.map_or(false, |at| at <= now) {
                continue;
            }
            credits.push(ResetCredit {
                title: row.get("title").and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty()).unwrap_or("额度重置卡").to_string(),
                expires_at,
            });
        }
        if row.get("status").and_then(Value::as_str) == Some("redeemed") {
            if let Some(at) = row.get("redeemed_at").and_then(timestamp) {
                result.last_redeemed_at = Some(result.last_redeemed_at.map_or(at, |old| old.max(at)));
            }
        }
    }
    credits.sort_by_key(|c| c.expires_at.unwrap_or(i64::MAX));
    result.credits = Some(credits);
    Ok(result)
}

pub fn unavailable(previous_summary: Option<ResetCredits>, message: String) -> ResetCredits {
    let mut result = previous_summary.unwrap_or(ResetCredits {
        available_count: None, credits: None, last_redeemed_at: None,
        updated_at: 0, error: None,
    });
    result.error = Some(message);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inventory_dates_statuses_and_authoritative_count() {
        let r = inventory(&json!({"available_count": 7, "credits": [
            {"status":"available", "expires_at":null},
            {"status":"available", "expires_at":"2026-11-01T00:00:00.123Z", "title":"Full reset"},
            {"status":"available", "expires_at":1793404800000_i64},
            {"status":"available", "expires_at":1},
            {"status":"expired", "expires_at":1793404800},
            {"status":"redeemed", "redeemed_at":"2026-10-29T12:30:00Z"},
            {"status":"redeemed", "redeemed_at":"2026-10-28T12:30:00Z"}
        ]}), 1793318400).unwrap();
        assert_eq!(r.available_count, Some(7)); // 明细可能被上游截断
        let cards = r.credits.unwrap();
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].expires_at, Some(1793404800));
        assert_eq!(cards[1].title, "Full reset");
        assert_eq!(cards[2].expires_at, None);
        assert_eq!(r.last_redeemed_at, Some(1793277000));
    }

    #[test]
    fn unknown_is_distinct_from_zero_and_empty() {
        assert!(inventory(&json!({}), 100).is_err());
        assert!(inventory(&json!({"available_count":-1}), 100).is_err());
        assert!(inventory(&json!({"available_count":1,"credits":[
            {"status":"available","expires_at":"invalid"}
        ]}), 100).is_err());
        let empty = inventory(&json!({"available_count":0,"credits":[]}), 100).unwrap();
        assert_eq!(empty.credits.unwrap().len(), 0);
        let partial = inventory(&json!({"available_count":2,"credits":null}), 100).unwrap();
        assert!(partial.credits.is_none());
        let failed = unavailable(Some(partial), "HTTP 403".into());
        assert_eq!(failed.available_count, Some(2));
        assert!(failed.error.is_some());
        assert!(unavailable(None, "超时".into()).available_count.is_none());
    }
}
