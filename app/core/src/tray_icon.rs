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
            '1' => return (2, [2, 3, 2, 2, 7]),
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

fn badge_padding(size: u32) -> (u32, u32) {
    let pad_x = (f64::from(size) * 0.06).round().max(1.0) as u32;
    let pad_y = (f64::from(size) * 0.10).round().max(1.0) as u32;
    (pad_x, pad_y)
}

fn render_bitmap_mask(size: u32, text: &str) -> Vec<u8> {
    let mut mask = vec![0u8; (size * size) as usize];
    let glyphs: Vec<_> = text.chars().map(glyph).collect();
    if glyphs.is_empty() {
        return mask;
    }
    let glyph_width = glyphs.iter().map(|(width, _)| width + 1).sum::<u32>() - 1;
    let (pad_x, pad_y) = badge_padding(size);
    let avail_w = f64::from(size.saturating_sub(pad_x * 2).max(1));
    let avail_h = f64::from(size.saturating_sub(pad_y * 2).max(1));
    let scale = (avail_w / f64::from(glyph_width)).min(avail_h / 5.0);
    let side = f64::from(size);
    let left = (side - scale * f64::from(glyph_width)) / 2.0;
    let top = (side - scale * 5.0) / 2.0;
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
    for y in 0..size {
        for x in 0..size {
            let mut lit = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = f64::from(x) + (f64::from(sx) + 0.5) / 4.0;
                    let fy = f64::from(y) + (f64::from(sy) + 0.5) / 4.0;
                    lit += u32::from(has_ink(fx, fy));
                }
            }
            mask[(y * size + x) as usize] = ((lit * 255 + 8) / 16) as u8;
        }
    }
    mask
}

#[cfg(windows)]
mod gdi {
    use super::badge_padding;
    use std::ffi::c_void;
    use windows_sys::Win32::{
        Foundation::SIZE,
        Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, GdiFlush,
            GetTextExtentPoint32W, GetTextFaceW, SelectObject, SetBkMode, SetTextColor, TextOutW,
            ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS,
            DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, FF_DONTCARE, FW_BOLD, HGDIOBJ, HDC,
            OUT_TT_PRECIS, TRANSPARENT,
        },
    };

    struct DcGuard(HDC);
    impl Drop for DcGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    DeleteDC(self.0);
                }
            }
        }
    }

    struct ObjGuard(HGDIOBJ);
    impl Drop for ObjGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    DeleteObject(self.0);
                }
            }
        }
    }

    struct SelectGuard {
        dc: HDC,
        old: HGDIOBJ,
    }
    impl SelectGuard {
        fn new(dc: HDC, obj: HGDIOBJ) -> Option<Self> {
            let old = unsafe { SelectObject(dc, obj) };
            if old.is_null() {
                None
            } else {
                Some(Self { dc, old })
            }
        }
    }
    impl Drop for SelectGuard {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.dc, self.old);
            }
        }
    }

    struct FitResult {
        mask: Vec<u8>,
        ink_h: u32,
        scaled: bool,
    }

    fn create_dib(dc: HDC, width: i32, height: i32) -> Option<(ObjGuard, *mut u8)> {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bmp = unsafe {
            CreateDIBSection(
                dc,
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            )
        };
        if bmp.is_null() || bits.is_null() {
            if !bmp.is_null() {
                unsafe {
                    DeleteObject(bmp);
                }
            }
            return None;
        }
        Some((ObjGuard(bmp), bits.cast::<u8>()))
    }

    fn create_font(height: i32, wide_face: &[u16]) -> Option<ObjGuard> {
        let font = unsafe {
            CreateFontW(
                -height,
                0,
                0,
                0,
                FW_BOLD as i32,
                0,
                0,
                0,
                u32::from(DEFAULT_CHARSET),
                u32::from(OUT_TT_PRECIS),
                u32::from(CLIP_DEFAULT_PRECIS),
                u32::from(ANTIALIASED_QUALITY),
                u32::from(DEFAULT_PITCH | FF_DONTCARE),
                wide_face.as_ptr(),
            )
        };
        if font.is_null() {
            None
        } else {
            Some(ObjGuard(font))
        }
    }

    fn verify_font(dc: HDC, expected: &str) -> bool {
        let mut buf = [0u16; 128];
        let len = unsafe { GetTextFaceW(dc, buf.len() as i32, buf.as_mut_ptr()) };
        if len <= 0 {
            return false;
        }
        let actual = String::from_utf16_lossy(&buf[..len as usize]);
        actual.trim_end_matches('\0').eq_ignore_ascii_case(expected)
    }

    fn text_extent(dc: HDC, wide_text: &[u16]) -> Option<SIZE> {
        let mut ext = SIZE { cx: 0, cy: 0 };
        let ok = unsafe {
            GetTextExtentPoint32W(dc, wide_text.as_ptr(), wide_text.len() as i32, &mut ext)
        };
        (ok != 0).then_some(ext)
    }

    fn ink_bounds(buf: &[u8], buf_w: i32, buf_h: i32) -> Option<(i32, i32, i32, i32)> {
        let (mut min_x, mut max_x) = (buf_w, -1);
        let (mut min_y, mut max_y) = (buf_h, -1);
        for y in 0..buf_h {
            let row = (y * buf_w * 4) as usize;
            for x in 0..buf_w {
                if buf[row + (x as usize) * 4 + 2] > 0 {
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        (max_x >= min_x && max_y >= min_y).then_some((min_x, max_x, min_y, max_y))
    }

    fn draw_and_measure(
        dc: HDC,
        bits: *mut u8,
        buf_w: i32,
        buf_h: i32,
        margin: i32,
        wide_text: &[u16],
    ) -> Option<(i32, i32, i32, i32)> {
        let len = (buf_w as usize) * (buf_h as usize) * 4;
        let buf = unsafe { std::slice::from_raw_parts_mut(bits, len) };
        buf.fill(0);
        let ok = unsafe {
            TextOutW(
                dc,
                margin,
                margin,
                wide_text.as_ptr(),
                wide_text.len() as i32,
            )
        };
        unsafe {
            GdiFlush();
        }
        if ok == 0 {
            return None;
        }
        ink_bounds(buf, buf_w, buf_h)
    }

    fn copy_centered(
        size: u32,
        buf: &[u8],
        buf_w: i32,
        bounds: (i32, i32, i32, i32),
    ) -> Vec<u8> {
        let (min_x, max_x, min_y, max_y) = bounds;
        let ink_w = (max_x - min_x + 1) as u32;
        let ink_h = (max_y - min_y + 1) as u32;
        let dst_x = (size - ink_w) / 2;
        let dst_y = (size - ink_h) / 2;
        let mut mask = vec![0u8; (size * size) as usize];
        for sy in 0..ink_h {
            let src_row = (((min_y + sy as i32) * buf_w) as usize) * 4;
            let dst_row = ((dst_y + sy) * size + dst_x) as usize;
            for sx in 0..ink_w {
                let src_idx = src_row + ((min_x + sx as i32) as usize) * 4 + 2;
                mask[dst_row + sx as usize] = buf[src_idx];
            }
        }
        mask
    }

    fn downsample_centered(
        size: u32,
        avail_w: u32,
        avail_h: u32,
        buf: &[u8],
        buf_w: i32,
        bounds: (i32, i32, i32, i32),
    ) -> (Vec<u8>, u32) {
        let (min_x, max_x, min_y, max_y) = bounds;
        let ink_w = (max_x - min_x + 1) as u32;
        let ink_h = (max_y - min_y + 1) as u32;
        let target_w = avail_w.saturating_sub(1).max(1);
        let target_h = avail_h.saturating_sub(1).max(1);
        let scale = 1.0_f64
            .min(f64::from(target_w) / f64::from(ink_w))
            .min(f64::from(target_h) / f64::from(ink_h));
        let out_w = (f64::from(ink_w) * scale)
            .round()
            .clamp(1.0, f64::from(avail_w)) as u32;
        let out_h = (f64::from(ink_h) * scale)
            .round()
            .clamp(1.0, f64::from(avail_h)) as u32;
        let dst_x = (size - out_w) / 2;
        let dst_y = (size - out_h) / 2;
        let mut mask = vec![0u8; (size * size) as usize];
        for oy in 0..out_h {
            let sy0 = f64::from(oy) * f64::from(ink_h) / f64::from(out_h);
            let sy1 = f64::from(oy + 1) * f64::from(ink_h) / f64::from(out_h);
            let iy0 = (sy0.floor() as u32).min(ink_h);
            let iy1 = (sy1.ceil() as u32).min(ink_h);
            for ox in 0..out_w {
                let sx0 = f64::from(ox) * f64::from(ink_w) / f64::from(out_w);
                let sx1 = f64::from(ox + 1) * f64::from(ink_w) / f64::from(out_w);
                let ix0 = (sx0.floor() as u32).min(ink_w);
                let ix1 = (sx1.ceil() as u32).min(ink_w);
                let mut sum = 0.0_f64;
                let mut weight_sum = 0.0_f64;
                for sy in iy0..iy1 {
                    let wy = (sy1.min(f64::from(sy + 1)) - sy0.max(f64::from(sy))).max(0.0);
                    let src_row = (((min_y + sy as i32) * buf_w) as usize) * 4;
                    for sx in ix0..ix1 {
                        let wx = (sx1.min(f64::from(sx + 1)) - sx0.max(f64::from(sx))).max(0.0);
                        let w = wx * wy;
                        let val = f64::from(buf[src_row + ((min_x + sx as i32) as usize) * 4 + 2]);
                        sum += w * val;
                        weight_sum += w;
                    }
                }
                let alpha = if weight_sum > 0.0 {
                    (sum / weight_sum).round().clamp(0.0, 255.0) as u8
                } else {
                    0
                };
                mask[((dst_y + oy) * size + dst_x + ox) as usize] = alpha;
            }
        }
        (mask, out_h)
    }

    fn fit_with_face(
        dc: HDC,
        size: u32,
        avail_w: u32,
        avail_h: u32,
        wide_text: &[u16],
        face: &str,
    ) -> Option<FitResult> {
        let wide_face: Vec<u16> = face.encode_utf16().chain(Some(0)).collect();
        let ref_h = (size as i32 * 2).max(32);
        let start_h = {
            let font = create_font(ref_h, &wide_face)?;
            let _sel = SelectGuard::new(dc, font.0)?;
            if !verify_font(dc, face) {
                return None;
            }
            let ext = text_extent(dc, wide_text)?;
            let width_cap =
                ((f64::from(avail_w) * f64::from(ref_h) / f64::from(ext.cx.max(1))) * 1.4).ceil()
                    as i32;
            ((f64::from(size) * 1.85).ceil() as i32)
                .min(width_cap.max(6))
                .max(6)
        };

        let margin = 8_i32;
        let buf_w = ((avail_w * 3).max(size * 2).max(96) as i32) + margin * 2;
        let buf_h = (((start_h as u32) * 2).max(size * 2).max(96) as i32) + margin * 2;
        let (dib, bits) = create_dib(dc, buf_w, buf_h)?;
        let _dib_sel = SelectGuard::new(dc, dib.0)?;

        for h in (6..=start_h).rev() {
            let font = create_font(h, &wide_face)?;
            let _font_sel = SelectGuard::new(dc, font.0)?;
            if let Some(ext) = text_extent(dc, wide_text) {
                if ext.cx > (avail_w as i32) * 2 {
                    continue;
                }
            }
            let Some((min_x, max_x, min_y, max_y)) =
                draw_and_measure(dc, bits, buf_w, buf_h, margin, wide_text)
            else {
                return Some(FitResult {
                    mask: vec![0u8; (size * size) as usize],
                    ink_h: 0,
                    scaled: false,
                });
            };
            let ink_w = (max_x - min_x + 1) as u32;
            let ink_h = (max_y - min_y + 1) as u32;
            if ink_w <= avail_w && ink_h <= avail_h {
                let len = (buf_w as usize) * (buf_h as usize) * 4;
                let buf = unsafe { std::slice::from_raw_parts(bits, len) };
                return Some(FitResult {
                    mask: copy_centered(size, buf, buf_w, (min_x, max_x, min_y, max_y)),
                    ink_h,
                    scaled: false,
                });
            }
        }
        drop(_dib_sel);
        drop(dib);

        let fallback_h = 20_i32;
        let font = create_font(fallback_h, &wide_face)?;
        let _font_sel = SelectGuard::new(dc, font.0)?;
        let ext = text_extent(dc, wide_text)?;
        let fb_w = ext.cx.max(32) + margin * 2;
        let fb_h = 48 + margin * 2;
        let (fb_dib, fb_bits) = create_dib(dc, fb_w, fb_h)?;
        let _fb_sel = SelectGuard::new(dc, fb_dib.0)?;
        let bounds = draw_and_measure(dc, fb_bits, fb_w, fb_h, margin, wide_text)?;
        let len = (fb_w as usize) * (fb_h as usize) * 4;
        let buf = unsafe { std::slice::from_raw_parts(fb_bits, len) };
        let (mask, out_h) = downsample_centered(size, avail_w, avail_h, buf, fb_w, bounds);
        Some(FitResult {
            mask,
            ink_h: out_h,
            scaled: true,
        })
    }

    pub(super) fn render_text_mask(size: u32, text: &str) -> Option<Vec<u8>> {
        let wide_text: Vec<u16> = text.encode_utf16().collect();
        if wide_text.is_empty() {
            return None;
        }
        let dc = DcGuard(unsafe { CreateCompatibleDC(std::ptr::null_mut()) });
        if dc.0.is_null() {
            return None;
        }
        unsafe {
            SetBkMode(dc.0, TRANSPARENT as i32);
            SetTextColor(dc.0, 0x00FF_FFFF);
        }
        let (pad_x, pad_y) = badge_padding(size);
        let avail_w = size.saturating_sub(pad_x * 2).max(1);
        let avail_h = size.saturating_sub(pad_y * 2).max(1);

        if let Some(semi) = fit_with_face(
            dc.0,
            size,
            avail_w,
            avail_h,
            &wide_text,
            "Bahnschrift SemiCondensed",
        ) {
            if !semi.scaled && semi.ink_h >= avail_h {
                return Some(semi.mask);
            }
            if let Some(cond) = fit_with_face(
                dc.0,
                size,
                avail_w,
                avail_h,
                &wide_text,
                "Bahnschrift Condensed",
            ) {
                if !cond.scaled && (semi.scaled || cond.ink_h > semi.ink_h) {
                    return Some(cond.mask);
                }
            }
            return Some(semi.mask);
        }

        for face in ["Bahnschrift Condensed", "Segoe UI", "Arial"] {
            if let Some(fit) = fit_with_face(dc.0, size, avail_w, avail_h, &wide_text, face) {
                return Some(fit.mask);
            }
        }
        None
    }
}

pub fn render_badge_rgba(size: u32, text: &str, tone: Tone) -> Vec<u8> {
    if !(16..=64).contains(&size) || text.is_empty() {
        return Vec::new();
    }
    let clipped: String = text.chars().take(12).collect();
    #[cfg(windows)]
    let mask = gdi::render_text_mask(size, &clipped)
        .unwrap_or_else(|| render_bitmap_mask(size, &clipped));
    #[cfg(not(windows))]
    let mask = render_bitmap_mask(size, &clipped);

    let side = f64::from(size);
    let radius = side * 0.22;
    let background = match tone {
        Tone::Ok => [139u32, 201, 162],
        Tone::Warn => [237, 193, 133],
        Tone::Bad => [239, 157, 148],
        Tone::Off => [58, 67, 86],
    };
    let foreground = if tone == Tone::Off {
        [255u32, 255, 255]
    } else {
        [20u32, 25, 33]
    };
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut covered = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = f64::from(x) + (f64::from(sx) + 0.5) / 4.0;
                    let fy = f64::from(y) + (f64::from(sy) + 0.5) / 4.0;
                    let dx = fx - fx.clamp(radius, side - radius);
                    let dy = fy - fy.clamp(radius, side - radius);
                    if dx * dx + dy * dy <= radius * radius {
                        covered += 1;
                    }
                }
            }
            if covered == 0 {
                rgba.extend([0, 0, 0, 0]);
            } else {
                let a = u32::from(mask[(y * size + x) as usize]);
                for (bg, fg) in background.into_iter().zip(foreground) {
                    rgba.push(((bg * (255 - a) + fg * a + 127) / 255) as u8);
                }
                rgba.push(((covered * 255 + 8) / 16) as u8);
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
    use std::collections::HashSet;

    fn ink_bbox(size: u32, rgba: &[u8], tone: Tone) -> Option<(u32, u32, u32, u32)> {
        let bg = match tone {
            Tone::Ok => [139i32, 201, 162],
            Tone::Warn => [237, 193, 133],
            Tone::Bad => [239, 157, 148],
            Tone::Off => [58, 67, 86],
        };
        let (mut min_x, mut max_x) = (size, 0);
        let (mut min_y, mut max_y) = (size, 0);
        let mut any = false;
        for y in 0..size {
            for x in 0..size {
                let idx = ((y * size + x) * 4) as usize;
                if rgba[idx + 3] < 200 {
                    continue;
                }
                let diff = (0..3)
                    .map(|c| (i32::from(rgba[idx + c]) - bg[c]).abs())
                    .max()
                    .unwrap_or(0);
                if diff >= 24 {
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                    any = true;
                }
            }
        }
        any.then_some((min_x, max_x, min_y, max_y))
    }

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
    fn badge_stays_large_centered_and_unclipped() {
        for size in [16u32, 20, 24, 32, 48, 64] {
            let (pad_x, pad_y) = badge_padding(size);
            for (text, min_pct) in [
                ("74", 68),
                ("0.4", 55),
                ("100", 50),
                ("1.2k", 45),
                ("10w", 45),
                ("-0.4", 40),
                ("12.5", 45),
            ] {
                let pixels = render_badge_rgba(size, text, Tone::Ok);
                let (min_x, max_x, min_y, max_y) =
                    ink_bbox(size, &pixels, Tone::Ok).expect("non-empty glyph ink");
                assert!(
                    min_x >= pad_x && max_x < size - pad_x,
                    "x out of bounds for {text} at {size}px: {min_x}..={max_x}, pad_x={pad_x}"
                );
                assert!(
                    min_y >= pad_y && max_y < size - pad_y,
                    "y out of bounds for {text} at {size}px: {min_y}..={max_y}, pad_y={pad_y}"
                );
                let right_margin = size - 1 - max_x;
                let bottom_margin = size - 1 - max_y;
                assert!(
                    min_x.abs_diff(right_margin) <= 1,
                    "x not centered for {text} at {size}px: left={min_x}, right={right_margin}"
                );
                assert!(
                    min_y.abs_diff(bottom_margin) <= 1,
                    "y not centered for {text} at {size}px: top={min_y}, bottom={bottom_margin}"
                );
                #[cfg(windows)]
                {
                    let ink_h = max_y - min_y + 1;
                    let required = size * min_pct / 100;
                    assert!(
                        ink_h >= required,
                        "glyph too small for {text} at {size}px: ink_h={ink_h} < {required}"
                    );
                }
                #[cfg(not(windows))]
                let _ = min_pct;
            }
        }
        for size in [16u32, 20, 24, 32] {
            let distinct: HashSet<Vec<u8>> = (0..=9)
                .map(|d| render_badge_rgba(size, &d.to_string(), Tone::Ok))
                .collect();
            assert_eq!(distinct.len(), 10, "digits 0..9 must differ at {size}px");
        }
        let off = render_badge_rgba(20, "74", Tone::Off);
        assert!(off.chunks_exact(4).any(|p| p[3] == 255 && p[0] > 200));
        for tone in [Tone::Ok, Tone::Warn, Tone::Bad] {
            let px = render_badge_rgba(20, "74", tone);
            assert!(px.chunks_exact(4).any(|p| p[3] == 255 && p[0] < 90));
        }
    }

    #[test]
    fn badge_long_text_fallback_stays_within_bounds_and_bitmap_mask_works() {
        for size in [16u32, 20, 32] {
            let (pad_x, pad_y) = badge_padding(size);
            let pixels = render_badge_rgba(size, "123456789w", Tone::Warn);
            let (min_x, max_x, min_y, max_y) =
                ink_bbox(size, &pixels, Tone::Warn).expect("long text should still render ink");
            assert!(min_x >= pad_x && max_x < size - pad_x);
            assert!(min_y >= pad_y && max_y < size - pad_y);

            for sample in ["74", "1.2k", "10w", "100%"] {
                let mask = render_bitmap_mask(size, sample);
                assert_eq!(mask.len(), (size * size) as usize);
                assert!(mask.iter().any(|&v| v > 128));
            }
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
