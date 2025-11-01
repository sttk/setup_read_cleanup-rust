// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::phase::*;
use crate::{Phase, PhasedCell, PhasedError, PhasedErrorKind};

use std::{any, cell, error, marker, sync::atomic};

unsafe impl<T: Send + Sync> Sync for PhasedCell<T> {}
unsafe impl<T: Send + Sync> Send for PhasedCell<T> {}

impl<T: Send + Sync> PhasedCell<T> {
    pub const fn new(data: T) -> Self {
        Self {
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            data_cell: cell::UnsafeCell::new(data),
            _marker: marker::PhantomData,
        }
    }

    pub fn phase_fast(&self) -> Phase {
        let phase = self.phase.load(atomic::Ordering::Relaxed);
        u8_to_phase(phase)
    }

    pub fn phase_properly(&self) -> Phase {
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

        let data = unsafe { &*self.data_cell.get() };
        Ok(data)
    }

    pub fn read_properly(&self) -> Result<&T, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase != PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseRead(Self::method_name("read_properly")),
            ));
        }

        let data = unsafe { &*self.data_cell.get() };
        Ok(data)
    }

    pub fn transition_to_cleanup<F, E>(&self, mut f: F) -> Result<(), PhasedError>
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
            Ok(old_phase_cd) => {
                let current_phase_cd = match old_phase_cd {
                    PHASE_READ => PHASE_READ_TO_CLEANUP,
                    _ => PHASE_SETUP_TO_CLEANUP,
                };
                let data = unsafe { &mut *self.data_cell.get() };
                if let Err(e) = f(data) {
                    self.change_phase(current_phase_cd, old_phase_cd);
                    Err(PhasedError::with_source(
                        u8_to_phase(current_phase_cd),
                        PhasedErrorKind::FailToRunClosureDuringTransitionToCleanup,
                        e,
                    ))
                } else {
                    self.change_phase(current_phase_cd, PHASE_CLEANUP);
                    Ok(())
                }
            }
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
                let data = unsafe { &mut *self.data_cell.get() };
                if let Err(e) = f(data) {
                    self.change_phase(PHASE_SETUP_TO_READ, old_phase_cd);
                    Err(PhasedError::with_source(
                        u8_to_phase(PHASE_SETUP_TO_READ),
                        PhasedErrorKind::FailToRunClosureDuringTransitionToRead,
                        e,
                    ))
                } else {
                    self.change_phase(PHASE_SETUP_TO_READ, PHASE_READ);
                    Ok(())
                }
            }
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

    #[allow(clippy::mut_from_ref)]
    pub fn get_mut(&self) -> Result<&mut T, PhasedError> {
        match self.phase.load(atomic::Ordering::Acquire) {
            PHASE_READ => Err(PhasedError::new(
                u8_to_phase(PHASE_READ),
                PhasedErrorKind::CannotCallOnPhaseRead(Self::method_name("get_mut")),
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
            _ => {
                let data = unsafe { &mut *self.data_cell.get() };
                Ok(data)
            }
        }
    }

    #[inline]
    fn method_name(m: &str) -> String {
        format!("{}::{}", any::type_name::<PhasedCell<T>>(), m)
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
mod tests_of_phased_cell {
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
        let cell = PhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_properly(), Phase::Setup);

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_properly(), Phase::Read);

        if let Err(e) = cell.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);
    }

    #[test]
    fn transition_from_setup_to_cleanup() {
        let cell = PhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_properly(), Phase::Setup);

        if let Err(e) = cell.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);
    }

    #[test]
    fn update_internal_data_in_setup_and_cleanup_phases() {
        let cell = PhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_properly(), Phase::Setup);

        match cell.get_mut() {
            Ok(data) => data.add("Hello".to_string()),
            Err(e) => panic!("{e:?}"),
        }

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|data| {
            data.add("World".to_string());
            Ok::<(), MyError>(())
        }) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_properly(), Phase::Read);

        if let Ok(data) = cell.read_fast() {
            assert_eq!(&data.vec, &["Hello".to_string(), "World".to_string()]);
        } else {
            panic!();
        }

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);

        if let Ok(data) = cell.get_mut() {
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
    }

    #[test]
    fn read_fast_internal_data_in_multi_threads() {
        let cell = PhasedCell::new(MyStruct::new());

        match cell.get_mut() {
            Ok(data) => data.add("Hello".to_string()),
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
        if let Err(e) = cell.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);

        if let Ok(data) = cell.get_mut() {
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
    fn read_properly_internal_data_in_multi_threads() {
        let cell = PhasedCell::new(MyStruct::new());

        match cell.get_mut() {
            Ok(data) => data.add("Hello".to_string()),
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
                let data = cell_clone.read_properly().unwrap();
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
        if let Err(e) = cell.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);

        if let Ok(data) = cell.get_mut() {
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
        let cell = PhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell.read_fast() {
            assert_eq!(e.phase, Phase::Setup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phased_cell::MyStruct>::read_fast".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_properly(), Phase::Setup);
    }

    #[test]
    fn fail_to_read_fast_if_phase_is_cleanup() {
        let cell = PhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.read_fast() {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phased_cell::MyStruct>::read_fast".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_read_properly_if_phase_is_setup() {
        let cell = PhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_properly(), Phase::Setup);

        if let Err(e) = cell.read_properly() {
            assert_eq!(e.phase, Phase::Setup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phased_cell::MyStruct>::read_properly".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_properly(), Phase::Setup);
    }

    #[test]
    fn fail_to_read_properly_if_phase_is_cleanup() {
        let cell = PhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_properly(), Phase::Setup);

        if let Err(e) = cell.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.read_properly() {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phased_cell::MyStruct>::read_properly".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_get_mut_if_phase_is_read() {
        let cell = PhasedCell::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.get_mut() {
            assert_eq!(e.phase, Phase::Read);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallOnPhaseRead("setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phased_cell::MyStruct>::get_mut".to_string()),
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_properly(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_read_if_phase_is_read() {
        let cell = PhasedCell::new(MyStruct::new());

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
        assert_eq!(cell.phase_properly(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_read_if_phase_is_cleanup() {
        let cell = PhasedCell::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(e.kind, PhasedErrorKind::TransitionToReadFailed);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_cleanup_if_phase_is_cleanup() {
        let cell = PhasedCell::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(e.kind, PhasedErrorKind::PhaseIsAlreadyCleanup);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_get_mut_during_transition_to_read() {
        let cell = PhasedCell::new(MyStruct::new());

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

        if let Err(e) = cell.get_mut() {
            assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToRead);
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_properly(), Phase::Read);
    }

    #[test]
    fn fail_to_get_mut_during_transition_to_cleanup_from_setup() {
        let cell = PhasedCell::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(|_data| {
                std::thread::sleep(std::time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(std::time::Duration::from_millis(100));

        if let Err(e) = cell.get_mut() {
            assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToCleanup);
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_get_mut_during_transition_to_cleanup_from_read() {
        let cell = PhasedCell::new(MyStruct::new());

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(|_data| {
                std::thread::sleep(std::time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(std::time::Duration::from_millis(100));

        if let Err(e) = cell.get_mut() {
            assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToCleanup);
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).join();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_read_because_of_failure_of_closure() {
        let cell = PhasedCell::new(MyStruct::new());

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
        assert_eq!(cell.phase_properly(), Phase::Setup);
    }

    #[test]
    fn fail_to_transition_to_read_during_transition_to_read() {
        let cell = PhasedCell::new(MyStruct::new());

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
        assert_eq!(cell.phase_properly(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_read_during_transition_to_cleanup() {
        let cell = PhasedCell::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(|_data| {
                std::thread::sleep(std::time::Duration::from_secs(1));
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
        assert_eq!(cell.phase_properly(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_cleanup_during_transition_to_read() {
        let cell = PhasedCell::new(MyStruct::new());

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
            if let Err(e) = cell_clone.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
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
        assert_eq!(cell.phase_properly(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_cleanup_during_transition_to_cleanup_from_setup() {
        let cell = PhasedCell::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(|_data| {
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
            if let Err(e) = cell_clone.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
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
        assert_eq!(cell.phase_properly(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_cleanup_during_transition_to_cleanup_from_read() {
        let cell = PhasedCell::new(MyStruct::new());

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(|_data| {
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
            if let Err(e) = cell_clone.transition_to_cleanup(|_data| Ok::<(), MyError>(())) {
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
        assert_eq!(cell.phase_properly(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_cleanup_from_setup_because_of_failure_of_closure() {
        let cell = PhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_properly(), Phase::Setup);

        if let Err(e) = cell.transition_to_cleanup(|_data| Err(MyError {})) {
            assert_eq!(format!("{:?}", e), "setup_read_cleanup::PhasedError { phase: Cleanup, kind: FailToRunClosureDuringTransitionToCleanup, source: MyError }");
        } else {
            panic!();
        }
        assert_eq!(cell.phase_properly(), Phase::Setup);
    }

    #[test]
    fn fail_to_transition_to_cleanup_from_read_because_of_failure_of_closure() {
        let cell = PhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_properly(), Phase::Setup);

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_properly(), Phase::Read);

        if let Err(e) = cell.transition_to_cleanup(|_data| Err(MyError {})) {
            assert_eq!(format!("{:?}", e), "setup_read_cleanup::PhasedError { phase: Cleanup, kind: FailToRunClosureDuringTransitionToCleanup, source: MyError }");
        } else {
            panic!();
        }
        assert_eq!(cell.phase_properly(), Phase::Read);
    }
}
