// Copyright (C) 2025-2026 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

//! A library for managing data through distinct lifecycle phases.
//!
//! This crate provides "phased cells", a collection of smart pointer types that enforce
//! a specific lifecycle for the data they manage: `Setup` -> `Read` -> `Cleanup`.
//! This is useful for data that is initialized once, read by multiple threads or tasks
//! for a period, and then explicitly destroyed.
//!
//! # Core Concepts
//!
//! The lifecycle is divided into three phases:
//! - **`Setup`**: The initial phase. The data can be mutably accessed for initialization.
//! - **`Read`**: The operational phase. The data can only be accessed immutably. This phase
//!   is optimized for concurrent, lock-free reads.
//! - **`Cleanup`**: The final phase. The data can be mutably accessed again for deconstruction
//!   or resource cleanup.
//!
//! ## Cell Variants
//!
//! This crate offers several cell variants to suit different concurrency needs:
//!
//! - [`PhasedCell`]: The basic cell. It is `Sync` if the contained data `T` is `Send + Sync`,
//!   allowing it to be shared across threads for reading. However, mutable access via
//!   [`get_mut_unlocked`](PhasedCell::get_mut_unlocked) is not thread-safe and requires the
//!   caller to ensure exclusive access.
//!
//! - [`PhasedCellSync`]: A thread-safe version that uses a `std::sync::Mutex` to allow for
//!   safe concurrent mutable access during the `Setup` and `Cleanup` phases.
//!
//! - [`PhasedCellAsync`]: (Requires the `tokio` feature) An `async` version
//!   of `PhasedCellSync` that uses a `tokio::sync::Mutex`.
//!
//! ## Graceful Cleanup
//!
//! (Requires the `graceful` feature)
//!
//! The `graceful` module provides wrappers that add graceful cleanup capabilities. When
//! transitioning to the `Cleanup` phase, these cells will wait for a specified duration
//! for all active read operations to complete.
//!
//! - [`GracefulPhasedCell`](graceful::GracefulPhasedCell)
//! - [`GracefulPhasedCellSync`](graceful::GracefulPhasedCellSync)
//! - [`GracefulPhasedCellAsync`](graceful::GracefulPhasedCellAsync) (Requires both features)
//!
//! # Examples
//!
//! Using a `static PhasedCellSync` to initialize data, read it from multiple threads, and then
//! clean it up.
//!
//! ```
//! use setup_read_cleanup::{PhasedCellSync, Phase};
//! use std::thread;
//!
//! struct MyData {
//!     items: Vec<i32>,
//! }
//!
//! // Declare a static PhasedCellSync instance
//! static CELL: PhasedCellSync<MyData> = PhasedCellSync::new(MyData { items: Vec::new() });
//!
//! fn main() {
//!     // --- Setup Phase ---
//!     assert_eq!(CELL.phase(), Phase::Setup);
//!     {
//!         let mut data = CELL.lock().unwrap();
//!         data.items.push(10);
//!         data.items.push(20);
//!     } // Lock is released here
//!
//!     // --- Transition to Read Phase ---
//!     CELL.transition_to_read(|data| {
//!         data.items.push(30);
//!         Ok::<(), std::io::Error>(())
//!     }).unwrap();
//!     assert_eq!(CELL.phase(), Phase::Read);
//!
//!     // --- Read Phase ---
//!     // Now, multiple threads can read the data concurrently.
//!     let mut handles = Vec::new();
//!     for i in 0..3 {
//!         handles.push(thread::spawn(move || {
//!             let data = CELL.read().unwrap(); // Access the static CELL
//!             println!("Thread {} reads: {:?}", i, data.items);
//!             assert_eq!(data.items, &[10, 20, 30]);
//!         }));
//!     }
//!
//!     for handle in handles {
//!         handle.join().unwrap();
//!     }
//!
//!     // --- Transition to Cleanup Phase ---
//!     CELL.transition_to_cleanup(|data| {
//!         println!("Cleaning up. Final item: {:?}", data.items.pop());
//!         Ok::<(), std::io::Error>(())
//!     }).unwrap();
//!     assert_eq!(CELL.phase(), Phase::Cleanup);
//! }
//! ```
//!
//! # Features
//!
//! - `tokio`: Enables the `async` cell variants (`PhasedCellAsync`, `GracefulPhasedCellAsync`)
//!   which use `tokio::sync`.
//! - `graceful`: Enables the `graceful` module, which provides cells with
//!   graceful cleanup capabilities.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod errors;
mod phase;
mod phased_cell;
mod phased_cell_sync;

#[cfg(feature = "tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
mod phased_cell_async;

/// A module for graceful cleanup of phased cells.
///
/// This module provides extensions and wrappers for `PhasedCell` and its variants
/// to support graceful cleanup, allowing ongoing operations to complete before
/// transitioning to the `Cleanup` phase.
#[cfg(feature = "graceful")]
#[cfg_attr(docsrs, doc(cfg(feature = "graceful")))]
pub mod graceful;

use std::{cell, error, marker, sync::atomic};

/// Represents the current operational phase of a phased cell.
///
/// The lifecycle of a phased cell progresses through these three distinct phases:
/// 1. `Setup`: The initial phase where the data is constructed and initialized.
/// 2. `Read`: The main operational phase where the data is accessed for read-only operations.
/// 3. `Cleanup`: The final phase where the data is deconstructed and resources are released.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Phase {
    /// The initial phase for setting up the data.
    Setup,
    /// The phase for reading the data.
    Read,
    /// The final phase for cleaning up the data.
    Cleanup,
}

/// An enumeration of possible error kinds that can occur in a phased cell.
///
/// This enum categorizes the various errors that can arise during phase transitions
/// or data access, providing specific information about the nature of the failure.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum PhasedErrorKind {
    /// An error indicating that a method was called before or after the `Read` phase.
    CannotCallUnlessPhaseRead(&'static str),
    /// An error indicating that a method was called during the `Setup` phase.
    CannotCallOnPhaseSetup(&'static str),
    /// An error indicating that a method was called during the `Read` phase.
    CannotCallOnPhaseRead(&'static str),
    /// An error indicating that the internal data is not available.
    InternalDataUnavailable,
    /// An error indicating that the phase is already `Read`.
    PhaseIsAlreadyRead,
    /// An error indicating that the phase is already `Cleanup`.
    PhaseIsAlreadyCleanup,
    /// An error indicating that a phase transition to `Read` is in progress.
    DuringTransitionToRead,
    /// An error indicating that a phase transition to `Cleanup` is in progress.
    DuringTransitionToCleanup,
    /// An error indicating that a closure failed to run during the transition to `Read`.
    FailToRunClosureDuringTransitionToRead,
    /// An error indicating that a closure failed to run during the transition to `Cleanup`.
    FailToRunClosureDuringTransitionToCleanup,
    /// An error indicating that a `std::sync::Mutex` for internal data is poisoned.
    InternalDataMutexIsPoisoned,

    /// An error indicating that the internal data mutex could not be locked non-blockingly.
    MutexTryLockFailed,

    /// An error indicating a timeout occurred while waiting for a graceful cleanup.
    #[cfg(feature = "graceful")]
    #[cfg_attr(docsrs, doc(cfg(feature = "graceful")))]
    GracefulWaitTimeout(std::time::Duration),

    /// An error indicating that a `std::sync::Mutex` for graceful-wait is poisoned.
    #[cfg(feature = "graceful")]
    #[cfg_attr(docsrs, doc(cfg(feature = "graceful")))]
    GracefulWaitMutexIsPoisoned,
}

/// A structure representing an error that occurred within a phased cell.
///
/// It contains the phase in which the error occurred, the kind of error, and an
/// optional source error for more context.
pub struct PhasedError {
    phase: Phase,
    kind: PhasedErrorKind,
    source: Option<Box<dyn error::Error + Send + Sync>>,
}

/// A cell that manages data through distinct `Setup`, `Read`, and `Cleanup` phases.
///
/// `PhasedCell` enforces a specific data lifecycle: initialization in the `Setup`
/// phase, a read-only operational period in the `Read` phase, and deconstruction
/// in the `Cleanup` phase.
///
/// This cell is `Sync` if the contained data `T` is `Send + Sync`, which allows
/// the cell to be shared across threads. During the `Read` phase, the `read` and
/// `read_relaxed` methods can be safely called from multiple threads simultaneously
/// if the cell is `Sync`. Access during phase transitions and mutable access in
/// other phases are not thread-safe.
pub struct PhasedCell<T> {
    phase: atomic::AtomicU8,
    data_cell: cell::UnsafeCell<T>,
    _marker: marker::PhantomData<T>,
}

/// A thread-safe cell that manages data through `Setup`, `Read`, and `Cleanup` phases
/// with support for concurrent mutable access.
///
/// `PhasedCellSync` is similar to `PhasedCell` but uses a `std::sync::Mutex` to
/// synchronize access to the internal data. This is particularly useful when
/// multiple threads need to mutate the data during the `Setup` or `Cleanup`
/// phases, which is not safely supported by `PhasedCell`.
pub struct PhasedCellSync<T: Send + Sync> {
    phase: atomic::AtomicU8,
    data_mutex: std::sync::Mutex<Option<T>>,
    data_cell: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}

/// A RAII implementation of a scoped lock for a `PhasedCellSync`.
///
/// When this structure is dropped (falls out of scope), the lock will be released.
pub struct StdMutexGuard<'mutex, T> {
    inner: std::sync::MutexGuard<'mutex, Option<T>>,
}

/// An asynchronous, thread-safe cell for managing data through `Setup`, `Read`, and `Cleanup` phases.
///
/// `PhasedCellAsync` is similar to `PhasedCellSync` but is designed for asynchronous
/// contexts using `tokio`. It leverages a `tokio::sync::Mutex` to provide asynchronous,
/// non-blocking locking, making it suitable for use in async applications.
#[cfg(feature = "tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
pub struct PhasedCellAsync<T: Send + Sync> {
    phase: atomic::AtomicU8,
    data_mutex: tokio::sync::Mutex<Option<T>>,
    data_cell: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}

/// A RAII implementation of a scoped lock for a `PhasedCellAsync`.
///
/// When this structure is dropped (falls out of scope), the lock will be released.
#[cfg(feature = "tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
pub struct TokioMutexGuard<'mutex, T> {
    inner: tokio::sync::MutexGuard<'mutex, Option<T>>,
}
