//! 积分到账事件:从余额快照的**正增量**里认出"今天到账了"。
//!
//! 为什么要有这一层:WorkBuddy AI(国际版)没有签到接口,「每天对话一次 +30 积分」
//! 只在余额上留痕。平台没有发放记录可查,只能靠两次取数的余额差反推 ——
//! 而反推的东西**不能当事实说**,所以每条事件都带 `source`,界面按来源标
//! "平台确认 / 按余额推算"。等哪天探到平台的发放接口,只换 `source`,表与界面不动。
//!
//! 设计见 `docs/design/design-workbuddy-ai-channel-2026-10-23.html`。

use serde::{Deserialize, Serialize};

/// 低于这个数的正增量当浮点噪声(接口给的是字符串小数)。
pub const GRANT_EPS: f64 = 0.5;

/// 每日活跃奖励的额度。2026-10-23 用户确认:「每天要对话一次才会有 +30 积分」。
pub const ACTIVITY_GRANT: f64 = 30.0;

/// 认活跃的容差。取数间隔里的正常消耗会把净增量压到 30 以下(+30 与 -4 落在同一轮),
/// 所以不能只认恰好 30.0;而充值通常是几百上千,不会落进这个窗口。
/// 窗口取 ±5:低于 25 的更可能是"小额充值 + 消耗"或"半截增量",宁可不算活跃。
pub const ACTIVITY_TOL: f64 = 5.0;

/// 两次成功取数之间的正增量。`None` = 没涨(消耗、持平,或首次取数没有基线)。
pub fn detect(prev: f64, cur: f64) -> Option<f64> {
    let d = cur - prev;
    if d < GRANT_EPS {
        None
    } else {
        Some(d)
    }
}

/// 这笔增量算不算"每日活跃奖励"。
pub fn is_activity(amount: f64) -> bool {
    (ACTIVITY_GRANT - ACTIVITY_TOL..=ACTIVITY_GRANT + ACTIVITY_TOL).contains(&amount)
}

/// 一条到账事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub channel: String,
    /// 本地自然日 `YYYY-MM-DD`。**后端算** —— 前端拿 `new Date()` 再算一遍必然对不上
    /// (与 `done_today` 同一条纪律)。
    pub day: String,
    pub ts: i64,
    pub amount: f64,
    /// `api` = 平台发放记录确认;`delta` = 余额正增量推算;`manual` = 用户手记。
    pub source: String,
    /// 观测前后的余额。只用于界面解释,不参与判定。
    pub from_bal: Option<f64>,
    pub to_bal: Option<f64>,
}

impl Grant {
    /// 这条算不算"活跃获取"。由金额反推,不单独存一列 —— 判定口径改了,老行跟着新口径走。
    pub fn is_activity(&self) -> bool {
        is_activity(self.amount)
    }
}

/// 某一天的汇总(界面只读这个,不自己遍历)。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayGrants {
    /// 当天全部正增量之和(含充值)
    pub total: f64,
    /// 其中落在活跃窗口里的部分
    pub activity_total: f64,
    /// 今天有没有拿到活跃奖励 —— 用户问的就是这一句
    pub has_activity: bool,
    /// 事件条数
    pub count: usize,
    /// 覆盖到几个自然日(近 7 天汇总用:事件条数不等于天数)
    pub days: usize,
    /// 这批事件里有没有平台确认的(`source=api`)。现在恒为 false ——
    /// 国际版还没有发放记录接口,全靠余额推算;接上以后这里会自己变 true。
    pub from_api: bool,
    pub last_at: Option<i64>,
}

/// 把明细压成一行。空切片返回全零(`has_activity=false`),不是 `None` ——
/// "今天没获取"是一个确定的结论,和"读不到"要分开(后者由调用方决定)。
pub fn summarize(rows: &[Grant]) -> DayGrants {
    let mut d = DayGrants::default();
    let mut seen: Vec<&str> = Vec::new();
    for g in rows {
        d.total += g.amount;
        d.count += 1;
        if g.is_activity() {
            d.activity_total += g.amount;
            d.has_activity = true;
        }
        if g.source == "api" {
            d.from_api = true;
        }
        if !seen.contains(&g.day.as_str()) {
            seen.push(&g.day);
        }
        d.last_at = Some(d.last_at.map_or(g.ts, |t| t.max(g.ts)));
    }
    d.days = seen.len();
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(ts: i64, amount: f64) -> Grant {
        Grant {
            channel: "workbuddy-ai".into(),
            day: "2026-10-23".into(),
            ts,
            amount,
            source: "delta".into(),
            from_bal: None,
            to_bal: None,
        }
    }

    #[test]
    fn detect_only_counts_increases() {
        assert_eq!(detect(100.0, 130.0), Some(30.0));
        assert_eq!(detect(100.0, 100.0), None);
        assert_eq!(detect(100.0, 60.0), None, "消耗不是到账");
        assert_eq!(detect(100.0, 100.4), None, "浮点噪声不算");
        assert_eq!(detect(100.0, 100.5), Some(0.5), "正好到阈值算");
        // 切片包过期会让余额掉,那更不是到账
        assert_eq!(detect(1000.0, 0.0), None);
    }

    #[test]
    fn activity_window_is_thirty_plus_minus_five() {
        assert!(is_activity(30.0));
        assert!(is_activity(25.0), "取数间隔里被消耗压下来的 30 也要认");
        assert!(is_activity(35.0));
        assert!(!is_activity(24.9), "太低更像半截增量,宁可不算");
        assert!(!is_activity(36.0));
        assert!(!is_activity(500.0), "充值是另一个量级");
        assert!(!is_activity(0.0));
    }

    #[test]
    fn summarize_splits_activity_from_topups() {
        let d = summarize(&[g(100, 30.0), g(200, 500.0)]);
        assert_eq!(d.total, 530.0);
        assert_eq!(d.activity_total, 30.0);
        assert!(d.has_activity);
        assert_eq!(d.count, 2);
        assert_eq!(d.days, 1, "同一天两条事件只算一天");
        assert!(!d.from_api, "推算出来的不是平台确认");
        assert_eq!(d.last_at, Some(200));
    }

    #[test]
    fn summarize_counts_distinct_days() {
        let mut a = g(100, 30.0);
        a.day = "2026-10-22".into();
        let b = g(200, 30.0);
        let d = summarize(&[a, b]);
        assert_eq!(d.days, 2);
        assert_eq!(d.count, 2);
    }

    #[test]
    fn summarize_empty_day_is_a_known_zero() {
        let d = summarize(&[]);
        assert_eq!(d.total, 0.0);
        assert!(!d.has_activity);
        assert_eq!(d.count, 0);
        assert_eq!(d.last_at, None);
    }

    #[test]
    fn topup_only_day_is_not_activity() {
        let d = summarize(&[g(100, 500.0)]);
        assert!(d.total > 0.0);
        assert!(!d.has_activity, "充值到账 ≠ 活跃获取");
    }
}
