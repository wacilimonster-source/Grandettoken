use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokenscope_core::{fetch::ChannelView, tray_icon, view::Tone};
use tokio::{sync::Notify, time::Instant};

use crate::{home_view_of, lock_rw, now_ts, window_cmds, AppState};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(crate) struct TrayState {
    channels: Mutex<Arc<[ChannelView]>>,
    changed: Notify,
}

pub(crate) fn publish(app: &AppHandle, channels: &[ChannelView]) {
    if let Some(state) = app.try_state::<TrayState>() {
        *lock_rw(&state.channels) = Arc::from(channels);
        state.changed.notify_one();
    }
}

pub(crate) fn wake(app: &AppHandle) {
    if let Some(state) = app.try_state::<TrayState>() {
        state.changed.notify_one();
    }
}

pub(crate) async fn run(app: AppHandle) {
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    let state = app.state::<TrayState>();
    let mut size = 16;
    let mut next_dpi = Instant::now();
    let mut next_turn = next_dpi;
    let mut dwell = Duration::ZERO;
    let mut index = 0;
    let mut last_tip = String::new();
    let mut last_frame: Option<(u32, String, Tone)> = None;
    let mut last_digest = None;
    let mut icon_after = Instant::now();

    loop {
        let now = Instant::now();
        let wall = now_ts();
        if now >= next_dpi {
            size = window_cmds::tray_icon_size(&app);
            next_dpi = now + Duration::from_secs(30);
        }
        let cfg = lock_rw(&app.state::<AppState>().config).clone();
        let channels = lock_rw(&state.channels).clone();
        let sequence = tray_icon::rotation(&channels, &cfg, wall);
        let interval = Duration::from_secs(if cfg.pill_dwell_sec == 0 {
            12
        } else {
            cfg.pill_dwell_sec.clamp(4, 86_400)
        });
        if index >= sequence.len() {
            index = 0;
        }
        if sequence.len() < 2 || interval != dwell {
            next_turn = now + interval;
        } else if now >= next_turn {
            index = (index + 1) % sequence.len();
            next_turn = now + interval;
        }
        dwell = interval;
        let home = home_view_of(&cfg, &channels, wall);
        let configured = channels
            .iter()
            .any(|c| c.has_key && !c.hidden && !cfg.hidden_channels.contains(&c.id));
        let face = tray_icon::face(sequence.get(index).copied(), &home, &cfg, configured);
        if face.tooltip != last_tip {
            match tray.set_tooltip(Some(&face.tooltip)) {
                Ok(()) => last_tip = face.tooltip,
                Err(e) => tokenscope_core::diaglog::write_keyed("tray", "tooltip", &e.to_string()),
            }
        }
        let frame = face.badge.map(|text| (size, text, face.tone));
        let mut retry = None;
        if frame != last_frame {
            if now < icon_after {
                retry = Some(icon_after);
            } else {
                let (image, digest) = if let Some((size, text, tone)) = &frame {
                    let rgba = tray_icon::render_badge_rgba(*size, text, *tone);
                    let digest = Some(window_cmds::tray_icon_digest(*size, &rgba));
                    (
                        Some(tauri::image::Image::new_owned(rgba, *size, *size)),
                        digest,
                    )
                } else {
                    (app.default_window_icon().cloned(), None)
                };
                let rendered = if digest == last_digest {
                    Ok(())
                } else {
                    tray.set_icon(image)
                };
                icon_after = now + Duration::from_millis(1500);
                match rendered {
                    Ok(()) => {
                        last_frame = frame;
                        last_digest = digest;
                    }
                    Err(e) => {
                        retry = Some(icon_after);
                        tokenscope_core::diaglog::write_keyed("tray", "icon", &e.to_string());
                    }
                }
            }
        }
        let wake_at = [
            Some(next_dpi),
            (sequence.len() > 1).then_some(next_turn),
            retry,
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(next_dpi);
        tokio::select! {
            _ = state.changed.notified() => {},
            _ = tokio::time::sleep_until(wake_at) => {},
        }
    }
}
