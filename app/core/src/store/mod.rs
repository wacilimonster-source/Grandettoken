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
mod schema;
mod snapshots;
pub mod gateway;
use std::sync::{Arc, atomic::AtomicU64};

pub struct Store {
    conn: Connection,
    pub(crate) vault_revision: Arc<AtomicU64>,
    pub(crate) model_revision: Arc<AtomicU64>,
}

/// 变动台账里"同方向合并"的时间窗:轮询是分钟级的,一次连续消费会攒成几十条。
/// 20 分钟能把最长的退避轮询(15 分钟)也并进同一条,又不会把"隔小时的两笔"误并。
const MERGE_GAP: i64 = 20 * 60;

/// 判"数据有缺口"的空档线:窗口起点/一天的两头离开观测时刻超过它,才算缺口。
/// 正常轮询是分钟级的,不会被误伤。定在 store 这一层,因为**归档(日级摘要)、
/// 曲线、界面文案三处都用它**,必须只有一个数字。
pub const OBSERVE_GAP_SEC: i64 = 10 * 60;

/// 一个窗口里的快照覆盖情况(缺口标注用)。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WindowCoverage {
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    pub count: i64,
}

/// 某一自然日的快照覆盖(长期趋势柱图的"缺口日"判据)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DayCoverage {
    pub day_start: i64,
    pub first_ts: i64,
    pub last_ts: i64,
    pub count: i64,
}

/// 一条历史快照。`remaining = None` 的行仍然是一次**观测**(缺口/覆盖要算它),
/// 只是不能参与余额差推算。
///
/// 批量读取(P1)返回这个形状,再交给下面那组纯函数切窗口 —— 同一份历史只从
/// SQLite 读一次,不再为每个窗口重复执行重叠查询。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HistoryRow {
    pub ts: i64,
    pub remaining: Option<f64>,
}

/// 批量历史读取一次最多带回来的行数。超过就报错,让调用方退回逐窗口查询 ——
/// 90 天 × 15 秒间隔约 52 万行,一次性展开到内存不划算。
const MAX_HISTORY_ROWS: usize = 500_000;

/// 一次查询的 SQL 上限。多读一行用来判断"是否超限",不会把超限结果交给调用方。
const HISTORY_QUERY_LIMIT: usize = MAX_HISTORY_ROWS + 1;

// 测试用:本线程调用过几次 `history_between`。
//
// 用 thread_local 而不是全局原子量:`cargo test` 默认多线程跑,每个测试线程
// 各数各的;`build_view` 本身是同步函数,同样在测试线程里完成。
#[cfg(test)]
thread_local! {
    static HISTORY_READS: std::cell::Cell<usize> = std::cell::Cell::new(0);
    // 测试钩子:强制 `history_between` 失败,验证调用方会整块回退旧查询。
    static FORCE_HISTORY_FAIL: std::cell::Cell<bool> = std::cell::Cell::new(false);
}

#[cfg(test)]
pub fn reset_history_reads() {
    HISTORY_READS.with(|c| c.set(0));
}

#[cfg(test)]
pub fn history_read_count() -> usize {
    HISTORY_READS.with(|c| c.get())
}

#[cfg(test)]
pub fn force_history_failure(on: bool) {
    FORCE_HISTORY_FAIL.with(|c| c.set(on));
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
    fn from_conn(conn: Connection) -> Self {
        Self {
            conn,
            vault_revision: Arc::default(),
            model_revision: Arc::default(),
        }
    }

    const ADOPT_ORPHANS: &str =
        "UPDATE snapshots SET key_fp = ?2 WHERE provider_id = ?1 AND key_fp = ''";
    const FINGERPRINT_COUNT: &str =
        "SELECT COUNT(DISTINCT key_fp) FROM snapshots WHERE provider_id = ?1";
    const HISTORY_FP: &str = "SELECT ts, remaining FROM snapshots
        WHERE provider_id = ?1 AND key_fp = ?2 AND ts >= ?3 AND ts <= ?4
        ORDER BY ts ASC, remaining ASC, id ASC LIMIT ?5";
    const ROWS_FP: &str = "SELECT ts, remaining FROM snapshots
        WHERE provider_id = ?1 AND key_fp = ?4 AND ts >= ?2 AND ts <= ?3
          AND remaining IS NOT NULL ORDER BY ts ASC, remaining ASC, id ASC";



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

mod checkin;
pub use checkin::{CheckinDayAgg, CheckinRow};

mod vault;
pub use vault::VaultItem;

impl Store {
    /// 校验数据库页与索引完整性（正常返回 `"ok"`）。
    pub fn integrity_check(&self) -> Result<String, String> {
        self.conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .map_err(|e| format!("数据库完整性检查失败: {e}"))
    }
    pub(crate) fn conn(&self) -> &Connection { &self.conn }
}

// ───────────── 批量历史(P1)的纯函数 ─────────────
//
// 这些函数只接收已经读进内存的行,不碰数据库。公式与原 SQL 查询**逐行一致**
// (负差值 clamp 成 0、跨窗口起点按时间折算、金额保留两位),用于:
//   ① `build_view` 每轮只读一次历史,再在内存里切出所有窗口;
//   ② 与旧查询做逐值差分测试 —— 旧实现保留为 fallback 与测试 oracle。

/// 把游标里的行收进 Vec;坏行记一行日志后跳过(与 `rows_between` 同一条纪律:
/// 静默丢行 = 数字悄悄偏小且无从排查)。
fn collect_history_rows<I>(rows: I, out: &mut Vec<HistoryRow>)
where
    I: Iterator<Item = rusqlite::Result<HistoryRow>>,
{
    for r in rows {
        match r {
            Ok(v) => out.push(v),
            Err(e) => eprintln!("读取快照行失败(该行已跳过): {e}"),
        }
    }
}

/// [`Store::consumption_since`] 的纯函数版本。
///
/// 只看 `[from_ts - 86400, to_ts]` 内、余额非空的行 —— 下界与原查询相同:
/// 窗口起点前一天是"基线"的范围,更早的行不属于这个窗口(否则长期空洞前后的
/// 两条快照会被按时间折算,数字静默偏大)。
pub(crate) fn consumption_since_rows(rows: &[HistoryRow], from_ts: i64, to_ts: i64) -> Option<f64> {
    let lo = from_ts - 86_400;
    let in_range = |r: &&HistoryRow| {
        r.remaining.is_some() && r.ts >= lo && r.ts <= to_ts
    };

    // 窗口内一条都没有:只剩窗口起点之前的基线 —— "这一期还没数据",不是 0。
    let mut any_in_window = false;
    let mut has_baseline = false;
    for r in rows.iter().filter(in_range) {
        if r.ts >= from_ts {
            any_in_window = true;
        }
        if r.ts <= from_ts {
            has_baseline = true;
        }
    }
    if !any_in_window {
        return None;
    }

    let mut sum = 0.0;
    if !has_baseline {
        // 没有基线:退化为「窗口内首快照起算」的下限估算,不足两条不出数。
        let mut prev: Option<f64> = None;
        let mut n = 0;
        for r in rows.iter().filter(in_range).filter(|r| r.ts >= from_ts) {
            if let Some(v) = r.remaining {
                if let Some(p) = prev {
                    sum += (p - v).max(0.0);
                }
                prev = Some(v);
                n += 1;
            }
        }
        if n < 2 {
            return None;
        }
        return Some((sum * 100.0).round() / 100.0);
    }

    let mut prev: Option<(i64, f64)> = None;
    for r in rows.iter().filter(in_range) {
        let Some(remaining) = r.remaining else {
            continue;
        };
        let ts = r.ts;
        if let Some((pts, prem)) = prev {
            if pts < from_ts {
                if ts < from_ts {
                    prev = Some((ts, remaining));
                    continue;
                }
                // 跨窗口起点的一对:基线→起点的下降发生在窗口外,按时间线性折算。
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
    Some((sum * 100.0).round() / 100.0)
}

/// [`Store::consumed_in_window`] 的纯函数版本:只累加窗口内相邻快照的下降。
#[cfg(test)]
pub(crate) fn consumed_in_window_rows(rows: &[HistoryRow], from_ts: i64, to_ts: i64) -> f64 {
    let mut sum = 0.0;
    let mut prev: Option<f64> = None;
    for r in rows
        .iter()
        .filter(|r| r.ts >= from_ts && r.ts <= to_ts)
        .filter_map(|r| r.remaining)
    {
        if let Some(p) = prev {
            sum += (p - r).max(0.0);
        }
        prev = Some(r);
    }
    (sum * 100.0).round() / 100.0
}

/// [`Store::window_coverage`] 的纯函数版本。覆盖统计**包含** `remaining IS NULL`
/// 的行 —— 那也是一次"应用在跑"的观测。
pub(crate) fn window_coverage_rows(rows: &[HistoryRow], from_ts: i64, to_ts: i64) -> WindowCoverage {
    let mut cov = WindowCoverage::default();
    for r in rows.iter().filter(|r| r.ts >= from_ts && r.ts <= to_ts) {
        cov.count += 1;
        if cov.first_ts.map_or(true, |f| r.ts < f) {
            cov.first_ts = Some(r.ts);
        }
        if cov.last_ts.map_or(true, |l| r.ts > l) {
            cov.last_ts = Some(r.ts);
        }
    }
    cov
}

/// [`Store::daily_coverage`] 的纯函数版本。分桶用调用方给的时区偏移,
/// 与 SQL 的 `(ts + tz) / 86400` 同一个截断口径;行已按 ts 升序,桶天然有序。
#[cfg(test)]
pub(crate) fn daily_coverage_rows(
    rows: &[HistoryRow],
    from_ts: i64,
    to_ts: i64,
    tz_offset: i64,
) -> Vec<DayCoverage> {
    let mut out: Vec<DayCoverage> = Vec::new();
    for r in rows.iter().filter(|r| r.ts >= from_ts && r.ts <= to_ts) {
        let day_start = ((r.ts + tz_offset) / 86_400) * 86_400 - tz_offset;
        match out.last_mut() {
            Some(last) if last.day_start == day_start => {
                last.last_ts = last.last_ts.max(r.ts);
                last.count += 1;
            }
            _ => out.push(DayCoverage {
                day_start,
                first_ts: r.ts,
                last_ts: r.ts,
                count: 1,
            }),
        }
    }
    out
}

/// [`Store::last_jump_since`] 的纯函数版本:顺序扫描,取最后一次"像申请的"跳升。
#[cfg(test)]
pub(crate) fn last_jump_rows(
    rows: &[HistoryRow],
    from_ts: i64,
    min_delta: f64,
    target: f64,
    max_gap: i64,
) -> Option<i64> {
    let mut last = None;
    let mut prev: Option<(i64, f64)> = None;
    for (ts, remaining) in rows
        .iter()
        .filter(|r| r.ts >= from_ts)
        .filter_map(|r| r.remaining.map(|v| (r.ts, v)))
    {
        if let Some((pts, prem)) = prev {
            // 只看涨幅会把预扣退还记成申请;落在额度线附近、且相邻两条不能隔太久。
            if remaining - prem >= min_delta
                && (target <= 0.0 || (remaining - target).abs() <= target * 0.15)
                && (max_gap <= 0 || ts - pts <= max_gap)
            {
                last = Some(ts);
            }
        }
        prev = Some((ts, remaining));
    }
    last
}

/// [`Store::first_ts_since`] 的纯函数版本(包含 `remaining IS NULL` 的行)。
#[cfg(test)]
pub(crate) fn first_ts_rows(rows: &[HistoryRow], from_ts: i64) -> Option<i64> {
    rows.iter()
        .filter(|r| r.ts >= from_ts)
        .map(|r| r.ts)
        .min()
}

#[cfg(test)]
mod tests;
