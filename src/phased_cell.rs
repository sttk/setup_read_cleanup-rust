// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::phase::{
    u8_to_phase, PHASE_CLEANUP, PHASE_READ, PHASE_READ_TO_CLEANUP, PHASE_SETUP, PHASE_SETUP_TO_READ,
};
use crate::{Phase, PhasedCell, PhasedError, PhasedErrorKind, PhasedMutexGuard, WaitStrategy};

use std::ops::{Deref, DerefMut};
use std::{any, cell, error, marker, sync, sync::atomic, thread, time};

macro_rules! cannot_call_on_tokio_runtime {
    ( $plock:ident, $method:literal ) => {
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        {
            if tokio::runtime::Handle::try_current().is_ok() {
                return Err(PhasedError::new(
                    u8_to_phase($plock.phase.load(std::sync::atomic::Ordering::Acquire)),
                    PhasedErrorKind::CannotCallOnTokioRuntime(Self::method_name($method)),
                ));
            }
        }
    };
}

unsafe impl<T: Send + Sync> Sync for PhasedCell<T> {}
unsafe impl<T: Send + Sync> Send for PhasedCell<T> {}

impl<T: Send + Sync> PhasedCell<T> {
    pub const fn new(data: T) -> Self {
        Self {
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            read_count: atomic::AtomicUsize::new(0),
            wait_cvar: sync::Condvar::new(),
            data_mutex: sync::Mutex::new(Some(data)),
            data_fixed: cell::UnsafeCell::new(None),
            _marker: marker::PhantomData,
        }
    }

    pub fn transition_to_read<F, E>(&self, mut f: F) -> Result<(), PhasedError>
    where
        F: FnMut(&mut T) -> Result<(), E>,
        E: error::Error + Send + Sync + 'static,
    {
        cannot_call_on_tokio_runtime!(self, "transition_to_read");

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
                Err(_e) => {
                    // rollback phase transition
                    let _result = self.phase.compare_exchange(
                        PHASE_SETUP_TO_READ,
                        old_phase_cd,
                        atomic::Ordering::AcqRel,
                        atomic::Ordering::Acquire,
                    );

                    Err(PhasedError::new(
                        u8_to_phase(old_phase_cd),
                        PhasedErrorKind::MutexIsPoisoned,
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

    pub fn transition_to_cleanup(&self, wait_strategy: WaitStrategy) -> Result<(), PhasedError> {
        cannot_call_on_tokio_runtime!(self, "transition_to_cleanup");

        match wait_strategy {
            WaitStrategy::NoWait => self.transition_to_cleanup_simply(),
            WaitStrategy::FixedWait(tm) => {
                thread::sleep(tm);
                self.transition_to_cleanup_simply()
            }
            WaitStrategy::GracefulWait { timeout } => {
                self.transition_to_cleanup_gracefully(timeout)
            }
        }
    }

    fn transition_to_cleanup_simply(&self) -> Result<(), PhasedError> {
        match self.phase.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |current_phase| match current_phase {
                PHASE_SETUP => Some(PHASE_CLEANUP),
                PHASE_READ => Some(PHASE_READ_TO_CLEANUP),
                _ => None,
            },
        ) {
            Ok(PHASE_READ) => match self.data_mutex.lock() {
                Ok(mut data_opt) => {
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
                Err(_e) => {
                    // rollback phase transition
                    let _result = self.phase.compare_exchange(
                        PHASE_READ_TO_CLEANUP,
                        PHASE_READ,
                        atomic::Ordering::AcqRel,
                        atomic::Ordering::Acquire,
                    );

                    Err(PhasedError::new(
                        u8_to_phase(PHASE_READ_TO_CLEANUP),
                        PhasedErrorKind::MutexIsPoisoned,
                    ))
                }
            },
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

    fn transition_to_cleanup_gracefully(&self, timeout: time::Duration) -> Result<(), PhasedError> {
        match self.data_mutex.lock() {
            Ok(mut data_opt) => {
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

                            if let Ok(result) =
                                self.wait_cvar.wait_timeout(data_opt, timeout - elapsed)
                            {
                                data_opt = result.0;

                                if result.1.timed_out() {
                                    is_timeout = true;
                                    break;
                                }
                            } else {
                                return Err(PhasedError::new(
                                    u8_to_phase(PHASE_READ_TO_CLEANUP),
                                    PhasedErrorKind::MutexIsPoisoned,
                                ));
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
                                PhasedErrorKind::TransitionToCleanupTimeout(
                                    WaitStrategy::GracefulWait { timeout },
                                ),
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
            Err(_e) => Err(PhasedError::new(
                u8_to_phase(self.phase.load(atomic::Ordering::Acquire)),
                PhasedErrorKind::MutexIsPoisoned,
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

    pub fn lock_for_update(&self) -> Result<PhasedMutexGuard<'_, T>, PhasedError> {
        cannot_call_on_tokio_runtime!(self, "lock_for_update");

        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase == PHASE_READ {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallInReadPhase(Self::method_name("lock_for_update")),
            ));
        }

        match self.data_mutex.lock() {
            Ok(guarded_opt) => {
                if let Some(new_guard) = PhasedMutexGuard::try_new(guarded_opt) {
                    Ok(new_guard)
                } else {
                    Err(PhasedError::new(
                        u8_to_phase(phase),
                        PhasedErrorKind::InternalDataIsEmpty,
                    ))
                }
            }
            Err(_e) => Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::MutexIsPoisoned,
            )),
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

    pub fn finish_reading_gracefully(&self) -> Result<(), PhasedError> {
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
                if let Ok(_g) = self.data_mutex.lock() {
                    let phase = self.phase.load(atomic::Ordering::Acquire);
                    if phase == PHASE_READ_TO_CLEANUP {
                        self.wait_cvar.notify_one();
                    }
                } else {
                    self.wait_cvar.notify_one(); // if the lock fails, just send it anyway.
                }
            }
            Ok(_) => {}
            Err(_) => {
                eprintln!(
                    "{}::finish_reading_gracefully is called excessively.",
                    any::type_name::<PhasedCell<T>>(),
                );
            }
        }

        Ok(())
    }

    #[inline]
    fn method_name(m: &str) -> String {
        format!("{}::{}", any::type_name::<PhasedCell<T>>(), m)
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
mod tests_of_phase_cell {
    use super::*;
    use once_cell::sync::Lazy;
    use std::fmt;
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

    #[derive(Debug)]
    struct MyError {}
    impl fmt::Display for MyError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "MyError")
        }
    }
    impl error::Error for MyError {}

    fn my_func(_data: &mut MyStruct) -> Result<(), MyError> {
        Ok(())
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
                const MY_STRUCT: PhasedCell<MyStruct> = PhasedCell::new(MyStruct {});
                assert_eq!(MY_STRUCT.phase_fast(), Phase::Setup); // drop!

                // drop!
                if let Err(e) = MY_STRUCT.transition_to_read(|_data| Ok::<(), MyError>(())) {
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
                let _my_struct: PhasedCell<MyStruct> = PhasedCell::new(MyStruct {});
            }
            if let Ok(mut vec) = LOGGER.lock() {
                assert_eq!(vec.len(), 1);
                assert_eq!(vec[0], "drop MyStruct");
                vec.clear();
            }

            {
                let my_struct: PhasedCell<MyStruct> = PhasedCell::new(MyStruct {});
                assert_eq!(my_struct.phase_fast(), Phase::Setup);

                if let Err(e) = my_struct.transition_to_read(|_data| Ok::<(), MyError>(())) {
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
        static MY_STRUCT: PhasedCell<MyStruct> = PhasedCell::new(MyStruct {});

        #[test]
        fn new_static() {
            assert_eq!(MY_STRUCT.phase_fast(), Phase::Setup);
            assert_eq!(MY_STRUCT.phase_exact(), Phase::Setup);
        }
    }

    #[cfg(feature = "setup_read_cleanup-on-tokio")]
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
                let my_struct: PhasedCell<MyStruct> = PhasedCell::new(MyStruct {});
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
            let my_struct: PhasedCell<MyStruct> = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_read(my_func) {
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
            let my_struct: PhasedCell<MyStruct> = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn fail_setup_read_read() {
            let my_struct: PhasedCell<MyStruct> = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_read(my_func) {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            if let Err(e) = my_struct.transition_to_read(my_func) {
                assert_eq!(e.phase, Phase::Read);
                assert_eq!(e.kind, PhasedErrorKind::PhaseIsAlreadyRead);
            } else {
                panic!();
            }
        }

        #[test]
        fn fail_setup_cleanup_cleanup() {
            let my_struct: PhasedCell<MyStruct> = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                assert_eq!(e.phase, Phase::Cleanup);
                assert_eq!(e.kind, PhasedErrorKind::PhaseIsAlreadyCleanup);
            } else {
                panic!();
            }
        }

        #[test]
        fn fail_setup_cleanup_read() {
            let my_struct: PhasedCell<MyStruct> = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            if let Err(e) = my_struct.transition_to_read(my_func) {
                assert_eq!(e.phase, Phase::Cleanup);
                assert_eq!(e.kind, PhasedErrorKind::TransitionToReadFailed);
            } else {
                panic!();
            }
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }
    }

    #[cfg(feature = "setup_read_cleanup-on-tokio")]
    mod tests_of_phase_transition_on_tokio_rt {
        use super::*;

        #[tokio::test]
        async fn fail_setup_to_read() {
            let my_struct: PhasedCell<MyStruct> = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_read(my_func) {
                assert_eq!(e.phase, Phase::Setup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::CannotCallOnTokioRuntime(
                        "setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::transition_to_read".to_string()
                    )
                );
            } else {
                panic!();
            }
            assert_eq!(my_struct.phase_fast(), Phase::Setup);
        }

        #[test]
        fn fail_read_to_cleanup() {
            let my_struct: PhasedCell<MyStruct> = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_read(my_func) {
                panic!("{:?}", e);
            }
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                    assert_eq!(e.phase, Phase::Read);
                    assert_eq!(
                        e.kind,
                        PhasedErrorKind::CannotCallOnTokioRuntime(
                            "setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::transition_to_cleanup".to_string()
                        )
                    );
                } else {
                    panic!();
                }
                assert_eq!(my_struct.phase_fast(), Phase::Read);
            });
        }

        #[test]
        fn fail_to_read_gracefully_during_cleanup_transition() {
            let pl = sync::Arc::new(PhasedCell::new(MyStruct::new()));
            pl.transition_to_read(my_func).unwrap();

            let pl_slow_reader = sync::Arc::clone(&pl);
            let slow_reader_handle = thread::spawn(move || {
                let _data = pl_slow_reader.read_gracefully().unwrap();
                thread::sleep(time::Duration::from_millis(1000));
                pl_slow_reader.finish_reading_gracefully().unwrap();
            });

            // wait that the slow thread calls read_gracefully()
            thread::sleep(time::Duration::from_millis(100));

            let pl_writer = sync::Arc::clone(&pl);
            let writer_handle = thread::spawn(move || {
                pl_writer
                    .transition_to_cleanup(WaitStrategy::GracefulWait {
                        timeout: time::Duration::from_millis(2000),
                    })
                    .unwrap();
            });

            // wait that the writer thread starts transition to cleanup (PHASE_READ_TO_CLEANUP)
            thread::sleep(time::Duration::from_millis(100));
            assert_eq!(pl.phase_exact(), Phase::Cleanup);

            match pl.read_gracefully() {
                Err(e) => {
                    assert_eq!(
                        e.kind,
                        PhasedErrorKind::CannotCallOutOfReadPhase(
                            "setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::read_gracefully".to_string()
                        )
                    );
                }
                Ok(_) => panic!("Should not be able to read while transitioning to cleanup"),
            }

            slow_reader_handle.join().unwrap();
            writer_handle.join().unwrap();

            assert_eq!(pl.phase_exact(), Phase::Cleanup);
        }

        #[tokio::test]
        async fn fail_setup_to_cleanup() {
            let my_struct: PhasedCell<MyStruct> = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_cleanup(WaitStrategy::NoWait) {
                assert_eq!(e.phase, Phase::Setup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::CannotCallOnTokioRuntime(
                        "setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::transition_to_cleanup".to_string()
                    )
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
            let my_struct = PhasedCell::new(MyStruct::new());

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
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);
        }

        #[test]
        fn ok_phase_exact() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_exact(), Phase::Setup);
        }

        #[test]
        fn fail_read_fast() {
            let my_struct = PhasedCell::new(MyStruct::new());

            if let Err(e) = my_struct.read_fast() {
                assert_eq!(e.phase, Phase::Setup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::CannotCallOutOfReadPhase("setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::read_fast".to_string())
                );
            } else {
                panic!();
            }
        }

        #[test]
        fn fail_read_gracefully() {
            let my_struct = PhasedCell::new(MyStruct::new());

            if let Err(e) = my_struct.read_gracefully() {
                assert_eq!(e.phase, Phase::Setup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::CannotCallOutOfReadPhase(
                        "setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::read_gracefully".to_string()
                    )
                );
            } else {
                panic!();
            }
        }

        #[test]
        fn fail_finish_reading_gracefully() {
            let my_struct = PhasedCell::new(MyStruct::new());

            if let Err(e) = my_struct.finish_reading_gracefully() {
                assert_eq!(e.phase, Phase::Setup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::CannotCallInSetupPhase(
                        "setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::finish_reading_gracefully".to_string()
                    )
                );
            } else {
                panic!();
            }
        }
    }

    #[cfg(feature = "setup_read_cleanup-on-tokio")]
    mod tests_in_setup_phase_on_tokio_rt {
        use super::*;

        #[tokio::test]
        async fn ok_lock_for_update() {
            let my_struct = PhasedCell::new(MyStruct::new());

            if let Err(e) = my_struct.lock_for_update() {
                assert_eq!(e.phase, Phase::Setup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::CannotCallOnTokioRuntime(
                        "setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::lock_for_update".to_string()
                    )
                );
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
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);
        }

        #[tokio::test]
        async fn ok_phase_exact() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_exact(), Phase::Setup);
        }

        #[tokio::test]
        async fn fail_read_fast() {
            let my_struct = PhasedCell::new(MyStruct::new());

            if let Err(e) = my_struct.read_fast() {
                assert_eq!(e.phase, Phase::Setup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::CannotCallOutOfReadPhase("setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::read_fast".to_string())
                );
            } else {
                panic!();
            }
        }

        #[tokio::test]
        async fn fail_read_gracefully() {
            let my_struct = PhasedCell::new(MyStruct::new());

            if let Err(e) = my_struct.read_gracefully() {
                assert_eq!(e.phase, Phase::Setup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::CannotCallOutOfReadPhase(
                        "setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::read_gracefully".to_string()
                    )
                );
            } else {
                panic!();
            }
        }

        #[tokio::test]
        async fn fail_finish_reading_gracefully() {
            let my_struct = PhasedCell::new(MyStruct::new());

            if let Err(e) = my_struct.finish_reading_gracefully() {
                assert_eq!(e.phase, Phase::Setup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::CannotCallInSetupPhase(
                        "setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::finish_reading_gracefully".to_string()
                    )
                );
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
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("hello".to_string());
                }
                Err(e) => panic!("{e:?}"),
            }

            my_struct.transition_to_read(my_func).unwrap();
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
                    timeout: time::Duration::from_secs(2),
                })
                .unwrap();
            assert!(start.elapsed() < time::Duration::from_millis(10));

            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn ok_read_and_finish_gracefully() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("hello".to_string());
                }
                Err(e) => panic!("{e:?}"),
            }

            my_struct.transition_to_read(my_func).unwrap();
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
                    timeout: time::Duration::from_secs(2),
                })
                .unwrap();
            assert!(start.elapsed() < time::Duration::from_millis(10));
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn fail_lock_for_update() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read(my_func).unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            match my_struct.lock_for_update() {
                Ok(_) => {
                    panic!();
                }
                Err(e) => {
                    assert_eq!(e.phase, Phase::Read);
                    assert_eq!(
                        e.kind,
                        PhasedErrorKind::CannotCallInReadPhase(
                            "setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::lock_for_update".to_string()
                        )
                    );
                }
            }

            // Before the 2024 edition, adding a semicolon was required to prevent
            // an E0597 error because the compiler's NLL couldn't correctly infer
            // the lifetime of my_struct. In the 2024 edition, this semicolon is
            // no longer needed due to improvements in the NLL logic.
            ;
        }
    }

    #[cfg(feature = "setup_read_cleanup-on-tokio")]
    mod tests_in_read_phase_on_tokio_rt {
        use super::*;

        #[test]
        fn ok_phase_fast() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read(my_func).unwrap();

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
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read(my_func).unwrap();

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
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("hello".to_string());
                }
                Err(e) => panic!("{e:?}"),
            }

            my_struct.transition_to_read(my_func).unwrap();
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

            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("hello".to_string());
                }
                Err(e) => panic!("{e:?}"),
            }

            my_struct.transition_to_read(my_func).unwrap();
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
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read(my_func).unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn ok_phase_exact() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_exact(), Phase::Setup);

            my_struct.transition_to_read(my_func).unwrap();
            assert_eq!(my_struct.phase_exact(), Phase::Read);

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_exact(), Phase::Cleanup);
        }

        #[test]
        fn ok_lock_for_update() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            match my_struct.lock_for_update() {
                Ok(mut data) => {
                    data.add("hello".to_string());
                }
                Err(e) => panic!("{e:?}"),
            }

            my_struct.transition_to_read(my_func).unwrap();
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
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read(my_func).unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            if let Err(e) = my_struct.read_fast() {
                assert_eq!(e.phase, Phase::Cleanup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::CannotCallOutOfReadPhase("setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::read_fast".to_string())
                );
            } else {
                panic!();
            }
        }

        #[test]
        fn fail_read_gracefully() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read(my_func).unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            if let Err(e) = my_struct.read_gracefully() {
                assert_eq!(e.phase, Phase::Cleanup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::CannotCallOutOfReadPhase(
                        "setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::read_gracefully".to_string()
                    )
                );
            } else {
                panic!();
            }
        }

        #[test]
        fn ok_finish_reading_gracefully_if_finished() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read(my_func).unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            if let Err(e) = my_struct.read_gracefully() {
                panic!("{e:?}");
            }

            if let Err(e) = my_struct.finish_reading_gracefully() {
                panic!("{e:?}");
            }

            let start = time::Instant::now();
            my_struct
                .transition_to_cleanup(WaitStrategy::GracefulWait {
                    timeout: time::Duration::from_secs(2),
                })
                .unwrap();
            assert!(start.elapsed() < time::Duration::from_millis(10));
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn ok_finish_reading_gracefully_if_finish_before_timeout() {
            let pl = sync::Arc::new(PhasedCell::new(MyStruct::new()));
            assert_eq!(pl.phase_fast(), Phase::Setup);

            if let Ok(mut data) = pl.lock_for_update() {
                data.add("hello".to_string());
            }

            pl.transition_to_read(my_func).unwrap();
            assert_eq!(pl.phase_fast(), Phase::Read);

            let mut handler_vec = Vec::<thread::JoinHandle<()>>::new();

            for _i in 0..10 {
                let pl_clone = pl.clone();
                let handler = thread::spawn(move || {
                    let data = pl_clone.read_gracefully().unwrap();
                    assert_eq!(data.vec[0], "hello");
                    thread::sleep(time::Duration::from_secs(1));
                    pl_clone.finish_reading_gracefully().unwrap();
                });
                handler_vec.push(handler);
            }

            // wait that all threads calls read_gracefully()
            thread::sleep(time::Duration::from_millis(500));

            let start = time::Instant::now();
            if let Err(e) = pl.transition_to_cleanup(WaitStrategy::GracefulWait {
                timeout: time::Duration::from_secs(2),
            }) {
                panic!("{e:?}");
            }
            let elapsed = start.elapsed();
            assert!(
                elapsed < time::Duration::from_secs(2),
                "elapsed = {elapsed:?}"
            );
            assert_eq!(pl.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn fail_finish_reading_gracefully_if_timeout() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read(my_func).unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            if let Err(e) = my_struct.read_gracefully() {
                panic!("{e:?}");
            }

            let ws = WaitStrategy::GracefulWait {
                timeout: time::Duration::from_secs(2),
            };

            let start = time::Instant::now();
            if let Err(e) = my_struct.transition_to_cleanup(ws) {
                assert_eq!(e.phase, Phase::Cleanup);
                assert_eq!(e.kind, PhasedErrorKind::TransitionToCleanupTimeout(ws));
            } else {
                panic!();
            }
            let elapsed = start.elapsed();
            println!("elapsed = {:?}", elapsed);
            #[cfg(target_os = "windows")]
            assert!(
                elapsed > time::Duration::from_millis(1980),
                "elapsed = {elapsed:?}"
            );
            #[cfg(not(target_os = "windows"))]
            assert!(
                elapsed > time::Duration::from_secs(2),
                "elapsed = {elapsed:?}"
            );
            assert!(
                elapsed < time::Duration::from_millis(2200),
                "elapsed = {elapsed:?}"
            );
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);
        }

        #[test]
        fn ok_finish_reading_gracefully_if_called_excessively() {
            let pl = PhasedCell::new(MyStruct::new());
            pl.transition_to_read(my_func).unwrap();

            let result = pl.finish_reading_gracefully(); // output a message to stderr
            assert!(result.is_ok());

            pl.read_gracefully().unwrap();
            pl.finish_reading_gracefully().unwrap();

            let result = pl.finish_reading_gracefully(); // output a message to stderr
            assert!(result.is_ok());
        }
    }

    #[cfg(feature = "setup_read_cleanup-on-tokio")]
    mod tests_in_cleanup_phase_on_tokio_rt {
        use super::*;

        #[test]
        fn fail_lock_for_update() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            my_struct.transition_to_read(my_func).unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Read);

            my_struct
                .transition_to_cleanup(WaitStrategy::NoWait)
                .unwrap();
            assert_eq!(my_struct.phase_fast(), Phase::Cleanup);

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                if let Err(e) = my_struct.lock_for_update() {
                    assert_eq!(e.phase, Phase::Cleanup);
                    assert_eq!(
                        e.kind,
                        PhasedErrorKind::CannotCallOnTokioRuntime(
                            "setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::MyStruct>::lock_for_update".to_string()
                        )
                    );
                } else {
                    panic!();
                }
            });
        }
    }

    mod tests_of_running_closure_during_transition_to_read {
        use super::*;
        use std::fmt;

        struct MyStruct {
            flag: bool,
        }

        #[derive(Debug)]
        struct MyError {}
        impl fmt::Display for MyError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
                write!(f, "MyError occured")
            }
        }
        impl error::Error for MyError {}

        impl MyStruct {
            const fn new() -> Self {
                Self { flag: false }
            }
            fn set_flag(&mut self, flag: bool) -> Result<(), MyError> {
                if flag {
                    self.flag = flag;
                    Ok(())
                } else {
                    Err(MyError {})
                }
            }
        }

        #[test]
        fn ok() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_read(|data| data.set_flag(true)) {
                panic!("{e:?}");
            }

            assert!(my_struct.read_fast().unwrap().flag);
        }

        #[test]
        fn fail() {
            let my_struct = PhasedCell::new(MyStruct::new());
            assert_eq!(my_struct.phase_fast(), Phase::Setup);

            if let Err(e) = my_struct.transition_to_read(|data| data.set_flag(false)) {
                assert_eq!(e.phase, Phase::Setup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::FailToRunClosureDuringTransitionToRead
                );
                assert!(e.source.is_some());
                match e.source {
                    Some(s) => {
                        let _err: &MyError = s.downcast_ref::<MyError>().unwrap();
                    }
                    None => panic!(),
                }
            } else {
                panic!();
            }

            if let Err(e) = my_struct.read_fast() {
                assert_eq!(e.phase, Phase::Setup);
                assert_eq!(
                    e.kind,
                    PhasedErrorKind::CannotCallOutOfReadPhase("setup_read_cleanup::PhasedCell<setup_read_cleanup::phased_cell::tests_of_phase_cell::tests_of_running_closure_during_transition_to_read::MyStruct>::read_fast".to_string())
                );
            } else {
                panic!();
            }
        }
    }

    mod test_of_mutex_poisoned {
        use super::*;
        use std::{panic, sync, thread};

        #[test]
        fn fail_when_mutex_is_poisoned() {
            let pl = sync::Arc::new(PhasedCell::new(MyStruct::new()));

            let pl_clone = sync::Arc::clone(&pl);
            let handle = thread::spawn(move || {
                let _lock = pl_clone.lock_for_update().unwrap();
                panic!("Intentionally poisoning the mutex");
            });

            assert!(handle.join().is_err()); // wait for finishing a panic thread.

            // All operations below should fail.

            match pl.lock_for_update() {
                Err(e) => {
                    assert_eq!(e.phase, Phase::Setup);
                    assert_eq!(e.kind, PhasedErrorKind::MutexIsPoisoned);
                }
                Ok(_) => panic!("Should have failed due to poisoned mutex"),
            }

            match pl.transition_to_read(|_| Ok::<(), MyError>(())) {
                Err(e) => {
                    assert_eq!(e.phase, Phase::Setup);
                    assert_eq!(e.kind, PhasedErrorKind::MutexIsPoisoned);
                }
                Ok(_) => panic!("Should have failed due to poisoned mutex"),
            }

            match pl.transition_to_cleanup(WaitStrategy::GracefulWait {
                timeout: time::Duration::from_secs(1),
            }) {
                Err(e) => {
                    assert_eq!(e.phase, Phase::Setup);
                    assert_eq!(e.kind, PhasedErrorKind::MutexIsPoisoned);
                }
                Ok(_) => panic!("Should have failed due to poisoned mutex"),
            }
        }
    }
}
