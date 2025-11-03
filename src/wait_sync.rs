// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::{GracefulWaitError, GracefulWaitErrorKind, GracefulWaitSync};

use std::{any, sync, sync::atomic, time};

#[allow(clippy::new_without_default)]
impl GracefulWaitSync {
    pub const fn new() -> Self {
        Self {
            counter: atomic::AtomicUsize::new(0),
            blocker: sync::Mutex::new(false),
            condvar: sync::Condvar::new(),
        }
    }

    pub fn count_up(&self) {
        self.counter.fetch_add(1, atomic::Ordering::AcqRel);
    }

    pub fn count_down<F>(&self, f: F)
    where
        F: Fn() -> bool,
    {
        match self
            .counter
            .fetch_update(atomic::Ordering::AcqRel, atomic::Ordering::Acquire, |c| {
                if c == 0 {
                    None
                } else {
                    Some(c - 1)
                }
            }) {
            Ok(1) => {
                if let Ok(mut flag) = self.blocker.lock() {
                    if f() {
                        *flag = true;
                        self.condvar.notify_all();
                    }
                }
            }
            Ok(_) => {}
            Err(_) => {
                eprintln!(
                    "{}::count_down is called excessively.",
                    any::type_name::<GracefulWaitSync>(),
                );
            }
        }
    }

    pub fn wait_gracefully(&self, timeout: std::time::Duration) -> Result<(), GracefulWaitError> {
        let started = time::Instant::now();

        let mut guard = match self.blocker.lock() {
            Ok(guard) => guard,
            Err(_) => {
                return Err(GracefulWaitError::new(
                    GracefulWaitErrorKind::MutexIsPoisoned,
                ))
            }
        };

        loop {
            if self.counter.load(atomic::Ordering::Acquire) == 0 {
                return Ok(());
            }

            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Err(GracefulWaitError::new(GracefulWaitErrorKind::TimedOut(
                    timeout,
                )));
            }

            if let Ok(wt_result) = self.condvar.wait_timeout(guard, timeout - elapsed) {
                guard = wt_result.0;
            } else {
                return Err(GracefulWaitError::new(
                    GracefulWaitErrorKind::MutexIsPoisoned,
                ));
            }
        }
    }
}

#[cfg(test)]
mod graceful_wait_sync {
    use super::*;
    use crate::{Phase, PhasedCellSync};
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
    impl Error for MyError {}

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

        let wait = GracefulWaitSync::new();
        let wait = Arc::new(wait);

        for _i in 0..10 {
            let cell_clone = Arc::clone(&cell);
            let counter_clone = Arc::clone(&counter);
            let wait_clone = Arc::clone(&wait);
            let _handler = std::thread::spawn(move || {
                let data = cell_clone.read_properly().unwrap();
                wait_clone.count_up();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                std::thread::sleep(std::time::Duration::from_secs(1));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                wait_clone.count_down(|| cell_clone.phase_properly() == Phase::Cleanup);
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(300));

        let mut is_timed_out = true;

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup(|_data| {
            if let Err(e) = wait.wait_gracefully(std::time::Duration::from_secs(1)) {
                panic!("{e:?}");
            }
            is_timed_out = false;
            Ok::<(), MyError>(())
        }) {
            panic!("{e:?}");
        }
        assert_eq!(is_timed_out, false);
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);

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
    fn read_gracefully_internal_data_in_multi_threads_and_timeout() {
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

        let wait = GracefulWaitSync::new();
        let wait = Arc::new(wait);

        for i in 0..10 {
            let cell_clone = Arc::clone(&cell);
            let counter_clone = Arc::clone(&counter);
            let wait_clone = Arc::clone(&wait);
            let _handler = std::thread::spawn(move || {
                let data = cell_clone.read_properly().unwrap();
                wait_clone.count_up();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                std::thread::sleep(std::time::Duration::from_secs(i + 1));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                wait_clone.count_down(|| cell_clone.phase_properly() == Phase::Cleanup);
                //println!("{}. {}", i, data.vec.as_slice().join(", "));
            });
        }
        std::thread::sleep(std::time::Duration::from_secs(3));

        let is_timed_out = Arc::new(std::cell::Cell::new(false));
        let is_timed_out_clone = Arc::clone(&is_timed_out);

        // Read -> Cleanup
        if let Err(e) = cell.transition_to_cleanup(move |_data| {
            if let Err(e) = wait.wait_gracefully(std::time::Duration::from_secs(1)) {
                assert_eq!(
                    e.kind,
                    GracefulWaitErrorKind::TimedOut(std::time::Duration::from_secs(1),)
                );
                is_timed_out_clone.set(true);
                Ok::<(), MyError>(())
            } else {
                panic!();
            }
        }) {
            panic!("{e:?}");
        }
        assert_eq!(is_timed_out.get(), true);
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);

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

        assert!(counter.load(atomic::Ordering::Acquire) >= 3);
        assert!(counter.load(atomic::Ordering::Acquire) <= 4);
    }
}
