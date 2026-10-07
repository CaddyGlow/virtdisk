//! Unit-test coordination for process-local descriptor inheritance.
//!
//! A spawned process can inherit another thread's locked file descriptor until
//! exec closes it. An immediate drop/reopen assertion can then see WouldBlock.
//! Writer tests share the read lock, preserving their parallel execution; tests
//! spawning subprocesses hold the exclusive guard for their entire lifetime.
//! This module never participates in production locking or retries.
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

static PROCESS_BOUNDARY: RwLock<()> = RwLock::new(());

pub(crate) fn writer_test() -> RwLockReadGuard<'static, ()> {
    PROCESS_BOUNDARY.read().unwrap_or_else(|e| e.into_inner())
}
pub(crate) fn subprocess_test() -> RwLockWriteGuard<'static, ()> {
    PROCESS_BOUNDARY.write().unwrap_or_else(|e| e.into_inner())
}
