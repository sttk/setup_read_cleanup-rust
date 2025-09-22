// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::{PhasedLock, PhasedStdMutexGuard};

use std::ops::{Deref, DerefMut};
use std::sync;

impl<'mutex, T> PhasedStdMutexGuard<'mutex, T> {
    pub fn try_new(guarded_option: sync::MutexGuard<'mutex, Option<T>>) -> Option<Self> {
        if guarded_option.is_some() {
            Some(Self {
                inner: guarded_option,
            })
        } else {
            None
        }
    }
}

impl<'mutex, T> Deref for PhasedStdMutexGuard<'mutex, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.inner.as_ref().unwrap()
    }
}

impl<'mutex, T> DerefMut for PhasedStdMutexGuard<'mutex, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.inner.as_mut().unwrap()
    }
}
