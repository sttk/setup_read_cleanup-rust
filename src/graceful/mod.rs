// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

mod errors;
mod phased_cell;
mod phased_cell_sync;
mod wait_sync;

#[cfg(feature = "setup_read_cleanup-on-tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "setup_read_cleanup-on-tokio")))]
mod phased_cell_async;

#[cfg(feature = "setup_read_cleanup-on-tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "setup_read_cleanup-on-tokio")))]
mod wait_async;

use std::{cell, marker, sync::atomic};

/// A gracefully shutdownable, non-thread-safe cell that manages data through distinct `Setup`, `Read`, and `Cleanup` phases.
///
/// `GracefulPhasedCell` extends `PhasedCell` with graceful shutdown capabilities.
/// It ensures that all read operations are completed before transitioning to the `Cleanup` phase.
pub struct GracefulPhasedCell<T: Send + Sync> {
    wait: GracefulWaitSync,
    phase: atomic::AtomicU8,
    data_cell: cell::UnsafeCell<T>,
    _marker: marker::PhantomData<T>,
}

/// A gracefully shutdownable, thread-safe cell that manages data through `Setup`, `Read`, and `Cleanup` phases.
///
/// `GracefulPhasedCellSync` is the thread-safe counterpart to `GracefulPhasedCell`.
/// It uses a `std::sync::Mutex` for synchronization and supports graceful shutdown.
pub struct GracefulPhasedCellSync<T: Send + Sync> {
    phase: atomic::AtomicU8,
    wait: GracefulWaitSync,
    data_mutex: std::sync::Mutex<Option<T>>,
    data_cell: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}

/// A synchronization primitive for graceful shutdown in synchronous contexts.
///
/// `GracefulWaitSync` is used by `GracefulPhasedCellSync` to block the cleanup process
/// until all read operations have completed.
pub struct GracefulWaitSync {
    counter: atomic::AtomicUsize,
    blocker: std::sync::Mutex<bool>,
    condvar: std::sync::Condvar,
}

/// An asynchronous, gracefully shutdownable, thread-safe cell.
///
/// `GracefulPhasedCellAsync` is the asynchronous version of `GracefulPhasedCellSync`,
/// designed for `tokio`-based applications. It uses `tokio::sync::Mutex` for non-blocking
/// synchronization and supports graceful shutdown.
#[cfg(feature = "setup_read_cleanup-on-tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "setup_read_cleanup-on-tokio")))]
pub struct GracefulPhasedCellAsync<T: Send + Sync> {
    phase: atomic::AtomicU8,
    wait: GracefulWaitAsync,
    data_mutex: tokio::sync::Mutex<Option<T>>,
    data_cell: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}

/// A synchronization primitive for graceful shutdown in asynchronous contexts.
///
/// `GracefulWaitAsync` is used by `GracefulPhasedCellAsync` to await the completion
/// of all read operations before cleanup.
#[cfg(feature = "setup_read_cleanup-on-tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "setup_read_cleanup-on-tokio")))]
pub struct GracefulWaitAsync {
    counter: atomic::AtomicUsize,
    notify: tokio::sync::Notify,
}

/// An enumeration of possible error kinds that can occur during a graceful wait.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum GracefulWaitErrorKind {
    /// An error indicating that the wait timed out.
    TimedOut(std::time::Duration),
    /// An error indicating that a mutex is poisoned.
    MutexIsPoisoned,
}

/// A structure representing an error that occurred during a graceful wait.
pub struct GracefulWaitError {
    kind: GracefulWaitErrorKind,
}
