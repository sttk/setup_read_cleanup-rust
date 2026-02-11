// Copyright (C) 2025-2026 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::phase::*;
use crate::{Phase, PhasedCellAsync, PhasedError, PhasedErrorKind, TokioMutexGuard};

use futures::future::FutureExt;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::{cell, error, marker, panic, sync::atomic};
use tokio::sync;

impl<'mutex, T> TokioMutexGuard<'mutex, T> {
    /// Tries to create a new `TokioMutexGuard`.
    ///
    /// This method returns `None` if the guarded data is `None`.
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

impl<'mutex, T> Deref for TokioMutexGuard<'mutex, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // unwrap() here is always safe because it's guaranteed to be Some in the constructor.
        self.inner.as_ref().unwrap()
    }
}

impl<'mutex, T> DerefMut for TokioMutexGuard<'mutex, T> {
    fn deref_mut(&mut self) -> &mut T {
        // unwrap() here is always safe because it's guaranteed to be Some in the constructor.
        self.inner.as_mut().unwrap()
    }
}

unsafe impl<T: Send + Sync> Sync for PhasedCellAsync<T> {}
unsafe impl<T: Send + Sync> Send for PhasedCellAsync<T> {}

impl<T: Send + Sync> PhasedCellAsync<T> {
    /// Creates a new `PhasedCellAsync` in the `Setup` phase, containing the provided data.
    ///
    /// # Examples
    ///
    /// ```
    /// use setup_read_cleanup::PhasedCellAsync;
    ///
    /// let cell = PhasedCellAsync::new(10);
    /// ```
    pub const fn new(data: T) -> Self {
        Self {
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
    /// use setup_read_cleanup::{Phase, PhasedCellAsync};
    ///
    /// let cell = PhasedCellAsync::new(10);
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
    /// use setup_read_cleanup::{Phase, PhasedCellAsync};
    ///
    /// let cell = PhasedCellAsync::new(10);
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
    /// It is only successful if the cell is in the `Read` phase.
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
            Ok(data)
        } else {
            Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::InternalDataUnavailable,
            ))
        }
    }

    /// Asynchronously transitions the cell to the `Cleanup` phase.
    ///
    /// This method takes an async closure `f` which is executed on the contained data.
    /// It can be called from either the `Setup` or the `Read` phase.
    ///
    /// # Errors
    ///
    /// Returns an error if the phase transition fails or the closure returns an error.
    /// If the provided async closure panics, the cell's phase will be transitioned
    /// to `Cleanup`, and the panic will be resumed.
    pub async fn transition_to_cleanup_async<F, E>(&self, mut f: F) -> Result<(), PhasedError>
    where
        for<'a> F: FnMut(&'a mut T) -> Pin<Box<dyn Future<Output = Result<(), E>> + Send + 'a>>,
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
                let mut guard = self.data_mutex.lock().await;

                let data_opt = unsafe { &mut *self.data_cell.get() };
                if data_opt.is_none() {
                    // impossible case
                    self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);
                    return Err(PhasedError::new(
                        u8_to_phase(PHASE_CLEANUP),
                        PhasedErrorKind::InternalDataUnavailable,
                    ));
                }
                let data = data_opt.as_mut().unwrap();

                let future = f(data);
                let result_f = panic::AssertUnwindSafe(future).catch_unwind().await;
                unsafe {
                    core::ptr::swap(data_opt, &mut *guard);
                }
                self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);

                match result_f {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(PhasedError::with_source(
                        u8_to_phase(PHASE_CLEANUP),
                        PhasedErrorKind::FailToRunClosureDuringTransitionToCleanup,
                        e,
                    )),
                    Err(panic_cause) => {
                        drop(guard);
                        panic::resume_unwind(panic_cause);
                    }
                }
            }
            Ok(_ /*PHASE_SETUP*/) => {
                let mut guard = self.data_mutex.lock().await;

                let data_opt = &mut *guard;
                if data_opt.is_none() {
                    // impossible case
                    self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);
                    return Err(PhasedError::new(
                        u8_to_phase(PHASE_CLEANUP),
                        PhasedErrorKind::InternalDataUnavailable,
                    ));
                }
                let data = data_opt.as_mut().unwrap();
                let future = f(data);
                let result_f = panic::AssertUnwindSafe(future).catch_unwind().await;

                self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);

                match result_f {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(PhasedError::with_source(
                        u8_to_phase(PHASE_CLEANUP),
                        PhasedErrorKind::FailToRunClosureDuringTransitionToCleanup,
                        e,
                    )),
                    Err(cause) => {
                        drop(guard);
                        panic::resume_unwind(cause);
                    }
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

    /// Forcibly transitions the cell to the `Cleanup` phase.
    ///
    /// This method is a synchronous version of `transition_to_cleanup_async`
    /// and can be called from non-async contexts. It takes a synchronous closure `f`
    /// which is executed on the contained data.
    ///
    /// Unlike `transition_to_cleanup_async`, this method does not require `await`
    /// and performs the cleanup synchronously.
    ///
    /// It can be called from either the `Setup` or the `Read` phase.
    ///
    /// # Errors
    ///
    /// Returns an error if the phase transition fails or the closure returns an error.
    /// If the provided synchronous closure panics, the cell's phase will be transitioned
    /// to `Cleanup`, and the panic will be resumed.
    pub fn force_to_cleanup<F, E>(&self, mut f: F) -> Result<(), PhasedError>
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
                let mut guard = self.data_mutex.try_lock().map_err(|_| {
                    // impossible case
                    self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);
                    PhasedError::new(
                        u8_to_phase(PHASE_CLEANUP),
                        PhasedErrorKind::MutexTryLockFailed,
                    )
                })?;
                let data_in_mutex = &mut *guard;

                let data_opt = unsafe { &mut *self.data_cell.get() };
                if data_opt.is_none() {
                    // impossible case
                    self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);
                    return Err(PhasedError::new(
                        u8_to_phase(PHASE_CLEANUP),
                        PhasedErrorKind::InternalDataUnavailable,
                    ));
                }
                let data = data_opt.as_mut().unwrap();
                let result_f = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(data)));

                unsafe {
                    core::ptr::swap(data_opt, data_in_mutex);
                }
                self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);

                match result_f {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(PhasedError::with_source(
                        u8_to_phase(PHASE_CLEANUP),
                        PhasedErrorKind::FailToRunClosureDuringTransitionToCleanup,
                        e,
                    )),
                    Err(panic_cause) => {
                        panic::resume_unwind(panic_cause);
                    }
                }
            }
            Ok(_ /*PHASE_SETUP*/) => {
                let mut guard = self.data_mutex.try_lock().map_err(|_| {
                    // impossible case
                    self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);
                    PhasedError::new(
                        u8_to_phase(PHASE_CLEANUP),
                        PhasedErrorKind::MutexTryLockFailed,
                    )
                })?;
                let data_opt = &mut *guard;

                if data_opt.is_none() {
                    // impossible case
                    self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);
                    return Err(PhasedError::new(
                        u8_to_phase(PHASE_CLEANUP),
                        PhasedErrorKind::InternalDataUnavailable,
                    ));
                }
                let data = data_opt.as_mut().unwrap();
                let result_f = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(data)));

                self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);

                match result_f {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(PhasedError::with_source(
                        u8_to_phase(PHASE_CLEANUP),
                        PhasedErrorKind::FailToRunClosureDuringTransitionToCleanup,
                        e,
                    )),
                    Err(cause) => {
                        panic::resume_unwind(cause);
                    }
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

    /// Asynchronously transitions the cell from the `Setup` phase to the `Read` phase.
    ///
    /// This method takes an async closure `f` which is executed on the contained data
    /// during the transition.
    ///
    /// # Errors
    ///
    /// Returns an error if the cell is not in the `Setup` phase or if the closure
    /// returns an error. If the provided async closure panics, the cell's phase
    /// will be reverted to `Setup`, and the panic will be resumed.
    pub async fn transition_to_read_async<F, E>(&self, mut f: F) -> Result<(), PhasedError>
    where
        for<'a> F: FnMut(&'a mut T) -> Pin<Box<dyn Future<Output = Result<(), E>> + Send + 'a>>,
        E: error::Error + Send + Sync + 'static,
    {
        match self.phase.compare_exchange(
            PHASE_SETUP,
            PHASE_SETUP_TO_READ,
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
        ) {
            Ok(old_phase) => {
                let mut data_opt = self.data_mutex.lock().await;
                if data_opt.is_none() {
                    // impossible case
                    self.change_phase(PHASE_SETUP_TO_READ, old_phase);
                    return Err(PhasedError::new(
                        u8_to_phase(PHASE_SETUP_TO_READ),
                        PhasedErrorKind::InternalDataUnavailable,
                    ));
                }
                let data = data_opt.as_mut().unwrap();
                let future = f(data);
                let result_f = panic::AssertUnwindSafe(future).catch_unwind().await;

                match result_f {
                    Ok(Ok(())) => {
                        unsafe {
                            core::ptr::swap(self.data_cell.get(), &mut *data_opt);
                        }
                        self.change_phase(PHASE_SETUP_TO_READ, PHASE_READ);
                        Ok(())
                    }
                    Ok(Err(e)) => {
                        self.change_phase(PHASE_SETUP_TO_READ, old_phase);
                        Err(PhasedError::with_source(
                            u8_to_phase(PHASE_SETUP),
                            PhasedErrorKind::FailToRunClosureDuringTransitionToRead,
                            e,
                        ))
                    }
                    Err(panic_cause) => {
                        self.change_phase(PHASE_SETUP_TO_READ, old_phase);
                        drop(data_opt);
                        panic::resume_unwind(panic_cause);
                    }
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
            Err(old_phase) => Err(PhasedError::new(
                u8_to_phase(old_phase),
                PhasedErrorKind::DuringTransitionToCleanup,
            )),
        }
    }

    /// Forcibly transitions the cell to the `Read` phase.
    ///
    /// This method is a synchronous version of `transition_to_read_async`
    /// and can be called from non-async contexts. It takes a synchronous closure `f`
    /// which is executed on the contained data.
    ///
    /// Unlike `transition_to_read_async`, this method does not require `await`
    /// and performs the transition synchronously.
    ///
    /// It can be called from the `Setup` phase.
    ///
    /// # Errors
    ///
    /// Returns an error if the phase transition fails or the closure returns an error.
    /// If the provided synchronous closure panics, the cell's phase will be transitioned
    /// to `Setup`, and the panic will be resumed.
    pub fn force_to_read<F, E>(&self, mut f: F) -> Result<(), PhasedError>
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
            Ok(old_phase) => {
                let mut guard = self.data_mutex.try_lock().map_err(|_| {
                    // impossible case
                    self.change_phase(PHASE_SETUP_TO_READ, PHASE_SETUP);
                    PhasedError::new(
                        u8_to_phase(PHASE_SETUP),
                        PhasedErrorKind::MutexTryLockFailed,
                    )
                })?;
                let data_opt = &mut *guard;

                if data_opt.is_none() {
                    // impossible case
                    self.change_phase(PHASE_SETUP_TO_READ, old_phase);
                    return Err(PhasedError::new(
                        u8_to_phase(PHASE_SETUP_TO_READ),
                        PhasedErrorKind::InternalDataUnavailable,
                    ));
                }
                let data = data_opt.as_mut().unwrap();
                let result_f = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(data)));

                match result_f {
                    Ok(Ok(())) => {
                        unsafe {
                            core::ptr::swap(self.data_cell.get(), &mut *data_opt);
                        }
                        self.change_phase(PHASE_SETUP_TO_READ, PHASE_READ);
                        Ok(())
                    }
                    Ok(Err(e)) => {
                        self.change_phase(PHASE_SETUP_TO_READ, old_phase);
                        Err(PhasedError::with_source(
                            u8_to_phase(PHASE_SETUP),
                            PhasedErrorKind::FailToRunClosureDuringTransitionToRead,
                            e,
                        ))
                    }
                    Err(panic_cause) => {
                        self.change_phase(PHASE_SETUP_TO_READ, old_phase);
                        panic::resume_unwind(panic_cause);
                    }
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
            Err(old_phase) => Err(PhasedError::new(
                u8_to_phase(old_phase),
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
    use tokio::time;

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
        let cell = PhasedCellAsync::new(MyStruct::new());
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
            .transition_to_cleanup_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn transition_from_setup_to_cleanup() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell
            .transition_to_cleanup_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_relaxed(), Phase::Cleanup);
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn update_internal_data_in_setup_and_cleanup_phases() {
        let cell = PhasedCellAsync::new(MyStruct::new());
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

        // Read -> Cleanup
        if let Err(e) = cell
            .transition_to_cleanup_async(|data| {
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

    #[tokio::test]
    async fn read_relaxed_internal_data_in_multi_threads() {
        let cell = PhasedCellAsync::new(MyStruct::new());

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
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

        // Read -> Cleanup
        if let Err(e) = cell
            .transition_to_cleanup_async(|_data| {
                Box::pin(async {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    Ok::<(), MyError>(())
                })
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
        let cell = PhasedCellAsync::new(MyStruct::new());
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

    #[tokio::test]
    async fn fail_to_read_relaxed_if_phase_is_cleanup() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);

        if let Err(e) = cell.read() {
            assert_eq!(e.phase(), Phase::Setup);
            assert_eq!(e.kind(), PhasedErrorKind::CannotCallUnlessPhaseRead("read"),);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_relaxed(), Phase::Setup);
        assert_eq!(cell.phase(), Phase::Setup);
    }

    #[tokio::test]
    async fn fail_to_read_if_phase_is_setup() {
        let cell = PhasedCellAsync::new(MyStruct::new());
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

    #[tokio::test]
    async fn fail_to_read_if_phase_is_cleanup() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_relaxed(), Phase::Setup);

        if let Err(e) = cell
            .transition_to_cleanup_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
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

    #[tokio::test]
    async fn fail_to_lock_if_phase_is_read() {
        let cell = PhasedCellAsync::new(MyStruct::new());

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
        let cell = PhasedCellAsync::new(MyStruct::new());

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
        let cell = PhasedCellAsync::new(MyStruct::new());

        // Setup -> Read
        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }

        if let Err(e) = cell
            .transition_to_cleanup_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
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
        let cell = PhasedCellAsync::new(MyStruct::new());

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
        let cell = PhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(|_data| {
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
        let cell = PhasedCellAsync::new(MyStruct::new());

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
                .transition_to_cleanup_async(|_data| {
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
        let cell = PhasedCellAsync::new(MyStruct::new());

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
        let cell = PhasedCellAsync::new(MyStruct::new());

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
        let cell = PhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(|_data| {
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
        let cell = PhasedCellAsync::new(MyStruct::new());

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
                .transition_to_cleanup_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
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
        let cell = PhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(|_data| {
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
                .transition_to_cleanup_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
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
        let cell = PhasedCellAsync::new(MyStruct::new());

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
                .transition_to_cleanup_async(|_data| {
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
                .transition_to_cleanup_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
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
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell
            .transition_to_cleanup_async(|_data| Box::pin(async { Err(MyError {}) }))
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
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase(), Phase::Read);

        if let Err(e) = cell
            .transition_to_cleanup_async(|_data| Box::pin(async { Err(MyError {}) }))
            .await
        {
            assert_eq!(format!("{:?}", e), "setup_read_cleanup::PhasedError { phase: Cleanup, kind: FailToRunClosureDuringTransitionToCleanup, source: MyError }");
        } else {
            panic!();
        }
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn panic_during_transition_to_read() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        let future = cell.transition_to_read_async(|_data| {
            Box::pin(async {
                panic!("Panic during transition to read");
                #[allow(unused)]
                Ok::<(), MyError>(())
            })
        });
        let result = panic::AssertUnwindSafe(future).catch_unwind().await;

        assert!(result.is_err());
        assert_eq!(cell.phase(), Phase::Setup);

        let mut data = cell.lock_async().await.unwrap();
        data.add("still works".to_string());
        assert_eq!(data.vec, &["still works".to_string()]);
    }

    #[tokio::test]
    async fn panic_during_transition_to_cleanup_from_setup() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        let future = cell.transition_to_cleanup_async(|_data| {
            Box::pin(async {
                panic!("Panic during transition to cleanup");
                #[allow(unused)]
                Ok::<(), MyError>(())
            })
        });
        let result = panic::AssertUnwindSafe(future).catch_unwind().await;

        assert!(result.is_err());
        assert_eq!(cell.phase(), Phase::Cleanup);

        let mut data = cell.lock_async().await.unwrap();
        data.add("still works".to_string());
        assert_eq!(data.vec, &["still works".to_string()]);
    }

    #[tokio::test]
    async fn panic_during_transition_to_cleanup_from_read() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        cell.transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
            .unwrap();
        assert_eq!(cell.phase(), Phase::Read);

        let future = cell.transition_to_cleanup_async(|_data| {
            Box::pin(async {
                panic!("Panic during transition to cleanup");
                #[allow(unused)]
                Ok::<(), MyError>(())
            })
        });
        let result = panic::AssertUnwindSafe(future).catch_unwind().await;

        assert!(result.is_err());
        assert_eq!(cell.phase(), Phase::Cleanup);

        let mut data = cell.lock_async().await.unwrap();
        data.add("still works".to_string());
        assert_eq!(data.vec, &["still works".to_string()]);
    }

    #[tokio::test]
    async fn try_hrtb_effect_of_transition_to_cleanup_async() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        let _ = cell
            .transition_to_read_async(|data| {
                Box::pin(async {
                    data.add("hello".to_string());
                    Ok::<(), MyError>(())
                })
            })
            .await;
    }

    #[tokio::test]
    async fn try_hrtb_effect_of_transition_to_read_async() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        let _ = cell
            .transition_to_read_async(|data| {
                Box::pin(async {
                    data.add("hello".to_string());
                    Ok::<(), MyError>(())
                })
            })
            .await;
    }

    #[tokio::test]
    async fn test_force_to_cleanup() {
        let mut cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        {
            let mut data = cell.lock_async().await.unwrap();
            data.add("hello".to_string());
        }

        cleanup(&mut cell);
    }

    #[tokio::test]
    async fn test_read_and_force_to_cleanup() {
        let mut cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        {
            let mut data = cell.lock_async().await.unwrap();
            data.add("hello".to_string());
        }

        cell.transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
            .unwrap();
        assert_eq!(cell.phase(), Phase::Read);

        cleanup(&mut cell);
    }

    fn cleanup(cell: &PhasedCellAsync<MyStruct>) {
        let _ = cell.force_to_cleanup(|data| {
            data.clear();
            Ok::<(), MyError>(())
        });
        assert_eq!(cell.phase(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn test_force_to_read() {
        let mut cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase(), Phase::Setup);

        {
            let mut data = cell.lock_async().await.unwrap();
            data.add("hello".to_string());
        }

        setup(&mut cell);
    }

    fn setup(cell: &PhasedCellAsync<MyStruct>) {
        let _ = cell.force_to_read(|data| {
            data.clear();
            Ok::<(), MyError>(())
        });
        assert_eq!(cell.phase(), Phase::Read);
    }
}
