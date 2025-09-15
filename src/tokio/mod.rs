// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

mod lock;

use std::{cell, marker, sync::atomic};

extern crate tokio;
use tokio::sync;

pub struct PhasedLock<T: Send + Sync> {
    phase: atomic::AtomicU8,
    read_count: atomic::AtomicUsize,

    wait_notify: sync::Notify,
    data_mutex: sync::Mutex<Option<T>>,

    data_fixed: cell::UnsafeCell<Option<T>>,
    _marker: marker::PhantomData<T>,
}

pub struct PhasedMutexGuard<'mutex, T> {
    inner: sync::MutexGuard<'mutex, Option<T>>,
}
