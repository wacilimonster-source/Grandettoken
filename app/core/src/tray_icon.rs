//! Tray selection and RGBA badges are available while the webview is suspended.
use crate::{
    antigravity_accounts,
    config::Config,
    fetch::{local_date_start, ChannelView},
    providers::Kind,
    view::{FormRow, HomeView, Tone, ViewState},
};
use chrono::{Datelike, Days, Local, TimeZone};

fn glyph(ch: char) -> (u32, [u8; 5]) {
    (
        3,
        match ch {
            '0' => [7, 5, 5, 5, 7],
            '1' => [2, 6, 2, 2, 7],
            '2' => [7, 1, 7, 4, 7],
            '3' => [7, 1, 3, 1, 7],
            '4' => [5, 5, 7, 1, 1],
            '5' => [7, 4, 7, 1, 7],
            '6' => [7, 4, 7, 5, 7],
            '7' => [7, 1, 2, 2, 2],
            '8' => [7, 5, 7, 5, 7],
            '9' => [7, 5, 7, 1, 7],
            'k' => [4, 5, 6, 5, 5],
            'M' => return (5, [17, 27, 21, 17, 17]),
            'w' => return (5, [0, 17, 21, 21, 10]),
            '%' => return (5, [25, 26, 4, 11, 19]),
            '-' => [0, 0, 7, 0, 0],
            '.' => return (1, [0, 0, 0, 0, 1]),
            _ => [7, 1, 3, 0, 2],
        },
    )
}

pub fn render_badge_rgba(size: u32, text: &str, tone: Tone) -> Vec<u8> {
    if !(16..=64).contains(&size) || text.is_empty() {
        return Vec::new();
    }
    let glyphs: Vec<_> = text.chars().take(12).map(glyph).collect();
    let glyph_width = glyphs.iter().map(|(width, _)| width + 1).sum::<u32>() - 1;
    let side = f64::from(size);
    let scale = ((side - 2.0) / f64::from(glyph_width)).min(side * 0.8 / 5.0);
    let left = (side - scale * f64::from(glyph_width)) / 2.0;
    let top = (side - scale * 5.0) / 2.0;
    let radius = side * 0.22;
    let background = match tone {
        Tone::Ok => [139, 201, 162],
        Tone::Warn => [237, 193, 133],
        Tone::Bad => [239, 157, 148],
        Tone::Off => [58, 67, 86],
    };
    let foreground = if tone == Tone::Off {
        [255, 255, 255]
    } else {
        [20, 25, 33]
    };
    let has_ink = |x: f64, y: f64| {
        if x < left || y < top || y >= top + scale * 5.0 {
            return false;
        }
        let mut col = ((x - left) / scale) as u32;
        let row = ((y - top) / scale) as usize;
        for (width, bits) in &glyphs {
            if col < *width {
                return bits[row] & (1 << (*width - 1 - col)) != 0;
            }
            if col == *width {
                break;
            }
            col -= width + 1;
        }
        false
    };
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut covered = 0u32;
            let mut lit = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = f64::from(x) + (f64::from(sx) + 0.5) / 4.0;
                    let fy = f64::from(y) + (f64::from(sy) + 0.5) / 4.0;
                    let dx = fx - fx.clamp(radius, side - radius);
                    let dy = fy - fy.clamp(radius, side - radius);
                    if dx * dx + dy * dy <= radius * radius {
                        covered += 1;
                        lit += u32::from(has_ink(fx, fy));
                    }
                }
            }
            if covered == 0 {
                rgba.extend([0, 0, 0, 0]);
            } else {
                for (bg, fg) in background.into_iter().zip(foreground) {
                    rgba.push(((bg * (covered - lit) + fg * lit) / covered) as u8);
                }
                rgba.push((covered * 255 / 16) as u8);
            }
        }
    }
    rgba
}

fn primary_percent(channel: &ChannelView) -> Option<f64> {
    let wins = &channel.windows;
    let preferred = if channel.id == "codex" {
        wins.iter().find(|w| w.label == "本周")
    } else {
        None
    };
    preferred
        .or_else(|| wins.iter().find(|w| w.main))
        .or_else(|| wins.iter().find(|w| w.label == "周期"))
        .or_else(|| wins.first())
        .map(|w| w.remain_percent)
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(0.0, 100.0))
}

fn abbreviation(value: f64) -> String {
    let decimal = |v: f64| {
        let absolute = v.abs();
        let scaled = absolute * 10.0;
        let rounded = if scaled.fract() == 0.5 && absolute.mul_add(10.0, -scaled) < 0.0 {
            scaled.floor()
        } else {
            scaled.round()
        };
        let tenths = (rounded / 10.0).copysign(v);
        let s = format!("{tenths:.1}");
        if v == 0.0 {
            return "0".into();
        }
        if v.abs() < 0.05 {
            return s;
        }
        s.strip_suffix(".0").unwrap_or(&s).into()
    };
    let abs = value.abs();
    if abs < 10.0 {
        decimal(value)
    } else if abs < 1_000.0 {
        format!("{:.0}", (value + 0.5).floor())
    } else if abs < 10_000.0 {
        format!("{}k", decimal(value / 1_000.0))
    } else if abs < 100_000.0 {
        format!("{}w", decimal(value / 10_000.0))
    } else {
        format!("{:.0}w", (value / 10_000.0 + 0.5).floor())
    }
}

fn number(channel: &ChannelView) -> Option<String> {
    if !channel.has_key || (!channel.valid && !channel.stale) {
        return None;
    }
    if channel.kind == Kind::Percent {
        return primary_percent(channel).map(|v| format!("{:.0}", (v + 0.5).floor()));
    }
    channel
        .remaining
        .filter(|v| v.is_finite())
        .map(abbreviation)
}

pub fn rotation<'a>(channels: &'a [ChannelView], cfg: &Config, now: i64) -> Vec<&'a ChannelView> {
    let visible = |c: &&ChannelView| !c.hidden && !cfg.hidden_channels.contains(&c.id);
    let antigravity_account =
        |c: &ChannelView| c.id != "antigravity" && antigravity_accounts::is_channel(&c.id);
    let mut picks = Vec::new();
    for id in &cfg.pill_channels {
        let accounts = id == "antigravity" && channels.iter().any(antigravity_account);
        for channel in channels.iter().filter(visible).filter(|c| {
            if accounts {
                antigravity_account(c)
            } else {
                c.id == *id
            }
        }) {
            if !picks
                .iter()
                .any(|other: &&ChannelView| other.id == channel.id)
            {
                picks.push(channel);
            }
        }
    }
    if !picks.is_empty() {
        return picks;
    }
    let expiry_end = Local
        .timestamp_opt(now, 0)
        .single()
        .and_then(|date| date.date_naive().checked_add_days(Days::new(7)))
        .map_or(now, |date| {
            local_date_start(date.year(), date.month(), date.day())
        });
    let tightness = |c: &ChannelView| {
        let amount = c.remaining.unwrap_or_default();
        match c.kind {
            Kind::Percent => primary_percent(c).map_or(2.0, |v| v / 100.0),
            Kind::Amount if c.remaining.is_some() => {
                let floor = if cfg.money_floor > 0.0 {
                    cfg.money_floor
                } else {
                    10.0
                };
                let denom = c
                    .claim
                    .as_ref()
                    .map(|x| x.amount)
                    .filter(|x| *x > 0.0)
                    .unwrap_or(floor);
                (amount / denom).clamp(0.0, 1.0)
            }
            Kind::Points if !c.stale && !c.expiry_stale => {
                let expiring: f64 = c
                    .expiring
                    .iter()
                    .filter(|x| {
                        x.at > now && x.at < expiry_end && x.amount.is_finite() && x.amount > 0.0
                    })
                    .map(|x| x.amount)
                    .sum();
                1.0 - (expiring / amount.max(1.0)).min(1.0)
            }
            Kind::Points => 1.0,
            _ => 2.0,
        }
    };
    channels
        .iter()
        .filter(visible)
        .filter(|c| {
            c.has_key
                && (c.valid
                    || c.stale
                    || c.recovery.as_ref().is_some_and(|r| r.phase == "retrying"))
        })
        .min_by(|a, b| tightness(a).total_cmp(&tightness(b)))
        .into_iter()
        .collect()
}

pub struct TrayFace {
    pub badge: Option<String>,
    pub tone: Tone,
    pub tooltip: String,
}

fn grouped(value: f64, fixed: bool) -> String {
    let raw = format!("{value:.2}");
    let (whole, frac) = raw.split_once('.').unwrap_or((&raw, ""));
    let mut out = String::new();
    let digits = whole.trim_start_matches('-');
    if whole.starts_with('-') {
        out.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    let decimals = if fixed {
        frac
    } else {
        frac.trim_end_matches('0')
    };
    if !decimals.is_empty() {
        out.push('.');
        out.push_str(decimals);
    }
    out
}

fn headline(row: &FormRow, cfg: &Config) -> String {
    let h = &row.headline;
    if let Some(value) = h.percent {
        let shown = if cfg.show_used { 100.0 - value } else { value };
        format!("{shown:.1}%")
    } else if let Some(value) = h.amount {
        let unit = h.unit.as_deref().unwrap_or("CNY");
        let prefix = if unit.is_empty() || unit == "CNY" {
            "¥".into()
        } else {
            format!("{unit} ")
        };
        prefix + &grouped(value, true)
    } else if let Some(value) = h.points {
        format!("{} 分", grouped(value, false))
    } else {
        "——".into()
    }
}

pub fn face(
    channel: Option<&ChannelView>,
    view: &HomeView,
    cfg: &Config,
    has_visible_key: bool,
) -> TrayFace {
    let mut face = TrayFace {
        badge: None,
        tone: Tone::Off,
        tooltip: if has_visible_key {
            "渠道全部取数失败".into()
        } else {
            "尚未配置密钥".into()
        },
    };
    if let Some(c) = channel {
        face.badge = cfg.tray_show_number.then(|| number(c)).flatten();
        face.tooltip = c.name.clone();
        if let Some(row) = view.rows.iter().find(|row| row.id == c.id) {
            face.tone = if c.valid { row.tone } else { Tone::Off };
            let qualifier = row.qualifier.as_deref().filter(|x| *x != "~").unwrap_or("");
            let stale = if c.stale || row.state == ViewState::Stale {
                "~"
            } else {
                ""
            };
            face.tooltip
                .push_str(&format!(" {stale}{}{qualifier}", headline(row, cfg)));
        } else {
            face.tooltip.push_str(" ——");
        }
    }
    face
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_sizes_colors_corners_and_glyphs() {
        for size in [16, 20, 24, 32, 48, 64] {
            let pixels = render_badge_rgba(size, "12.5", Tone::Ok);
            assert_eq!(pixels.len(), (size * size * 4) as usize);
            assert_eq!(pixels[3], 0);
            assert!(pixels.chunks_exact(4).any(|p| p[3] == 255 && p[0] < 90));
            assert_ne!(pixels, render_badge_rgba(size, "12.6", Tone::Ok));
            assert_ne!(pixels, render_badge_rgba(size, "12.5", Tone::Bad));
        }
        for size in [0, 15, 65, u32::MAX] {
            assert!(render_badge_rgba(size, "1", Tone::Off).is_empty());
        }
    }

    #[test]
    fn rotation_falls_back_from_hidden_and_badge_preserves_a_stale_value() {
        let cfg = Config {
            pill_channels: vec!["hidden".into()],
            ..Default::default()
        };
        let channels = [
            ChannelView {
                id: "hidden".into(),
                hidden: true,
                has_key: true,
                valid: true,
                ..Default::default()
            },
            ChannelView {
                id: "live".into(),
                has_key: true,
                stale: true,
                remaining: Some(321.0),
                ..Default::default()
            },
        ];
        assert_eq!(rotation(&channels, &cfg, 0)[0].id, "live");
        assert_eq!(number(&channels[1]).as_deref(), Some("321"));
        assert_eq!(
            number(&ChannelView {
                stale: false,
                ..channels[1].clone()
            }),
            None
        );
        assert_eq!(abbreviation(0.01), "0.0");
        assert_eq!(abbreviation(12_500.0), "1.3w");
    }
}
