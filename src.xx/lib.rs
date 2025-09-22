// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

mod errors;
mod flags;
mod lock;

use std::{cell, error, marker, sync::atomic, time};

/// Represents the execution phase of a process.
#[derive(Debug, PartialEq, Eq)]
pub enum Phase {
    /// The setup phase, executed at the beginning.
    Setup,
    /// The read phase, for main processing.
    Read,
    /// The cleanup phase, executed at the end.
    Cleanup,
}

/// Provides enums for configuring locking behavior in different phases.
pub mod locking {
    /// Defines the locking strategy for the `Setup` phase.
    #[derive(Debug, PartialEq, Eq)]
    pub enum Setup {
        /// No lock is acquired.
        Unlock,
        /// Acquires a lock, blocking until it is available.
        Blocking,
        /// Acquires a lock without blocking (requires the `setup_read_cleanup-on-tokio` feature).
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        NonBlocking,
    }

    /// Defines the locking strategy for the `Read` phase.
    #[derive(Debug, PartialEq, Eq)]
    pub enum Read {
        /// A normal, non-graceful read.
        Normal,
        /// A graceful read that may be slower but ensures cleaner state.
        Graceful,
    }

    /// Defines the locking strategy for the `Cleanup` phase.
    #[derive(Debug, PartialEq, Eq)]
    pub enum Cleanup {
        /// Releases the acquired lock.
        Unlock,
        /// Blocks until the cleanup process is complete.
        Blocking,
        /// Performs cleanup without blocking (requires the `setup_read_cleanup-on-tokio` feature).
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        NonBlocking,
    }
}

/// Represents the kinds of errors that can occur in each phase.
#[derive(Debug, PartialEq, Eq)]
pub enum PhasedErrorKind {
    /// Indicates that a mutex was poisoned.
    MutexIsPoisoned,

    /// Indicates a failure to transition to the `Read` phase.
    TransitionToReadFailed,

    /// Indicates an attempt to transition to the `Read` phase when already in it.
    PhaseIsAlreadyRead,

    /// Indicates that a transition to the `Read` phase is already in progress.
    DuringTransitionToRead,

    /// Indicates a failure to transition to the `Cleanup` phase.
    TransitionToCleanupFailed,

    /// Indicates an attempt to transition to the `Cleanup` phase when already in it.
    PhaseIsAlreadyCleanup,

    /// Indicates that a transition to the `Cleanup` phase is already in progress.
    DuringTransitionToCleanup,

    /// Indicates a timeout while waiting to transition to the `Cleanup` phase.
    TransitionToCleanupTimeout(WaitStrategy),

    /// Indicates that the provided closure failed to execute during the transition to the `Read` phase.
    FailToRunClosureDuringTranstionToRead,

    /// Indicates that the internal data is empty.
    InternalDataIsEmpty,

    /// Indicates an attempt to call a method that is not allowed in the `Setup` phase.
    CannotCallInSetupPhase(String),

    /// Indicates an attempt to call a method that is not allowed in the `Read` phase.
    CannotCallInReadPhase(String),

    /// Indicates an attempt to call a method that is not allowed outside of the `Read` phase.
    CannotCallOutOfReadPhase(String),
}

/// Represents an error that occurs in a specific execution phase.
pub struct PhasedError {
    /// The phase in which the error occurred.
    pub phase: Phase,
    /// The specific kind of error.
    pub kind: PhasedErrorKind,
    /// The underlying error that caused this error, if any.
    pub source: Option<Box<dyn error::Error + Send + Sync>>,
}

/// Defines the strategy for waiting for a phase transition to complete.
#[derive(Debug, PartialEq, Eq)]
pub enum WaitStrategy {
    /// Do not wait.
    NoWait,
    /// Wait for a fixed amount of time.
    FixedWait(time::Duration),
    /// Wait gracefully, with a specified timeout.
    GracefulWait { timeout: time::Duration },
}

pub struct PhasedLock<T: Send + Sync> {
    flags: atomic::AtomicU8,
    read_count: atomic::AtomicUsize,

    data_fixed: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,

    wait_std_condvar: std::sync::Condvar,
    data_std_mutex: std::sync::Mutex<Option<T>>,

    #[cfg(feature = "setup_read_cleanup-on-tokio")]
    wait_tokio_notify: tokio::sync::Notify,
    #[cfg(feature = "setup_read_cleanup-on-tokio")]
    data_tokio_mutex: tokio::sync::Mutex<Option<T>>,
}

pub struct PhasedStdMutexGuard<'mutex, T> {
    inner: std::sync::MutexGuard<'mutex, Option<T>>,
}

#[cfg(feature = "setup_read_cleanup-on-tokio")]
pub struct PhasedTokioMutexGuard<'mutex, T> {
    inner: tokio::sync::MutexGuard<'mutex, Option<T>>,
}
