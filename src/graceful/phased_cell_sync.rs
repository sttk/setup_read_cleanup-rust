// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use super::GracefulPhasedCellSync;
use crate::phase::*;
use crate::{Phase, PhasedError, PhasedErrorKind, StdMutexGuard};

use std::{any, cell, error, marker, sync, sync::atomic, time};

const MAX_READ_COUNT: usize = (isize::MAX) as usize;

unsafe impl<T: Send + Sync> Sync for GracefulPhasedCellSync<T> {}
unsafe impl<T: Send + Sync> Send for GracefulPhasedCellSync<T> {}

impl<T: Send + Sync> GracefulPhasedCellSync<T> {
    /// Creates a new `GracefulPhasedCellSync` in the `Setup` phase, containing the provided data.
    ///
    /// # Examples
    ///
    /// ```
    /// use setup_read_cleanup::graceful::GracefulPhasedCellSync;
    ///
    /// let cell = GracefulPhasedCellSync::new(10);
    /// ```
    pub const fn new(data: T) -> Self {
        Self {
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            graceful_counter: atomic::AtomicUsize::new(0),
            graceful_condvar: sync::Condvar::new(),
            graceful_mutex: sync::Mutex::new(()),
            data_mutex: sync::Mutex::<Option<T>>::new(Some(data)),
            data_cell: cell::UnsafeCell::<Option<T>>::new(None),
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
    /// use setup_read_cleanup::{Phase, graceful::GracefulPhasedCellSync};
    ///
    /// let cell = GracefulPhasedCellSync::new(10);
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
    /// use setup_read_cleanup::{Phase, graceful::GracefulPhasedCellSync};
    ///
    /// let cell = GracefulPhasedCellSync::new(10);
    /// assert_eq!(cell.phase(), Phase::Setup);
    /// ```
    pub fn phase(&self) -> Phase {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        u8_to_phase(phase)
    }

    /// Returns a reference to the contained data with relaxed memory ordering.
    ///
    /// This method attempts to return a reference to the contained data immediately without
    /// waiting.
    /// It is only successful if the cell is in the `Read` phase. It increments the internal counter
    /// to track active readers for graceful cleanup.
    /// It provides weaker memory ordering guarantees.
    ///
    /// # Errors
    ///
    /// Returns an error if the cell is not in the `Read` phase or the data is unavailable.
    pub fn read_relaxed(&self) -> Result<&T, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Relaxed);
        if phase != PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseRead("read_relaxed"),
            ));
        }

        if let Some(data) = unsafe { &*self.data_cell.get() }.as_ref() {
            self.count_up("read_relaxed");
            Ok(data)
        } else {
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataUnavailable,
            ))
        }
    }

    /// Returns a reference to the contained data with acquire memory ordering.
    ///
    /// This method attempts to return a reference to the contained data immediately without
    /// waiting.
    /// It is only successful if the cell is in the `Read` phase.
    /// If the cell is in a `Setup -> Read` transition, this method will wait until
    /// the transition is complete and the cell enters the `Read` phase.
    /// It increments the internal counter to track active readers for graceful cleanup.
    /// It provides stronger memory ordering guarantees than `read_relaxed`.
    ///
    /// # Errors
    ///
    /// Returns an error if the cell is not in the `Read` phase (after waiting, if applicable) or the data is unavailable.
    pub fn read(&self) -> Result<&T, PhasedError> {
        match self.phase.load(atomic::Ordering::Acquire) {
            PHASE_READ => {}
            PHASE_SETUP_TO_READ => {
                self.wait_for_read_phase()?;
            }
            phase => {
                return Err(PhasedError::new(
                    u8_to_phase(phase),
                    PhasedErrorKind::CannotCallUnlessPhaseRead("read"),
                ));
            }
        }

        if let Some(data) = unsafe { &*self.data_cell.get() }.as_ref() {
            self.count_up("read");
            Ok(data)
        } else {
            let phase = self.phase.load(atomic::Ordering::Acquire);
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataUnavailable,
            ))
        }
    }

    /// Signals that a read operation has finished.
    ///
    /// This decrements the internal counter of active readers.
    pub fn finish_reading(&self) {
        self.count_down("finish_reading");
    }

    /// Transitions the cell to the `Cleanup` phase gracefully.
    ///
    /// This method can be called from either the `Setup` or the `Read` phase.
    /// It waits for all active read operations to complete before executing
    /// the provided closure `f` and moving to the `Cleanup` phase.
    ///
    /// # Errors
    ///
    /// Returns an error if the wait times out, the phase transition fails, the mutex
    /// is poisoned, or the closure returns an error.
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
            Ok(PHASE_READ) => {
                let result_w = self.wait_for_cleanup_phase(timeout);
                if let Ok(mut guard) = self.data_mutex.lock() {
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
                            Err(e)
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
                } else {
                    self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);
                    Err(PhasedError::new(
                        u8_to_phase(PHASE_READ_TO_CLEANUP),
                        PhasedErrorKind::InternalDataMutexIsPoisoned,
                    ))
                }
            }
            Ok(_ /* PHASE_SETUP */) => {
                if let Ok(mut data_opt) = self.data_mutex.lock() {
                    if data_opt.is_some() {
                        let result_f = f(data_opt.as_mut().unwrap());
                        self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);
                        if let Err(e) = result_f {
                            Err(PhasedError::with_source(
                                u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                                PhasedErrorKind::FailToRunClosureDuringTransitionToCleanup,
                                e,
                            ))
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
                } else {
                    self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);
                    Err(PhasedError::new(
                        u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                        PhasedErrorKind::InternalDataMutexIsPoisoned,
                    ))
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
            Err(old_phase) => Err(PhasedError::new(
                u8_to_phase(old_phase),
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
    /// Returns an error if the cell is not in the `Setup` phase, the mutex is
    /// poisoned, or if the closure returns an error.
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
            Ok(old_phase) => match self.data_mutex.lock() {
                Ok(mut data_opt) => {
                    if data_opt.is_some() {
                        if let Err(e) = f(data_opt.as_mut().unwrap()) {
                            self.change_phase(PHASE_SETUP_TO_READ, old_phase);
                            drop(data_opt);
                            self.notify_read_phase()?;
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
                            drop(data_opt);
                            self.notify_read_phase()?;
                            Ok(())
                        }
                    } else {
                        // impossible case
                        self.change_phase(PHASE_SETUP_TO_READ, old_phase);
                        drop(data_opt);
                        self.notify_read_phase()?;
                        Err(PhasedError::new(
                            u8_to_phase(PHASE_SETUP_TO_READ),
                            PhasedErrorKind::InternalDataUnavailable,
                        ))
                    }
                }
                Err(_e) => {
                    self.change_phase(PHASE_SETUP_TO_READ, old_phase);
                    self.notify_read_phase()?;
                    Err(PhasedError::new(
                        u8_to_phase(old_phase),
                        PhasedErrorKind::InternalDataMutexIsPoisoned,
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
            Err(old_phase) => Err(PhasedError::new(
                u8_to_phase(old_phase),
                PhasedErrorKind::DuringTransitionToCleanup,
            )),
        }
    }

    /// Locks the cell and returns a guard that allows mutable access to the data.
    ///
    /// This method is only successful if the cell is in the `Setup` or `Cleanup` phase.
    /// The returned guard releases the lock when it is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the cell is in the `Read` phase, is transitioning,
    /// or if the mutex is poisoned.
    pub fn lock(&self) -> Result<StdMutexGuard<'_, T>, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        match phase {
            PHASE_READ => {
                return Err(PhasedError::new(
                    u8_to_phase(PHASE_READ),
                    PhasedErrorKind::CannotCallOnPhaseRead("lock"),
                ));
            }
            PHASE_SETUP_TO_READ => {
                return Err(PhasedError::new(
                    u8_to_phase(PHASE_SETUP_TO_READ),
                    PhasedErrorKind::DuringTransitionToRead,
                ));
            }
            PHASE_SETUP_TO_CLEANUP => {
                return Err(PhasedError::new(
                    u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                    PhasedErrorKind::DuringTransitionToCleanup,
                ));
            }
            PHASE_READ_TO_CLEANUP => {
                return Err(PhasedError::new(
                    u8_to_phase(PHASE_READ_TO_CLEANUP),
                    PhasedErrorKind::DuringTransitionToCleanup,
                ));
            }
            _ => {}
        };

        if let Ok(guarded_opt) = self.data_mutex.lock() {
            match self.phase.load(atomic::Ordering::Acquire) {
                PHASE_READ => {
                    return Err(PhasedError::new(
                        u8_to_phase(PHASE_READ),
                        PhasedErrorKind::CannotCallOnPhaseRead("lock"),
                    ));
                }
                PHASE_SETUP_TO_READ => {
                    return Err(PhasedError::new(
                        u8_to_phase(PHASE_SETUP_TO_READ),
                        PhasedErrorKind::DuringTransitionToRead,
                    ));
                }
                PHASE_SETUP_TO_CLEANUP => {
                    return Err(PhasedError::new(
                        u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                        PhasedErrorKind::DuringTransitionToCleanup,
                    ));
                }
                PHASE_READ_TO_CLEANUP => {
                    return Err(PhasedError::new(
                        u8_to_phase(PHASE_READ_TO_CLEANUP),
                        PhasedErrorKind::DuringTransitionToCleanup,
                    ));
                }
                _ => {}
            };

            if let Some(new_guard) = StdMutexGuard::try_new(guarded_opt) {
                Ok(new_guard)
            } else {
                Err(PhasedError::new(
                    u8_to_phase(phase),
                    PhasedErrorKind::InternalDataUnavailable,
                ))
            }
        } else {
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataMutexIsPoisoned,
            ))
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

    fn wait_for_read_phase(&self) -> Result<(), PhasedError> {
        let started = time::Instant::now();

        let mut guard = match self.graceful_mutex.lock() {
            Ok(guard) => guard,
            Err(_) => {
                return Err(PhasedError::new(
                    u8_to_phase(PHASE_SETUP_TO_READ),
                    PhasedErrorKind::GracefulWaitMutexIsPoisoned,
                ));
            }
        };

        const TIMEOUT: time::Duration = time::Duration::from_millis(300);
        loop {
            match self.phase.load(atomic::Ordering::Acquire) {
                PHASE_READ => {
                    return Ok(());
                }
                PHASE_SETUP_TO_READ => {}
                phase => {
                    return Err(PhasedError::new(
                        u8_to_phase(phase),
                        PhasedErrorKind::CannotCallUnlessPhaseRead("read"),
                    ));
                }
            };

            let elapsed = started.elapsed();
            if elapsed >= TIMEOUT {
                return Err(PhasedError::new(
                    u8_to_phase(PHASE_SETUP_TO_READ),
                    PhasedErrorKind::GracefulWaitTimeout(TIMEOUT),
                ));
            }

            if let Ok(r) = self.graceful_condvar.wait_timeout(guard, TIMEOUT - elapsed) {
                guard = r.0;
            } else {
                return Err(PhasedError::new(
                    u8_to_phase(PHASE_SETUP_TO_READ),
                    PhasedErrorKind::GracefulWaitMutexIsPoisoned,
                ));
            }
        }
    }

    fn notify_read_phase(&self) -> Result<(), PhasedError> {
        if let Ok(mut _unit) = self.graceful_mutex.lock() {
            self.graceful_condvar.notify_all();
            Ok(())
        } else {
            Err(PhasedError::new(
                u8_to_phase(PHASE_SETUP_TO_READ),
                PhasedErrorKind::GracefulWaitMutexIsPoisoned,
            ))
        }
    }

    fn count_up(&self, method: &str) {
        let prev = self.graceful_counter.fetch_add(1, atomic::Ordering::AcqRel);
        assert!(
            prev < MAX_READ_COUNT,
            "{}::{} : read counter overflow",
            any::type_name::<GracefulPhasedCellSync<T>>(),
            method,
        );
    }

    fn count_down(&self, method: &str) {
        match self.graceful_counter.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |c| {
                if c == 0 {
                    None
                } else {
                    Some(c - 1)
                }
            },
        ) {
            Ok(1) => {
                if let Ok(mut _unit) = self.graceful_mutex.lock() {
                    if self.phase.load(atomic::Ordering::Acquire) == PHASE_READ_TO_CLEANUP {
                        self.graceful_condvar.notify_all();
                    }
                }
            }
            Ok(_) => {}
            Err(_) => {
                eprintln!(
                    "{}::{} is called excessively.",
                    any::type_name::<GracefulPhasedCellSync<T>>(),
                    method,
                );
            }
        }
    }

    fn wait_for_cleanup_phase(&self, timeout: time::Duration) -> Result<(), PhasedError> {
        let started = time::Instant::now();

        let mut guard = match self.graceful_mutex.lock() {
            Ok(guard) => guard,
            Err(_) => {
                let phase = self.phase.load(atomic::Ordering::Acquire);
                return Err(PhasedError::new(
                    u8_to_phase(phase),
                    PhasedErrorKind::GracefulWaitMutexIsPoisoned,
                ));
            }
        };

        loop {
            if self.graceful_counter.load(atomic::Ordering::Acquire) == 0 {
                return Ok(());
            }

            let elapsed = started.elapsed();
            if elapsed >= timeout {
                let phase = self.phase.load(atomic::Ordering::Acquire);
                return Err(PhasedError::new(
                    u8_to_phase(phase),
                    PhasedErrorKind::GracefulWaitTimeout(timeout),
                ));
            }

            if let Ok(result) = self.graceful_condvar.wait_timeout(guard, timeout - elapsed) {
                guard = result.0;
            } else {
                let phase = self.phase.load(atomic::Ordering::Acquire);
                return Err(PhasedError::new(
                    u8_to_phase(phase),
                    PhasedErrorKind::GracefulWaitMutexIsPoisoned,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests_of_phased_cell_sync {
    use super::*;
    use std::error::Error;
    use std::fmt;
    use std::sync::Arc;
    use std::time;

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
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
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
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
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
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
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
    fn fail_to_lock_if_phase_is_read() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell.transition_to_read(|_data| Ok::<(), MyError>(())) {
            panic!("{e:?}");
        }

        if let Err(e) = cell.lock() {
            assert_eq!(e.phase(), Phase::Read);
            assert_eq!(e.kind(), PhasedErrorKind::CannotCallOnPhaseRead("lock"),);
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
            assert_eq!(e.phase(), Phase::Cleanup);
            assert_eq!(e.kind(), PhasedErrorKind::PhaseIsAlreadyCleanup);
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
    fn fail_to_transition_to_read_if_closure_causes_an_error() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());

        if let Err(e) = cell.transition_to_read(|_data| {
            std::thread::sleep(std::time::Duration::from_secs(1));
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
                match e.kind() {
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

    #[test]
    fn read_waits_for_transition() {
        let cell = Arc::new(GracefulPhasedCellSync::new(MyStruct::new()));
        assert_eq!(cell.phase(), Phase::Setup);

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            cell_clone
                .transition_to_read(|_data| {
                    std::thread::sleep(time::Duration::from_millis(100));
                    Ok::<(), MyError>(())
                })
                .unwrap();
        });

        std::thread::sleep(time::Duration::from_millis(10));

        let data = cell.read().unwrap();
        assert_eq!(cell.phase(), Phase::Read);
        assert_eq!(data.vec.len(), 0);
        cell.finish_reading();

        handler.join().unwrap();
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[test]
    fn read_on_read_phase() {
        let cell = GracefulPhasedCellSync::new(MyStruct::new());
        cell.transition_to_read(|_| Ok::<(), MyError>(())).unwrap();
        assert_eq!(cell.phase(), Phase::Read);

        let data = cell.read().unwrap();
        assert_eq!(data.vec.len(), 0);
        cell.finish_reading();
    }

    #[test]
    fn read_returns_error_if_transition_fails() {
        let cell = Arc::new(GracefulPhasedCellSync::new(MyStruct::new()));
        assert_eq!(cell.phase(), Phase::Setup);

        let cell_clone = Arc::clone(&cell);
        let handler = std::thread::spawn(move || {
            cell_clone
                .transition_to_read(|_data| {
                    std::thread::sleep(time::Duration::from_millis(100));
                    Err(MyError {})
                })
                .unwrap_err();
        });

        std::thread::sleep(time::Duration::from_millis(10));

        if let Err(e) = cell.read() {
            assert_eq!(e.phase(), Phase::Setup);
            assert_eq!(e.kind(), PhasedErrorKind::CannotCallUnlessPhaseRead("read"));
        } else {
            panic!();
        }

        handler.join().unwrap();
        assert_eq!(cell.phase(), Phase::Setup);
    }
}
