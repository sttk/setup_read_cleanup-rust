// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::{GracefulWaitAsync, GracefulWaitError, GracefulWaitErrorKind};

use std::{any, sync::atomic};
use tokio::sync;

impl GracefulWaitAsync {
    pub const fn new() -> Self {
        Self {
            counter: atomic::AtomicUsize::new(0),
            notify: sync::Notify::const_new(),
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
                if f() {
                    self.notify.notify_waiters();
                }
            }
            Ok(_) => {}
            Err(_) => {
                eprintln!(
                    "{}::count_down is called excessively.",
                    any::type_name::<GracefulWaitAsync>(),
                );
            }
        }
    }

    pub async fn wait_gracefully_async(
        &self,
        timeout: tokio::time::Duration,
    ) -> Result<(), GracefulWaitError> {
        let wait_future = async {
            while self.counter.load(atomic::Ordering::Acquire) != 0 {
                self.notify.notified().await;
            }
        };

        match tokio::time::timeout(timeout, wait_future).await {
            Ok(_) => Ok(()),
            Err(_) => Err(GracefulWaitError::new(GracefulWaitErrorKind::TimedOut(
                timeout,
            ))),
        }
    }
}

#[cfg(test)]
mod graceful_wait_async {
    use super::*;
    use crate::{Phase, PhasedCellAsync};
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

        let wait = GracefulWaitAsync::new();
        let wait = Arc::new(wait);

        for _i in 0..10 {
            let cell_clone = Arc::clone(&cell);
            let counter_clone = Arc::clone(&counter);
            let wait_clone = Arc::clone(&wait);
            let _handler = tokio::task::spawn(async move {
                let data = cell_clone.read_properly().unwrap();
                wait_clone.count_up();
                assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                wait_clone.count_down(|| cell_clone.phase_properly() == Phase::Cleanup);
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

        let is_timed_out = Arc::new(atomic::AtomicBool::new(true));
        let is_timed_out_clone = Arc::clone(&is_timed_out);

        // Read -> Cleanup
        if let Err(e) = cell
            .transition_to_cleanup_async(move |_data| {
                let wait_clone = wait.clone();
                let is_timed_out_clone = Arc::clone(&is_timed_out_clone);
                Box::pin(async move {
                    if let Err(e) = wait_clone
                        .wait_gracefully_async(tokio::time::Duration::from_secs(10))
                        .await
                    {
                        panic!("{e:?}");
                    }
                    is_timed_out_clone.store(false, atomic::Ordering::Release);
                    Ok::<(), MyError>(())
                })
            })
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(is_timed_out.load(atomic::Ordering::Acquire), false);
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);

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
    async fn read_gracefully_internal_data_in_multi_threads_and_timeout() {
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

        let wait = GracefulWaitAsync::new();
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

        let is_timed_out = Arc::new(tokio::sync::Mutex::new(false));
        let is_timed_out_clone = Arc::clone(&is_timed_out);

        // Read -> Cleanup
        if let Err(e) = cell
            .transition_to_cleanup_async(move |_data| {
                let wait_clone = wait.clone();
                let is_timed_out_clone = is_timed_out_clone.clone();
                Box::pin(async move {
                    if let Err(e) = wait_clone
                        .wait_gracefully_async(std::time::Duration::from_secs(1))
                        .await
                    {
                        assert_eq!(
                            e.kind,
                            GracefulWaitErrorKind::TimedOut(std::time::Duration::from_secs(1),)
                        );
                        let mut flag = is_timed_out_clone.lock().await;
                        *flag = true;
                        Ok::<(), MyError>(())
                    } else {
                        panic!();
                    }
                })
            })
            .await
        {
            panic!("{e:?}");
        }
        assert_eq!(*is_timed_out.lock().await, true);
        assert_eq!(cell.phase_fast(), Phase::Cleanup);
        assert_eq!(cell.phase_properly(), Phase::Cleanup);

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

        assert!(counter.load(atomic::Ordering::Acquire) >= 3);
        assert!(counter.load(atomic::Ordering::Acquire) <= 4);
    }
}
