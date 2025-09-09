// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::phase::{u8_to_phase, PHASE_CLEANUP, PHASE_READ, PHASE_SETUP};
use crate::{Phase, PhasedError, PhasedErrorKind, PhasedLock, PhasedMutexGuard, WaitStrategy};

use std::ops::{Deref, DerefMut};
use std::{cell, marker, mem, sync, sync::atomic};

macro_rules! cannot_call_on_tokio_runtime {
    ( $plock:ident, $method:literal ) => {
        #[cfg(feature = "setup_read_cleanup-on-tokio-rt")]
        {
            if tokio::runtime::Handle::try_current().is_ok() {
                return Err(PhasedError::new(
                    u8_to_phase($plock.phase.load(std::sync::atomic::Ordering::Acquire)),
                    PhasedErrorKind::CannotCallOnTokioRuntime(String::from($method)),
                    &format!("Cannot call {} on tokio runtime.", $method),
                ));
            }
        }
    };
}

unsafe impl<T: Send + Sync> Sync for PhasedLock<T> {}
unsafe impl<T: Send + Sync> Send for PhasedLock<T> {}

impl<T: Send + Sync> PhasedLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            read_count: atomic::AtomicUsize::new(0),
            data_mutex: sync::Mutex::new(Some(data)),
            data_fixed: cell::UnsafeCell::new(None),
            _marker: marker::PhantomData,
        }
    }

    pub fn transition_to_read(&self) -> Result<(), PhasedError> {
        cannot_call_on_tokio_runtime!(self, "PhasedLock::transition_to_read");

        let mut mutex_error: Option<sync::PoisonError<sync::MutexGuard<'_, Option<T>>>> = None;
        let result = self.phase.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |current_phase| {
                if current_phase != PHASE_SETUP {
                    return None;
                }

                match self.data_mutex.lock() {
                    Ok(mut data_opt) => {
                        if data_opt.is_some() {
                            unsafe {
                                mem::swap(&mut *self.data_fixed.get(), &mut *data_opt);
                            }
                        }
                        Some(PHASE_READ)
                    }
                    Err(e) => {
                        mutex_error = Some(e);
                        None
                    }
                }
            },
        );

        if let Err(current_phase) = result {
            if let Some(_) = mutex_error {
                return Err(PhasedError::new(
                    u8_to_phase(current_phase),
                    PhasedErrorKind::MutexIsPoisoned,
                    "Mutex poisoned during the phase transition to Read.",
                ));
            }

            if current_phase == PHASE_CLEANUP {
                return Err(PhasedError::new(
                    Phase::Cleanup,
                    PhasedErrorKind::TransitionToReadFailed,
                    "Failed phase transition to Read because phase is now Cleanup.",
                ));
            }
        }

        Ok(())
    }

    pub fn transition_to_cleanup(&self, wait: WaitStrategy) -> Result<(), PhasedError> {
        cannot_call_on_tokio_runtime!(self, "PhasedLock::transition_to_cleanup");

        let _result = self.phase.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |current_phase| match current_phase {
                PHASE_SETUP | PHASE_READ => Some(PHASE_CLEANUP),
                _ => None,
            },
        );

        // Even if it has already transitioned to the Cleanup phase, there are cases where the
        // Graceful Wait has not been completed or the mem::swap from data_fixed to data_mutex
        // has not been performed.
        // Therefore, they have to be executed even if the phase has not been changed.

        let wait_result = self.wait_gracefully(wait);

        match self.data_mutex.lock() {
            Ok(mut data_opt) => {
                if data_opt.is_none() {
                    unsafe {
                        mem::swap(&mut *self.data_fixed.get(), &mut *data_opt);
                    }
                }
            }
            Err(_e) => {
                return Err(PhasedError::new(
                    Phase::Cleanup,
                    PhasedErrorKind::MutexIsPoisoned,
                    "Mutex was poisoned during the phase transition to Cleanup.",
                ));
            }
        }

        if wait_result.is_err() {
            return Err(PhasedError::new(
                Phase::Cleanup,
                PhasedErrorKind::TransitionToCleanupTimeout(wait),
                "The reading would not complete, but the phase has transitioned to Cleanup due to a timeout.",
            ));
        }

        Ok(())
    }

    pub fn phase_fast(&self) -> Phase {
        let phase = self.phase.load(atomic::Ordering::Relaxed);
        u8_to_phase(phase)
    }

    pub fn phase_exact(&self) -> Phase {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        u8_to_phase(phase)
    }

    pub fn lock_for_update(&self) -> Result<PhasedMutexGuard<'_, T>, PhasedError> {
        cannot_call_on_tokio_runtime!(self, "PhasedLock::lock_for_update");

        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase == PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallInReadPhase("PhasedLock::lock_for_update".to_string()),
                "Cannot call PhaseLock::lock_for_update out of Setup or Cleanup phases",
            ));
        }

        match self.data_mutex.lock() {
            Ok(guarded_opt) => {
                if let Some(new_guard) = PhasedMutexGuard::try_new(guarded_opt) {
                    return Ok(new_guard);
                } else {
                    return Err(PhasedError::new(
                        u8_to_phase(phase),
                        PhasedErrorKind::GracefulPhaseTransitionIsInProgress,
                        "Cannot retrieve internal data for updates because a graceful phase transition is in progress.",
                    ));
                }
            }
            Err(_e) => {
                return Err(PhasedError::new(
                    u8_to_phase(phase),
                    PhasedErrorKind::MutexIsPoisoned,
                    "Failed to retrieve internal data for updates because Mutex was poisoned.",
                ));
            }
        }
    }

    pub fn read_fast(&self) -> Result<&T, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Relaxed);
        if phase != PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallOutOfReadPhase("PhasedLock::read_fast".to_string()),
                "Cannot call PhaseLock::read_fast out of Read phases",
            ));
        }

        if let Some(data) = unsafe { &*self.data_fixed.get() }.as_ref() {
            return Ok(data);
        }

        Err(PhasedError::new(
            u8_to_phase(phase),
            PhasedErrorKind::InternalDataIsEmpty,
            "Failed to retrieve read-only data: internal data is unexpectedly empty.",
        ))
    }

    pub fn read_gracefully(&self) -> Result<&T, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase != PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallOutOfReadPhase(
                    "PhasedLock::read_gracefully".to_string(),
                ),
                "Cannot call PhaseLock::read_gracefully out of Read phases",
            ));
        }

        let _ = self.read_count.fetch_add(1, atomic::Ordering::AcqRel);

        if let Some(data) = unsafe { &*self.data_fixed.get() }.as_ref() {
            return Ok(data);
        }

        Err(PhasedError::new(
            u8_to_phase(phase),
            PhasedErrorKind::InternalDataIsEmpty,
            "Failed to retrieve read-only data: internal data is unexpectedly empty.",
        ))
    }

    pub fn finish_reading_gracefully(&self) -> Result<(), PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase == PHASE_SETUP {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallInSetupPhase(
                    "PhasedLock::finish_reading_gracefully".to_string(),
                ),
                "Cannot call PhaseLock::finish_reading_gracefully in Setup phases",
            ));
        }

        let _ = self.read_count.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |c| {
                if c == 0 {
                    return None;
                }
                Some(c - 1)
            },
        );

        Ok(())
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
    use std::sync::Mutex;

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

    mod tests_of_new {
        use super::*;

        static LOGGER: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::new()));

        struct MyStruct {}

        impl Drop for MyStruct {
            fn drop(&mut self) {
                LOGGER.lock().unwrap().push("drop MyStruct".to_string());
            }
        }

        #[test]
        fn new_const_and_let() {
            {
                const MY_STRUCT: PhasedLock<MyStruct> = PhasedLock::new(MyStruct {});
                assert_eq!(MY_STRUCT.phase_fast(), Phase::Setup); // drop!

                // drop!
                if let Err(e) = MY_STRUCT.transition_to_read() {
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

                if let Err(e) = my_struct.transition_to_read() {
                    panic!("{:?}", e);
                }

                assert_eq!(my_struct.phase_fast(), Phase::Read);
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

    #[cfg(feature = "setup_read_cleanup-on-tokio-rt")]
    mod tests_of_new_on_tokio_rt {
        use super::*;

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
                let my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct {});
                assert_eq!(my_struct.phase_fast(), Phase::Setup);
            }
            if let Ok(mut vec) = LOGGER.lock() {
                assert_eq!(vec.len(), 1);
                assert_eq!(vec[0], "drop MyStruct");
                vec.clear();
            }
        }
    }

    mod tests_of_phase_transition {
        use super::*;

        #[test]
        fn ok_setup_read_cleanup() {
            let my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_read() {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn ok_setup_cleanup() {
            let my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn ok_setup_read_read() {
            let my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_read() {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            if let Err(e) = my_struct.transition_to_read() {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Read);
        }

        #[test]
        fn ok_setup_cleanup_cleanup() {
            let my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn ok_setup_read_read_cleanup_cleanup() {
            let my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_read() {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            if let Err(e) = my_struct.transition_to_read() {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn fail_setup_cleanup_read() {
            let my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            if let Err(e) = my_struct.transition_to_read() {
                assert_eq!(
                    format!("{e:?}"),
                    "setup_read_cleanup::PhasedError { phase: Cleanup, kind: TransitionToReadFailed, message: \"Failed phase transition to Read because phase is now Cleanup.\" }"
                );
            } else {
                panic!();
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }
    }

    #[cfg(feature = "setup_read_cleanup-on-tokio-rt")]
    mod tests_of_phase_transition_on_tokio_rt {
        use super::*;

        #[tokio::test]
        async fn fail_setup_to_read() {
            let my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_read() {
                assert_eq!(
                    format!("{e:?}"),
                    "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallOnTokioRuntime(\"PhasedLock::transition_to_read\"), message: \"Cannot call PhasedLock::transition_to_read on tokio runtime.\" }"
                );
            } else {
                panic!();
            }
            assert_eq!(my_struct.phase_fast(), Phase::Setup);
        }

        #[test]
        fn fail_read_to_cleanup() {
            let my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_read() {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                    assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phase: Read, kind: CannotCallOnTokioRuntime(\"PhasedLock::transition_to_cleanup\"), message: \"Cannot call PhasedLock::transition_to_cleanup on tokio runtime.\" }");
                } else {
                    panic!();
                }
                assert_eq!(my_struct.phase_fast(), Phase::Read);
            });
        }

        #[tokio::test]
        async fn fail_setup_to_cleanup() {
            let my_struct: PhasedLock<MyStruct> = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                assert_eq!(
                    format!("{e:?}"),
                    "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallOnTokioRuntime(\"PhasedLock::transition_to_cleanup\"), message: \"Cannot call PhasedLock::transition_to_cleanup on tokio runtime.\" }"
                );
            } else {
                panic!();
            }
            assert_eq!(my_struct.phase_fast(), Phase::Setup);
        }
    }

    mod tests_in_setup_phase {
        use super::*;

        #[test]
        fn ok_lock_for_update() {
            let my_struct = PhasedLock::new(MyStruct::new());

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("hello".to_string());
                }
                Err(e) => {
                    panic!("{e:?}");
                }
            }

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("world".to_string());
                }
                Err(e) => {
                    panic!("{e:?}");
                }
            }

            match my_struct.lock_for_update() {
                Ok(data) => {
                    assert_eq!(data.vec, vec!["hello".to_string(), "world".to_string(),]);
                },
                Err(e) => {
                    panic!("{e:?}");
                },
            }

            // Before the 2024 edition, adding a semicolon was required to prevent
            // an E0597 error because the compiler's NLL couldn't correctly infer
            // the lifetime of my_struct. In the 2024 edition, this semicolon is
            // no longer needed due to improvements in the NLL logic.
            ;
        }

        #[test]
        fn ok_phase_fast() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);
        }

        #[test]
        fn ok_phase_exact() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_exact(), Phase::Setup);
        }

        #[test]
        fn fail_read_fast() {
            let my_struct = PhasedLock::new(MyStruct::new());

            if let Err(e) = my_struct.read_fast() {
                assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallOutOfReadPhase(\"PhasedLock::read_fast\"), message: \"Cannot call PhaseLock::read_fast out of Read phases\" }");
            } else {
                panic!();
            }
        }

        #[test]
        fn fail_read_gracefully() {
            let my_struct = PhasedLock::new(MyStruct::new());

            if let Err(e) = my_struct.read_gracefully() {
                assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallOutOfReadPhase(\"PhasedLock::read_gracefully\"), message: \"Cannot call PhaseLock::read_gracefully out of Read phases\" }");
            } else {
                panic!();
            }
        }

        #[test]
        fn fail_finish_reading_gracefully() {
            let my_struct = PhasedLock::new(MyStruct::new());

            if let Err(e) = my_struct.finish_reading_gracefully() {
                assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallInSetupPhase(\"PhasedLock::finish_reading_gracefully\"), message: \"Cannot call PhaseLock::finish_reading_gracefully in Setup phases\" }");
            } else {
                panic!();
            }
        }
    }

    #[cfg(feature = "setup_read_cleanup-on-tokio-rt")]
    mod tests_in_setup_phase_on_tokio_rt {
        use super::*;

        #[tokio::test]
        async fn ok_lock_for_update() {
            let my_struct = PhasedLock::new(MyStruct::new());

            if let Err(e) = my_struct.lock_for_update() {
                assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallOnTokioRuntime(\"PhasedLock::lock_for_update\"), message: \"Cannot call PhasedLock::lock_for_update on tokio runtime.\" }");
            } else {
                panic!();
            }

            // Before the 2024 edition, adding a semicolon was required to prevent
            // an E0597 error because the compiler's NLL couldn't correctly infer
            // the lifetime of my_struct. In the 2024 edition, this semicolon is
            // no longer needed due to improvements in the NLL logic.
            ;
        }

        #[tokio::test]
        async fn ok_phase_fast() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);
        }

        #[tokio::test]
        async fn ok_phase_exact() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_exact(), Phase::Setup);
        }

        #[tokio::test]
        async fn fail_read_fast() {
            let my_struct = PhasedLock::new(MyStruct::new());

            if let Err(e) = my_struct.read_fast() {
                assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallOutOfReadPhase(\"PhasedLock::read_fast\"), message: \"Cannot call PhaseLock::read_fast out of Read phases\" }");
            } else {
                panic!();
            }
        }

        #[tokio::test]
        async fn fail_read_gracefully() {
            let my_struct = PhasedLock::new(MyStruct::new());

            if let Err(e) = my_struct.read_gracefully() {
                assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallOutOfReadPhase(\"PhasedLock::read_gracefully\"), message: \"Cannot call PhaseLock::read_gracefully out of Read phases\" }");
            } else {
                panic!();
            }
        }

        #[tokio::test]
        async fn fail_finish_reading_gracefully() {
            let my_struct = PhasedLock::new(MyStruct::new());

            if let Err(e) = my_struct.finish_reading_gracefully() {
                assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallInSetupPhase(\"PhasedLock::finish_reading_gracefully\"), message: \"Cannot call PhaseLock::finish_reading_gracefully in Setup phases\" }");
            } else {
                panic!();
            }
        }
    }

    mod tests_in_read_phase {
        use super::*;
        use std::time;

        #[test]
        fn ok_read_fast() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("hello".to_string());
                }
                Err(e) => panic!("{e:?}"),
            }

            my_struct.transition_to_read().unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            match my_struct.read_fast() {
                Ok(data) => {
                    assert_eq!(data.vec, vec!["hello".to_string()]);
                }
                Err(e) => panic!("{e:?}"),
            }

            match my_struct.read_fast() {
                Ok(data) => {
                    assert_eq!(data.vec, vec!["hello".to_string()]);
                }
                Err(e) => panic!("{e:?}"),
            }

            let start = time::Instant::now();
            my_struct
                .transition_to_cleanup(WaitStrategy::GracefulWait {
                    first: time::Duration::from_secs(0),
                    interval: time::Duration::from_millis(100),
                    timeout: time::Duration::from_secs(2),
                })
                .unwrap();
            assert!(start.elapsed() < time::Duration::from_millis(10));

            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn ok_read_and_finish_gracefully() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("hello".to_string());
                }
                Err(e) => panic!("{e:?}"),
            }

            my_struct.transition_to_read().unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            match my_struct.read_gracefully() {
                Ok(data) => {
                    assert_eq!(data.vec, vec!["hello".to_string()]);
                }
                Err(e) => panic!("{e:?}"),
            }

            match my_struct.finish_reading_gracefully() {
                Ok(_) => {}
                Err(e) => panic!("{e:?}"),
            }

            let start = time::Instant::now();
            my_struct
                .transition_to_cleanup(WaitStrategy::GracefulWait {
                    first: time::Duration::from_secs(0),
                    interval: time::Duration::from_millis(100),
                    timeout: time::Duration::from_secs(2),
                })
                .unwrap();
            assert!(start.elapsed() < time::Duration::from_millis(10));

            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn fail_lock_for_update() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read().unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            match my_struct.lock_for_update() {
                Ok(_) => {
                    panic!();
                }
                Err(e) => {
                    assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phase: Read, kind: CannotCallInReadPhase(\"PhasedLock::lock_for_update\"), message: \"Cannot call PhaseLock::lock_for_update out of Setup or Cleanup phases\" }");
                }
            }

            // Before the 2024 edition, adding a semicolon was required to prevent
            // an E0597 error because the compiler's NLL couldn't correctly infer
            // the lifetime of my_struct. In the 2024 edition, this semicolon is
            // no longer needed due to improvements in the NLL logic.
            ;
        }
    }

    #[cfg(feature = "setup_read_cleanup-on-tokio-rt")]
    mod tests_in_read_phase_on_tokio_rt {
        use super::*;

        #[test]
        fn ok_phase_fast() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read().unwrap();

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                assert_eq!(my_struct.phase_fast(), Phase::Read);
            });

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn ok_phase_exact() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read().unwrap();

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                assert_eq!(my_struct.phase_exact(), Phase::Read);
            });

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn ok_read_fast() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("hello".to_string());
                }
                Err(e) => panic!("{e:?}"),
            }

            my_struct.transition_to_read().unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                match my_struct.read_fast() {
                    Ok(data) => {
                        assert_eq!(data.vec, vec!["hello".to_string()]);
                    }
                    Err(e) => panic!("{e:?}"),
                }
            });

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn ok_read_and_finish_gracefully() {
            use std::time;

            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("hello".to_string());
                }
                Err(e) => panic!("{e:?}"),
            }

            my_struct.transition_to_read().unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                match my_struct.read_gracefully() {
                    Ok(data) => {
                        assert_eq!(data.vec, vec!["hello".to_string()]);
                    }
                    Err(e) => panic!("{e:?}"),
                }

                if let Err(e) = my_struct.finish_reading_gracefully() {
                    panic!("{e:?}");
                }
            });

            let start = time::Instant::now();
            my_struct
                .transition_to_cleanup(WaitStrategy::GracefulWait {
                    first: time::Duration::from_secs(0),
                    interval: time::Duration::from_millis(100),
                    timeout: time::Duration::from_secs(2),
                })
                .unwrap();
            assert!(start.elapsed() < time::Duration::from_millis(10));
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }
    }

    mod tests_in_cleanup_phase {
        use super::*;

        #[test]
        fn ok_phase_fast() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read().unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn ok_phase_exact() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_exact(), Phase::Setup);

            my_struct.transition_to_read().unwrap();
            assert_eq!(my_struct.phase_exact(), Phase::Read);

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_exact(), Phase::Cleanup);
        }

        #[test]
        fn ok_lock_for_update() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("hello".to_string());
                }
                Err(e) => panic!("{e:?}"),
            }

            my_struct.transition_to_read().unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("world".to_string());
                }
                Err(e) => panic!("{e:?}"),
            }

            match my_struct.lock_for_update() {
                Ok(data) => {
                    assert_eq!(data.vec, vec!["hello".to_string(), "world".to_string(),]);
                }
                Err(e) => panic!("{e:?}"),
            }

            // Before the 2024 edition, adding a semicolon was required to prevent
            // an E0597 error because the compiler's NLL couldn't correctly infer
            // the lifetime of my_struct. In the 2024 edition, this semicolon is
            // no longer needed due to improvements in the NLL logic.
            ;
        }

        #[test]
        fn fail_read_fast() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read().unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            if let Err(e) = my_struct.read_fast() {
                assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phase: Cleanup, kind: CannotCallOutOfReadPhase(\"PhasedLock::read_fast\"), message: \"Cannot call PhaseLock::read_fast out of Read phases\" }");
            } else {
                panic!();
            }
        }

        #[test]
        fn fail_read_gracefully() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read().unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            if let Err(e) = my_struct.read_gracefully() {
                assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phase: Cleanup, kind: CannotCallOutOfReadPhase(\"PhasedLock::read_gracefully\"), message: \"Cannot call PhaseLock::read_gracefully out of Read phases\" }");
            } else {
                panic!();
            }
        }

        #[test]
        fn ok_finish_reading_gracefully() {
            use std::time;

            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read().unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            if let Err(e) = my_struct.read_gracefully() {
                panic!("{e:?}");
            }

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            if let Err(e) = my_struct.finish_reading_gracefully() {
                panic!("{e:?}");
            }

            let start = time::Instant::now();
            my_struct
                .transition_to_cleanup(WaitStrategy::GracefulWait {
                    first: time::Duration::from_secs(0),
                    interval: time::Duration::from_millis(100),
                    timeout: time::Duration::from_secs(2),
                })
                .unwrap();
            assert!(start.elapsed() < time::Duration::from_millis(10));
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }
    }

    #[cfg(feature = "setup_read_cleanup-on-tokio-rt")]
    mod tests_in_cleanup_phase_on_tokio_rt {
        use super::*;

        #[test]
        fn fail_lock_for_update() {
            let my_struct = PhasedLock::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read().unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                if let Err(e) = my_struct.lock_for_update() {
                    assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phase: Cleanup, kind: CannotCallOnTokioRuntime(\"PhasedLock::lock_for_update\"), message: \"Cannot call PhasedLock::lock_for_update on tokio runtime.\" }");
                } else {
                    panic!();
                }
            });
        }
    }
}
