//! 互斥锁中毒恢复。
//!
//! 挂件 7×24 常驻:单次后台取数或子任务 panic 使 `Mutex` 中毒后,后续轮询或命令不该连锁
//! panic 导致整个进程退出(与 `src-tauri` 的 `lock_rw` 口径一致)。恢复锁内部状态的同时,
//! 按被保护类型节流写入一条诊断日志供排查。
//!
//! 对 `Store`(`rusqlite::Connection`)的恢复安全性:`rusqlite::Transaction` 在 drop 时默认
//! 执行 `ROLLBACK`,因此持锁线程在事务中途 panic 后,恢复出来的连接不会停在半截事务里。

use std::sync::{Mutex, MutexGuard};

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| {
        let ty = std::any::type_name::<T>();
        crate::diaglog::write_keyed(
            "lock-poisoned",
            ty,
            &format!("检测到互斥锁中毒并已恢复 ({ty})"),
        );
        p.into_inner()
    })
}
