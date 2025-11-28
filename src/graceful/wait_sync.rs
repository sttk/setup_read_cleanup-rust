// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use super::{GracefulWaitError, GracefulWaitErrorKind, GracefulWaitSync};

use std::{any, sync, sync::atomic, time};

const MAX_READ_COUNT: usize = (isize::MAX) as usize;

#[allow(clippy::new_without_default)]
impl GracefulWaitSync {
    /// Creates a new `GracefulWaitSync`.
    pub const fn new() -> Self {
        Self {
            counter: atomic::AtomicUsize::new(0),
            blocker: sync::Mutex::new(false),
            condvar: sync::Condvar::new(),
        }
    }

    /// Increments the internal counter of active readers.
    pub fn count_up(&self) {
        let prev = self.counter.fetch_add(1, atomic::Ordering::AcqRel);
        assert!(
            prev < MAX_READ_COUNT,
            "{}::{} : {}",
            any::type_name::<GracefulWaitSync>(),
            "count_up",
            "read counter overflow"
        );
    }

    /// Decrements the internal counter of active readers.
    ///
    /// If the counter becomes zero and the provided closure `f` returns true,
    /// it notifies the waiting thread.
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

    /// Waits until all active readers have finished, or until the timeout is reached.
    ///
    /// # Errors
    ///
    /// Returns an error if the wait times out or if the underlying mutex is poisoned.
    pub fn wait_gracefully(&self, timeout: std::time::Duration) -> Result<(), GracefulWaitError> {
        let started = time::Instant::now();

        let mut guard = match self.blocker.lock() {
            Ok(guard) => guard,
            Err(_) => {
                return Err(GracefulWaitError::new(
                    GracefulWaitErrorKind::MutexIsPoisoned,
                ));
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

            if let Ok(result) = self.condvar.wait_timeout(guard, timeout - elapsed) {
                guard = result.0;
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
    use std::sync::Arc;

    #[test]
    fn read_gracefully_internal_data_in_multi_threads_and_wait_gracefully() {
        let counter = Arc::new(atomic::AtomicU8::new(0));
        let transition = Arc::new(atomic::AtomicBool::new(false));

        let wait = GracefulWaitSync::new();
        let wait = Arc::new(wait);

        for _i in 0..10 {
            let counter_clone = Arc::clone(&counter);
            let transition_clone = Arc::clone(&transition);
            let wait_clone = Arc::clone(&wait);

            let _handler = std::thread::spawn(move || {
                wait_clone.count_up();
                std::thread::sleep(std::time::Duration::from_secs(1));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                wait_clone.count_down(|| transition_clone.load(atomic::Ordering::Acquire));
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        transition.store(true, atomic::Ordering::Release);

        if let Err(e) = wait.wait_gracefully(std::time::Duration::from_secs(1)) {
            panic!("{e:?}");
        }

        assert_eq!(counter.load(atomic::Ordering::Acquire), 10);
    }

    #[test]
    fn read_gracefully_internal_data_in_multi_threads_and_timeout() {
        let counter = Arc::new(atomic::AtomicU8::new(0));
        let transition = Arc::new(atomic::AtomicBool::new(false));

        let wait = GracefulWaitSync::new();
        let wait = Arc::new(wait);

        for i in 0..10 {
            let counter_clone = Arc::clone(&counter);
            let transition_clone = Arc::clone(&transition);
            let wait_clone = Arc::clone(&wait);

            let _handler = std::thread::spawn(move || {
                wait_clone.count_up();
                std::thread::sleep(std::time::Duration::from_secs(i + 1));
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                wait_clone.count_down(|| transition_clone.load(atomic::Ordering::Acquire));
                //println!("{}. {}", i, data.vec.as_slice().join(", "));
            });
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
        transition.store(true, atomic::Ordering::Release);

        if let Err(e) = wait.wait_gracefully(std::time::Duration::from_secs(1)) {
            assert_eq!(
                e.kind(),
                GracefulWaitErrorKind::TimedOut(std::time::Duration::from_secs(1)),
            );
        } else {
            panic!();
        }

        assert!(counter.load(atomic::Ordering::Acquire) >= 3);
        assert!(counter.load(atomic::Ordering::Acquire) <= 4);
    }
}
