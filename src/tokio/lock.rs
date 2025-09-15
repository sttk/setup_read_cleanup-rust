// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use super::{PhasedLock, PhasedMutexGuard};
use crate::phase::{
    u8_to_phase, PHASE_CLEANUP, PHASE_READ, PHASE_READ_TO_CLEANUP, PHASE_SETUP, PHASE_SETUP_TO_READ,
};
use crate::{Phase, PhasedError, PhasedErrorKind, WaitStrategy};

use std::ops::{Deref, DerefMut};
use std::future::Future;
use std::pin::Pin;
use std::{any, cell, error, marker, sync::atomic};

extern crate tokio;
use tokio::{sync, time};

unsafe impl<T: Send + Sync> Sync for PhasedLock<T> {}
unsafe impl<T: Send + Sync> Send for PhasedLock<T> {}

impl<T: Send + Sync> PhasedLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            read_count: atomic::AtomicUsize::new(0),
            wait_notify: sync::Notify::const_new(),
            data_mutex: sync::Mutex::const_new(Some(data)),
            data_fixed: cell::UnsafeCell::new(None),
            _marker: marker::PhantomData,
        }
    }

    pub async fn transition_to_read<F, E>(&self, mut f: F) -> Result<(), PhasedError>
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
                let mut data_opt = self.data_mutex.lock().await;
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

    pub async fn transition_to_cleanup(
        &self,
        wait_strategy: WaitStrategy,
    ) -> Result<(), PhasedError> {
        match wait_strategy {
            WaitStrategy::NoWait => self.transition_to_cleanup_simply().await,
            WaitStrategy::FixedWait(tm) => {
                time::sleep(tm).await;
                self.transition_to_cleanup_simply().await
            }
            WaitStrategy::GracefulWait { timeout } => {
                self.transition_to_cleanup_gracefully(timeout).await
            }
        }
    }

    async fn transition_to_cleanup_simply(&self) -> Result<(), PhasedError> {
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
                let mut data_opt = self.data_mutex.lock().await;
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

    async fn transition_to_cleanup_gracefully(
        &self,
        timeout: time::Duration,
    ) -> Result<(), PhasedError> {
        let mut data_opt = self.data_mutex.lock().await;
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

                    if time::timeout(timeout, self.wait_notify.notified())
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

    pub fn phase_fast(&self) -> Phase {
        let phase = self.phase.load(atomic::Ordering::Relaxed);
        u8_to_phase(phase)
    }

    pub fn phase_exact(&self) -> Phase {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        u8_to_phase(phase)
    }

    pub async fn lock_for_update(&self) -> Result<PhasedMutexGuard<'_, T>, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase == PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallInReadPhase(Self::method_name("lock_for_update")),
            ));
        }

        let guarded_opt = self.data_mutex.lock().await;
        if let Some(new_guard) = PhasedMutexGuard::try_new(guarded_opt) {
            Ok(new_guard)
        } else {
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataIsEmpty,
            ))
        }
    }

    pub fn read_fast(&self) -> Result<&T, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Relaxed);
        if phase != PHASE_READ {
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

    pub async fn finish_reading_gracefully(&self) -> Result<(), PhasedError> {
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
                let _guard = self.data_mutex.lock().await;
                let phase = self.phase.load(atomic::Ordering::Acquire);
                if phase == PHASE_READ_TO_CLEANUP {
                    self.wait_notify.notify_one();
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

impl<'mutex, T> PhasedMutexGuard<'mutex, T> {
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

impl<'mutex, T> Deref for PhasedMutexGuard<'mutex, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // unwrap() here is always safe because it's guaranteed to be Some in the constructor.
        self.inner.as_ref().unwrap()
    }
}

impl<'mutex, T> DerefMut for PhasedMutexGuard<'mutex, T> {
    fn deref_mut(&mut self) -> &mut T {
        // unwrap() here is always safe because it's guaranteed to be Some in the constructor.
        self.inner.as_mut().unwrap()
    }
}

#[cfg(test)]
mod tests_of_phase_lock {
    use super::*;
    use once_cell::sync::Lazy;
    use std::fmt;

    struct MyStruct {
        vec: Vec<String>,
    }

    impl MyStruct {
        const fn new() -> Self {
            Self { vec: Vec::new() }
        }

        fn add(&mut self, s: String) {
            self.vec.push(s);
        }
    }

    #[derive(Debug)]
    struct MyError {}
    impl fmt::Display for MyError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "MyError")
        }
    }
    impl error::Error for MyError {}

    fn my_func(_data: &mut MyStruct) -> Pin<Box<dyn Future<Output = Result<(), MyError>>>> {
        Box::pin(async { Ok(()) })
    }

    mod tests_of_new {
        use super::*;
        use std::sync::Mutex;

        static LOGGER: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::new()));

        struct MyStruct {}

        impl Drop for MyStruct {
            fn drop(&mut self) {
                LOGGER.lock().unwrap().push("drop MyStruct".to_string());
            }
        }

        #[tokio::test]
        async fn new_const_and_let() {
            {
                const MY_STRUCT: PhasedLock<MyStruct> = PhasedLock::new(MyStruct {});
                assert_eq!(MY_STRUCT.phase_fast(), Phase::Setup); // drop!

                // drop!
                if let Err(e) = MY_STRUCT.transition_to_read(|_data| Box::pin(async {
                    Ok::<(), MyError>(())
                })).await {
                    panic!("{:?}", e);
                }

                // The phase did not transition because my_struct is const.
                assert_eq!(MY_STRUCT.phase_fast(), Phase::Setup); // drop!
            }
            if let Ok(mut vec) = LOGGER.lock() {
                assert_eq!(vec.len(), 3);
                assert_eq!(vec[0], "drop MyStruct");
                assert_eq!(vec[1], "drop MyStruct");
                assert_eq!(vec[2], "drop MyStruct");
                vec.clear();
            }

            {
                let _my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct {});
            }
            if let Ok(mut vec) = LOGGER.lock() {
                assert_eq!(vec.len(), 1);
                assert_eq!(vec[0], "drop MyStruct");
                vec.clear();
            }

            {
                let my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct {});
                assert_eq!(my_struct.phase_fast(), Phase::Setup);

                if let Err(e) = my_struct.transition_to_read(|_data| Box::pin(async {
                    Ok::<(), MyError>(())
                })).await {
                    panic!("{:?}", e);
                }
            }
            if let Ok(mut vec) = LOGGER.lock() {
                assert_eq!(vec.len(), 1);
                assert_eq!(vec[0], "drop MyStruct");
                vec.clear();
            }
        }

        // static variables are not called their drop functions.
        static MY_STRUCT: PhasedLock<MyStruct> = PhasedLock::new(MyStruct {});

        #[test]
        fn new_static() {
            assert_eq!(MY_STRUCT.phase_fast(), Phase::Setup);
            assert_eq!(MY_STRUCT.phase_exact(), Phase::Setup);
        }
    }

    mod tests_of_phase_transition {
        use super::*;

        #[tokio::test]
        async fn ok_setup_read_cleanup() {
            let my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_read(my_func).await {
                panic!("{e:?}");
            }
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait).await {
                panic!("{e:?}");
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }
    }
}
