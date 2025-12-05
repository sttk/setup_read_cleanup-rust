// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use super::{GracefulPhasedCell, GracefulWaitErrorKind, GracefulWaitSync};
use crate::phase::*;
use crate::{Phase, PhasedError, PhasedErrorKind};

use std::{cell, error, marker, sync::atomic, time};

unsafe impl<T: Send + Sync> Sync for GracefulPhasedCell<T> {}
unsafe impl<T: Send + Sync> Send for GracefulPhasedCell<T> {}

impl<T: Send + Sync> GracefulPhasedCell<T> {
    /// Creates a new `GracefulPhasedCell` in the `Setup` phase, containing the provided data.
    ///
    /// # Examples
    ///
    /// ```
    /// use setup_read_cleanup::graceful::GracefulPhasedCell;
    ///
    /// let cell = GracefulPhasedCell::new(10);
    /// ```
    pub const fn new(data: T) -> Self {
        Self {
            wait: GracefulWaitSync::new(),
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            data_cell: cell::UnsafeCell::new(data),
            _marker: marker::PhantomData,
        }
    }

    /// Returns the current phase of the cell with relaxed memory ordering.
    ///
    /// This method is faster than `phase` but provides weaker memory ordering guarantees.
    ///
    /// # Examples
    ///
    /// ```
    /// use setup_read_cleanup::{Phase, graceful::GracefulPhasedCell};
    ///
    /// let cell = GracefulPhasedCell::new(10);
    /// assert_eq!(cell.phase_relaxed(), Phase::Setup);
    /// ```
    pub fn phase_relaxed(&self) -> Phase {
        let phase = self.phase.load(atomic::Ordering::Relaxed);
        u8_to_phase(phase)
    }

    /// Returns the current phase of the cell with acquire memory ordering.
    ///
    /// This method provides stronger memory ordering guarantees than `phase_relaxed`.
    ///
    /// # Examples
    ///
    /// ```
    /// use setup_read_cleanup::{Phase, graceful::GracefulPhasedCell};
    ///
    /// let cell = GracefulPhasedCell::new(10);
    /// assert_eq!(cell.phase(), Phase::Setup);
    /// ```
    pub fn phase(&self) -> Phase {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        u8_to_phase(phase)
    }

    /// Returns a reference to the contained data with relaxed memory ordering.
    ///
    /// This method is only successful if the cell is in the `Read` phase.
    /// It increments the internal counter to track active readers for graceful cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error if the cell is not in the `Read` phase.
    pub fn read_relaxed(&self) -> Result<&T, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Relaxed);
        if phase != PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseRead("read_relaxed"),
            ));
        }

        let data = unsafe { &*self.data_cell.get() };
        self.wait.count_up();
        Ok(data)
    }

    /// Returns a reference to the contained data with acquire memory ordering.
    ///
    /// This method is only successful if the cell is in the `Read` phase.
    /// It increments the internal counter to track active readers for graceful cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error if the cell is not in the `Read` phase.
    pub fn read(&self) -> Result<&T, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase != PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseRead("read"),
            ));
        }

        let data = unsafe { &*self.data_cell.get() };
        self.wait.count_up();
        Ok(data)
    }

    /// Signals that a read operation has finished.
    ///
    /// This decrements the internal counter of active readers.
    pub fn finish_reading(&self) {
        self.wait
            .count_down(|| self.phase.load(atomic::Ordering::Acquire) == PHASE_READ_TO_CLEANUP);
    }

    /// Transitions the cell to the `Cleanup` phase gracefully.
    ///
    /// This method waits for all active read operations to complete before executing
    /// the provided closure `f` and moving to the `Cleanup` phase.
    ///
    /// # Errors
    ///
    /// Returns an error if the wait times out, the phase transition fails, or the closure returns an error.
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
            Ok(old_phase_cd) => {
                let result_w = self.wait.wait_gracefully(timeout);
                let current_phase_cd = match old_phase_cd {
                    PHASE_READ => PHASE_READ_TO_CLEANUP,
                    _ => PHASE_SETUP_TO_CLEANUP,
                };
                let data = unsafe { &mut *self.data_cell.get() };
                let result_f = f(data);
                self.change_phase(current_phase_cd, PHASE_CLEANUP);
                if let Err(e) = result_f {
                    Err(PhasedError::with_source(
                        u8_to_phase(current_phase_cd),
                        PhasedErrorKind::FailToRunClosureDuringTransitionToCleanup,
                        e,
                    ))
                } else if let Err(e) = result_w {
                    match e.kind {
                        GracefulWaitErrorKind::TimedOut(_) => Err(PhasedError::new(
                            u8_to_phase(current_phase_cd),
                            PhasedErrorKind::GracefulWaitTimeout(timeout),
                        )),
                        GracefulWaitErrorKind::MutexIsPoisoned => Err(PhasedError::new(
                            u8_to_phase(current_phase_cd),
                            PhasedErrorKind::StdMutexIsPoisoned,
                        )),
                    }
                } else {
                    Ok(())
                }
            }
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

    /// Transitions the cell from the `Setup` phase to the `Read` phase.
    ///
    /// This method takes a closure `f` which is executed on the contained data
    /// during the transition.
    ///
    /// # Errors
    ///
    /// Returns an error if the cell is not in the `Setup` phase or if the closure
    /// returns an error.
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

    /// Returns a mutable reference to the contained data without acquiring a lock.
    ///
    /// This method is only successful if the cell is in the `Setup` or `Cleanup` phase.
    ///
    /// # Safety
    ///
    /// This method does not use a lock to protect the data and is therefore **not thread-safe**.
    /// It must only be called when you can guarantee that no other thread is accessing the
    /// `GracefulPhasedCell`. Concurrent access can lead to data races and undefined behavior.
    /// If you need to share the cell across threads and mutate it, use `GracefulPhasedCellSync` instead.
    #[allow(clippy::mut_from_ref)]
    pub fn get_mut_unlocked(&self) -> Result<&mut T, PhasedError> {
        match self.phase.load(atomic::Ordering::Acquire) {
            PHASE_READ => Err(PhasedError::new(
                u8_to_phase(PHASE_READ),
                PhasedErrorKind::CannotCallOnPhaseRead("get_mut_unlocked"),
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
    impl std::error::Error for MyError {}

    #[test]
    fn transition_from_setup_to_read_then_cleanup() {
        let cell = GracefulPhasedCell::new(MyStruct::new());
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
        let cell = GracefulPhasedCell::new(MyStruct::new());
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
        let cell = GracefulPhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);

        match cell.get_mut_unlocked() {
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
        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);

        if let Ok(data) = cell.read_relaxed() {
            assert_eq!(&data.vec, &["Hello".to_string(), "World".to_string()]);
        } else {
            panic!();
        }
        cell.finish_reading();

        // Read -> Cleanup
        if let Err(e) =
            cell.transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);

        if let Ok(data) = cell.get_mut_unlocked() {
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
    fn read_relaxed_internal_data_in_multi_threads() {
        let cell = GracefulPhasedCell::new(MyStruct::new());

        match cell.get_mut_unlocked() {
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
                let data = cell_clone.read_relaxed().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                std::thread::sleep(time::Duration::from_secs(1));
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
        if let Err(e) =
            cell.transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);

        if let Ok(data) = cell.get_mut_unlocked() {
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
    fn fail_to_read_relaxed_if_phase_is_setup() {
        let cell = GracefulPhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);

        if let Err(e) = cell.read_relaxed() {
            assert_eq!(e.phase(), Phase::Setup);
            assert_eq!(
                e.kind(),
                PhasedErrorKind::CannotCallUnlessPhaseRead("read_relaxed"),
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);
    }

    #[test]
    fn fail_to_read_relaxed_if_phase_is_cleanup() {
        let cell = GracefulPhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);

        if let Err(e) =
            cell.transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
        {
            panic!("{e:?}");
        }

        if let Err(e) = cell.read_relaxed() {
            assert_eq!(e.phase(), Phase::Cleanup);
            assert_eq!(
                e.kind(),
                PhasedErrorKind::CannotCallUnlessPhaseRead("read_relaxed"),
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_read_if_phase_is_setup() {
        let cell = GracefulPhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell.read() {
            assert_eq!(e.phase(), Phase::Setup);
            assert_eq!(e.kind(), PhasedErrorKind::CannotCallUnlessPhaseRead("read"),);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);
    }

    #[test]
    fn fail_to_read_if_phase_is_cleanup() {
        let cell = GracefulPhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) =
            cell.transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
        {
            panic!("{e:?}");
        }

        if let Err(e) = cell.read() {
            assert_eq!(e.phase(), Phase::Cleanup);
            assert_eq!(e.kind(), PhasedErrorKind::CannotCallUnlessPhaseRead("read"),);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_get_mut_unlocked_if_phase_is_read() {
        let cell = GracefulPhasedCell::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.get_mut_unlocked() {
            assert_eq!(e.phase(), Phase::Read);
            assert_eq!(
                e.kind(),
                PhasedErrorKind::CannotCallOnPhaseRead("get_mut_unlocked"),
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_read_if_phase_is_read() {
        let cell = GracefulPhasedCell::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            assert_eq!(e.phase(), Phase::Read);
            assert_eq!(e.kind(), PhasedErrorKind::PhaseIsAlreadyRead);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[test]
    fn fail_to_transition_to_read_if_phase_is_cleanup() {
        let cell = GracefulPhasedCell::new(MyStruct::new());

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
            assert_eq!(e.phase(), Phase::Cleanup);
            assert_eq!(e.kind(), PhasedErrorKind::PhaseIsAlreadyCleanup);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_transition_to_cleanup_if_phase_is_cleanup() {
        let cell = GracefulPhasedCell::new(MyStruct::new());

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
            assert_eq!(e.phase(), Phase::Cleanup);
            assert_eq!(e.kind(), PhasedErrorKind::PhaseIsAlreadyCleanup);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[test]
    fn fail_to_get_mut_unlocked_during_transition_to_read() {
        let cell = GracefulPhasedCell::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_read(|_data| {
                std::thread::sleep(time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(time::Duration::from_millis(100));

        if let Err(e) = cell.get_mut_unlocked() {
            assert_eq!(e.kind(), PhasedErrorKind::DuringTransitionToRead);
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
    fn fail_to_get_mut_unlocked_during_transition_to_cleanup_from_setup() {
        let cell = GracefulPhasedCell::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) =
                cell_clone.transition_to_cleanup(time::Duration::from_secs(0), |_data| {
                    std::thread::sleep(time::Duration::from_secs(5));
                    Ok::<(), MyError>(())
                })
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(time::Duration::from_millis(100));

        if let Err(e) = cell.get_mut_unlocked() {
            assert_eq!(e.kind(), PhasedErrorKind::DuringTransitionToCleanup);
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
    fn fail_to_get_mut_unlocked_during_transition_to_cleanup_from_read() {
        let cell = GracefulPhasedCell::new(MyStruct::new());

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(time::Duration::ZERO, |_data| {
                std::thread::sleep(time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(time::Duration::from_millis(100));

        if let Err(e) = cell.get_mut_unlocked() {
            assert_eq!(e.kind(), PhasedErrorKind::DuringTransitionToCleanup);
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
    fn fail_to_transition_to_read_because_of_failure_of_closure() {
        let cell = GracefulPhasedCell::new(MyStruct::new());

        if let Err(e) = cell.transition_to_read(|_data| {
            std::thread::sleep(time::Duration::from_secs(1));
            Err(MyError {})
        }) {
            match e.kind() {
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
        let cell = GracefulPhasedCell::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();
        for _i in 0..2 {
            let cell_clone = Arc::clone(&cell);
            let handler = std::thread::spawn(move || {
                if let Err(e) = cell_clone.transition_to_read(|_data| {
                    std::thread::sleep(time::Duration::from_secs(1));
                    Ok::<(), MyError>(())
                }) {
                    match e.kind() {
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
        let cell = GracefulPhasedCell::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(time::Duration::ZERO, |_data| {
                std::thread::sleep(time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_read(|_data| Ok::<(), MyError>(())) {
                match e.kind() {
                    PhasedErrorKind::PhaseIsAlreadyCleanup => {}
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
        let cell = GracefulPhasedCell::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_read(|_data| {
                std::thread::sleep(time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(time::Duration::from_millis(100));

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone
                .transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
            {
                assert_eq!(e.kind(), PhasedErrorKind::DuringTransitionToRead);
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
        let cell = GracefulPhasedCell::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(time::Duration::ZERO, |_data| {
                std::thread::sleep(time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(time::Duration::from_millis(100));

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone
                .transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
            {
                assert_eq!(e.kind(), PhasedErrorKind::DuringTransitionToCleanup);
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
        let cell = GracefulPhasedCell::new(MyStruct::new());

        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<std::thread::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone.transition_to_cleanup(time::Duration::ZERO, |_data| {
                std::thread::sleep(time::Duration::from_secs(1));
                Ok::<(), MyError>(())
            }) {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        std::thread::sleep(time::Duration::from_millis(100));

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            if let Err(e) = cell_clone
                .transition_to_cleanup(time::Duration::ZERO, |_data| Ok::<(), MyError>(()))
            {
                assert_eq!(e.kind(), PhasedErrorKind::DuringTransitionToCleanup);
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
        let cell = GracefulPhasedCell::new(MyStruct::new());
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
        let cell = GracefulPhasedCell::new(MyStruct::new());
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
        let cell = GracefulPhasedCell::new(MyStruct::new());

        match cell.get_mut_unlocked() {
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
                let data = cell_clone.read_relaxed().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                std::thread::sleep(time::Duration::from_secs(1));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                cell_clone.finish_reading();
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
            join_handlers.push(handler);
        }

        std::thread::sleep(time::Duration::from_millis(100));

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

        if let Ok(data) = cell.get_mut_unlocked() {
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
        let cell = GracefulPhasedCell::new(MyStruct::new());

        match cell.get_mut_unlocked() {
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
        for i in 0..10 {
            let cell_clone = Arc::clone(&cell);
            let counter_clone = Arc::clone(&counter);
            let handler = std::thread::spawn(move || {
                let data = cell_clone.read_relaxed().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                std::thread::sleep(time::Duration::from_secs(i + 1));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                cell_clone.finish_reading();
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
            join_handlers.push(handler);
        }

        std::thread::sleep(time::Duration::from_millis(100));

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup(time::Duration::from_secs(1), |data| {
            assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
            data.add("!!".to_string());
            assert_eq!(data.vec.as_slice().join(", "), "Hello, World, !!");
            Ok::<(), MyError>(())
        }) {
            match e.kind() {
                PhasedErrorKind::GracefulWaitTimeout(tm) => {
                    assert_eq!(tm, time::Duration::from_secs(1));
                }
                _ => panic!(),
            }
            assert_eq!(e.phase(), Phase::Cleanup);
        } else {
            panic!();
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);

        if let Ok(data) = cell.get_mut_unlocked() {
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
