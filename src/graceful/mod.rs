// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

mod errors;
mod phased_cell;
mod phased_cell_sync;
mod wait_sync;

#[cfg(feature = "setup_read_cleanup-on-tokio")]
mod phased_cell_async;

#[cfg(feature = "setup_read_cleanup-on-tokio")]
mod wait_async;

use crate::{PhasedCell, PhasedCellSync};

#[cfg(feature = "setup_read_cleanup-on-tokio")]
use crate::PhasedCellAsync;

use std::{cell, sync::atomic, sync::Arc};

pub struct GracefulPhasedCell<T: Send + Sync> {
    cell: PhasedCell<T>,
    wait: GracefulWaitSync,
}

pub struct GracefulPhasedCellSync<T: Send + Sync> {
    cell: PhasedCellSync<T>,
    wait: GracefulWaitSync,
}

pub struct GracefulWaitSync {
    counter: atomic::AtomicUsize,
    blocker: std::sync::Mutex<bool>,
    condvar: std::sync::Condvar,
}

#[cfg(feature = "setup_read_cleanup-on-tokio")]
pub struct GracefulPhasedCellAsync<T: Send + Sync> {
    cell: PhasedCellAsync<T>,
    wait: cell::LazyCell<Arc<GracefulWaitAsync>>,
}

#[cfg(feature = "setup_read_cleanup-on-tokio")]
pub struct GracefulWaitAsync {
    counter: atomic::AtomicUsize,
    notify: tokio::sync::Notify,
}

#[derive(Debug, PartialEq, Eq)]
pub enum GracefulWaitErrorKind {
    TimedOut(std::time::Duration),
    MutexIsPoisoned,
}

pub struct GracefulWaitError {
    pub kind: GracefulWaitErrorKind,
}
