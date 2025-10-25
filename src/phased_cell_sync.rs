// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::phase::*;
use crate::{Phase, PhasedCellSync, PhasedError, PhasedErrorKind, PhasedStdMutexGuard, Wait};

use std::ops::{Deref, DerefMut};
use std::{any, cell, error, marker, sync, sync::atomic};

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

unsafe impl<T: Send + Sync> Sync for PhasedCellSync<T> {}
unsafe impl<T: Send + Sync> Send for PhasedCellSync<T> {}

impl<T: Send + Sync> PhasedCellSync<T> {
    pub const fn new(data: T) -> Self {
        Self {
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            read_count: atomic::AtomicUsize::new(0),
            data_mutex: sync::Mutex::<Option<T>>::new(Some(data)),
            data_cell: cell::UnsafeCell::<Option<T>>::new(None),
            _marker: marker::PhantomData,
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
        if phase != PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseRead(Self::method_name("read_fast")),
            ));
        }

        if let Some(data) = unsafe { &*self.data_cell.get() }.as_ref() {
            Ok(data)
        } else {
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataUnavailable,
            ))
        }
    }

    pub fn read_gracefully(&self) -> Result<&T, PhasedError> {
        let mut phase = 0u8;
        let result = self.read_count.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |count| {
                phase = self.phase.load(atomic::Ordering::Acquire);
                if phase != PHASE_READ {
                    None
                } else {
                    Some(count + 1)
                }
            },
        );

        if result.is_err() {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseRead(Self::method_name("read_gracefully")),
            ));
        }

        if let Some(data) = unsafe { &*self.data_cell.get() }.as_ref() {
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

        if let Err(_) = self.read_count.fetch_update(
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
            eprintln!(
                "{} is called excessively.",
                Self::method_name("finish_reading_gracefully"),
            );
        }

        Ok(())
    }

    pub fn transition_to_cleanup(&self, wait: Wait) -> Result<(), PhasedError> {
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
                let _result = self.pause(wait);
                self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);
                Ok(())
            }
            Ok(PHASE_READ) => {
                let result = self.pause(wait);
                self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);
                match self.data_mutex.lock() {
                    Ok(mut data_opt) => {
                        unsafe {
                            core::ptr::swap(self.data_cell.get(), &mut *data_opt);
                        }
                        match result {
                            Ok(_) => Ok(()),
                            Err(w) => Err(PhasedError::new(
                                u8_to_phase(PHASE_READ_TO_CLEANUP),
                                PhasedErrorKind::TransitionToCleanupTimeout(w),
                            )),
                        }
                    }
                    Err(_) => Err(PhasedError::new(
                        u8_to_phase(PHASE_READ),
                        PhasedErrorKind::StdMutexIsPoisoned,
                    )),
                }
            }
            Err(PHASE_CLEANUP) => Err(PhasedError::new(
                u8_to_phase(PHASE_CLEANUP),
                PhasedErrorKind::PhaseIsAlreadyCleanup,
            )),
            Err(PHASE_READ_TO_CLEANUP) => Err(PhasedError::new(
                u8_to_phase(PHASE_READ_TO_CLEANUP),
                PhasedErrorKind::DuringTransitionToCleanup,
            )),
            Err(PHASE_SETUP_TO_CLEANUP) => Err(PhasedError::new(
                u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                PhasedErrorKind::DuringTransitionToCleanup,
            )),
            Err(old_phase_cd) => Err(PhasedError::new(
                u8_to_phase(old_phase_cd),
                PhasedErrorKind::TransitionToCleanupFailed,
            )),
            Ok(_) => Ok(()), // impossible case.
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
            Ok(old_phase_cd) => match self.data_mutex.lock() {
                Ok(mut data_opt) => {
                    if data_opt.is_some() {
                        if let Err(e) = f(data_opt.as_mut().unwrap()) {
                            self.change_phase(PHASE_SETUP_TO_READ, old_phase_cd);
                            Err(PhasedError::with_source(
                                u8_to_phase(PHASE_SETUP_TO_READ),
                                PhasedErrorKind::FailToRunClosureDuringTransitionToRead,
                                e,
                            ))
                        } else {
                            unsafe {
                                core::ptr::swap(self.data_cell.get(), &mut *data_opt);
                            }
                            self.change_phase(PHASE_SETUP_TO_READ, PHASE_READ);
                            Ok(())
                        }
                    } else {
                        self.change_phase(PHASE_SETUP_TO_READ, old_phase_cd);
                        Err(PhasedError::new(
                            u8_to_phase(PHASE_SETUP_TO_READ),
                            PhasedErrorKind::InternalDataUnavailable,
                        ))
                    }
                }
                Err(_e) => {
                    self.change_phase(PHASE_SETUP_TO_READ, old_phase_cd);
                    Err(PhasedError::new(
                        u8_to_phase(old_phase_cd),
                        PhasedErrorKind::StdMutexIsPoisoned,
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

    pub fn lock(&self) -> Result<PhasedStdMutexGuard<'_, T>, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        match phase {
            PHASE_READ => Err(PhasedError::new(
                u8_to_phase(PHASE_READ),
                PhasedErrorKind::CannotCallOnPhaseRead(Self::method_name("lock")),
            )),
            PHASE_SETUP_TO_READ => Err(PhasedError::new(
                u8_to_phase(PHASE_SETUP_TO_READ),
                PhasedErrorKind::DuringTransitionToRead,
            )),
            PHASE_SETUP_TO_CLEANUP => Err(PhasedError::new(
                u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                PhasedErrorKind::DuringTransitionToCleanup,
            )),
            PHASE_READ_TO_CLEANUP => Err(PhasedError::new(
                u8_to_phase(PHASE_READ_TO_CLEANUP),
                PhasedErrorKind::DuringTransitionToCleanup,
            )),
            _ => match self.data_mutex.lock() {
                Ok(guarded_opt) => {
                    if let Some(new_guard) = PhasedStdMutexGuard::try_new(guarded_opt) {
                        Ok(new_guard)
                    } else {
                        Err(PhasedError::new(
                            u8_to_phase(phase),
                            PhasedErrorKind::InternalDataUnavailable,
                        ))
                    }
                }
                Err(_e) => Err(PhasedError::new(
                    u8_to_phase(phase),
                    PhasedErrorKind::StdMutexIsPoisoned,
                )),
            },
        }
    }

    #[inline]
    fn method_name(m: &str) -> String {
        format!("{}::{}", any::type_name::<PhasedCellSync<T>>(), m)
    }

    #[inline]
    fn change_phase(&self, current_phase: u8, new_phase: u8) {
        let _result = self.phase.compare_exchange(
            current_phase,
            new_phase,
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
        );
    }
}

#[cfg(test)]
mod tests_of_phased_cell_sync {
    use super::*;
    use std::error::Error;
    use std::fmt;
    use std::sync::Arc;

    #[derive(Debug)]
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
        fn clear(&mut self) {
            self.vec.clear();
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

    #[test]
    fn transition_from_setup_to_read_then_cleanup() {
        let cell = PhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);

        if let Err(e) = cell.transition_to_cleanup(Wait::Zero) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[test]
    fn transition_from_setup_to_cleanup() {
        let cell = PhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);

        if let Err(e) = cell.transition_to_cleanup(Wait::Zero) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[test]
    fn update_internal_data_in_setup_and_cleanup_phases() {
        let cell = PhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();
        for _i in 0..3 {
            let cell_clone = Arc::clone(&cell);
            let handler = std::thread::spawn(move || match cell_clone.lock() {
                Ok(mut data) => {
                    data.add("H".to_string());
                    data.add("e".to_string());
                    data.add("l".to_string());
                    data.add("l".to_string());
                    data.add("o".to_string());
                }
                Err(e) => panic!("{e:?}"),
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).join() {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|data| {
            data.add(",".to_string());
            Ok::<(), MyError>(())
        }) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);

        if let Ok(data) = cell.read_fast() {
            assert_eq!(
                &data.vec,
                &[
                    "H".to_string(),
                    "e".to_string(),
                    "l".to_string(),
                    "l".to_string(),
                    "o".to_string(),
                    // --
                    "H".to_string(),
                    "e".to_string(),
                    "l".to_string(),
                    "l".to_string(),
                    "o".to_string(),
                    // --
                    "H".to_string(),
                    "e".to_string(),
                    "l".to_string(),
                    "l".to_string(),
                    "o".to_string(),
                    // --
                    ",".to_string(),
                ]
            );
        } else {
            panic!();
        }

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup(Wait::Zero) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();
        for _i in 0..3 {
            let cell_clone = Arc::clone(&cell);
            let handler = std::thread::spawn(move || match cell_clone.lock() {
                Ok(mut data) => {
                    data.add("W".to_string());
                    data.add("o".to_string());
                    data.add("r".to_string());
                    data.add("l".to_string());
                    data.add("d".to_string());
                }
                Err(e) => panic!("{e:?}"),
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).join() {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        if let Ok(mut data) = cell.lock() {
            assert_eq!(&data.vec, &[
              "H".to_string(),
              "e".to_string(),
              "l".to_string(),
              "l".to_string(),
              "o".to_string(),
              // --
              "H".to_string(),
              "e".to_string(),
              "l".to_string(),
              "l".to_string(),
              "o".to_string(),
              // --
              "H".to_string(),
              "e".to_string(),
              "l".to_string(),
              "l".to_string(),
              "o".to_string(),
              // --
              ",".to_string(),
              // --
              "W".to_string(),
              "o".to_string(),
              "r".to_string(),
              "l".to_string(),
              "d".to_string(),
              // --
              "W".to_string(),
              "o".to_string(),
              "r".to_string(),
              "l".to_string(),
              "d".to_string(),
              // --
              "W".to_string(),
              "o".to_string(),
              "r".to_string(),
              "l".to_string(),
              "d".to_string(),
            ]);
            data.clear();
        } else {
            panic!();
        }

        // Before the 2024 edition, adding a semicolon was required to prevent
        // an E0597 error because the compiler's NLL couldn't correctly infer
        // the lifetime of my_struct. In the 2024 edition, this semicolon is
        // no longer needed due to improvements in the NLL logic.
        ;
    }

    #[test]
    fn read_fast_internal_data_in_multi_threads_and_wait_zero() {
        let cell = PhasedCellSync::new(MyStruct::new());

        match cell.lock() {
            Ok(mut data) => data.add("Hello".to_string()),
            Err(e) => panic!("{e:?}"),
        }

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|data| {
            data.add("World".to_string());
            Ok::<(), MyError>(())
        }) {
            panic!("{e:?}");
        }

        let counter = Arc::new(atomic::AtomicU8::new(0));
        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();
        for _i in 0..10 {
            let cell_clone = Arc::clone(&cell);
            let counter_clone = Arc::clone(&counter);
            let handler = std::thread::spawn(move || {
                let data = cell_clone.read_fast().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                std::thread::sleep(std::time::Duration::from_secs(1));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).join() {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup(Wait::Zero) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);

        if let Ok(mut data) = cell.lock() {
            data.add("!".to_string());
            assert_eq!(
                &data.vec,
                &["Hello".to_string(), "World".to_string(), "!".to_string()]
            );
            data.clear();
            assert_eq!(&data.vec, &[] as &[String]);
        } else {
            panic!();
        }

        assert_eq!(counter.load(atomic::Ordering::Acquire), 10);
    }

    #[test]
    fn read_fast_internal_data_in_multi_threads_and_wait_fixed() {
        let cell = PhasedCellSync::new(MyStruct::new());

        match cell.lock() {
            Ok(mut data) => data.add("Hello".to_string()),
            Err(e) => panic!("{e:?}"),
        }

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|data| {
            data.add("World".to_string());
            Ok::<(), MyError>(())
        }) {
            panic!("{e:?}");
        }

        let counter = Arc::new(atomic::AtomicU8::new(0));
        let cell = Arc::new(cell);

        for _i in 0..10 {
            let cell_clone = Arc::clone(&cell);
            let counter_clone = Arc::clone(&counter);
            let _handler = std::thread::spawn(move || {
                let data = cell_clone.read_fast().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                std::thread::sleep(std::time::Duration::from_secs(1));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup(Wait::Fixed(std::time::Duration::from_secs(1))) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);

        if let Ok(mut data) = cell.lock() {
            data.add("!".to_string());
            assert_eq!(
                &data.vec,
                &["Hello".to_string(), "World".to_string(), "!".to_string()]
            );
            data.clear();
            assert_eq!(&data.vec, &[] as &[String]);
        } else {
            panic!();
        }

        assert_eq!(counter.load(atomic::Ordering::Acquire), 10);
    }

    #[test]
    fn read_gracefully_internal_data_in_multi_threads_and_wait_gracefully() {
        let cell = PhasedCellSync::new(MyStruct::new());

        match cell.lock() {
            Ok(mut data) => data.add("Hello".to_string()),
            Err(e) => panic!("{e:?}"),
        }

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|data| {
            data.add("World".to_string());
            Ok::<(), MyError>(())
        }) {
            panic!("{e:?}");
        }

        let counter = Arc::new(atomic::AtomicU8::new(0));
        let cell = Arc::new(cell);

        for _i in 0..10 {
            let cell_clone = Arc::clone(&cell);
            let counter_clone = Arc::clone(&counter);
            let _handler = std::thread::spawn(move || {
                let data = cell_clone.read_gracefully().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                std::thread::sleep(std::time::Duration::from_secs(1));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                cell_clone.finish_reading_gracefully().unwrap();
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup(Wait::Graceful {
            timeout: std::time::Duration::from_secs(5),
        }) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);

        if let Ok(mut data) = cell.lock() {
            data.add("!".to_string());
            assert_eq!(
                &data.vec,
                &["Hello".to_string(), "World".to_string(), "!".to_string()]
            );
            data.clear();
            assert_eq!(&data.vec, &[] as &[String]);
        } else {
            panic!();
        }

        assert_eq!(counter.load(atomic::Ordering::Acquire), 10);
    }

    #[test]
    fn fail_to_read_fast_if_phase_is_setup() {
        let cell = PhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell.read_fast() {
            assert_eq!(e.phase, Phase::Setup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedCellSync<setup_read_cleanup::phased_cell_sync::tests_of_phased_cell_sync::MyStruct>::read_fast".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);
    }

    #[test]
    fn fail_to_read_fast_if_phase_is_cleanup() {
        let cell = PhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell.read_gracefully() {
            assert_eq!(e.phase, Phase::Setup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedCellSync<setup_read_cleanup::phased_cell_sync::tests_of_phased_cell_sync::MyStruct>::read_gracefully".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);
    }

    #[test]
    fn fail_to_read_gracefully_if_phase_is_cleanup() {
        let cell = PhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell.transition_to_cleanup(Wait::Zero) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.read_gracefully() {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedCellSync<setup_read_cleanup::phased_cell_sync::tests_of_phased_cell_sync::MyStruct>::read_gracefully".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_finish_reading_gracefully_if_phase_is_setup() {
        let cell = PhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell.finish_reading_gracefully() {
            assert_eq!(e.phase, Phase::Setup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallOnPhaseSetup("setup_read_cleanup::PhasedCellSync<setup_read_cleanup::phased_cell_sync::tests_of_phased_cell_sync::MyStruct>::finish_reading_gracefully".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);
    }

    #[test]
    fn dont_fail_to_finish_reading_gracefully_if_phase_is_cleanup() {
        let cell = PhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell.transition_to_read(|_| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        let _result = cell.read_gracefully();

        if let Err(e) = cell.transition_to_cleanup(Wait::Zero) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.finish_reading_gracefully() {
            panic!("{e:?}");
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[test]
    fn dont_fail_to_finish_reading_gracefully_if_it_is_called_excessively() {
        let cell = PhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell.transition_to_read(|_| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        let _result = cell.read_gracefully();
        let _result = cell.finish_reading_gracefully();

        if let Err(e) = cell.transition_to_cleanup(Wait::Zero) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.finish_reading_gracefully() {
            panic!("{e:?}");
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[test]
    fn dont_fail_but_return_error_if_graceful_wait_for_transition_to_cleanup_is_timeout() {
        let cell = PhasedCellSync::new(MyStruct::new());

        match cell.lock() {
            Ok(mut data) => data.add("Hello".to_string()),
            Err(e) => panic!("{e:?}"),
        }

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|data| {
            data.add("World".to_string());
            Ok::<(), MyError>(())
        }) {
            panic!("{e:?}");
        }

        let counter = Arc::new(atomic::AtomicU8::new(0));
        let cell = Arc::new(cell);

        for i in 0..10 {
            let cell_clone = Arc::clone(&cell);
            let counter_clone = Arc::clone(&counter);
            let _handler = std::thread::spawn(move || {
                let data = cell_clone.read_gracefully().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                std::thread::sleep(std::time::Duration::from_secs(i + 1));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                cell_clone.finish_reading_gracefully().unwrap();
                //println!("{}. {}", i, data.vec.as_slice().join(", "));
            });
        }
        std::thread::sleep(std::time::Duration::from_secs(3));

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup(Wait::Graceful {
            timeout: std::time::Duration::from_secs(1),
        }) {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::TransitionToCleanupTimeout(Wait::Graceful {
                    timeout: std::time::Duration::from_secs(1),
                })
            );
        } else {
            panic!();
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);

        if let Ok(mut data) = cell.lock() {
            data.add("!".to_string());
            assert_eq!(
                &data.vec,
                &["Hello".to_string(), "World".to_string(), "!".to_string()]
            );
            data.clear();
            assert_eq!(&data.vec, &[] as &[String]);
        } else {
            panic!();
        }

        #[cfg(target_os = "linux")]
        assert!(counter.load(atomic::Ordering::Acquire) >= 3);
        #[cfg(target_os = "linux")]
        assert!(counter.load(atomic::Ordering::Acquire) <= 4);
        #[cfg(target_os = "windows")]
        assert!(counter.load(atomic::Ordering::Acquire) >= 3);
        #[cfg(target_os = "windows")]
        assert!(counter.load(atomic::Ordering::Acquire) <= 4);
        #[cfg(target_os = "macos")]
        assert!(counter.load(atomic::Ordering::Acquire) >= 3);
        #[cfg(target_os = "macos")]
        assert!(counter.load(atomic::Ordering::Acquire) <= 4);
    }

    #[test]
    fn fail_to_lock_if_phase_is_read() {
        let cell = PhasedCellSync::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.lock() {
            assert_eq!(e.phase, Phase::Read);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallOnPhaseRead("setup_read_cleanup::PhasedCellSync<setup_read_cleanup::phased_cell_sync::tests_of_phased_cell_sync::MyStruct>::lock".to_string()),
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_read_if_phase_is_read() {
        let cell = PhasedCellSync::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            assert_eq!(e.phase, Phase::Read);
            assert_eq!(e.kind, PhasedErrorKind::PhaseIsAlreadyRead);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_read_if_phase_is_cleanup() {
        let cell = PhasedCellSync::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.transition_to_cleanup(Wait::Zero) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(e.kind, PhasedErrorKind::TransitionToReadFailed);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_finish_reading_gracefully_during_transition_to_read() {
        let cell = PhasedCellSync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_read(|_data| {
                std::thread::sleep(std::time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(std::time::Duration::from_millis(100));

        if let Err(e) = cell.finish_reading_gracefully() {
            assert_eq!(e.kind, PhasedErrorKind::CannotCallOnPhaseSetup("setup_read_cleanup::PhasedCellSync<setup_read_cleanup::phased_cell_sync::tests_of_phased_cell_sync::MyStruct>::finish_reading_gracefully".to_string()),
            );
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);
    }

    #[test]
    fn fail_to_lock_during_transition_to_read() {
        let cell = PhasedCellSync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_read(|_data| {
                std::thread::sleep(std::time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(std::time::Duration::from_millis(100));

        if let Err(e) = cell.lock() {
            assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToRead);
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);
    }

    #[test]
    fn fail_to_lock_during_transition_to_cleanup_from_setup() {
        let cell = PhasedCellSync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) =
                cell_clone.transition_to_cleanup(Wait::Fixed(std::time::Duration::from_secs(1)))
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(std::time::Duration::from_millis(100));

        if let Err(e) = cell.lock() {
            assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToCleanup);
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_lock_during_transition_to_cleanup_from_read() {
        let cell = PhasedCellSync::new(MyStruct::new());

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) =
                cell_clone.transition_to_cleanup(Wait::Fixed(std::time::Duration::from_secs(1)))
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(std::time::Duration::from_millis(100));

        if let Err(e) = cell.lock() {
            assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToCleanup);
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_read_if_closure_causes_an_error() {
        let cell = PhasedCellSync::new(MyStruct::new());

        if let Err(e) = cell.transition_to_read(|_data| {
            std::thread::sleep(std::time::Duration::from_secs(1));
            Err(MyError {})
        }) {
            match e.kind {
                PhasedErrorKind::FailToRunClosureDuringTransitionToRead => {}
                _ => panic!("{e:?}"),
            }
            match e.source().unwrap().downcast_ref::<MyError>() {
                Some(_ee) => {}
                None => panic!(),
            }
        }

        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);
    }

    #[test]
    fn fail_to_transition_to_read_during_transition_to_read() {
        let cell = PhasedCellSync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();
        for _i in 0..2 {
            let cell_clone = Arc::clone(&cell);
            let handler = std::thread::spawn(move || {
                if let Err(e) = cell_clone.transition_to_read(|_data| {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    Ok::<(), MyError>(())
                }) {
                    match e.kind {
                        PhasedErrorKind::DuringTransitionToRead => {}
                        _ => panic!("{e:?}"),
                    }
                }
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_read_during_transition_to_cleanup() {
        let cell = PhasedCellSync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) =
                cell_clone.transition_to_cleanup(Wait::Fixed(std::time::Duration::from_secs(1)))
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_read(|_data| Ok::<(), MyError>(())) {
                match e.kind {
                    PhasedErrorKind::TransitionToReadFailed => {}
                    _ => panic!("{e:?}"),
                }
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_cleanup_during_transition_to_read() {
        let cell = PhasedCellSync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_read(|_data| {
                std::thread::sleep(std::time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(std::time::Duration::from_millis(100));

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(Wait::Zero) {
                assert_eq!(e.kind, PhasedErrorKind::TransitionToCleanupFailed);
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_cleanup_during_transition_to_cleanup_from_setup() {
        let cell = PhasedCellSync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) =
                cell_clone.transition_to_cleanup(Wait::Fixed(std::time::Duration::from_secs(1)))
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(std::time::Duration::from_millis(100));

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(Wait::Zero) {
                assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToCleanup);
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_cleanup_during_transition_to_cleanup_from_read() {
        let cell = PhasedCellSync::new(MyStruct::new());

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) =
                cell_clone.transition_to_cleanup(Wait::Fixed(std::time::Duration::from_secs(1)))
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(std::time::Duration::from_millis(100));

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(Wait::Zero) {
                assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToCleanup);
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }
}
