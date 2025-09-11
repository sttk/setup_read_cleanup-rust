// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::{PhasedLock, WaitStrategy};

use std::{sync::atomic, thread, time};

impl<T: Send + Sync> PhasedLock<T> {
    pub fn wait_with_strategy(&self, ws: WaitStrategy) -> Result<(), WaitStrategy> {
        match ws {
            WaitStrategy::NoWait => Ok(()),
            WaitStrategy::FixedWait(tm) => {
                thread::sleep(tm);
                Ok(())
            }
            WaitStrategy::GracefulWait {
                first,
                interval,
                timeout,
            } => {
                thread::sleep(first);
                if self.read_count.load(atomic::Ordering::Acquire) == 0 {
                    return Ok(());
                }
                let start = time::Instant::now();
                while start.elapsed() < timeout {
                    thread::sleep(interval);
                    if self.read_count.load(atomic::Ordering::Acquire) == 0 {
                        return Ok(());
                    }
                }
                Err(ws)
            }
        }
    }
}

#[cfg(test)]
mod tests_of_wait_strategy {
    use super::*;

    struct MyStruct {}

    #[test]
    fn test_no_wait() {
        let pl = PhasedLock::new(MyStruct {});
        let ws = WaitStrategy::NoWait;

        let start = time::Instant::now();
        let result = pl.wait_with_strategy(ws);
        let elapsed = start.elapsed();

        println!("elapsed = {elapsed:?}");
        assert!(elapsed >= time::Duration::ZERO);
        assert!(elapsed <= time::Duration::from_millis(1));
        assert!(result.is_ok());
    }

    #[test]
    fn test_fixed_wait() {
        let pl = PhasedLock::new(MyStruct {});
        let ws = WaitStrategy::FixedWait(time::Duration::from_millis(100));

        let start = time::Instant::now();
        let result = pl.wait_with_strategy(ws);
        let elapsed = start.elapsed();

        println!("elapsed = {elapsed:?}");
        assert!(elapsed >= time::Duration::from_millis(100));
        #[cfg(target_os = "macos")]
        assert!(elapsed <= time::Duration::from_millis(250));
        #[cfg(target_os = "linux")]
        assert!(elapsed <= time::Duration::from_millis(110));
        #[cfg(target_os = "windows")]
        assert!(elapsed <= time::Duration::from_millis(120));
        assert!(result.is_ok());
    }

    #[test]
    fn test_graceful_wait_timeout() {
        let pl = PhasedLock::new(MyStruct {});
        pl.read_count.fetch_add(1, atomic::Ordering::AcqRel);

        let ws = WaitStrategy::GracefulWait {
            first: time::Duration::from_millis(100),
            interval: time::Duration::from_millis(10),
            timeout: time::Duration::from_secs(1),
        };

        let start = time::Instant::now();
        let result = pl.wait_with_strategy(ws);
        let elapsed = start.elapsed();

        println!("elapsed = {elapsed:?}");
        assert!(elapsed >= time::Duration::from_millis(1100));
        #[cfg(target_os = "macos")]
        assert!(elapsed <= time::Duration::from_millis(1300));
        #[cfg(not(target_os = "macos"))]
        assert!(elapsed <= time::Duration::from_millis(1150));

        assert!(result.is_err());
        match result.unwrap_err() {
            WaitStrategy::GracefulWait {
                first,
                interval,
                timeout,
            } => match ws {
                WaitStrategy::GracefulWait {
                    first: f,
                    interval: i,
                    timeout: t,
                } => {
                    assert_eq!(first, f);
                    assert_eq!(interval, i);
                    assert_eq!(timeout, t);
                }
                _ => panic!(),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn test_graceful_wait_zero() {
        let pl = PhasedLock::new(MyStruct {});

        let ws = WaitStrategy::GracefulWait {
            first: time::Duration::from_millis(100),
            interval: time::Duration::from_millis(10),
            timeout: time::Duration::from_secs(1),
        };

        let start = time::Instant::now();
        let result = pl.wait_with_strategy(ws);
        let elapsed = start.elapsed();

        println!("elapsed = {elapsed:?}");
        assert!(elapsed >= time::Duration::from_millis(100));
        #[cfg(target_os = "macos")]
        assert!(elapsed <= time::Duration::from_millis(250));
        #[cfg(not(target_os = "macos"))]
        assert!(elapsed <= time::Duration::from_millis(110));
        assert!(result.is_ok());
    }

    #[test]
    fn test_graceful_wait_one() {
        let pl = PhasedLock::new(MyStruct {});
        pl.read_count.fetch_add(1, atomic::Ordering::AcqRel);

        let ws = WaitStrategy::GracefulWait {
            first: time::Duration::from_millis(100),
            interval: time::Duration::from_millis(10),
            timeout: time::Duration::from_secs(1),
        };

        let pl_1 = std::sync::Arc::new(pl);
        let pl_2 = pl_1.clone();

        let handler = thread::spawn(move || {
            thread::sleep(time::Duration::from_millis(500));
            let _ = pl_1.read_count.fetch_update(
                atomic::Ordering::AcqRel,
                atomic::Ordering::Acquire,
                |c| {
                    if c > 0 {
                        Some(c - 1)
                    } else {
                        None
                    }
                },
            );
        });

        let start = time::Instant::now();
        let result = pl_2.wait_with_strategy(ws);
        let elapsed = start.elapsed();
        assert_eq!(pl_2.read_count.load(atomic::Ordering::Acquire), 0);

        println!("elapsed = {elapsed:?}");
        assert!(elapsed >= time::Duration::from_millis(500));
        #[cfg(target_os = "macos")]
        assert!(elapsed <= time::Duration::from_millis(700));
        #[cfg(target_os = "linux")]
        assert!(elapsed <= time::Duration::from_millis(510));
        #[cfg(target_os = "windows")]
        assert!(elapsed <= time::Duration::from_millis(525));
        assert!(result.is_ok());

        handler.join().unwrap();
    }

    #[test]
    fn test_graceful_wait_ten() {
        let pl = PhasedLock::new(MyStruct {});
        pl.read_count.fetch_add(10, atomic::Ordering::AcqRel);

        let ws = WaitStrategy::GracefulWait {
            first: time::Duration::from_millis(100),
            interval: time::Duration::from_millis(10),
            timeout: time::Duration::from_secs(10),
        };

        let pl_1 = std::sync::Arc::new(pl);

        let mut handler_vec = Vec::<thread::JoinHandle<()>>::new();

        for _i in 0..10 {
            let pl_clone = pl_1.clone();
            let handler = thread::spawn(move || {
                thread::sleep(time::Duration::from_millis(500));
                let _ = pl_clone.read_count.fetch_update(
                    atomic::Ordering::AcqRel,
                    atomic::Ordering::Acquire,
                    |c| {
                        if c > 0 {
                            Some(c - 1)
                        } else {
                            None
                        }
                    },
                );
            });
            handler_vec.push(handler);
        }

        let start = time::Instant::now();
        let result = pl_1.wait_with_strategy(ws);
        let elapsed = start.elapsed();

        println!("elapsed = {elapsed:?}");
        assert_eq!(pl_1.read_count.load(atomic::Ordering::Acquire), 0);

        assert!(elapsed >= time::Duration::from_millis(500));
        #[cfg(target_os = "macos")]
        assert!(elapsed <= time::Duration::from_millis(700));
        #[cfg(target_os = "linux")]
        assert!(elapsed <= time::Duration::from_millis(510));
        #[cfg(target_os = "windows")]
        assert!(elapsed <= time::Duration::from_millis(540));
        assert!(result.is_ok());

        for handler in handler_vec {
            handler.join().unwrap();
        }
    }
}
