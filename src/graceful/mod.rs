// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

mod phased_cell;
mod phased_cell_sync;

#[cfg(feature = "tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
mod phased_cell_async;

use std::{cell, marker, sync::atomic};

/// A `PhasedCell` that supports graceful cleanup and graceful read.
///
/// This cell extends [`PhasedCell`](crate::PhasedCell) with two main capabilities:
/// 1. **Graceful Cleanup**: It ensures that all ongoing read operations complete before allowing
///    the cell to fully transition to the `Cleanup` phase.
/// 2. **Graceful Read**: If a read operation is attempted while the cell is in the
///    `Setup` phase and transitioning to `Read`, the read operation
///    will wait for the transition to complete and for the cell to enter the `Read` phase.
///
/// Like `PhasedCell`, this cell is `Sync` if the contained data `T` is `Send + Sync`.
pub struct GracefulPhasedCell<T> {
    phase: atomic::AtomicU8,
    graceful_counter: atomic::AtomicUsize,
    graceful_condvar: std::sync::Condvar,
    graceful_mutex: std::sync::Mutex<()>,
    data_cell: cell::UnsafeCell<T>,
    _marker: marker::PhantomData<T>,
}

/// A thread-safe `PhasedCellSync` that supports graceful cleanup and graceful read.
///
/// This cell is the thread-safe counterpart to [`GracefulPhasedCell`], building upon
/// [`PhasedCellSync`](crate::PhasedCellSync). It offers:
/// 1. **Graceful Cleanup**: It ensures that all ongoing read operations complete before allowing
///    the cell to fully transition to the `Cleanup` phase.
/// 2. **Graceful Read**: If a read operation is attempted while the cell is in the
///    `Setup` phase and transitioning to `Read`, the read operation
///    will wait for the transition to complete and for the cell to enter the `Read` phase.
pub struct GracefulPhasedCellSync<T: Send + Sync> {
    phase: atomic::AtomicU8,
    graceful_counter: atomic::AtomicUsize,
    graceful_condvar: std::sync::Condvar,
    graceful_mutex: std::sync::Mutex<()>,
    data_mutex: std::sync::Mutex<Option<T>>,
    data_cell: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}

/// An asynchronous, thread-safe `PhasedCellAsync` that supports graceful cleanup and graceful read.
///
/// This cell is the asynchronous version of [`GracefulPhasedCellSync`], designed for
/// `tokio`-based applications and building upon [`PhasedCellAsync`](crate::PhasedCellAsync).
/// It provides:
/// 1. **Graceful Cleanup**: It ensures that all ongoing read operations complete before allowing
///    the cell to fully transition to the `Cleanup` phase.
/// 2. **Graceful Read**: If a read operation is attempted while the cell is in the
///    `Setup` phase and transitioning to `Read`, the read operation
///    will wait for the transition to complete and for the cell to enter the `Read` phase.
#[cfg(feature = "tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
pub struct GracefulPhasedCellAsync<T: Send + Sync> {
    phase: atomic::AtomicU8,
    graceful_counter: atomic::AtomicUsize,
    graceful_notify: tokio::sync::Notify,
    data_mutex: tokio::sync::Mutex<Option<T>>,
    data_cell: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}
