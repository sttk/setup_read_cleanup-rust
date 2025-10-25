// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::phase::*;
use crate::{Phase, PhasedCellAsync, PhasedError, PhasedErrorKind, PhasedTokioMutexGuard, Wait};

use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::{any, cell, error, marker, sync::atomic};
use tokio::sync;

impl<'mutex, T> PhasedTokioMutexGuard<'mutex, T> {
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

unsafe impl<T: Send + Sync> Sync for PhasedCellAsync<T> {}
unsafe impl<T: Send + Sync> Send for PhasedCellAsync<T> {}

impl<T: Send + Sync> PhasedCellAsync<T> {
    pub const fn new(data: T) -> Self {
        Self {
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            read_count: atomic::AtomicUsize::new(0),
            data_mutex: sync::Mutex::<Option<T>>::const_new(Some(data)),
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

        if self
            .read_count
            .fetch_update(
                atomic::Ordering::AcqRel,
                atomic::Ordering::Acquire,
                |count| {
                    if count == 0 {
                        None
                    } else {
                        Some(count - 1)
                    }
                },
            )
            .is_err()
        {
            eprintln!(
                "{} is called excessively.",
                Self::method_name("finish_reading_gracefully"),
            );
        }

        Ok(())
    }

    pub async fn transition_to_cleanup_async(&self, wait: Wait) -> Result<(), PhasedError> {
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
                let _result = self.pause_async(wait).await;
                self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);
                Ok(())
            }
            Ok(PHASE_READ) => {
                let result = self.pause_async(wait).await;
                self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);
                let mut data_opt = self.data_mutex.lock().await;
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

    pub async fn lock_async(&self) -> Result<PhasedTokioMutexGuard<'_, T>, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        match phase {
            PHASE_READ => Err(PhasedError::new(
                u8_to_phase(PHASE_READ),
                PhasedErrorKind::CannotCallOnPhaseRead(Self::method_name("lock_async")),
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
                if let Some(new_guard) = PhasedTokioMutexGuard::try_new(guarded_opt) {
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
    fn method_name(m: &str) -> String {
        format!("{}::{}", any::type_name::<PhasedCellAsync<T>>(), m)
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
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);

        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);

        if let Err(e) = cell.transition_to_cleanup_async(Wait::Zero).await {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn transition_from_setup_to_cleanup() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);

        if let Err(e) = cell.transition_to_cleanup_async(Wait::Zero).await {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn update_internal_data_in_setup_and_cleanup_phases() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);

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
        if let Err(e) = cell.transition_to_cleanup_async(Wait::Zero).await {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);

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
    async fn read_fast_internal_data_in_multi_threads_and_wait_zero() {
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

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();
        for _i in 0..10 {
            let cell_clone = Arc::clone(&cell);
            let counter_clone = Arc::clone(&counter);
            let handler = tokio::task::spawn(async move {
                let data = cell_clone.read_fast().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).await {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup_async(Wait::Zero).await {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);

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
    async fn read_fast_internal_data_in_multi_threads_and_wait_fixed() {
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
                let data = cell_clone.read_fast().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

        // Read -> Cleanup
        if let Err(e) = cell
            .transition_to_cleanup_async(Wait::Fixed(std::time::Duration::from_secs(1)))
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);

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
    async fn read_gracefully_internal_data_in_multi_threads_and_wait_gracefully() {
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
                let data = cell_clone.read_gracefully().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                cell_clone.finish_reading_gracefully().unwrap();
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Read -> Cleanup
        if let Err(e) = cell
            .transition_to_cleanup_async(Wait::Graceful {
                timeout: std::time::Duration::from_secs(5),
            })
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);

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
    async fn fail_to_read_fast_if_phase_is_setup() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell.read_fast() {
            assert_eq!(e.phase, Phase::Setup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedCellAsync<setup_read_cleanup::phased_cell_async::tests_of_phased_cell_async::MyStruct>::read_fast".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);
    }

    #[tokio::test]
    async fn fail_to_read_fast_if_phase_is_cleanup() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell.read_gracefully() {
            assert_eq!(e.phase, Phase::Setup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedCellAsync<setup_read_cleanup::phased_cell_async::tests_of_phased_cell_async::MyStruct>::read_gracefully".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);
    }

    #[tokio::test]
    async fn fail_to_read_gracefully_if_phase_is_cleanup() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell.transition_to_cleanup_async(Wait::Zero).await {
            panic!("{e:?}");
        }

        if let Err(e) = cell.read_gracefully() {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedCellAsync<setup_read_cleanup::phased_cell_async::tests_of_phased_cell_async::MyStruct>::read_gracefully".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn fail_to_finish_reading_gracefully_if_phase_is_setup() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell.finish_reading_gracefully() {
            assert_eq!(e.phase, Phase::Setup);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallOnPhaseSetup("setup_read_cleanup::PhasedCellAsync<setup_read_cleanup::phased_cell_async::tests_of_phased_cell_async::MyStruct>::finish_reading_gracefully".to_string())
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);
    }

    #[tokio::test]
    async fn dont_fail_to_finish_reading_gracefully_if_phase_is_cleanup() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }

        let _result = cell.read_gracefully();

        if let Err(e) = cell.transition_to_cleanup_async(Wait::Zero).await {
            panic!("{e:?}");
        }

        if let Err(e) = cell.finish_reading_gracefully() {
            panic!("{e:?}");
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn dont_fail_to_finish_reading_gracefully_if_it_is_called_excessively() {
        let cell = PhasedCellAsync::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);

        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            panic!("{e:?}");
        }

        let _result = cell.read_gracefully();
        let _result = cell.finish_reading_gracefully();

        if let Err(e) = cell.transition_to_cleanup_async(Wait::Zero).await {
            panic!("{e:?}");
        }

        if let Err(e) = cell.finish_reading_gracefully() {
            panic!("{e:?}");
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn dont_fail_but_return_error_if_graceful_wait_for_transition_to_cleanup_is_timeout() {
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

        for i in 0..10 {
            let cell_clone = Arc::clone(&cell);
            let counter_clone = Arc::clone(&counter);
            let _handler = tokio::task::spawn(async move {
                let data = cell_clone.read_gracefully().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                let _ = tokio::time::sleep(std::time::Duration::from_secs(i + 1)).await;
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                cell_clone.finish_reading_gracefully().unwrap();
                println!("{}. {}", i, data.vec.as_slice().join(", "));
            });
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        // Read -> Cleanup
        if let Err(e) = cell
            .transition_to_cleanup_async(Wait::Graceful {
                timeout: std::time::Duration::from_secs(1),
            })
            .await
        {
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
            assert_eq!(e.phase, Phase::Read);
            assert_eq!(
                e.kind,
                PhasedErrorKind::CannotCallOnPhaseRead("setup_read_cleanup::PhasedCellAsync<setup_read_cleanup::phased_cell_async::tests_of_phased_cell_async::MyStruct>::lock_async".to_string()),
            );
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);
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
            assert_eq!(e.phase, Phase::Read);
            assert_eq!(e.kind, PhasedErrorKind::PhaseIsAlreadyRead);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);
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

        if let Err(e) = cell.transition_to_cleanup_async(Wait::Zero).await {
            panic!("{e:?}");
        }

        if let Err(e) = cell
            .transition_to_read_async(|_data| Box::pin(async { Ok::<(), MyError>(()) }))
            .await
        {
            assert_eq!(e.phase, Phase::Cleanup);
            assert_eq!(e.kind, PhasedErrorKind::TransitionToReadFailed);
        } else {
            panic!();
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn fail_to_finish_reading_gracefully_during_transition_to_read() {
        let cell = PhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_read_async(|_data| {
                    Box::pin(async {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        Ok::<(), MyError>(())
                    })
                })
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        if let Err(e) = cell.finish_reading_gracefully() {
            assert_eq!(e.kind, PhasedErrorKind::CannotCallOnPhaseSetup("setup_read_cleanup::PhasedCellAsync<setup_read_cleanup::phased_cell_async::tests_of_phased_cell_async::MyStruct>::finish_reading_gracefully".to_string()),
            );
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);
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
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        Ok::<(), MyError>(())
                    })
                })
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        if let Err(e) = cell.lock_async().await {
            assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToRead);
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);
    }

    #[tokio::test]
    async fn fail_to_lock_during_transition_to_cleanup_from_setup() {
        let cell = PhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(Wait::Fixed(std::time::Duration::from_secs(1)))
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        if let Err(e) = cell.lock_async().await {
            assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToCleanup);
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
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
                .transition_to_cleanup_async(Wait::Fixed(std::time::Duration::from_secs(1)))
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        if let Err(e) = cell.lock_async().await {
            assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToCleanup);
        } else {
            panic!();
        }

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }

    #[tokio::test]
    async fn fail_to_transition_to_read_if_closure_causes_an_error() {
        let cell = PhasedCellAsync::new(MyStruct::new());

        if let Err(e) = cell
            .transition_to_read_async(|_data| {
                Box::pin(async {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    Err(MyError {})
                })
            })
            .await
        {
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
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            Ok::<(), MyError>(())
                        })
                    })
                    .await
                {
                    match e.kind {
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

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);
    }

    #[tokio::test]
    async fn fail_to_transition_to_read_during_transition_to_cleanup() {
        let cell = PhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(Wait::Fixed(std::time::Duration::from_secs(1)))
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
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
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
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        Ok::<(), MyError>(())
                    })
                })
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone.transition_to_cleanup_async(Wait::Zero).await {
                assert_eq!(e.kind, PhasedErrorKind::TransitionToCleanupFailed);
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_fast(), Phase::Read);
        assert_eq!(cell.phase_exact(), Phase::Read);
    }

    #[tokio::test]
    async fn fail_to_transition_to_cleanup_during_transition_to_cleanup_from_setup() {
        let cell = PhasedCellAsync::new(MyStruct::new());

        let cell = Arc::new(cell);

        let mut join_handlers = Vec::<tokio::task::JoinHandle<_>>::new();

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone
                .transition_to_cleanup_async(Wait::Fixed(std::time::Duration::from_secs(1)))
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone.transition_to_cleanup_async(Wait::Zero).await {
                assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToCleanup);
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
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
                .transition_to_cleanup_async(Wait::Fixed(std::time::Duration::from_secs(1)))
                .await
            {
                panic!("{e:?}");
            }
        });
        join_handlers.push(handler);

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let cell_clone = Arc::clone(&cell);
        let handler = tokio::task::spawn(async move {
            if let Err(e) = cell_clone.transition_to_cleanup_async(Wait::Zero).await {
                assert_eq!(e.kind, PhasedErrorKind::DuringTransitionToCleanup);
            } else {
                panic!();
            }
        });
        join_handlers.push(handler);

        while join_handlers.len() > 0 {
            let _result = join_handlers.remove(0).await;
        }

        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_exact(), Phase::Cleanup);
    }
}
