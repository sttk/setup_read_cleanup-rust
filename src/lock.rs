// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::locking::{Cleanup, Setup};
use crate::lockings::*;
use crate::phase::*;
use crate::{Phase, PhasedError, PhasedErrorKind, PhasedLock};

use std::{any, cell, marker, sync::atomic};

unsafe impl<T: Send + Sync> Sync for PhasedLock<T> {}
unsafe impl<T: Send + Sync> Send for PhasedLock<T> {}

impl<T: Send + Sync> PhasedLock<T> {
    pub const fn new(data: T, locking_of_setup: Setup, locking_of_cleanup: Cleanup) -> Self {
        let mut lockings = 0u8;
        let mut data_fixed: Option<T> = None;
        #[cfg(feature = "setup_read_cleanup-blocking")]
        let mut data_std: Option<T> = None;
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        let mut data_tokio: Option<T> = None;

        lockings = match locking_of_setup {
            Setup::Unlock => {
                data_fixed = Some(data);
                set_locking_of_setup_to_u8(lockings, LOCKING_SETUP_UNLOCK)
            }
            #[cfg(feature = "setup_read_cleanup-blocking")]
            Setup::Blocking => {
                data_std = Some(data);
                set_locking_of_setup_to_u8(lockings, LOCKING_SETUP_BLOCKING)
            }
            #[cfg(feature = "setup_read_cleanup-on-tokio")]
            Setup::NonBlocking => {
                data_tokio = Some(data);
                set_locking_of_setup_to_u8(lockings, LOCKING_SETUP_NON_BLOCKING)
            }
        };

        lockings = match locking_of_cleanup {
            Cleanup::Unlock => set_locking_of_cleanup_to_u8(lockings, LOCKING_CLEANUP_UNLOCK),
            #[cfg(feature = "setup_read_cleanup-blocking")]
            Cleanup::Blocking => set_locking_of_cleanup_to_u8(lockings, LOCKING_CLEANUP_BLOCKING),
            #[cfg(feature = "setup_read_cleanup-on-tokio")]
            Cleanup::NonBlocking => {
                set_locking_of_cleanup_to_u8(lockings, LOCKING_CLEANUP_NON_BLOCKING)
            }
        };

        Self {
            lockings: lockings,
            phase: atomic::AtomicU8::new(PHASE_SETUP),

            read_count: atomic::AtomicUsize::new(0),
            finish_reading_mutex: std::sync::Mutex::new(false),

            data_fixed: cell::UnsafeCell::new(data_fixed),
            _marker: marker::PhantomData,

            #[cfg(feature = "setup_read_cleanup-blocking")]
            wait_std_condvar: std::sync::Condvar::new(),
            #[cfg(feature = "setup_read_cleanup-blocking")]
            data_std_mutex: std::sync::Mutex::new(data_std),

            #[cfg(feature = "setup_read_cleanup-on-tokio")]
            wait_tokio_notify: tokio::sync::Notify::const_new(),
            #[cfg(feature = "setup_read_cleanup-on-tokio")]
            data_tokio_mutex: tokio::sync::Mutex::const_new(data_tokio),
        }
    }

    pub fn phase_fast(&self) -> Phase {
        let phase = self.phase.load(atomic::Ordering::Relaxed);
        u8_to_phase(phase)
    }

    pub fn phase_exact(&self) -> Phase {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        u8_to_phase(phase)
    }

    pub fn read_fast(&self) -> Result<&T, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Relaxed);
        if phase == PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallOutOfReadPhase(Self::method_name("read_fast")),
            ));
        }

        if let Some(data) = unsafe { &*self.data_fixed.get() }.as_ref() {
            return Ok(data);
        }

        Err(PhasedError::new(
            u8_to_phase(phase),
            PhasedErrorKind::InternalDataIsEmpty,
        ))
    }

    pub fn read_gracefully(&self) -> Result<&T, PhasedError> {
        let result = self.read_count.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |c| {
                let phase = self.phase.load(atomic::Ordering::Acquire);
                if phase != PHASE_READ {
                    return None;
                }
                Some(c + 1)
            },
        );

        if result.is_err() {
            let phase = self.phase.load(atomic::Ordering::Acquire);
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallOutOfReadPhase(Self::method_name("read_gracefully")),
            ));
        }

        if let Some(data) = unsafe { &*self.data_fixed.get() }.as_ref() {
            return Ok(data);
        }

        let phase = self.phase.load(atomic::Ordering::Acquire);
        Err(PhasedError::new(
            u8_to_phase(phase),
            PhasedErrorKind::InternalDataIsEmpty,
        ))
    }

    pub fn finish_reading_gracefully(&self) -> Result<(), PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);

        match self.read_count.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |c| {
                if c == 0 {
                    return None;
                }
                Some(c - 1)
            },
        ) {
            Ok(1) => {
                if let Ok(mut flag) = self.finish_reading_mutex.lock() {
                    let phase = self.phase.load(atomic::Ordering::Acquire);
                    if phase == PHASE_READ_TO_CLEANUP {
                        if !*flag {
                            *flag = true;
                            match u8_to_locking_of_cleanup(self.lockings) {
                                #[cfg(feature = "setup_read_cleanup-blocking")]
                                Cleanup::Blocking => {
                                    self.wait_std_condvar.notify_one();
                                }
                                #[cfg(feature = "setup_read_cleanup-on-tokio")]
                                Cleanup::NonBlocking => {
                                    self.wait_tokio_notify.notify_one();
                                }
                                _ => {}
                            }
                        }
                    }
                } else {
                    match u8_to_locking_of_cleanup(self.lockings) {
                        #[cfg(feature = "setup_read_cleanup-blocking")]
                        Cleanup::Blocking => {
                            self.wait_std_condvar.notify_one();
                        }
                        #[cfg(feature = "setup_read_cleanup-on-tokio")]
                        Cleanup::NonBlocking => {
                            self.wait_tokio_notify.notify_one();
                        }
                        _ => {}
                    }
                }
            }
            Ok(_) => {}
            Err(_) => {
                eprintln!(
                    "{}::finish_reading_gracefully is called excessively.",
                    any::type_name::<PhasedLock<T>>(),
                );
            }
        }

        Ok(())
    }

    #[inline]
    fn method_name(m: &str) -> String {
        format!("{}::{}", any::type_name::<PhasedLock<T>>(), m)
    }
}
