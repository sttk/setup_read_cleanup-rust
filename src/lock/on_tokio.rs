// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::phase::{
    u8_to_phase, PHASE_CLEANUP, PHASE_READ, PHASE_READ_TO_CLEANUP, PHASE_SETUP, PHASE_SETUP_TO_READ,
};
use crate::{PhasedError, PhasedErrorKind, PhasedLock, PhasedTokioMutexGuard, WaitStrategy};

use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::{any, cell, error, marker, sync::atomic};

impl<T: Send + Sync> PhasedLock<T> {
    pub const fn new_for_async(data: T) -> Self {
        Self {
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            read_count: atomic::AtomicUsize::new(0),

            #[cfg(feature = "setup_read_cleanup-on-std")]
            wait_std_cvar: std::sync::Condvar::new(),
            #[cfg(feature = "setup_read_cleanup-on-std")]
            data_std_mutex: std::sync::Mutex::new(None),

            wait_tokio_notify: tokio::sync::Notify::const_new(),
            data_tokio_mutex: tokio::sync::Mutex::const_new(Some(data)),

            data_fixed: cell::UnsafeCell::new(None),
            _marker: marker::PhantomData,
        }
    }

    pub async fn transition_to_read_async<F, E>(&self, mut f: F) -> Result<(), PhasedError>
    where
        F: FnMut(&mut T) -> Pin<Box<dyn Future<Output = Result<(), E>>>>,
        E: error::Error + Send + Sync + 'static,
    {
        match self.phase.compare_exchange(
            PHASE_SETUP,
            PHASE_SETUP_TO_READ,
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
        ) {
            Ok(old_phase_cd) => {
                let mut data_opt = self.data_tokio_mutex.lock().await;
                if data_opt.is_some() {
                    if let Err(e) = f(data_opt.as_mut().unwrap()).await {
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
                u8_to_phase(PHASE_READ),
                PhasedErrorKind::DuringTransitionToRead,
            )),
            Err(old_phase_cd) => Err(PhasedError::new(
                u8_to_phase(old_phase_cd),
                PhasedErrorKind::TransitionToReadFailed,
            )),
        }
    }

    pub async fn transition_to_cleanup_async(
        &self,
        wait_strategy: WaitStrategy,
    ) -> Result<(), PhasedError> {
        match wait_strategy {
            WaitStrategy::NoWait => self.transition_to_cleanup_simply_async().await,
            WaitStrategy::FixedWait(tm) => {
                tokio::time::sleep(tm).await;
                self.transition_to_cleanup_simply_async().await
            }
            WaitStrategy::GracefulWait { timeout } => {
                self.transition_to_cleanup_gracefully_async(timeout).await
            }
        }
    }

    async fn transition_to_cleanup_simply_async(&self) -> Result<(), PhasedError> {
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
                let mut data_opt = self.data_tokio_mutex.lock().await;
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

    async fn transition_to_cleanup_gracefully_async(
        &self,
        timeout: std::time::Duration,
    ) -> Result<(), PhasedError> {
        let mut data_opt = self.data_tokio_mutex.lock().await;
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

                let start = tokio::time::Instant::now();
                while self.read_count.load(atomic::Ordering::Acquire) > 0 {
                    let elapsed = start.elapsed();
                    if elapsed >= timeout {
                        is_timeout = true;
                        break;
                    }

                    if tokio::time::timeout(timeout, self.wait_tokio_notify.notified())
                        .await
                        .is_err()
                    {
                        is_timeout = true;
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
                        PhasedErrorKind::TransitionToCleanupTimeout(WaitStrategy::GracefulWait {
                            timeout,
                        }),
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

    pub async fn lock_for_update_async(&self) -> Result<PhasedTokioMutexGuard<'_, T>, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase == PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallInReadPhase(Self::method_name("lock_for_update")),
            ));
        }

        let guarded_opt = self.data_tokio_mutex.lock().await;
        if let Some(new_guard) = PhasedTokioMutexGuard::try_new(guarded_opt) {
            Ok(new_guard)
        } else {
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataIsEmpty,
            ))
        }
    }

    pub async fn finish_reading_gracefully_async(&self) -> Result<(), PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase == PHASE_SETUP || phase == PHASE_SETUP_TO_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallInSetupPhase(Self::method_name(
                    "finish_reading_gracefully",
                )),
            ));
        }

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
                let _guard = self.data_tokio_mutex.lock().await;
                let phase = self.phase.load(atomic::Ordering::Acquire);
                if phase == PHASE_READ_TO_CLEANUP {
                    self.wait_tokio_notify.notify_one();
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
}

impl<'mutex, T> PhasedTokioMutexGuard<'mutex, T> {
    pub fn try_new(guarded_option: tokio::sync::MutexGuard<'mutex, Option<T>>) -> Option<Self> {
        if guarded_option.is_some() {
            Some(Self {
                inner: guarded_option,
            })
        } else {
            None
        }
    }
}

impl<'mutex, T> Deref for PhasedTokioMutexGuard<'mutex, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // unwrap() here is always safe because it's guaranteed to be Some in the constructor.
        self.inner.as_ref().unwrap()
    }
}

impl<'mutex, T> DerefMut for PhasedTokioMutexGuard<'mutex, T> {
    fn deref_mut(&mut self) -> &mut T {
        // unwrap() here is always safe because it's guaranteed to be Some in the constructor.
        self.inner.as_mut().unwrap()
    }
}
