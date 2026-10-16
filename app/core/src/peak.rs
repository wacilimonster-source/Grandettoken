//! DeepSeek 峰谷时段。
//!
//! 只回答两件事:**现在贵不贵**(高峰 / 谷价)和**下一次切换在什么时候**。
//! 刻意**不落任何单价数字** —— 规则两个月里官方改过三次(2026-08-17 首推、
//! 08-23 周末改谷价、09-19 补法定节假日条文),落单价就是把我们钉在一个会变的
//! 数字上;时段判据本身很稳,而且判错的代价只是"提示不准",不涉及账目。
//!
//! 规则(2026-10-16 口径,来源:官方价目页 <https://api-docs.deepseek.com/zh-cn/quick_start/pricing>
//! + 2026-08-22 调价公告的媒体口径):
//!   · 工作日(不含法定节假日)北京时间 9:00–12:00、14:00–18:00 = 高峰;
//!   · 其余时段(夜间、周末、法定节假日) = 谷价;
//!   · 周末全天谷价(2026-08-23 起),法定节假日全天谷价 —— **调休上班的周末也算谷价**
//!     (判据是日历上的周六日,不是"那天上不上班")。
//!
//! 时间一律按**北京时间**(UTC+8 固定偏移)判,不吃系统时区:定价参照的是中国日历,
//! 机器时区不是 +8 时用本地时间会把日子/时段算错。
//!
//! 节假日表是**清单式常量**:国务院每年 11 月前后发布次年安排,发布后要来这里补。
//! 表覆盖不到"当前这一年及之后"时,[`table_stale`] 会如实报出来 —— 界面据此提示
//! 「明年节假日表待更新,先按常规周末规则估算」,而不是假装知道。

/// 高峰时段(北京时间整点,左闭右开)。
pub const PEAK_HOURS: [(i64, i64); 2] = [(9, 12), (14, 18)];

/// 北京时间相对 UTC 的固定偏移(秒)。
pub const BJ_OFFSET: i64 = 8 * 3600;

/// 探下一跳时最多往前扫多少天。最长假期(2026 春节 9 天)也够,与规则无关地留足余量。
const SCAN_DAYS: i64 = 12;

/// 法定节假日(放假日期,北京时间日历日 YYYY-MM-DD)。
///
/// 只列**放假**的日子:调休上班的周六日本来就被"周末全天谷价"覆盖,不用单列
/// (2026 年的调休上班日 1/4、2/14、2/28、5/9、9/20、10/10 全部落在周末)。
/// 依据:《国务院办公厅关于 2026 年部分节假日安排的通知》。
pub const HOLIDAY_VALLEY: &[&str] = &[
    "2026-01-01", "2026-01-02", "2026-01-03", // 元旦
    "2026-02-15", "2026-02-16", "2026-02-17", "2026-02-18", "2026-02-19", // 春节(2/15–2/23)
    "2026-02-20", "2026-02-21", "2026-02-22", "2026-02-23",
    "2026-04-04", "2026-04-05", "2026-04-06", // 清明
    "2026-05-01", "2026-05-02", "2026-05-03", "2026-05-04", "2026-05-05", // 劳动节
    "2026-06-19", "2026-06-20", "2026-06-21", // 端午
    "2026-09-25", "2026-09-26", "2026-09-27", // 中秋
    "2026-10-01", "2026-10-02", "2026-10-03", "2026-10-04", // 国庆(10/1–10/7)
    "2026-10-05", "2026-10-06", "2026-10-07",
];

/// 一次判定的全部结论(界面直接用)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeakInfo {
    /// true = 高峰(工作日白天两段);false = 谷价。
    pub peak: bool,
    /// 下一次峰↔谷切换的时刻(epoch 秒);扫不到给 None(当前规则下不会发生)。
    pub next_change_at: Option<i64>,
    /// 此刻是不是法定节假日(整日谷价的原因之一)。
    pub holiday: bool,
    /// 此刻是不是周末(整日谷价的原因之一)。
    pub weekend: bool,
}

// ───────────────────────── 日历换算(纯算术,不引 chrono) ─────────────────────────

/// 1970-01-01 起的天数 → (y, m, d)。Howard Hinnant 的 civil_from_days 算法。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (y, m, d) → 1970-01-01 起的天数。Hinnant 的 days_from_civil 算法。
/// 生产路径只用得着反方向(`civil_from_days`);这个方向是**测试造时间戳**用的,
/// 所以按仓库惯例挂 `#[cfg(test)]`,不留死代码警告。真要用在生产里再摘掉。
#[cfg(test)]
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// 北京时间日历日索引(1970-01-01 为 0)。周末判定与节假日查表都以它为准。
fn bj_day_index(ts: i64) -> i64 {
    (ts + BJ_OFFSET).div_euclid(86_400)
}

/// 北京时间当日秒数 [0, 86400)。
fn bj_secs_of_day(ts: i64) -> i64 {
    (ts + BJ_OFFSET).rem_euclid(86_400)
}

/// Beijing 的星期几(0 = 周日)。
fn bj_weekday(ts: i64) -> i64 {
    // 1970-01-01 是周四 → 索引 0 那天 dow = 4(以周日为 0)
    (bj_day_index(ts) + 4).rem_euclid(7)
}

/// 北京时间日历日 → "YYYY-MM-DD"(节假日表就是按它比的)。
fn bj_date_string(ts: i64) -> String {
    let (y, m, d) = civil_from_days(bj_day_index(ts));
    format!("{y:04}-{m:02}-{d:02}")
}

fn is_holiday(ts: i64) -> bool {
    let key = bj_date_string(ts);
    HOLIDAY_VALLEY.binary_search(&key.as_str()).is_ok()
}

/// 此刻是不是高峰(工作日非节假日、且落在两段高峰小时内)。
pub fn is_peak_at(ts: i64) -> bool {
    if bj_weekday(ts) == 0 || bj_weekday(ts) == 6 {
        return false; // 周末全天谷价(调休上班的周六日也一样 —— 判据是日历)
    }
    if is_holiday(ts) {
        return false;
    }
    let hour = bj_secs_of_day(ts) / 3600;
    PEAK_HOURS.iter().any(|(a, b)| hour >= *a && hour < *b)
}

/// 下一次峰↔谷切换的时刻。只需扫北京时间的小时边界 0/9/12/14/18 点;
/// 与 [`is_peak_at`] 同源,所以周末/节假日规则一变,这里自动跟着对。
pub fn next_change_at(ts: i64) -> Option<i64> {
    let cur = is_peak_at(ts);
    let day0 = bj_day_index(ts) * 86_400 - BJ_OFFSET; // 北京当日 00:00 对应的 epoch
    for d in 0..=SCAN_DAYS {
        for edge in [0i64, 9, 12, 14, 18] {
            let cand = day0 + d * 86_400 + edge * 3600;
            if cand <= ts {
                continue;
            }
            if is_peak_at(cand) != cur {
                return Some(cand);
            }
        }
    }
    None
}

/// 一次判定(界面只用这一个入口)。
pub fn info_at(ts: i64) -> PeakInfo {
    PeakInfo {
        peak: is_peak_at(ts),
        next_change_at: next_change_at(ts),
        holiday: is_holiday(ts),
        weekend: {
            let w = bj_weekday(ts);
            w == 0 || w == 6
        },
    }
}

/// 节假日表里最大的年份。表为空返回 0(不假装知道任何一年)。
pub fn holiday_table_max_year() -> i64 {
    HOLIDAY_VALLEY
        .iter()
        .filter_map(|s| s.get(..4).and_then(|y| y.parse::<i64>().ok()))
        .max()
        .unwrap_or(0)
}

/// 表要不要补:北京时间已经进入"表最大年份的 11 月"(国务院通常这时发次年安排)。
///
/// 判据故意**保守**:10 月不报(安排还没发),只从 11 月起报 —— 免得每年 10 月
/// 就挂一个"表过期"的提示,喊了一个月狼来了。
pub fn table_stale(ts: i64) -> bool {
    let max = holiday_table_max_year();
    if max <= 0 {
        return true;
    }
    let (y, m, _) = civil_from_days(bj_day_index(ts));
    y > max || (y == max && m >= 11)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 北京时间 y-m-d h:mi 对应的 epoch 秒(测试造时间用)。
    fn bj(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
        days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 - BJ_OFFSET
    }

    #[test]
    fn calendar_roundtrip_matches_known_dates() {
        // 2026-10-16 是周五(先在纸面上验过:1970-01-01 为周四,2026-01-01 同为周四,
        // 再过 288 天 → 周五)
        let t = bj(2026, 10, 16, 0, 0);
        assert_eq!(bj_date_string(t), "2026-10-16");
        assert_eq!(bj_weekday(t), 5, "周五");
        assert_eq!(bj_date_string(bj(2026, 1, 1, 23, 59)), "2026-01-01");
        assert_eq!(bj_weekday(bj(2026, 1, 1, 12, 0)), 4, "2026-01-01 也是周四");
        // 跨日边界:北京时间 00:00 整应当翻到新的一天
        assert_eq!(bj_date_string(bj(2026, 10, 17, 0, 0)), "2026-10-17");
        assert_eq!(bj_date_string(bj(2026, 10, 16, 0, 0) - 1), "2026-10-15");
    }

    #[test]
    fn weekdays_switch_at_the_documented_edges() {
        // 周五 2026-10-16
        assert!(!is_peak_at(bj(2026, 10, 16, 8, 59)));
        assert!(is_peak_at(bj(2026, 10, 16, 9, 0)), "9:00 左闭");
        assert!(is_peak_at(bj(2026, 10, 16, 11, 59)));
        assert!(!is_peak_at(bj(2026, 10, 16, 12, 0)), "12:00 右开");
        assert!(!is_peak_at(bj(2026, 10, 16, 13, 0)));
        assert!(is_peak_at(bj(2026, 10, 16, 14, 0)));
        assert!(is_peak_at(bj(2026, 10, 16, 17, 59)));
        assert!(!is_peak_at(bj(2026, 10, 16, 18, 0)), "18:00 右开");
        assert!(!is_peak_at(bj(2026, 10, 16, 23, 30)));
    }

    #[test]
    fn weekends_are_valley_all_day() {
        // 周六 2026-10-17 / 周日 2026-10-18 —— 高峰时段也该是谷价
        for (d, h) in [(17u32, 10i64), (17, 15), (18, 10), (18, 15)] {
            assert!(!is_peak_at(bj(2026, 10, d, h, 0)), "周末 {d} 日 {h} 点应是谷价");
        }
    }

    #[test]
    fn legal_holidays_are_valley_even_on_weekdays() {
        // 2026-10-01 是周四,但它在国庆清单里 → 全天谷价
        let t = bj(2026, 10, 1, 10, 0);
        assert_eq!(bj_weekday(t), 4, "确实是工作日");
        assert!(!is_peak_at(t));
        assert!(info_at(t).holiday);
        assert!(!info_at(bj(2026, 10, 16, 10, 0)).holiday, "普通工作日不是节假日");
    }

    #[test]
    fn next_change_hits_the_right_edge() {
        // 周五早上 8:00(谷)→ 9:00 转高峰
        assert_eq!(
            next_change_at(bj(2026, 10, 16, 8, 0)),
            Some(bj(2026, 10, 16, 9, 0))
        );
        // 高峰中 9:30 → 12:00 转谷
        assert_eq!(
            next_change_at(bj(2026, 10, 16, 9, 30)),
            Some(bj(2026, 10, 16, 12, 0))
        );
        // 午休 13:00(谷)→ 14:00 转高峰
        assert_eq!(
            next_change_at(bj(2026, 10, 16, 13, 0)),
            Some(bj(2026, 10, 16, 14, 0))
        );
        // 高峰中 15:00 → 18:00 转谷
        assert_eq!(
            next_change_at(bj(2026, 10, 16, 15, 0)),
            Some(bj(2026, 10, 16, 18, 0))
        );
    }

    #[test]
    fn next_change_skips_whole_valley_weekend() {
        // 周五 19:00(谷)→ 一路到周一 9:00 才有切换(整个周末都是谷)
        assert_eq!(
            next_change_at(bj(2026, 10, 16, 19, 0)),
            Some(bj(2026, 10, 19, 9, 0))
        );
        // 周日中午(谷)→ 还是周一 9:00
        assert_eq!(
            next_change_at(bj(2026, 10, 18, 12, 0)),
            Some(bj(2026, 10, 19, 9, 0))
        );
        // 国庆最后一天(10-07,周三)中午 → 8 日 9:00(先把 8 日当普通工作日)
        assert_eq!(
            next_change_at(bj(2026, 10, 7, 12, 0)),
            Some(bj(2026, 10, 8, 9, 0))
        );
    }

    #[test]
    fn info_bundles_reason_flags() {
        let weekend = info_at(bj(2026, 10, 17, 10, 0));
        assert!(!weekend.peak && weekend.weekend && !weekend.holiday);
        let peak = info_at(bj(2026, 10, 16, 10, 0));
        assert!(peak.peak && !peak.weekend && !peak.holiday);
        let holiday = info_at(bj(2026, 10, 1, 10, 0));
        assert!(!holiday.peak && holiday.holiday && !holiday.weekend);
    }

    /// 判据说的是**中国日历**,不是机器时区:同一时刻(UTC)在任何时区下结论一致。
    #[test]
    fn judgement_is_timezone_independent_by_construction() {
        // 2026-10-16 01:00 UTC = 北京时间 09:00 → 高峰开始那一刻
        let utc_1 = days_from_civil(2026, 10, 16) * 86_400 + 3600;
        assert_eq!(bj_date_string(utc_1), "2026-10-16");
        assert!(is_peak_at(utc_1), "按北京时间算 9:00 整就是高峰");
    }

    #[test]
    fn holiday_table_is_well_formed_and_sorted() {
        assert!(!HOLIDAY_VALLEY.is_empty());
        let mut prev = "";
        for s in HOLIDAY_VALLEY {
            assert_eq!(s.len(), 10, "日期要 YYYY-MM-DD: {s}");
            assert_eq!(s.as_bytes()[4], b'-', "第 5 位是短横: {s}");
            assert_eq!(s.as_bytes()[7], b'-', "第 8 位是短横: {s}");
            assert!(s[..4].parse::<i64>().is_ok(), "年份要能解析: {s}");
            assert!(s[5..7].parse::<u32>().is_ok(), "月份要能解析: {s}");
            assert!(s[8..].parse::<u32>().is_ok(), "日要能解析: {s}");
            // 表必须有序(二分查找的前提)
            assert!(*s > prev, "表必须升序排列: {prev} → {s}");
            prev = s;
        }
        assert_eq!(holiday_table_max_year(), 2026);
    }

    /// 表过期要如实报,但**从 11 月起才报** —— 10 月国务院还没发次年安排,
    /// 那时报警等于每年喊一个月"狼来了"。
    #[test]
    fn table_staleness_is_conservative() {
        assert!(!table_stale(bj(2026, 10, 16, 12, 0)), "当年 10 月不算过期");
        assert!(table_stale(bj(2026, 11, 1, 0, 0)), "进入 11 月该提醒补次年表了");
        assert!(table_stale(bj(2027, 3, 1, 0, 0)), "次年还没补,一直算过期");
    }

    /// 表里没有的未来年份:节假日认不出 → 退化成"周末规则",不 panic。
    #[test]
    fn unknown_year_falls_back_to_weekend_rule_without_panic() {
        let t = bj(2030, 10, 1, 10, 0); // 2030-10-01 是周二,但表里没有
        assert!(!info_at(t).holiday, "表里没有就不假装是节假日");
        assert!(is_peak_at(t), "按常规工作日规则判:是高峰");
        assert!(table_stale(t));
    }
}
