// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

mod errors;
mod lock;
mod phase;
mod wait;

use std::{cell, error, marker, sync, sync::atomic, time};

#[derive(Debug, PartialEq, Eq)]
pub enum Phase {
    Setup,
    Read,
    Cleanup,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PhasedErrorKind {
    MutexIsPoisoned,
    TransitionToReadFailed,
    TransitionToCleanupTimeout(WaitStrategy),
    FailToRunClosureDuringTransitionToRead,
    GracefulPhaseTransitionIsInProgress,
    InternalDataIsEmpty,
    CannotCallInSetupPhase(String),
    CannotCallInReadPhase(String),
    CannotCallOutOfReadPhase(String),
    CannotCallOnTokioRuntime(String),
}

pub struct PhasedError {
    pub phase: Phase,
    pub kind: PhasedErrorKind,
    pub source: Option<Box<dyn error::Error + Send + Sync>>,
}

pub struct PhasedLock<T: Send + Sync> {
    phase: atomic::AtomicU8,
    read_count: atomic::AtomicUsize,
    data_mutex: sync::Mutex<Option<T>>,
    data_fixed: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}

pub struct PhasedMutexGuard<'mutex, T> {
    inner: sync::MutexGuard<'mutex, Option<T>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitStrategy {
    NoWait,
    FixedWait(time::Duration),
    GracefulWait {
        first: time::Duration,
        interval: time::Duration,
        timeout: time::Duration,
    },
}
