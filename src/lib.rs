// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

mod errors;
mod phase;
mod phased_cell;
mod phased_cell_sync;

#[cfg(feature = "setup_read_cleanup-on-tokio")]
mod phased_cell_async;

#[cfg(feature = "setup_read_cleanup-graceful")]
pub mod graceful;

use std::{cell, error, marker, sync::atomic};

#[derive(Debug, PartialEq, Eq)]
pub enum Phase {
    Setup,
    Read,
    Cleanup,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PhasedErrorKind {
    CannotCallUnlessPhaseRead(String),
    CannotCallOnPhaseSetup(String),
    CannotCallOnPhaseRead(String),
    InternalDataUnavailable,
    PhaseIsAlreadyRead,
    PhaseIsAlreadyCleanup,
    DuringTransitionToRead,
    DuringTransitionToCleanup,
    FailToRunClosureDuringTransitionToRead,
    FailToRunClosureDuringTransitionToCleanup,
    StdMutexIsPoisoned,

    #[cfg(feature = "setup_read_cleanup-graceful")]
    GracefulWaitTimeout(std::time::Duration),
}

pub struct PhasedError {
    pub phase: Phase,
    pub kind: PhasedErrorKind,
    source: Option<Box<dyn error::Error + Send + Sync>>,
}

pub struct PhasedCell<T: Send + Sync> {
    phase: atomic::AtomicU8,
    data_cell: cell::UnsafeCell<T>,
    _marker: marker::PhantomData<T>,
}

pub struct PhasedCellSync<T: Send + Sync> {
    phase: atomic::AtomicU8,
    data_mutex: std::sync::Mutex<Option<T>>,
    data_cell: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}

pub struct StdMutexGuard<'mutex, T> {
    inner: std::sync::MutexGuard<'mutex, Option<T>>,
}

#[cfg(feature = "setup_read_cleanup-on-tokio")]
pub struct PhasedCellAsync<T: Send + Sync> {
    phase: atomic::AtomicU8,
    data_mutex: tokio::sync::Mutex<Option<T>>,
    data_cell: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}

#[cfg(feature = "setup_read_cleanup-on-tokio")]
pub struct TokioMutexGuard<'mutex, T> {
    inner: tokio::sync::MutexGuard<'mutex, Option<T>>,
}
