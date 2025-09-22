// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::locking::{Cleanup, Setup};
use crate::lockings::*;
use crate::phase::*;
use crate::{Phase, PhasedError, PhasedErrorKind, PhasedLock, WaitStrategy};

use std::{any, cell, error, marker, sync::atomic};

unsafe impl<T: Send + Sync> Sync for PhasedLock<T> {}
unsafe impl<T: Send + Sync> Send for PhasedLock<T> {}

impl<T: Send + Sync> PhasedLock<T> {
    pub const fn new(data: T, locking_of_setup: Setup, locking_of_cleanup: Cleanup) -> Self {
        let mut lockings = 0u8;
        let mut data_fixed: Option<T> = None;

        lockings = match locking_of_setup {
            Setup::Unlock => {
                data_fixed = Some(data);
                set_locking_of_setup_to_u8(lockings, LOCKING_SETUP_UNLOCK)
            }
        };

        lockings = match locking_of_cleanup {
            Cleanup::Unlock => set_locking_of_cleanup_to_u8(lockings, LOCKING_CLEANUP_UNLOCK),
        };

        Self {
            lockings: lockings,
            phase: atomic::AtomicU8::new(PHASE_SETUP),

            graceful_read_count: atomic::AtomicUsize::new(0),
            graceful_std_mutex: std::sync::Mutex::new(false),
            graceful_std_cvar: std::sync::Condvar::new(),

            data_fixed: cell::UnsafeCell::new(data_fixed),
            _marker: marker::PhantomData,
        }
    }

    #[inline]
    fn method_name(m: &str) -> String {
        format!("{}::{}", any::type_name::<PhasedLock<T>>(), m)
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
        if phase != PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseRead(Self::method_name("read_fast")),
            ));
        }

        if let Some(data) = unsafe { &*self.data_fixed.get() }.as_ref() {
            Ok(data)
        } else {
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataUnavailable,
            ))
        }
    }

    pub fn read_gracefully(&self) -> Result<&T, PhasedError> {
        let mut phase: u8 = PHASE_SETUP;
        let result = self.graceful_read_count.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |count| {
                phase = self.phase.load(atomic::Ordering::Acquire);
                if phase != PHASE_READ {
                    return None;
                }
                Some(count + 1)
            },
        );

        if result.is_err() {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseRead(Self::method_name("read_gracefully")),
            ));
        }

        if let Some(data) = unsafe { &*self.data_fixed.get() }.as_ref() {
            Ok(data)
        } else {
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataUnavailable,
            ))
        }
    }

    pub fn finish_reading_gracefully(&self) -> Result<(), PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase == PHASE_SETUP || phase == PHASE_SETUP_TO_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallOnPhaseSetup(Self::method_name(
                    "finish_reading_gracefully",
                )),
            ));
        }

        match self.graceful_read_count.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |count| {
                if count == 0 {
                    None
                } else {
                    Some(count - 1)
                }
            },
        ) {
            Ok(1) => {
                if let Ok(mut flag) = self.graceful_std_mutex.lock() {
                    let phase = self.phase.load(atomic::Ordering::Acquire);
                    if phase == PHASE_READ_TO_CLEANUP {
                        *flag = true;
                        self.graceful_std_cvar.notify_one();
                    }
                    Ok(())
                } else {
                    Err(PhasedError::new(
                        u8_to_phase(phase),
                        PhasedErrorKind::StdMutexIsPoisoned,
                    ))
                }
            }
            Ok(_) => Ok(()),
            Err(_) => {
                eprintln!(
                    "{} is called excessively.",
                    Self::method_name("finish_read_gracefully")
                );
                Ok(())
            }
        }
    }

    fn transition_to_cleanup_gracefully(
        &self,
        timeout: std::time::Duration,
    ) -> Result<(), PhasedError> {
        if let Ok(mut flag) = self.graceful_std_mutex.lock() {
            match self.phase.fetch_update(
                atomic::Ordering::AcqRel,
                atomic::Ordering::Acquire,
                |current_phase| match current_phase {
                    PHASE_SETUP => Some(PHASE_SETUP_TO_CLEANUP),
                    PHASE_READ => Some(PHASE_READ_TO_CLEANUP),
                    _ => None,
                },
            ) {
                Ok(PHASE_SETUP) => {
                    let _result = self.phase.compare_exchange(
                        PHASE_SETUP_TO_CLEANUP,
                        PHASE_CLEANUP,
                        atomic::Ordering::AcqRel,
                        atomic::Ordering::Acquire,
                    );
                    Ok(())
                }
                Ok(PHASE_READ) => {
                    let mut is_timeout = false;

                    let count = self.graceful_read_count.load(atomic::Ordering::Acquire);
                    if count > 0 {
                        let start = std::time::Instant::now();
                        while !*flag {
                            let elapsed = start.elapsed();
                            if elapsed >= timeout {
                                is_timeout = true;
                                break;
                            }
                            let f = match self
                                .graceful_std_cvar
                                .wait_timeout(flag, timeout - elapsed)
                            {
                                Ok(f) => f,
                                Err(_) => {
                                    return Err(PhasedError::new(
                                        u8_to_phase(PHASE_READ_TO_CLEANUP),
                                        PhasedErrorKind::StdMutexIsPoisoned,
                                    ));
                                }
                            };
                            flag = f.0;

                            if f.1.timed_out() {
                                is_timeout = true;
                                break;
                            }
                        }
                    }

                    let _result = self.phase.compare_exchange(
                        PHASE_READ_TO_CLEANUP,
                        PHASE_CLEANUP,
                        atomic::Ordering::AcqRel,
                        atomic::Ordering::Acquire,
                    );

                    if is_timeout && self.graceful_read_count.load(atomic::Ordering::Acquire) > 0 {
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
                Ok(_) => Ok(()), // impossible case
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
        } else {
            let phase = self.phase.load(atomic::Ordering::Acquire);
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::StdMutexIsPoisoned,
            ))
        }
    }

    fn transition_to_cleanup_simply(&self) -> Result<(), PhasedError> {
        match self.phase.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |current_phase| match current_phase {
                PHASE_SETUP => Some(PHASE_SETUP_TO_CLEANUP),
                PHASE_READ => Some(PHASE_READ_TO_CLEANUP),
                _ => None,
            },
        ) {
            Ok(PHASE_SETUP) => {
                let _result = self.phase.compare_exchange(
                    PHASE_READ_TO_CLEANUP,
                    PHASE_CLEANUP,
                    atomic::Ordering::AcqRel,
                    atomic::Ordering::Acquire,
                );
                Ok(())
            }
            Ok(PHASE_READ) => {
                let _result = self.phase.compare_exchange(
                    PHASE_READ_TO_CLEANUP,
                    PHASE_CLEANUP,
                    atomic::Ordering::AcqRel,
                    atomic::Ordering::Acquire,
                );
                Ok(())
            }
            Ok(_) => Ok(()), // impossible case
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

    pub fn transition_to_cleanup(&self, strategy: WaitStrategy) -> Result<(), PhasedError> {
        match strategy {
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

    pub fn transition_to_read<F, E>(&self, mut f: F) -> Result<(), PhasedError>
    where
        F: FnMut(&mut T) -> Result<(), E>,
        E: error::Error + Send + Sync + 'static,
    {
        match self.phase.compare_exchange(
            PHASE_SETUP,
            PHASE_SETUP_TO_READ,
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
        ) {
            Ok(old_phase_cd) => {
                let data_opt = unsafe { &mut *self.data_fixed.get() };
                if data_opt.is_some() {
                    if let Err(e) = f(data_opt.as_mut().unwrap()) {
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
                } else {
                    return Err(PhasedError::new(
                        u8_to_phase(PHASE_SETUP_TO_READ),
                        PhasedErrorKind::InternalDataUnavailable,
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
            Err(PHASE_READ) => Err(PhasedError::new(
                u8_to_phase(PHASE_READ),
                PhasedErrorKind::PhaseIsAlreadyRead,
            )),
            Err(PHASE_SETUP_TO_READ) => Err(PhasedError::new(
                u8_to_phase(PHASE_SETUP_TO_READ),
                PhasedErrorKind::DuringTransitionToRead,
            )),
            Err(old_phase_u8) => Err(PhasedError::new(
                u8_to_phase(old_phase_u8),
                PhasedErrorKind::TransitionToReadFailed,
            )),
        }
    }

    pub fn get_mut_for_setup(&self) -> Result<&mut T, PhasedError> {
        if u8_to_locking_of_setup(self.lockings) != Setup::Unlock {
            let phase = self.phase.load(atomic::Ordering::Acquire);
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessSetupUnlock(Self::method_name(
                    "get_mut_for_setup",
                )),
            ));
        }

        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase != PHASE_SETUP {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseSetup(Self::method_name("get_mut_for_setup")),
            ));
        }

        let data_opt = unsafe { &mut *self.data_fixed.get() };
        if data_opt.is_some() {
            Ok(data_opt.as_mut().unwrap())
        } else {
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataUnavailable,
            ))
        }
    }

    pub fn get_mut_for_cleanup(&self) -> Result<&mut T, PhasedError> {
        if u8_to_locking_of_cleanup(self.lockings) != Cleanup::Unlock {
            let phase = self.phase.load(atomic::Ordering::Acquire);
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessCleanupUnlock(Self::method_name(
                    "get_mut_for_cleanup",
                )),
            ));
        }

        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase != PHASE_CLEANUP {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseCleanup(Self::method_name(
                    "get_mut_for_cleanup",
                )),
            ));
        }

        let data_opt = unsafe { &mut *self.data_fixed.get() };
        if data_opt.is_some() {
            Ok(data_opt.as_mut().unwrap())
        } else {
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataUnavailable,
            ))
        }
    }
}

#[cfg(test)]
mod setup_unlock_and_cleanup_unlock_test;
