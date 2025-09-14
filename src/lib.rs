// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

//! This library provides a mechanism to manage a data structure through three
//! phases: Setup, Read, and Cleanup.
//!
//! This is for data that is built in the **Setup** phase, then becomes readable
//! (but not updatable) in the **Read** phase, and is finally destroyed in the
//! **Cleanup** phase.
//!
//! In the **Setup** phase, the data can be updated.
//! In the **Read** phase, the data can be read.
//! In the **Cleanup** phase, the data can be updated again.
//!
//! The phase can transition from **Setup** to **Read**, and from **Read** or
//! **Setup** to **Cleanup**.
//!
//! This library provides `PhasedLock<T>` which is a phased lock for data `T`.
//!
//! # Example
//!
//! ```rust
//! use setup_read_cleanup::{PhasedLock, WaitStrategy};
//! use std::{error, fmt, thread};
//!
//! struct MyData {
//!     num: i32,
//!     str: Option<String>,
//! }
//!
//! #[derive(Debug)]
//! struct MyError;
//! impl fmt::Display for MyError {
//!     fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "MyError") }
//! }
//! impl error::Error for MyError {}
//!
//! static PHASED_LOCK: PhasedLock<MyData> = PhasedLock::new(MyData { num: 0, str: None });
//!
//! // In Setup phase
//! {
//!     let mut data = PHASED_LOCK.lock_for_update().unwrap();
//!     data.num = 123;
//!     data.str = Some("hello".to_string());
//! }
//!
//! // Transition to Read phase
//! PHASED_LOCK.transition_to_read(|_data| Ok::<(), MyError>(())).unwrap();
//!
//! // In Read phase
//! let handles: Vec<_> = (0..3).map(|i| {
//!     thread::spawn(move || {
//!         let data = PHASED_LOCK.read_gracefully().unwrap();
//!         println!("Thread {}: num={}, str={:?}", i, data.num, data.str);
//!         PHASED_LOCK.finish_reading_gracefully().unwrap();
//!     })
//! }).collect();
//!
//! for h in handles {
//!     h.join().unwrap();
//! }
//!
//! // Transition to Cleanup phase
//! PHASED_LOCK.transition_to_cleanup(WaitStrategy::NoWait).unwrap();
//!
//! // In Cleanup phase
//! {
//!     let mut data = PHASED_LOCK.lock_for_update().unwrap();
//!     data.num = 0;
//!     data.str = None;
//! }
//! ```

mod errors;
mod lock;
mod phase;

use std::{cell, error, marker, sync, sync::atomic, time};

/// An enum representing the current phase of a `PhasedLock`.
#[derive(Debug, PartialEq, Eq)]
pub enum Phase {
    /// The setup phase, where the data can be initialized and modified.
    Setup,

    /// The read phase, where the data is read-only.
    Read,

    /// The cleanup phase, where the data can be modified before being dropped.
    Cleanup,
}

/// An enum representing the kinds of errors that can occur in this library.
#[derive(Debug, PartialEq, Eq)]
pub enum PhasedErrorKind {
    /// An error indicating that a mutex is poisoned.
    MutexIsPoisoned,

    /// An error indicating that a phase transition to the Read phase failed.
    TransitionToReadFailed,

    /// An error indicating that the phase is already in the Read phase.
    PhaseIsAlreadyRead,

    /// An error indicating that a transition to the Read phase is currently in progress.
    DuringTransitionToRead,

    /// An error indicating that a closure failed to run during a phase
    /// transition to the Read phase.
    FailToRunClosureDuringTransitionToRead,

    /// An error indicating that a phase transition to the Cleanup phase failed.
    TransitionToCleanupFailed,

    /// An error indicating that a phase transition to the Cleanup phase timed out.
    TransitionToCleanupTimeout(WaitStrategy),

    /// An error indicating that the phase is already in the Cleanup phase.
    PhaseIsAlreadyCleanup,

    /// An error indicating that a transition to the Cleanup phase is currently in progress.
    DuringTransitionToCleanup,

    /// An error indicating that the internal data is empty.
    InternalDataIsEmpty,

    /// An error indicating that a method cannot be called in the Setup phase.
    CannotCallInSetupPhase(String),

    /// An error indicating that a method cannot be called in the Read phase.
    CannotCallInReadPhase(String),

    /// An error indicating that a method cannot be called outside of the Read phase.
    CannotCallOutOfReadPhase(String),

    /// An error indicating that a method cannot be called on a Tokio runtime.
    CannotCallOnTokioRuntime(String),
}

/// A struct representing an error that occurred in a `PhasedLock`.
pub struct PhasedError {
    /// The phase in which the error occurred.
    pub phase: Phase,
    /// The kind of error that occurred.
    pub kind: PhasedErrorKind,
    /// The source of the error, if any.
    pub source: Option<Box<dyn error::Error + Send + Sync>>,
}

/// A lock that manages a data structure through three phases: Setup, Read, and Cleanup.
pub struct PhasedLock<T: Send + Sync> {
    phase: atomic::AtomicU8,
    read_count: atomic::AtomicUsize,
    wait_cvar: sync::Condvar,
    data_mutex: sync::Mutex<Option<T>>,
    data_fixed: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}

/// A mutex guard for a `PhasedLock` in Setup and Cleanup phase.
pub struct PhasedMutexGuard<'mutex, T> {
    inner: sync::MutexGuard<'mutex, Option<T>>,
}

/// An enum representing the waiting strategy for a phase transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitStrategy {
    /// A strategy that does not wait.
    NoWait,
    /// A strategy that waits for a fixed amount of time.
    FixedWait(time::Duration),
    /// A strategy that waits gracefully for a certain amount of time.
    GracefulWait {
        /// The timeout for the wait.
        timeout: time::Duration,
    },
}
