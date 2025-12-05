// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use super::{GracefulPhasedCellAsync, GracefulWaitAsync, GracefulWaitErrorKind};
use crate::phase::*;
use crate::{Phase, PhasedError, PhasedErrorKind, TokioMutexGuard};

use std::future::Future;
use std::pin::Pin;
use std::{cell, error, marker, sync::atomic};
use tokio::{sync, time};

unsafe impl<T: Send + Sync> Sync for GracefulPhasedCellAsync<T> {}
unsafe impl<T: Send + Sync> Send for GracefulPhasedCellAsync<T> {}

impl<T: Send + Sync> GracefulPhasedCellAsync<T> {
    /// Creates a new `GracefulPhasedCellAsync` in the `Setup` phase, containing the provided data.
    ///
    /// # Examples
    ///
    /// ```
    /// use setup_read_cleanup::graceful::GracefulPhasedCellAsync;
    ///
    /// let cell = GracefulPhasedCellAsync::new(10);
    /// ```
    pub const fn new(data: T) -> Self {
        Self {
            wait: GracefulWaitAsync::new(),
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            data_mutex: sync::Mutex::<Option<T>>::const_new(Some(data)),
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
    /// use setup_read_cleanup::{Phase, graceful::GracefulPhasedCellAsync};
    ///
    /// let cell = GracefulPhasedCellAsync::new(10);
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
    /// use setup_read_cleanup::{Phase, graceful::GracefulPhasedCellAsync};
    ///
    /// let cell = GracefulPhasedCellAsync::new(10);
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
            self.wait.count_up();
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
    /// It is only successful if the cell is in the `Read` phase. It increments the internal counter
    /// to track active readers for graceful cleanup.
    /// It provides stronger memory ordering guarantees than `read_relaxed`.
    ///
    /// # Errors
    ///
    /// Returns an error if the cell is not in the `Read` phase or the data is unavailable.
    pub fn read(&self) -> Result<&T, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase != PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseRead("read"),
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

    /// Signals that a read operation has finished.
    ///
    /// This decrements the internal counter of active readers.
    pub fn finish_reading(&self) {
        self.wait
            .count_down(|| self.phase.load(atomic::Ordering::Acquire) == PHASE_READ_TO_CLEANUP);
    }

    /// Asynchronously and gracefully transitions the cell to the `Cleanup` phase.
    ///
    /// This method waits for all active read operations to complete before executing
    /// the provided async closure `f` and moving to the `Cleanup` phase.
    ///
    /// # Errors
    ///
    /// Returns an error if the wait times out, the phase transition fails, or the closure returns
    /// an error.
    pub async fn transition_to_cleanup_async<F, E>(
        &self,
        timeout: time::Duration,
        mut f: F,
    ) -> Result<(), PhasedError>
    where
        F: FnMut(&mut T) -> Pin<Box<dyn Future<Output = Result<(), E>> + Send>>,
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
                let result_w = self.wait.wait_gracefully_async(timeout).await;
                let mut guard = self.data_mutex.lock().await;
                let data_opt = unsafe { &mut *self.data_cell.get() };
                if data_opt.is_some() {
                    let result_f = f(data_opt.as_mut().unwrap()).await;
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
                    self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);
                    Err(PhasedError::new(
                        u8_to_phase(PHASE_READ),
                        PhasedErrorKind::InternalDataUnavailable,
                    ))
                }
            }
            Ok(PHASE_SETUP) => {
                let mut data_opt = self.data_mutex.lock().await;
                let result_w = self.wait.wait_gracefully_async(timeout).await;
                if data_opt.is_some() {
                    let result_f = f(data_opt.as_mut().unwrap()).await;
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
                    self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);
                    Err(PhasedError::new(
                        u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                        PhasedErrorKind::InternalDataUnavailable,
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
            Err(old_phase_cd) => Err(PhasedError::new(
                u8_to_phase(old_phase_cd),
                PhasedErrorKind::DuringTransitionToCleanup,
            )),
            Ok(_) => Ok(()), // impossible case.
        }
    }

    /// Asynchronously transitions the cell from the `Setup` phase to the `Read` phase.
    ///
    /// This method takes an async closure `f` which is executed on the contained data
    /// during the transition.
    ///
    /// # Errors
    ///
    /// Returns an error if the cell is not in the `Setup` phase or if the closure
    /// returns an error.
    pub async fn transition_to_read_async<F, E>(&self, mut f: F) -> Result<(), PhasedError>
    where
        F: FnMut(&mut T) -> Pin<Box<dyn Future<Output = Result<(), E>> + Send>>,
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

    /// Asynchronously locks the cell and returns a guard that allows mutable access to the data.
    ///
    /// This method is only successful if the cell is in the `Setup` or `Cleanup` phase.
    /// The returned guard releases the lock when it is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the cell is in the `Read` phase or is transitioning.
    pub async fn lock_async(&self) -> Result<TokioMutexGuard<'_, T>, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        match phase {
            PHASE_READ => Err(PhasedError::new(
                u8_to_phase(PHASE_READ),
                PhasedErrorKind::CannotCallOnPhaseRead("lock_async"),
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
                let guarded_opt = self.data_mutex.lock().await;
                if let Some(new_guard) = TokioMutexGuard::try_new(guarded_opt) {
                    Ok(new_guard)
                } else {
                    Err(PhasedError::new(
                        u8_to_phase(phase),
                        PhasedErrorKind::InternalDataUnavailable,
                    ))
                }
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
mod tests_of_phased_cell_async {
    use super::*;
    use std::error::Error;
    use std::fmt;
    use std::sync::Arc;

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

    #[tokio::test]
    async fn transition_from_setup_to_read_then_cleanup() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);

        if let Err(e) = cell
            .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                Box::pin(async { Ok::<(), MyError>(()) })
            })
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn transition_from_setup_to_cleanup() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell
            .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                Box::pin(async { Ok::<(), MyError>(()) })
            })
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn update_internal_data_in_setup_and_cleanup_phases() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();
        for _i in 0..3 {
            let cell_clone = Arc::clone(&cell);
            let handler = tokio::spawn(async move {
                match cell_clone.lock_async().await {
                    Ok(mut data) => {
                        data.add("H".to_string());
                        data.add("e".to_string());
                        data.add("l".to_string());
                        data.add("l".to_string());
                        data.add("o".to_string());
                    }
                    Err(e) => panic!("{e:?}"),
                }
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).await {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        // Setup -> Read
        if let Err(e) = cell
            .transition_to_read_async(|data| {
                data.add(",".to_string());
                Box::pin(async { Ok::<(), MyError>(()) })
            })
            .await
        {
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
        if let Err(e) = cell
            .transition_to_cleanup_async(time::Duration::ZERO, |data| {
                data.add(" ** ".to_string());
                Box::pin(async { Ok::<(), MyError>(()) })
            })
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();
        for _i in 0..3 {
            let cell_clone = Arc::clone(&cell);
            let handler = tokio::task::spawn(async move {
                match cell_clone.lock_async().await {
                    Ok(mut data) => {
                        data.add("W".to_string());
                        data.add("o".to_string());
                        data.add("r".to_string());
                        data.add("l".to_string());
                        data.add("d".to_string());
                    }
                    Err(e) => panic!("{e:?}"),
                }
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).await {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        if let Ok(mut data) = cell.lock_async().await {
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
                ]
            );
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

    #[tokio::test]
    async fn read_relaxed_internal_data_in_multi_threads() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        match cell.lock_async().await {
            Ok(mut data) => data.add("Hello".to_string()),
            Err(e) => panic!("{e:?}"),
        }

        // Setup -> Read
        if let Err(e) = cell
            .transition_to_read_async(|data| {
                data.add("World".to_string());
                Box::pin(async { Ok::<(), MyError>(()) })
            })
            .await
        {
            panic!("{e:?}");
        }

        let counter = Arc::new(atomic::AtomicU8::new(0));
        let cell = Arc::new(cell);

        for _i in 0..10 {
            let cell_clone = Arc::clone(&cell);
            let counter_clone = Arc::clone(&counter);
            let _handler = tokio::task::spawn(async move {
                let data = cell_clone.read_relaxed().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                cell_clone.finish_reading();
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

        // Read -> Cleanup
        if let Err(e) = cell
            .transition_to_cleanup_async(time::Duration::from_secs(1), |_data| {
                Box::pin(async { Ok::<(), MyError>(()) })
            })
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);

        if let Ok(mut data) = cell.lock_async().await {
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

    #[tokio::test]
    async fn fail_to_read_relaxed_if_phase_is_setup() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);

        if let Err(e) = cell.read_relaxed() {
            assert_eq!(e.phase(), Phase::Setup);
            assert_eq!(
                e.kind(),
                PhasedErrorKind::CannotCallUnlessPhaseRead("read_relaxed")
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);
    }

    #[tokio::test]
    async fn fail_to_read_relaxed_if_phase_is_cleanup() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);

        if let Err(e) = cell.read() {
            assert_eq!(e.phase(), Phase::Setup);
            assert_eq!(e.kind(), PhasedErrorKind::CannotCallUnlessPhaseRead("read"));
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);
    }

    #[tokio::test]
    async fn fail_to_read_if_phase_is_setup() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell.read() {
            assert_eq!(e.phase(), Phase::Setup);
            assert_eq!(e.kind(), PhasedErrorKind::CannotCallUnlessPhaseRead("read"));
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);
    }

    #[tokio::test]
    async fn fail_to_read_if_phase_is_cleanup() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);

        if let Err(e) = cell
            .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                Box::pin(async { Ok::<(), MyError>(()) })
            })
            .await
        {
            panic!("{e:?}");
        }

        if let Err(e) = cell.read() {
            assert_eq!(e.phase(), Phase::Cleanup);
            assert_eq!(e.kind(), PhasedErrorKind::CannotCallUnlessPhaseRead("read"));
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn fail_to_lock_if_phase_is_read() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }

        if let Err(e) = cell.lock_async().await {
            assert_eq!(e.phase(), Phase::Read);
            assert_eq!(
                e.kind(),
                PhasedErrorKind::CannotCallOnPhaseRead("lock_async"),
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[tokio::test]
    async fn fail_to_transition_to_read_if_phase_is_read() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }

        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            assert_eq!(e.phase(), Phase::Read);
            assert_eq!(e.kind(), PhasedErrorKind::PhaseIsAlreadyRead);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[tokio::test]
    async fn fail_to_transition_to_read_if_phase_is_cleanup() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }

        if let Err(e) = cell
            .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                Box::pin(async { Ok::<(), MyError>(()) })
            })
            .await
        {
            panic!("{e:?}");
        }

        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            assert_eq!(e.phase(), Phase::Cleanup);
            assert_eq!(e.kind(), PhasedErrorKind::PhaseIsAlreadyCleanup);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn fail_to_lock_during_transition_to_read() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_read_async(|_data| {
                    Box::pin(async {
                        tokio::time::sleep(time::Duration::from_secs(1)).await;
                        Ok::<(), MyError>(())
                    })
                })
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(time::Duration::from_millis(100)).await;

        if let Err(e) = cell.lock_async().await {
            assert_eq!(e.kind(), PhasedErrorKind::DuringTransitionToRead);
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[tokio::test]
    async fn fail_to_lock_during_transition_to_cleanup_from_setup() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                    Box::pin(async {
                        tokio::time::sleep(time::Duration::from_secs(1)).await;
                        Ok::<(), MyError>(())
                    })
                })
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(time::Duration::from_millis(100)).await;

        if let Err(e) = cell.lock_async().await {
            assert_eq!(e.kind(), PhasedErrorKind::DuringTransitionToCleanup);
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn fail_to_lock_during_transition_to_cleanup_from_read() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                    Box::pin(async {
                        tokio::time::sleep(time::Duration::from_secs(1)).await;
                        Ok::<(), MyError>(())
                    })
                })
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(time::Duration::from_millis(100)).await;

        if let Err(e) = cell.lock_async().await {
            assert_eq!(e.kind(), PhasedErrorKind::DuringTransitionToCleanup);
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn fail_to_transition_to_read_if_closure_causes_an_error() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        if let Err(e) = cell
            .transition_to_read_async(|_data| {
                Box::pin(async {
                    tokio::time::sleep(time::Duration::from_secs(1)).await;
                    Err(MyError {})
                })
            })
            .await
        {
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

    #[tokio::test]
    async fn fail_to_transition_to_read_during_transition_to_read() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();
        for _i in 0..2 {
            let cell_clone = Arc::clone(&cell);
            let handler = tokio::task::spawn(async move {
                if let Err(e) = cell_clone
                    .transition_to_read_async(|_data| {
                        Box::pin(async {
                            tokio::time::sleep(time::Duration::from_secs(1)).await;
                            Ok::<(), MyError>(())
                        })
                    })
                    .await
                {
                    match e.kind() {
                        PhasedErrorKind::DuringTransitionToRead => {}
                        _ => panic!("{e:?}"),
                    }
                }
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[tokio::test]
    async fn fail_to_transition_to_read_during_transition_to_cleanup() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                    Box::pin(async {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        Ok::<(), MyError>(())
                    })
                })
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
                .await
            {
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
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn fail_to_transition_to_cleanup_during_transition_to_read() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_read_async(|_data| {
                    Box::pin(async {
                        tokio::time::sleep(time::Duration::from_secs(1)).await;
                        Ok::<(), MyError>(())
                    })
                })
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(time::Duration::from_millis(100)).await;

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                    Box::pin(async { Ok::<(), MyError>(()) })
                })
                .await
            {
                assert_eq!(e.kind(), PhasedErrorKind::DuringTransitionToRead);
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_relaxed(), Phase::Read);
        assert_eq!(cell.phase(), Phase::Read);
    }

    #[tokio::test]
    async fn fail_to_transition_to_cleanup_during_transition_to_cleanup_from_setup() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                    Box::pin(async {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        Ok::<(), MyError>(())
                    })
                })
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(time::Duration::from_millis(100)).await;

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                    Box::pin(async { Ok::<(), MyError>(()) })
                })
                .await
            {
                assert_eq!(e.kind(), PhasedErrorKind::DuringTransitionToCleanup);
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn fail_to_transition_to_cleanup_during_transition_to_cleanup_from_read() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                    Box::pin(async {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        Ok::<(), MyError>(())
                    })
                })
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(time::Duration::from_millis(100)).await;

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                    Box::pin(async { Ok::<(), MyError>(()) })
                })
                .await
            {
                assert_eq!(e.kind(), PhasedErrorKind::DuringTransitionToCleanup);
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn transition_to_cleanup_from_setup_but_closure_failed() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell
            .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                Box::pin(async { Err(MyError {}) })
            })
            .await
        {
            assert_eq!(format!("{:?}", e), "setup_read_cleanup::PhasedError { phase: Cleanup, kind: FailToRunClosureDuringTransitionToCleanup, source: MyError }");
        } else {
            panic!();
        }
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn transition_to_cleanup_from_read_but_closure_failed() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase(), Phase::Read);

        if let Err(e) = cell
            .transition_to_cleanup_async(time::Duration::ZERO, |_data| {
                Box::pin(async { Err(MyError {}) })
            })
            .await
        {
            assert_eq!(format!("{:?}", e), "setup_read_cleanup::PhasedError { phase: Cleanup, kind: FailToRunClosureDuringTransitionToCleanup, source: MyError }");
        } else {
            panic!();
        }
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn transition_to_cleanup_gracefully() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        match cell.lock_async().await {
            Ok(mut data) => data.add("Hello".to_string()),
            Err(e) => panic!("{e:?}"),
        }

        // Setup -> Read
        if let Err(e) = cell
            .transition_to_read_async(|data| {
                data.add("World".to_string());
                Box::pin(async { Ok::<(), MyError>(()) })
            })
            .await
        {
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

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Read -> Cleanup
        if let Err(e) = cell
            .transition_to_cleanup_async(time::Duration::from_secs(5), |data| {
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                data.add("!!".to_string());
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World, !!");
                Box::pin(async { Ok::<(), MyError>(()) })
            })
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);

        if let Ok(mut data) = cell.lock_async().await {
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

    #[tokio::test]
    async fn transition_to_cleanup_gracefully_but_timeout() {
        let cell = GracefulPhasedCellAsync::new(MyStruct::new());

        match cell.lock_async().await {
            Ok(mut data) => data.add("Hello".to_string()),
            Err(e) => panic!("{e:?}"),
        }

        // Setup -> Read
        if let Err(e) = cell
            .transition_to_read_async(|data| {
                data.add("World".to_string());
                Box::pin(async { Ok::<(), MyError>(()) })
            })
            .await
        {
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

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Read -> Cleanup
        if let Err(e) = cell
            .transition_to_cleanup_async(time::Duration::from_secs(1), |data| {
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                data.add("!!".to_string());
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World, !!");
                Box::pin(async { Ok::<(), MyError>(()) })
            })
            .await
        {
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

        if let Ok(mut data) = cell.lock_async().await {
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
