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
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum Cleanup {
        Unlock,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PhasedErrorKind {
    CannotCallUnlessSetupUnlock(String),
    CannotCallUnlessCleanupUnlock(String),
    CannotCallOnPhaseSetup(String),
    CannotCallUnlessPhaseSetup(String),
    CannotCallUnlessPhaseRead(String),
    CannotCallUnlessPhaseCleanup(String),
    DuringTransitionToRead,
    DuringTransitionToCleanup,
    PhaseIsAlreadyRead,
    PhaseIsAlreadyCleanup,
    InternalDataUnavailable,
    TransitionToReadFailed,
    TransitionToCleanupFailed,
    TransitionToCleanupTimeout(WaitStrategy),
    FailToRunClosureDuringTransitionToRead,
    StdMutexIsPoisoned,
}

pub struct PhasedError {
    pub phase: Phase,
    pub kind: PhasedErrorKind,
    source: Option<Box<dyn error::Error + Send + Sync>>,
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

    graceful_read_count: atomic::AtomicUsize,
    graceful_std_mutex: std::sync::Mutex<bool>,
    graceful_std_cvar: std::sync::Condvar,

    data_fixed: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}
