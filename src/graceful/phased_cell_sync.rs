// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use super::{GracefulPhasedCellSync, GracefulWaitErrorKind, GracefulWaitSync};
use crate::phase::*;
use crate::{Phase, PhasedError, PhasedErrorKind, StdMutexGuard};

use std::{any, cell, error, marker, sync, sync::atomic, time};

unsafe impl<T: Send + Sync> Sync for GracefulPhasedCellSync<T> {}
unsafe impl<T: Send + Sync> Send for GracefulPhasedCellSync<T> {}

impl<T: Send + Sync> GracefulPhasedCellSync<T> {
    pub const fn new(data: T) -> Self {
        Self {
            wait: GracefulWaitSync::new(),
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            data_mutex: sync::Mutex::<Option<T>>::new(Some(data)),
            data_cell: cell::UnsafeCell::<Option<T>>::new(None),
            _marker: marker::PhantomData,
        }
    }

    pub fn phase_relaxed(&self) -> Phase {
        let phase = self.phase.load(atomic::Ordering::Relaxed);
        u8_to_phase(phase)
    }

    pub fn phase(&self) -> Phase {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        u8_to_phase(phase)
    }

    pub fn read_relaxed(&self) -> Result<&T, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Relaxed);
        if phase != PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseRead(Self::method_name("read_relaxed")),
            ));
        }

        if let Some(data) = unsafe { &*self.data_cell.get() }.as_ref() {
            self.wait.count_up();
            Ok(data)
        } else {
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataUnavailable,
            ))
        }
    }

    pub fn read(&self) -> Result<&T, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase != PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseRead(Self::method_name("read")),
            ));
        }

        if let Some(data) = unsafe { &*self.data_cell.get() }.as_ref() {
            self.wait.count_up();
            Ok(data)
        } else {
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataUnavailable,
            ))
        }
    }

    pub fn finish_reading(&self) {
        self.wait
            .count_down(|| self.phase.load(atomic::Ordering::Acquire) == PHASE_READ_TO_CLEANUP);
    }

    pub fn transition_to_cleanup<F, E>(
        &self,
        timeout: time::Duration,
        mut f: F,
    ) -> Result<(), PhasedError>
    where
        F: FnMut(&mut T) -> Result<(), E>,
        E: error::Error + Send + Sync + 'static,
    {
        match self.phase.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |current_phase| match current_phase {
                PHASE_SETUP => Some(PHASE_SETUP_TO_CLEANUP),
                PHASE_READ => Some(PHASE_READ_TO_CLEANUP),
                _ => None,
            },
        ) {
            Ok(PHASE_READ) => match self.data_mutex.lock() {
                Ok(mut guard) => {
                    let result_w = self.wait.wait_gracefully(timeout);
                    let data_opt = unsafe { &mut *self.data_cell.get() };
                    if data_opt.is_some() {
                        let result_f = f(data_opt.as_mut().unwrap());
                        unsafe {
                            core::ptr::swap(data_opt, &mut *guard);
                        }
                        self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);
                        if let Err(e) = result_f {
                            Err(PhasedError::with_source(
                                u8_to_phase(PHASE_READ_TO_CLEANUP),
                                PhasedErrorKind::FailToRunClosureDuringTransitionToCleanup,
                                e,
                            ))
                        } else if let Err(e) = result_w {
                            match e.kind {
                                GracefulWaitErrorKind::TimedOut(_) => Err(PhasedError::new(
                                    u8_to_phase(PHASE_READ_TO_CLEANUP),
                                    PhasedErrorKind::GracefulWaitTimeout(timeout),
                                )),
                                GracefulWaitErrorKind::MutexIsPoisoned => Err(PhasedError::new(
                                    u8_to_phase(PHASE_READ_TO_CLEANUP),
                                    PhasedErrorKind::StdMutexIsPoisoned,
                                )),
                            }
                        } else {
                            Ok(())
                        }
                    } else {
                        // impossible case
                        self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);
                        Err(PhasedError::new(
                            u8_to_phase(PHASE_SETUP_TO_READ),
                            PhasedErrorKind::InternalDataUnavailable,
                        ))
                    }
                }
                Err(_) => {
                    self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);
                    Err(PhasedError::new(
                        u8_to_phase(PHASE_READ_TO_CLEANUP),
                        PhasedErrorKind::StdMutexIsPoisoned,
                    ))
                }
            },
            Ok(PHASE_SETUP) => match self.data_mutex.lock() {
                Ok(mut data_opt) => {
                    let result_w = self.wait.wait_gracefully(timeout);
                    if data_opt.is_some() {
                        let result_f = f(data_opt.as_mut().unwrap());
                        self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);
                        if let Err(e) = result_f {
                            Err(PhasedError::with_source(
                                u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                                PhasedErrorKind::FailToRunClosureDuringTransitionToCleanup,
                                e,
                            ))
                        } else if let Err(e) = result_w {
                            match e.kind {
                                GracefulWaitErrorKind::TimedOut(_) => Err(PhasedError::new(
                                    u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                                    PhasedErrorKind::GracefulWaitTimeout(timeout),
                                )),
                                GracefulWaitErrorKind::MutexIsPoisoned => Err(PhasedError::new(
                                    u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                                    PhasedErrorKind::StdMutexIsPoisoned,
                                )),
                            }
                        } else {
                            Ok(())
                        }
                    } else {
                        // impossible case
                        self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);
                        Err(PhasedError::new(
                            u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                            PhasedErrorKind::InternalDataUnavailable,
                        ))
                    }
                }
                Err(_) => {
                    self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);
                    Err(PhasedError::new(
                        u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                        PhasedErrorKind::StdMutexIsPoisoned,
                    ))
                }
            },
            Err(PHASE_CLEANUP) => Err(PhasedError::new(
                u8_to_phase(PHASE_CLEANUP),
                PhasedErrorKind::PhaseIsAlreadyCleanup,
            )),
            Err(PHASE_SETUP_TO_READ) => Err(PhasedError::new(
                u8_to_phase(PHASE_SETUP_TO_READ),
                PhasedErrorKind::DuringTransitionToRead,
            )),
            Err(old_phase_cd) => Err(PhasedError::new(
                u8_to_phase(old_phase_cd),
                PhasedErrorKind::DuringTransitionToCleanup,
            )),
            Ok(_) => Ok(()), // impossible case
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
                        // impossible case
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
            Err(PHASE_CLEANUP) => Err(PhasedError::new(
                u8_to_phase(PHASE_CLEANUP),
                PhasedErrorKind::PhaseIsAlreadyCleanup,
            )),
            Err(PHASE_SETUP_TO_READ) => Err(PhasedError::new(
                u8_to_phase(PHASE_SETUP_TO_READ),
                PhasedErrorKind::DuringTransitionToRead,
            )),
            Err(old_phase_cd) => Err(PhasedError::new(
                u8_to_phase(old_phase_cd),
                PhasedErrorKind::DuringTransitionToCleanup,
            )),
        }
    }

    pub fn lock(&self) -> Result<StdMutexGuard<'_, T>, PhasedError> {
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
                    if let Some(new_guard) = StdMutexGuard::try_new(guarded_opt) {
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
        format!("{}::{}", any::type_name::<GracefulPhasedCellSync<T>>(), m)
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
    use tokio::time;

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
    impl std::error::Error for MyError {}

    #[test]
    fn transition_from_setup_to_read_then_cleanup() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);

        if let Err(e) =
            cell.transition_to_cleanup(time::Duration::from_secs(5), |_data| Ok::<(), MyError>(()))
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn transition_from_setup_to_cleanup() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) =
            cell.transition_to_cleanup(time::Duration::from_secs(5), |_data| Ok::<(), MyError>(()))
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn update_internal_data_in_setup_and_cleanup_phases() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);

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
        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);

        if let Ok(data) = cell.read_relaxed() {
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

        cell.finish_reading();

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup(time::Duration::ZERO, |data| {
            data.add(" ** ".to_string());
            Ok::<(), MyError>(())
        }) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);

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
              " ** ".to_string(),
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
    fn read_relaxed_internal_data_in_multi_threads() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

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
                let data = cell_clone.read_relaxed().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                std::thread::sleep(std::time::Duration::from_secs(1));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                cell_clone.finish_reading();
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
        if let Err(e) = cell.transition_to_cleanup(time::Duration::ZERO, |data| {
            data.add("**".to_string());
            Ok::<(), MyError>(())
        }) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);

        if let Ok(mut data) = cell.lock() {
            data.add("!".to_string());
            assert_eq!(
                &data.vec,
                &[
                    "Hello".to_string(),
                    "World".to_string(),
                    "**".to_string(),
                    "!".to_string()
                ]
            );
            data.clear();
            assert_eq!(&data.vec, &[] as &[String]);
        } else {
            panic!();
        }

        assert_eq!(counter.load(atomic::Ordering::Acquire), 10);
    }

    #[test]
    fn fail_to_read_relaxed_if_phase_is_setup() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);

        if let Err(e) = cell.read_relaxed() {
            assert_eq!(e.phase, Phase::Setup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::graceful::GracefulPhasedCellSync<setup_read_cleanup::graceful::phased_cell_sync::tests_of_phased_cell_sync::MyStruct>::read_relaxed".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);
    }

    #[test]
    fn fail_to_read_relaxed_if_phase_is_cleanup() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);

        if let Err(e) =
            cell.transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
        {
            panic!("{e:?}");
        }

        if let Err(e) = cell.read_relaxed() {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::graceful::GracefulPhasedCellSync<setup_read_cleanup::graceful::phased_cell_sync::tests_of_phased_cell_sync::MyStruct>::read_relaxed".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_read_if_phase_is_setup() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell.read() {
            assert_eq!(e.phase, Phase::Setup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::graceful::GracefulPhasedCellSync<setup_read_cleanup::graceful::phased_cell_sync::tests_of_phased_cell_sync::MyStruct>::read".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);
    }

    #[test]
    fn fail_to_read_if_phase_is_cleanup() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) =
            cell.transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
        {
            panic!("{e:?}");
        }

        if let Err(e) = cell.read() {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::graceful::GracefulPhasedCellSync<setup_read_cleanup::graceful::phased_cell_sync::tests_of_phased_cell_sync::MyStruct>::read".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_lock_if_phase_is_read() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.lock() {
            assert_eq!(e.phase, Phase::Read);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallOnPhaseRead("setup_read_cleanup::graceful::GracefulPhasedCellSync<setup_read_cleanup::graceful::phased_cell_sync::tests_of_phased_cell_sync::MyStruct>::lock".to_string()),
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_read_if_phase_is_read() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

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

        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_read_if_phase_is_cleanup() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) =
            cell.transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
        {
            panic!("{e:?}");
        }

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(e.kind, PhasedErrorKind::PhaseIsAlreadyCleanup);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_cleanup_if_phase_is_cleanup() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) =
            cell.transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
        {
            panic!("{e:?}");
        }

        if let Err(e) =
            cell.transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
        {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(e.kind, PhasedErrorKind::PhaseIsAlreadyCleanup);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_lock_during_transition_to_read() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

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

        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[test]
    fn fail_to_lock_during_transition_to_cleanup_from_setup() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(time::Duration::ZERO, |_data| {
                std::thread::sleep(std::time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
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

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_lock_during_transition_to_cleanup_from_read() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(time::Duration::ZERO, |_data| {
                std::thread::sleep(std::time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
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

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_read_if_closure_causes_an_error() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

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

        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);
    }

    #[test]
    fn fail_to_transition_to_read_during_transition_to_read() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

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

        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_read_during_transition_to_cleanup() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(time::Duration::ZERO, |_data| {
                std::thread::sleep(std::time::Duration::from_secs(2));
                Ok::<(), MyError>(())
            }) {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_read(|_data| Ok::<(), MyError>(())) {
                match e.kind {
                    PhasedErrorKind::DuringTransitionToCleanup => {}
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

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_cleanup_during_transition_to_read() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

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
            if let Err(e) = cell_clone
                .transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
            {
                assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToRead);
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_cleanup_during_transition_to_cleanup_from_setup() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(time::Duration::ZERO, |_data| {
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
            if let Err(e) = cell_clone
                .transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
            {
                assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToCleanup);
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_cleanup_during_transition_to_cleanup_from_read() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(time::Duration::ZERO, |_data| {
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
            if let Err(e) = cell_clone
                .transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
            {
                assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToCleanup);
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn transition_to_cleanup_from_setup_but_closure_failed() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell.transition_to_cleanup(time::Duration::ZERO, |_data| Err(MyError {})) {
            assert_eq!(format!("{:?}", e), "setup_read_cleanup::PhasedError { phase: Cleanup, kind: FailToRunClosureDuringTransitionToCleanup, source: MyError }");
        } else {
            panic!();
        }
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn transition_to_cleanup_from_read_but_closure_failed() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase(), Phase::Read);

        if let Err(e) = cell.transition_to_cleanup(time::Duration::ZERO, |_data| Err(MyError {})) {
            assert_eq!(format!("{:?}", e), "setup_read_cleanup::PhasedError { phase: Cleanup, kind: FailToRunClosureDuringTransitionToCleanup, source: MyError }");
        } else {
            panic!();
        }
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn transition_to_cleanup_gracefully() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

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
                let data = cell_clone.read_relaxed().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                std::thread::sleep(std::time::Duration::from_secs(1));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                cell_clone.finish_reading();
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
            join_handlers.push(handler);
        }

        std::thread::sleep(std::time::Duration::from_millis(100));

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup(time::Duration::from_secs(5), |data| {
            assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
            data.add("!!".to_string());
            assert_eq!(data.vec.as_slice().join(", "), "Hello, World, !!");
            Ok::<(), MyError>(())
        }) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);

        if let Ok(mut data) = cell.lock() {
            assert_eq!(
                &data.vec,
                &["Hello".to_string(), "World".to_string(), "!!".to_string()]
            );
            data.clear();
            assert_eq!(&data.vec, &[] as &[String]);
        } else {
            panic!();
        }

        assert_eq!(counter.load(atomic::Ordering::Acquire), 10);
    }

    #[test]
    fn transition_to_cleanup_gracefully_but_timeout() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

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
        for i in 0..10 {
            let cell_clone = Arc::clone(&cell);
            let counter_clone = Arc::clone(&counter);
            let handler = std::thread::spawn(move || {
                let data = cell_clone.read_relaxed().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                std::thread::sleep(std::time::Duration::from_secs(1 + i));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                cell_clone.finish_reading();
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
            join_handlers.push(handler);
        }

        std::thread::sleep(std::time::Duration::from_millis(100));

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup(time::Duration::from_secs(1), |data| {
            assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
            data.add("!!".to_string());
            assert_eq!(data.vec.as_slice().join(", "), "Hello, World, !!");
            Ok::<(), MyError>(())
        }) {
            match e.kind {
                PhasedErrorKind::GracefulWaitTimeout(tm) => {
                    assert_eq!(tm, time::Duration::from_secs(1));
                }
                _ => panic!(),
            }
            assert_eq!(e.phase, Phase::Cleanup);
        } else {
            panic!();
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);

        if let Ok(mut data) = cell.lock() {
            assert_eq!(
                &data.vec,
                &["Hello".to_string(), "World".to_string(), "!!".to_string()]
            );
            data.clear();
            assert_eq!(&data.vec, &[] as &[String]);
        } else {
            panic!();
        }

        assert!(counter.load(atomic::Ordering::Acquire) < 10);
    }
}
