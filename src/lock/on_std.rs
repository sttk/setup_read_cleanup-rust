// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::phase::{
    u8_to_phase, PHASE_CLEANUP, PHASE_READ, PHASE_READ_TO_CLEANUP, PHASE_SETUP, PHASE_SETUP_TO_READ,
};
use crate::{PhasedError, PhasedErrorKind, PhasedLock, PhasedStdMutexGuard, WaitStrategy};

use std::ops::{Deref, DerefMut};
use std::{cell, error, marker, sync::atomic, time};

macro_rules! cannot_call_on_tokio_runtime {
    ( $plock:ident, $method:literal ) => {
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        {
            if tokio::runtime::Handle::try_current().is_ok() {
                return Err(PhasedError::new(
                    u8_to_phase($plock.phase.load(std::sync::atomic::Ordering::Acquire)),
                    PhasedErrorKind::CannotCallOnTokioRuntime(Self::method_name($method)),
                ));
            }
        }
    };
}

impl<T: Send + Sync> PhasedLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            read_count: atomic::AtomicUsize::new(0),

            wait_std_cvar: std::sync::Condvar::new(),
            data_std_mutex: std::sync::Mutex::new(Some(data)),

            #[cfg(feature = "setup_read_cleanup-on-tokio")]
            wait_tokio_notify: tokio::sync::Notify::const_new(),
            #[cfg(feature = "setup_read_cleanup-on-tokio")]
            data_tokio_mutex: tokio::sync::Mutex::const_new(None),

            data_fixed: cell::UnsafeCell::new(None),
            _marker: marker::PhantomData,
        }
    }

    pub fn transition_to_read<F, E>(&self, mut f: F) -> Result<(), PhasedError>
    where
        F: FnMut(&mut T) -> Result<(), E>,
        E: error::Error + Send + Sync + 'static,
    {
        cannot_call_on_tokio_runtime!(self, "transition_to_read");

        match self.phase.compare_exchange(
            PHASE_SETUP,
            PHASE_SETUP_TO_READ,
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
        ) {
            Ok(old_phase_cd) => match self.data_std_mutex.lock() {
                Ok(mut data_opt) => {
                    if data_opt.is_some() {
                        if let Err(e) = f(data_opt.as_mut().unwrap()) {
                            // rollback phase transition
                            let _result = self.phase.compare_exchange(
                                PHASE_SETUP_TO_READ,
                                old_phase_cd,
                                atomic::Ordering::AcqRel,
                                atomic::Ordering::Acquire,
                            );

                            return Err(PhasedError::with_source(
                                u8_to_phase(PHASE_SETUP_TO_READ),
                                PhasedErrorKind::FailToRunClosureDuringTransitionToRead,
                                e,
                            ));
                        }

                        unsafe {
                            core::ptr::swap(self.data_fixed.get(), &mut *data_opt);
                        }
                    } else {
                        // rollback phase transition
                        let _result = self.phase.compare_exchange(
                            PHASE_SETUP_TO_READ,
                            old_phase_cd,
                            atomic::Ordering::AcqRel,
                            atomic::Ordering::Acquire,
                        );

                        return Err(PhasedError::new(
                            u8_to_phase(PHASE_SETUP_TO_READ),
                            PhasedErrorKind::InternalDataIsEmpty,
                        ));
                    }

                    let _result = self.phase.compare_exchange(
                        PHASE_SETUP_TO_READ,
                        PHASE_READ,
                        atomic::Ordering::AcqRel,
                        atomic::Ordering::Acquire,
                    );

                    Ok(())
                }
                Err(_e) => {
                    // rollback phase transition
                    let _result = self.phase.compare_exchange(
                        PHASE_SETUP_TO_READ,
                        old_phase_cd,
                        atomic::Ordering::AcqRel,
                        atomic::Ordering::Acquire,
                    );

                    Err(PhasedError::new(
                        u8_to_phase(old_phase_cd),
                        PhasedErrorKind::MutexIsPoisoned,
                    ))
                }
            },
            Err(PHASE_READ) => Err(PhasedError::new(
                u8_to_phase(PHASE_READ),
                PhasedErrorKind::PhaseIsAlreadyRead,
            )),
            Err(PHASE_SETUP_TO_READ) => Err(PhasedError::new(
                u8_to_phase(PHASE_SETUP_TO_READ),
                PhasedErrorKind::DuringTransitionToRead,
            )),
            Err(old_phase_cd) => Err(PhasedError::new(
                u8_to_phase(old_phase_cd),
                PhasedErrorKind::TransitionToReadFailed,
            )),
        }
    }

    pub fn transition_to_cleanup(&self, wait_strategy: WaitStrategy) -> Result<(), PhasedError> {
        cannot_call_on_tokio_runtime!(self, "transition_to_cleanup");

        match wait_strategy {
            WaitStrategy::NoWait => self.transition_to_cleanup_simply(),
            WaitStrategy::FixedWait(tm) => {
                std::thread::sleep(tm);
                self.transition_to_cleanup_simply()
            }
            WaitStrategy::GracefulWait { timeout } => {
                self.transition_to_cleanup_gracefully(timeout)
            }
        }
    }

    fn transition_to_cleanup_simply(&self) -> Result<(), PhasedError> {
        match self.phase.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |current_phase| match current_phase {
                PHASE_SETUP => Some(PHASE_CLEANUP),
                PHASE_READ => Some(PHASE_READ_TO_CLEANUP),
                _ => None,
            },
        ) {
            Ok(PHASE_READ) => match self.data_std_mutex.lock() {
                Ok(mut data_opt) => {
                    if data_opt.is_none() {
                        unsafe {
                            core::ptr::swap(self.data_fixed.get(), &mut *data_opt);
                        }
                    }

                    let _result = self.phase.compare_exchange(
                        PHASE_READ_TO_CLEANUP,
                        PHASE_CLEANUP,
                        atomic::Ordering::AcqRel,
                        atomic::Ordering::Acquire,
                    );

                    Ok(())
                }
                Err(_e) => {
                    // rollback phase transition
                    let _result = self.phase.compare_exchange(
                        PHASE_READ_TO_CLEANUP,
                        PHASE_READ,
                        atomic::Ordering::AcqRel,
                        atomic::Ordering::Acquire,
                    );

                    Err(PhasedError::new(
                        u8_to_phase(PHASE_READ_TO_CLEANUP),
                        PhasedErrorKind::MutexIsPoisoned,
                    ))
                }
            },
            Ok(_) => Ok(()), // PHASE_SETUP -> PHASE_CLEANUP
            Err(PHASE_READ_TO_CLEANUP) => Err(PhasedError::new(
                u8_to_phase(PHASE_READ_TO_CLEANUP),
                PhasedErrorKind::DuringTransitionToCleanup,
            )),
            Err(PHASE_CLEANUP) => Err(PhasedError::new(
                u8_to_phase(PHASE_CLEANUP),
                PhasedErrorKind::PhaseIsAlreadyCleanup,
            )),
            Err(old_phase_cd) => Err(PhasedError::new(
                u8_to_phase(old_phase_cd),
                PhasedErrorKind::TransitionToCleanupFailed,
            )),
        }
    }

    fn transition_to_cleanup_gracefully(&self, timeout: time::Duration) -> Result<(), PhasedError> {
        match self.data_std_mutex.lock() {
            Ok(mut data_opt) => {
                match self.phase.fetch_update(
                    atomic::Ordering::AcqRel,
                    atomic::Ordering::Acquire,
                    |current_phase| match current_phase {
                        PHASE_SETUP => Some(PHASE_CLEANUP),
                        PHASE_READ => Some(PHASE_READ_TO_CLEANUP),
                        _ => None,
                    },
                ) {
                    Ok(PHASE_READ) => {
                        let mut is_timeout = false;

                        let start = time::Instant::now();
                        while self.read_count.load(atomic::Ordering::Acquire) > 0 {
                            let elapsed = start.elapsed();
                            if elapsed >= timeout {
                                is_timeout = true;
                                break;
                            }

                            if let Ok(result) =
                                self.wait_std_cvar.wait_timeout(data_opt, timeout - elapsed)
                            {
                                data_opt = result.0;

                                if result.1.timed_out() {
                                    is_timeout = true;
                                    break;
                                }
                            } else {
                                return Err(PhasedError::new(
                                    u8_to_phase(PHASE_READ_TO_CLEANUP),
                                    PhasedErrorKind::MutexIsPoisoned,
                                ));
                            }
                        }

                        if data_opt.is_none() {
                            unsafe {
                                core::ptr::swap(self.data_fixed.get(), &mut *data_opt);
                            }
                        }

                        let _result = self.phase.compare_exchange(
                            PHASE_READ_TO_CLEANUP,
                            PHASE_CLEANUP,
                            atomic::Ordering::AcqRel,
                            atomic::Ordering::Acquire,
                        );

                        if is_timeout && self.read_count.load(atomic::Ordering::Acquire) > 0 {
                            Err(PhasedError::new(
                                u8_to_phase(PHASE_READ_TO_CLEANUP),
                                PhasedErrorKind::TransitionToCleanupTimeout(
                                    WaitStrategy::GracefulWait { timeout },
                                ),
                            ))
                        } else {
                            Ok(())
                        }
                    }
                    Ok(_) => Ok(()), // PHASE_SETUP -> PHASE_CLEANUP
                    Err(PHASE_READ_TO_CLEANUP) => Err(PhasedError::new(
                        u8_to_phase(PHASE_READ_TO_CLEANUP),
                        PhasedErrorKind::DuringTransitionToCleanup,
                    )),
                    Err(PHASE_CLEANUP) => Err(PhasedError::new(
                        u8_to_phase(PHASE_CLEANUP),
                        PhasedErrorKind::PhaseIsAlreadyCleanup,
                    )),
                    Err(old_phase_cd) => Err(PhasedError::new(
                        u8_to_phase(old_phase_cd),
                        PhasedErrorKind::TransitionToCleanupFailed,
                    )),
                }
            }
            Err(_e) => Err(PhasedError::new(
                u8_to_phase(self.phase.load(atomic::Ordering::Acquire)),
                PhasedErrorKind::MutexIsPoisoned,
            )),
        }
    }
}

impl<'mutex, T> PhasedStdMutexGuard<'mutex, T> {
    pub fn try_new(guarded_option: std::sync::MutexGuard<'mutex, Option<T>>) -> Option<Self> {
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
        // unwrap() here is always safe because it's guaranteed to be Some in the constructor.
        self.inner.as_ref().unwrap()
    }
}

impl<'mutex, T> DerefMut for PhasedStdMutexGuard<'mutex, T> {
    fn deref_mut(&mut self) -> &mut T {
        // unwrap() here is always safe because it's guaranteed to be Some in the constructor.
        self.inner.as_mut().unwrap()
    }
}
