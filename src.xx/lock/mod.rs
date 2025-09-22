// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

mod blocking;

#[cfg(feature = "setup_read_cleanup-on-tokio")]
mod non_blocking;

use crate::flags::*;
use crate::{
    locking::{Cleanup, Read, Setup},
    PhasedLock,
    PhasedErrorKind,
    PhasedError,
    Phase,
};

use std::{any, cell, marker, sync::atomic};

unsafe impl<T: Send + Sync> Sync for PhasedLock<T> {}
unsafe impl<T: Send + Sync> Send for PhasedLock<T> {}

impl<T: Send + Sync> PhasedLock<T> {
    pub const fn new(
        data: T,
        locking_of_setup: Setup,
        locking_of_read: Read,
        locking_of_cleanup: Cleanup,
    ) -> Self {
        let flags: u8 = PHASE_SETUP;

        let data_fixed: Option<T>;
        let data_std: Option<T>;
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        let data_tokio: Option<T>;

        match locking_of_setup {
            Setup::Unlock => {
                set_locking_of_setup_to_u8(flags, LOCKING_SETUP_UNLOCK);
                data_fixed = Some(data);
                data_std = None;
                #[cfg(feature = "setup_read_cleanup-on-tokio")]
                {
                    data_tokio = None;
                }
            }
            Setup::Blocking => {
                set_locking_of_setup_to_u8(flags, LOCKING_SETUP_BLOCKING);
                data_fixed = None;
                data_std = Some(data);
                #[cfg(feature = "setup_read_cleanup-on-tokio")]
                {
                    data_tokio = None;
                }
            }
            #[cfg(feature = "setup_read_cleanup-on-tokio")]
            Setup::NonBlocking => {
                set_locking_of_setup_to_u8(flags, LOCKING_SETUP_NON_BLOCKING);
                data_fixed = None;
                data_std = None;
                #[cfg(feature = "setup_read_cleanup-on-tokio")]
                {
                    data_tokio = Some(data);
                }
            }
        };

        let locking = match locking_of_read {
            Read::Fast => LOCKING_READ_FAST,
            Read::Graceful => LOCKING_READ_GRACEFUL,
        };
        set_locking_of_read_to_u8(flags, locking);

        let locking = match locking_of_cleanup {
            Cleanup::Unlock => LOCKING_CLEANUP_UNLOCK,
            Cleanup::Blocking => LOCKING_CLEANUP_BLOCKING,
            #[cfg(feature = "setup_read_cleanup-on-tokio")]
            Cleanup::NonBlocking => LOCKING_CLEANUP_NON_BLOCKING,
        };
        set_locking_of_cleanup_to_u8(flags, locking);

        Self {
            flags: atomic::AtomicU8::new(flags),
            read_count: atomic::AtomicUsize::new(0),

            data_fixed: cell::UnsafeCell::new(data_fixed),
            _marker: marker::PhantomData,

            wait_std_condvar: std::sync::Condvar::new(),
            data_std_mutex: std::sync::Mutex::new(data_std),

            #[cfg(feature = "setup_read_cleanup-on-tokio")]
            wait_tokio_notify: tokio::sync::Notify::const_new(),
            #[cfg(feature = "setup_read_cleanup-on-tokio")]
            data_tokio_mutex: tokio::sync::Mutex::const_new(data_tokio),
        }
    }

    pub fn phase(&self) -> Phase {
        let flags = self.flags.load(atomic::Ordering::Relaxed);
        let locking = flags && U8_TO_LOCKING_READ;

        u8_to_phase(flags)
    }

    pub fn phase_exact(&self) -> Phase {
        let locking = flags && U8_TO_LOCKING_READ;
        if locking == LOCKING_READ_GRACEFUL {
            let flags = self.flags.load(atomic::Ordering::Acquire);
            u8_to_phase(flags)
        } else {
        }
    }

    pub fn read_fast(&self) -> Result<&T, PhasedError> {
        let flags = self.flags.load(Relaxed);
        let phase = flags & U8_TO_PHASE;
        if phase != PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallOutOfReadPhase(Self::method_name("read_fast")),
            ));
        }

        let locking = flags && U8_TO_LOCKING_READ;
        if locking == LOCKING_READ_GRACEFUL {
            self.read_count.fetch_add(1, Ordering::AcqRel);
        }

        if let Some(data) = unsafe { &*self.data_fixed.get() }.as_ref() {
            return Ok(data);
        }

        Err(PhasedError::new(
            u8_to_phase(phase),
            PhasedErrorKind::InternalDataIsEmpty,
        ))
    }

    pub fn read_exact(&self) -> Result<&T, PhasedError> {
        match flags && U8_TO_LOCKING_READ  {
          LOCKING_READ_NORMAL => self.read_normally(atomic::Ordering::Acquire),
          LOCKING_READ_GRACEFUL => self.read_gracefully(atomic::Ordering::Acquire),
        }
    }

    fn fn read_normally(&self, ordering: atomic::Ordering) => Result<&T, ParsedError> {

    }

    fn fn read_gracefully(&self, ordering: atomic::Ordering) => Result<&T, ParsedError> {
        match
        let result = self.read_count.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |c| {
                let phase = self.phase.load(ordering);
                if phase != PHASE_READ {
                    return None;
                }
                Some(c + 1)
            },
        );
    }

    /*
    pub fn read_gracefully(&self) -> Result<&T, PhasedError> {
    }
    */

    #[inline]
    fn method_name(m: &str) -> String {
        format!("{}::{}", any::type_name::<PhasedLock<T>>(), m)
    }
}
