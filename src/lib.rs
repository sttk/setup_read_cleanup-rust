// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

mod errors;
mod lock;
mod lockings;
mod phase;

use std::{cell, error, marker, sync::atomic, time};

#[derive(Debug, PartialEq, Eq)]
pub enum Phase {
    Setup,
    Read,
    Cleanup,
}

pub mod locking {
    #[derive(Debug, PartialEq, Eq)]
    pub enum Setup {
        Unlock,
        #[cfg(feature = "setup_read_cleanup-blocking")]
        Blocking,
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        NonBlocking,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum Cleanup {
        Unlock,
        #[cfg(feature = "setup_read_cleanup-blocking")]
        Blocking,
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        NonBlocking,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PhasedErrorKind {
    /*
    #[cfg(feature = "setup_read_cleanup-blocking")]
    MutexIsPoisoned,
    TransitionToReadFailed,
    PhaseIsAlreadyRead,
    DuringTransitionToRead,
    TransitionToCleanupFailed,
    PhaseIsAlreadyCleanup,
    DuringTransitionToCleanup,
    FailToRunClosureDuringTranstionToRead,
    */
    InternalDataIsEmpty,
    TransitionToCleanupTimeout(WaitStrategy),
    CannotCallInSetupPhase(String),
    CannotCallInReadPhase(String),
    CannotCallOutOfReadPhase(String),
}

pub struct PhasedError {
    pub phase: Phase,
    pub kind: PhasedErrorKind,
    pub source: Option<Box<dyn error::Error + Send + Sync>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WaitStrategy {
    NoWait,
    FixedWait(time::Duration),
    GracefulWait { timeout: time::Duration },
}

pub struct PhasedLock<T: Send + Sync> {
    lockings: u8,
    phase: atomic::AtomicU8,

    read_count: atomic::AtomicUsize,
    finish_reading_mutex: std::sync::Mutex<bool>,

    data_fixed: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,

    #[cfg(feature = "setup_read_cleanup-blocking")]
    wait_std_condvar: std::sync::Condvar,
    #[cfg(feature = "setup_read_cleanup-blocking")]
    data_std_mutex: std::sync::Mutex<Option<T>>,

    #[cfg(feature = "setup_read_cleanup-on-tokio")]
    wait_tokio_notify: tokio::sync::Notify,
    #[cfg(feature = "setup_read_cleanup-on-tokio")]
    data_tokio_mutex: tokio::sync::Mutex<Option<T>>,
}

#[cfg(feature = "setup_read_cleanup-blocking")]
pub struct PhasedStdMutexGuard<'mutex, T> {
    inner: std::sync::MutexGuard<'mutex, Option<T>>,
}

#[cfg(feature = "setup_read_cleanup-on-tokio")]
pub struct PhasedTokioMutexGuard<'mutex, T> {
    inner: tokio::sync::MutexGuard<'mutex, Option<T>>,
}
