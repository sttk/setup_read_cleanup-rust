// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use super::{GracefulWaitAsync, GracefulWaitError, GracefulWaitErrorKind};

use std::{any, sync::atomic};
use tokio::{sync, time};

impl GracefulWaitAsync {
    pub(crate) const fn new() -> Self {
        Self {
            counter: atomic::AtomicUsize::new(0),
            notify: sync::Notify::const_new(),
        }
    }

    pub(crate) fn count_up(&self) {
        self.counter.fetch_add(1, atomic::Ordering::AcqRel);
    }

    pub(crate) fn count_down<F>(&self, f: F)
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

    pub(crate) async fn wait_gracefully_async(
        &self,
        timeout: time::Duration,
    ) -> Result<(), GracefulWaitError> {
        let wait_future = async {
            while self.counter.load(atomic::Ordering::Acquire) > 0 {
                self.notify.notified().await;
            }
        };

        match time::timeout(timeout, wait_future).await {
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
    use std::sync::Arc;

    #[tokio::test]
    async fn read_gracefully_internal_data_in_multi_threads_and_wait_gracefully() {
        let counter = Arc::new(atomic::AtomicU8::new(0));
        let transition = Arc::new(atomic::AtomicBool::new(false));

        let wait = GracefulWaitAsync::new();
        let wait = Arc::new(wait);

        for _i in 0..10 {
            let counter_clone = Arc::clone(&counter);
            let transition_clone = Arc::clone(&transition);
            let wait_clone = Arc::clone(&wait);

            let _handler = tokio::task::spawn(async move {
                wait_clone.count_up();
                time::sleep(tokio::time::Duration::from_secs(1)).await;
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                wait_clone.count_down(|| transition_clone.load(atomic::Ordering::Acquire));
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
        }
        time::sleep(tokio::time::Duration::from_millis(300)).await;
        transition.store(true, atomic::Ordering::Release);

        if let Err(e) = wait
            .wait_gracefully_async(time::Duration::from_secs(1))
            .await
        {
            panic!("{e:?}");
        }

        assert_eq!(counter.load(atomic::Ordering::Acquire), 10);
    }

    #[tokio::test]
    async fn read_gracefully_internal_data_in_multi_threads_and_timeout() {
        let counter = Arc::new(atomic::AtomicU8::new(0));
        let transition = Arc::new(atomic::AtomicBool::new(false));

        let wait = GracefulWaitAsync::new();
        let wait = Arc::new(wait);

        for i in 0..10 {
            let counter_clone = Arc::clone(&counter);
            let transition_clone = Arc::clone(&transition);
            let wait_clone = Arc::clone(&wait);

            let _handler = tokio::task::spawn(async move {
                wait_clone.count_up();
                time::sleep(tokio::time::Duration::from_secs(i + 1)).await;
                counter_clone.fetch_add(1, atomic::Ordering::Release);
                wait_clone.count_down(|| transition_clone.load(atomic::Ordering::Acquire));
                //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            });
        }
        time::sleep(tokio::time::Duration::from_secs(3)).await;
        transition.store(true, atomic::Ordering::Release);

        if let Err(e) = wait
            .wait_gracefully_async(time::Duration::from_secs(1))
            .await
        {
            assert_eq!(
                e.kind,
                GracefulWaitErrorKind::TimedOut(time::Duration::from_secs(1),)
            );
        } else {
            panic!();
        }

        assert!(counter.load(atomic::Ordering::Acquire) >= 3);
        assert!(counter.load(atomic::Ordering::Acquire) <= 4);
    }
}
