//! 同步载荷（SyncSnapshot）的数据模型、十进制口径与白名单校验。
//!
//! 这里刻意不复用 `ChannelView`：它含 `authLabel`、`error`、`extra`、`accounts`
//! 等字段，直接序列化就是泄露。同步载荷是从零定义的 DTO，新增字段必须显式
//! 加进本文件并通过 [`SyncSnapshot::validate`]。

use serde::{Deserialize, Serialize};

use super::error::{SyncError, PAYLOAD_INVALID, TOO_LARGE};

pub const SNAPSHOT_VERSION: u8 = 1;
pub const MAX_CHANNELS: usize = 64;
pub const MAX_HISTORY: usize = 7;
pub const MAX_WINDOWS: usize = 16;
pub const MAX_EXPIRING: usize = 64;
pub const MAX_CHECKIN_ITEMS: usize = 64;
pub const MAX_OTHER_CURRENCIES: usize = 16;
pub const MAX_NAME_CHARS: usize = 40;
pub const MAX_UNIT_CHARS: usize = 16;
pub const MAX_SOURCE_FILE_BYTES: usize = 256 * 1024;
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    Money,
    Quota,
    Points,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckinStatus {
    /// 今天已经完成（领过 / 平台侧已领 / 没活动也算当天完结）。
    Done,
    /// 还没做，今天仍可尝试。
    Pending,
    /// 该平台不支持签到，或活动已下线。
    Unsupported,
    /// 状态未知（凭据失效、接口异常），不能计入完成或待办。
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSnapshot {
    pub v: u8,
    pub source: String,
    pub seq: u64,
    /// 发布时刻。心跳会变，余额时效不看它。
    pub at: i64,
    pub app: String,
    #[serde(default)]
    pub channels: Vec<ChannelSnapshot>,
    #[serde(default)]
    pub checkin: Option<CheckinSnapshot>,
    #[serde(default)]
    pub totals: TotalsSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSnapshot {
    /// 跨来源合并键。内置平台用协议枚举；自定义平台用统一的同步平台标签。
    pub platform_id: String,
    /// 本机渠道 id，仅用于来源内部排查，不参与跨来源合并。
    pub id: String,
    pub name: String,
    pub kind: ChannelKind,
    pub unit: String,
    #[serde(default)]
    pub remaining: Option<String>,
    #[serde(default)]
    pub total: Option<String>,
    #[serde(default)]
    pub percent: Option<String>,
    #[serde(default)]
    pub windows: Vec<WindowSnapshot>,
    #[serde(default)]
    pub expiring: Vec<ExpiringSnapshot>,
    #[serde(default)]
    pub day: Option<String>,
    #[serde(default)]
    pub week: Option<String>,
    #[serde(default)]
    pub month: Option<String>,
    #[serde(default)]
    pub estimated: bool,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub error_code: Option<String>,
    /// 该渠道最后一次成功取数的时刻。心跳不更新它。
    pub updated_at: i64,
    #[serde(default)]
    pub history: Vec<HistoryPoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowSnapshot {
    pub label: String,
    #[serde(default)]
    pub remain_percent: Option<String>,
    /// 能解析成 unix 秒就填；填不了用 `reset_text`。
    #[serde(default)]
    pub reset_at: Option<i64>,
    #[serde(default)]
    pub reset_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpiringSnapshot {
    pub amount: String,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPoint {
    pub period_start: i64,
    pub period_end: i64,
    pub time_zone: String,
    pub unit: String,
    #[serde(default)]
    pub balance: Option<String>,
    #[serde(default)]
    pub spent: Option<String>,
    #[serde(default)]
    pub estimated: bool,
    pub revised_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinSnapshot {
    /// 业务日，UTC+8（v1 固定口径）。
    pub date: String,
    pub time_zone: String,
    #[serde(default)]
    pub items: Vec<CheckinItem>,
    pub done: u32,
    /// 分母 = done + pending；unsupported / unknown 不计入。
    pub total: u32,
    #[serde(default)]
    pub unsupported: u32,
    #[serde(default)]
    pub unknown: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinItem {
    /// 可跨来源去重的条目 ID。v1：`<platformId>:main`（当前每渠道一份凭据）。
    pub item_id: String,
    pub platform_id: String,
    pub status: CheckinStatus,
    pub observed_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TotalsSnapshot {
    #[serde(default)]
    pub cny: Option<String>,
    #[serde(default)]
    pub others: Vec<OtherMoney>,
    #[serde(default)]
    pub today: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtherMoney {
    pub unit: String,
    pub amount: String,
}

impl SyncSnapshot {
    pub fn to_bytes(&self) -> Result<Vec<u8>, SyncError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(SyncError::new(
                TOO_LARGE,
                format!("同步载荷 {} 字节，超过上限 {} 字节", bytes.len(), MAX_PAYLOAD_BYTES),
            ));
        }
        Ok(bytes)
    }

    pub fn validate(&self) -> Result<(), SyncError> {
        if self.v != SNAPSHOT_VERSION {
            return Err(SyncError::new(
                super::error::UNSUPPORTED_VERSION,
                format!("不支持的载荷版本: {}", self.v),
            ));
        }
        if !super::crypto::valid_source(&self.source) {
            return Err(SyncError::new(
                super::error::SOURCE_INVALID,
                "source 不是 s_ + 32 位十六进制",
            ));
        }
        if self.seq == 0 {
            return Err(invalid("seq 必须大于 0"));
        }
        if self.at <= 0 {
            return Err(invalid("at 必须是正 unix 秒"));
        }
        if self.app.chars().count() > 40 {
            return Err(invalid("app 过长"));
        }
        if self.channels.len() > MAX_CHANNELS {
            return Err(invalid("渠道数量超过上限"));
        }
        for ch in &self.channels {
            ch.validate()?;
        }
        if let Some(ck) = &self.checkin {
            ck.validate()?;
        }
        self.totals.validate()?;
        Ok(())
    }
}

impl ChannelSnapshot {
    fn validate(&self) -> Result<(), SyncError> {
        if !valid_platform_id(&self.platform_id) {
            return Err(invalid("platformId 不合法"));
        }
        if self.id.is_empty() || self.id.chars().count() > 64 {
            return Err(invalid("渠道 id 不合法"));
        }
        if self.name.trim().is_empty() || self.name.chars().count() > MAX_NAME_CHARS {
            return Err(invalid("渠道名不合法"));
        }
        if !valid_unit(&self.unit) {
            return Err(invalid("unit 不合法"));
        }
        for (label, v) in [
            ("remaining", &self.remaining),
            ("total", &self.total),
            ("percent", &self.percent),
            ("day", &self.day),
            ("week", &self.week),
            ("month", &self.month),
        ] {
            if let Some(v) = v {
                if !valid_decimal(v) {
                    return Err(invalid(format!("{label} 不是合法十进制字符串")));
                }
            }
        }
        if let Some(code) = &self.error_code {
            if code.chars().count() > 64 || !code.chars().all(|c| !c.is_control()) {
                return Err(invalid("errorCode 不合法"));
            }
        }
        if self.updated_at <= 0 {
            return Err(invalid("updatedAt 必须是正 unix 秒"));
        }
        if self.windows.len() > MAX_WINDOWS {
            return Err(invalid("窗口数量超过上限"));
        }
        for w in &self.windows {
            if w.label.chars().count() > 40 {
                return Err(invalid("窗口标签过长"));
            }
            if let Some(v) = &w.remain_percent {
                if !valid_decimal(v) {
                    return Err(invalid("窗口百分比不合法"));
                }
            }
            if let Some(t) = &w.reset_text {
                if t.chars().count() > 40 || t.chars().any(|c| c.is_control()) {
                    return Err(invalid("窗口重置文本不合法"));
                }
            }
        }
        if self.expiring.len() > MAX_EXPIRING {
            return Err(invalid("到期批次数量超过上限"));
        }
        for e in &self.expiring {
            if !valid_decimal(&e.amount) || e.at <= 0 {
                return Err(invalid("到期批次不合法"));
            }
        }
        if self.history.len() > MAX_HISTORY {
            return Err(invalid("历史记录超过 7 天"));
        }
        for h in &self.history {
            if h.period_end <= h.period_start {
                return Err(invalid("历史窗口区间不合法"));
            }
            if !valid_time_zone(&h.time_zone) || !valid_unit(&h.unit) {
                return Err(invalid("历史时区/单位不合法"));
            }
            for v in [&h.balance, &h.spent] {
                if let Some(v) = v {
                    if !valid_decimal(v) {
                        return Err(invalid("历史数值不是合法十进制字符串"));
                    }
                }
            }
            if h.revised_at < 0 {
                return Err(invalid("历史修订时刻不合法"));
            }
        }
        Ok(())
    }
}

impl CheckinSnapshot {
    fn validate(&self) -> Result<(), SyncError> {
        if !valid_date(&self.date) {
            return Err(invalid("签到业务日不合法"));
        }
        if !valid_time_zone(&self.time_zone) {
            return Err(invalid("签到 timeZone 不合法"));
        }
        if self.items.len() > MAX_CHECKIN_ITEMS {
            return Err(invalid("签到条目超过上限"));
        }
        if self.done > self.total || self.total as usize > self.items.len() {
            return Err(invalid("签到汇总与明细不一致"));
        }
        for item in &self.items {
            if !valid_item_id(&item.item_id) || !valid_platform_id(&item.platform_id) {
                return Err(invalid("签到条目 ID 不合法"));
            }
            if item.observed_at <= 0 {
                return Err(invalid("签到观测时刻不合法"));
            }
        }
        Ok(())
    }
}

impl TotalsSnapshot {
    fn validate(&self) -> Result<(), SyncError> {
        if let Some(v) = &self.cny {
            if !valid_decimal(v) {
                return Err(invalid("totals.cny 不合法"));
            }
        }
        if let Some(v) = &self.today {
            if !valid_decimal(v) {
                return Err(invalid("totals.today 不合法"));
            }
        }
        if self.others.len() > MAX_OTHER_CURRENCIES {
            return Err(invalid("币种数量超过上限"));
        }
        for o in &self.others {
            if !valid_unit(&o.unit) || !valid_decimal(&o.amount) {
                return Err(invalid("totals.others 不合法"));
            }
        }
        Ok(())
    }
}

fn invalid(msg: impl Into<String>) -> SyncError {
    SyncError::new(PAYLOAD_INVALID, msg)
}

pub fn valid_platform_id(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'))
}

pub fn valid_item_id(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > 96 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'))
}

pub fn valid_unit(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.chars().count() > MAX_UNIT_CHARS {
        return false;
    }
    s.chars()
        .all(|c| !c.is_control() && !c.is_whitespace() && c != '|')
}

pub fn valid_time_zone(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.chars().count() > 40 {
        return false;
    }
    s.chars().all(|c| {
        !c.is_control()
            && !c.is_whitespace()
            && (c.is_ascii_alphanumeric() || matches!(c, '/' | '+' | '-' | ':' | '_' | '.'))
    })
}

pub fn valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(|x| x.is_ascii_digit())
        && b[5..7].iter().all(|x| x.is_ascii_digit())
        && b[8..].iter().all(|x| x.is_ascii_digit())
}

/// 十进制字符串：可带负号、最多 6 位小数、最多 24 字符；拒绝 NaN/Inf/指数。
pub fn valid_decimal(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.len() > 24 {
        return false;
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    if bytes[0] == b'-' {
        i = 1;
        if bytes.len() == 1 {
            return false;
        }
    }
    let mut digits = 0usize;
    let mut dot = false;
    let mut frac = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                if dot {
                    return false;
                }
                dot = true;
            }
            b'0'..=b'9' => {
                digits += 1;
                if dot {
                    frac += 1;
                }
            }
            _ => return false,
        }
        i += 1;
    }
    digits > 0 && frac <= 6 && !(dot && bytes.last() == Some(&b'.'))
}

/// f64 → 规范十进制字符串；非有限值返回 None（绝不把 NaN/Inf 发出去）。
pub fn decimal_string(v: f64) -> Option<String> {
    if !v.is_finite() || v.abs() > 1_000_000_000_000.0 {
        return None;
    }
    let mut s = format!("{:.6}", v);
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s.is_empty() || s == "-0" {
        s = "0".into();
    }
    Some(s)
}

/// 解析为 1e-6 定点整数，用于精确合计。
pub fn parse_decimal_micros(s: &str) -> Option<i128> {
    if !valid_decimal(s) {
        return None;
    }
    let s = s.trim();
    let neg = s.starts_with('-');
    let body = if neg { &s[1..] } else { s };
    let (int_part, frac_part) = match body.split_once('.') {
        Some((a, b)) => (a, b),
        None => (body, ""),
    };
    let int_val: i128 = int_part.parse().ok()?;
    let mut frac = frac_part.to_string();
    while frac.len() < 6 {
        frac.push('0');
    }
    let frac_val: i128 = frac.parse().ok()?;
    let total = int_val.checked_mul(1_000_000)?.checked_add(frac_val)?;
    Some(if neg { -total } else { total })
}

pub fn format_micros(v: i128) -> String {
    let neg = v < 0;
    let abs = if neg { -v } else { v };
    let int_part = abs / 1_000_000;
    let frac = abs % 1_000_000;
    let mut s = if frac == 0 {
        format!("{int_part}")
    } else {
        let mut f = format!("{frac:06}");
        while f.ends_with('0') {
            f.pop();
        }
        format!("{int_part}.{f}")
    };
    if s == "0" && neg {
        s = "0".into();
    } else if neg {
        s.insert(0, '-');
    }
    s
}

pub fn sum_decimal_strings<'a, I>(values: I) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut total: i128 = 0;
    let mut any = false;
    for s in values {
        if let Some(v) = parse_decimal_micros(s) {
            total = total.checked_add(v)?;
            any = true;
        }
    }
    if any {
        Some(format_micros(total))
    } else {
        None
    }
}

/// 显示名净化：去掉控制字符、折叠空白、截断 40 字；像 URL / 邮箱 / 密钥的
/// 自由文本直接退回内置名。白名单结构不能自动保证“零泄露”，这是最后一道闸。
pub fn sanitize_name(input: &str, fallback: &str) -> String {
    let cleaned: String = input
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = cleaned.trim();
    let looks_sensitive = trimmed.contains("://")
        || trimmed.contains('@')
        || trimmed.starts_with("sk-")
        || trimmed.starts_with("sk_")
        || trimmed.starts_with("ghp_")
        || trimmed.starts_with("gho_")
        || trimmed.starts_with("github_pat_")
        || trimmed.len() > 40;
    let chosen = if trimmed.is_empty() || looks_sensitive {
        fallback
    } else {
        trimmed
    };
    chosen.chars().take(MAX_NAME_CHARS).collect()
}

pub fn safe_unit(input: &str, fallback: &str) -> String {
    if valid_unit(input) {
        input.trim().chars().take(MAX_UNIT_CHARS).collect()
    } else {
        fallback.chars().take(MAX_UNIT_CHARS).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_roundtrip() {
        assert_eq!(decimal_string(88.0).as_deref(), Some("88"));
        assert_eq!(decimal_string(0.1).as_deref(), Some("0.1"));
        assert_eq!(decimal_string(-0.0).as_deref(), Some("0"));
        assert_eq!(decimal_string(f64::NAN), None);
        assert_eq!(decimal_string(f64::INFINITY), None);
    }

    #[test]
    fn micros_are_exact() {
        assert_eq!(parse_decimal_micros("0.1"), Some(100_000));
        assert_eq!(parse_decimal_micros("-1.25"), Some(-1_250_000));
        assert_eq!(format_micros(100_000), "0.1");
        assert_eq!(format_micros(1_284_500_000), "1284.5");
        assert_eq!(sum_decimal_strings(["0.1", "0.2"]).as_deref(), Some("0.3"));
        assert_eq!(sum_decimal_strings(["1.00", "-0.50"]).as_deref(), Some("0.5"));
        assert_eq!(sum_decimal_strings(std::iter::empty()), None);
    }

    #[test]
    fn decimal_rejects_bad_values() {
        for bad in ["", "NaN", "Infinity", "1e3", "1.2345678", ".", "-", "1..2"] {
            assert!(!valid_decimal(bad), "{bad} 不应通过");
        }
        assert!(valid_decimal("0"));
        assert!(valid_decimal("-12.345678"));
    }

    #[test]
    fn name_sanitization() {
        assert_eq!(sanitize_name("  DeepSeek \n ", "默认"), "DeepSeek");
        assert_eq!(sanitize_name("https://evil.example", "默认"), "默认");
        assert_eq!(sanitize_name("a@b.com", "默认"), "默认");
        assert_eq!(sanitize_name("sk-123456", "默认"), "默认");
        assert_eq!(sanitize_name("", "默认"), "默认");
    }

    #[test]
    fn platform_and_item_ids() {
        assert!(valid_platform_id("deepseek"));
        assert!(valid_platform_id("custom:my-relay"));
        assert!(!valid_platform_id("https://x"));
        assert!(!valid_platform_id(""));
        assert!(valid_item_id("deepseek:main"));
    }

    #[test]
    fn snapshot_validation() {
        let snap = SyncSnapshot {
            v: 1,
            source: "s_0123456789abcdef0123456789abcdef".into(),
            seq: 1,
            at: 1_793_500_000,
            app: "0.3.2".into(),
            channels: vec![ChannelSnapshot {
                platform_id: "deepseek".into(),
                id: "deepseek".into(),
                name: "DeepSeek".into(),
                kind: ChannelKind::Money,
                unit: "CNY".into(),
                remaining: Some("88".into()),
                total: None,
                percent: None,
                windows: vec![],
                expiring: vec![],
                day: Some("1.2".into()),
                week: None,
                month: None,
                estimated: true,
                stale: false,
                error_code: None,
                updated_at: 1_793_500_000,
                history: vec![],
            }],
            checkin: None,
            totals: TotalsSnapshot {
                cny: Some("88".into()),
                others: vec![],
                today: Some("1.2".into()),
            },
        };
        assert!(snap.validate().is_ok());
        let mut bad = snap.clone();
        bad.channels[0].platform_id = "https://evil".into();
        assert_eq!(bad.validate().unwrap_err().code, PAYLOAD_INVALID);
    }

    #[test]
    fn checkin_consistency() {
        let ck = CheckinSnapshot {
            date: "2026-11-02".into(),
            time_zone: "Asia/Shanghai".into(),
            items: vec![
                CheckinItem {
                    item_id: "deepseek:main".into(),
                    platform_id: "deepseek".into(),
                    status: CheckinStatus::Done,
                    observed_at: 1_793_500_000,
                },
                CheckinItem {
                    item_id: "trae:main".into(),
                    platform_id: "trae".into(),
                    status: CheckinStatus::Pending,
                    observed_at: 1_793_500_000,
                },
            ],
            done: 1,
            total: 2,
            unsupported: 0,
            unknown: 0,
        };
        assert!(ck.validate().is_ok());
        let mut bad = ck.clone();
        bad.done = 3;
        assert!(bad.validate().is_err());
    }
}
