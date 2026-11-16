//! 合并同一轮的渐进结果。定时器由取数 future 驱动，结束或取消后没有迟到任务。
use crate::fetch::ChannelView;
use std::{future::Future, time::Duration};

#[derive(Default)]
pub(crate) struct Progress {
    pub rows: Vec<ChannelView>,
    first: bool,
    dirty: bool,
}

impl Progress {
    pub fn begin(&mut self) {
        self.first = true;
        self.dirty = false;
    }

    pub fn update(&mut self, view: ChannelView) -> Option<Vec<ChannelView>> {
        if let Some(old) = self.rows.iter_mut().find(|row| row.id == view.id) {
            *old = view;
        } else {
            self.rows.push(view);
        }
        self.dirty = true;
        if self.first {
            self.first = false;
            self.flush()
        } else {
            None
        }
    }

    pub fn flush(&mut self) -> Option<Vec<ChannelView>> {
        if !self.dirty { return None; }
        self.dirty = false;
        Some(self.rows.clone())
    }

    pub fn finish(&mut self, rows: Vec<ChannelView>) {
        self.rows = rows;
        self.dirty = false;
    }
}

pub(crate) async fn drive<F: Future>(future: F, mut flush: impl FnMut()) -> F::Output {
    futures::pin_mut!(future);
    loop {
        let delay = Box::pin(tokio::time::sleep(Duration::from_millis(150)));
        match futures::future::select(future.as_mut(), delay).await {
            futures::future::Either::Left((result, _)) => return result,
            futures::future::Either::Right(_) => flush(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_is_coalesced_and_final_state_cannot_arrive_late() {
        let mut progress = Progress::default();
        progress.begin();
        let mut pushes = 0;
        let store = crate::store::Store::open_memory().unwrap();
        let view = crate::google_cloud::channel(&store, &crate::config::Config::default(), 0);
        for def in crate::providers::PROVIDERS {
            let mut row = view.clone();
            row.id = def.id.into();
            pushes += usize::from(progress.update(row).is_some());
        }
        assert_eq!(pushes, 1);
        assert_eq!(progress.flush().unwrap().len(), crate::providers::PROVIDERS.len());
        assert!(progress.flush().is_none());
        progress.update(view.clone());
        progress.finish(vec![view.clone()]);
        assert!(progress.flush().is_none());
        progress.begin();
        assert!(progress.update(view).is_some());
    }

    #[test]
    fn timer_flushes_while_slow_channels_are_pending_and_stops_at_completion() {
        tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap().block_on(async {
            let count = std::cell::Cell::new(0);
            let result = drive(async {
                tokio::time::sleep(Duration::from_millis(380)).await;
                42
            }, || count.set(count.get() + 1)).await;
            assert_eq!(result, 42);
            assert!(count.get() >= 1);
            let completed = count.get();
            tokio::time::sleep(Duration::from_millis(180)).await;
            assert_eq!(count.get(), completed);
        });
    }
}
