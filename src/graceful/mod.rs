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

use std::{cell, marker, sync::atomic};

pub struct GracefulPhasedCell<T: Send + Sync> {
    wait: GracefulWaitSync,
    phase: atomic::AtomicU8,
    data_cell: cell::UnsafeCell<T>,
    _marker: marker::PhantomData<T>,
}

pub struct GracefulPhasedCellSync<T: Send + Sync> {
    phase: atomic::AtomicU8,
    wait: GracefulWaitSync,
    data_mutex: std::sync::Mutex<Option<T>>,
    data_cell: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}

pub struct GracefulWaitSync {
    counter: atomic::AtomicUsize,
    blocker: std::sync::Mutex<bool>,
    condvar: std::sync::Condvar,
}

#[cfg(feature = "setup_read_cleanup-on-tokio")]
pub struct GracefulPhasedCellAsync<T: Send + Sync> {
    phase: atomic::AtomicU8,
    wait: GracefulWaitAsync,
    data_mutex: tokio::sync::Mutex<Option<T>>,
    data_cell: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
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
