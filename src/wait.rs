// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::{PhasedCell, PhasedCellSync, PhasedError, Wait};

#[cfg(feature = "setup_read_cleanup-on-tokio")]
use crate::PhasedCellAsync;

use std::{sync::atomic, time};

impl<T: Send + Sync> PhasedCell<T> {
    pub(crate) fn pause(&self, wait: Wait) -> Result<(), Wait> {
        pause(&self.read_count, wait)
    }
}

impl<T: Send + Sync> PhasedCellSync<T> {
    pub(crate) fn pause(&self, wait: Wait) -> Result<(), Wait> {
        pause(&self.read_count, wait)
    }
}

#[cfg(feature = "setup_read_cleanup-on-tokio")]
impl<T: Send + Sync> PhasedCellAsync<T> {
    pub(crate) fn pause(&self, wait: Wait) -> Result<(), Wait> {
        pause(&self.read_count, wait)
    }
}

fn pause(counter: &atomic::AtomicUsize, wait: Wait) -> Result<(), Wait> {
    match wait {
        Wait::Zero => Ok(()),
        Wait::Fixed(tm) => {
            std::thread::sleep(tm);
            Ok(())
        }
        Wait::Graceful { timeout } => {
            if counter.load(atomic::Ordering::Acquire) == 0 {
                return Ok(());
            }

            // Σ2^n = 2^(n+1)-1 = 1, 3, 7, 15, 31, 63, 127, 255, 511, 1023
            let s = timeout.div_f32(1023.0);
            let mut r = 2.0;

            let start = time::Instant::now();
            let mut elapsed: time::Duration = start.elapsed();
            while elapsed < timeout {
                std::thread::sleep(s.mul_f32(r - 1.0).saturating_sub(elapsed));
                if counter.load(atomic::Ordering::Acquire) == 0 {
                    return Ok(());
                }
                r = r * 2.0;
                elapsed = start.elapsed();
            }
            Err(wait)
        }
    }
}

#[cfg(test)]
mod tests_of_wait_of_phased_cell {
    use super::*;

    #[test]
    fn test_of_zero_wait() {
        let pc = PhasedCell::<bool>::new(true);
        let st = time::Instant::now();
        let r = pc.pause(Wait::Zero);
        let d = st.elapsed();
        assert!(r.is_ok());
        assert!(d < time::Duration::from_millis(1));
    }

    #[test]
    fn test_of_fixed_wait() {
        let pc = PhasedCell::<bool>::new(true);
        let st = time::Instant::now();
        let r = pc.pause(Wait::Fixed(time::Duration::from_millis(50)));
        let d = st.elapsed();
        assert!(r.is_ok());
        assert!(d >= time::Duration::from_millis(50));
        assert!(d < time::Duration::from_millis(55));
    }

    #[test]
    fn test_of_graceful_wait_if_read_count_is_zero() {
        let pc = PhasedCell::<bool>::new(true);
        let st = time::Instant::now();
        let r = pc.pause(Wait::Graceful {
            timeout: time::Duration::from_secs(1),
        });
        let d = st.elapsed();
        assert!(r.is_ok());
        assert!(d < time::Duration::from_millis(1));
    }

    #[test]
    fn test_of_graceful_wait_if_timeout_is_zero() {
        let pc = PhasedCell::<bool>::new(true);
        pc.read_count.fetch_add(1, atomic::Ordering::Release);
        let st = time::Instant::now();
        let r = pc.pause(Wait::Graceful {
            timeout: time::Duration::from_secs(0),
        });
        let d = st.elapsed();
        assert!(r.is_err());
        assert_eq!(
            r.unwrap_err(),
            Wait::Graceful {
                timeout: time::Duration::from_secs(0)
            }
        );
        assert!(d < time::Duration::from_millis(1));
    }

    #[test]
    fn test_of_graceful_wait_if_read_count_is_zero_and_timeout_is_zero() {
        let pc = PhasedCell::<bool>::new(true);
        let st = time::Instant::now();
        let r = pc.pause(Wait::Graceful {
            timeout: time::Duration::from_secs(0),
        });
        let d = st.elapsed();
        assert!(r.is_ok());
        assert!(d < time::Duration::from_millis(1));
    }

    #[test]
    fn test_of_graceful_wait_if_read_count_becomes_zero_after_1st_sleep() {
        let pc = PhasedCell::<bool>::new(true);
        pc.read_count.fetch_add(1, atomic::Ordering::Release);

        let pc_1 = std::sync::Arc::new(pc);
        let pc_2 = pc_1.clone();

        std::thread::spawn(move || {
            std::thread::sleep(time::Duration::from_micros(500)); // < 1000_000 / 1023 ≒ 977
            pc_2.read_count.fetch_sub(1, atomic::Ordering::Release);
        });

        let st = time::Instant::now();
        let r = pc_1.pause(Wait::Graceful {
            timeout: time::Duration::from_secs(1),
        });
        let d = st.elapsed();
        assert!(r.is_ok());
        assert!(d > time::Duration::from_micros(977));
        assert!(d < time::Duration::from_micros(1500));
    }

    #[test]
    fn test_of_graceful_wait_if_read_count_becomes_zero_after_2nd_sleep() {
        let pc = PhasedCell::<bool>::new(true);
        pc.read_count.fetch_add(1, atomic::Ordering::Release);

        let pc_1 = std::sync::Arc::new(pc);
        let pc_2 = pc_1.clone();

        std::thread::spawn(move || {
            std::thread::sleep(time::Duration::from_micros(2500)); // < 1000_000 / 1023 * 3 ≒ 2932
            pc_2.read_count.fetch_sub(1, atomic::Ordering::Release);
        });

        let st = time::Instant::now();
        let r = pc_1.pause(Wait::Graceful {
            timeout: time::Duration::from_secs(1),
        });
        let d = st.elapsed();
        assert!(r.is_ok());
        assert!(d > time::Duration::from_micros(2932));
        assert!(d < time::Duration::from_micros(3500));
    }

    #[test]
    fn test_of_graceful_wait_if_read_count_becomes_zero_after_3rd_sleep() {
        let pc = PhasedCell::<bool>::new(true);
        pc.read_count.fetch_add(1, atomic::Ordering::Release);

        let pc_1 = std::sync::Arc::new(pc);
        let pc_2 = pc_1.clone();

        std::thread::spawn(move || {
            std::thread::sleep(time::Duration::from_micros(6000)); // < 1000_000 / 1023 * 7 ≒ 6842
            pc_2.read_count.fetch_sub(1, atomic::Ordering::Release);
        });

        let st = time::Instant::now();
        let r = pc_1.pause(Wait::Graceful {
            timeout: time::Duration::from_secs(1),
        });
        let d = st.elapsed();
        assert!(r.is_ok());
        assert!(d < time::Duration::from_micros(8000));
    }

    #[test]
    fn test_of_graceful_wait_if_read_count_becomes_zero_after_8th_sleep() {
        let pc = PhasedCell::<bool>::new(true);
        pc.read_count.fetch_add(1, atomic::Ordering::Release);

        let pc_1 = std::sync::Arc::new(pc);
        let pc_2 = pc_1.clone();

        std::thread::spawn(move || {
            std::thread::sleep(time::Duration::from_micros(240_000)); // < 1000_000 / 1023 * 255 ≒ 249_266
            pc_2.read_count.fetch_sub(1, atomic::Ordering::Release);
        });

        let st = time::Instant::now();
        let r = pc_1.pause(Wait::Graceful {
            timeout: time::Duration::from_secs(1),
        });
        let d = st.elapsed();
        assert!(r.is_ok());
        assert!(d < time::Duration::from_micros(255_000));
    }

    #[test]
    fn test_of_graceful_wait_if_read_count_becomes_zero_after_9th_sleep() {
        let pc = PhasedCell::<bool>::new(true);
        pc.read_count.fetch_add(1, atomic::Ordering::Release);

        let pc_1 = std::sync::Arc::new(pc);
        let pc_2 = pc_1.clone();

        std::thread::spawn(move || {
            std::thread::sleep(time::Duration::from_micros(490_000)); // < 1000_000 / 1023 * 511 ≒ 499_511
            pc_2.read_count.fetch_sub(1, atomic::Ordering::Release);
        });

        let st = time::Instant::now();
        let r = pc_1.pause(Wait::Graceful {
            timeout: time::Duration::from_secs(1),
        });
        let d = st.elapsed();
        assert!(r.is_ok());
        assert!(d < time::Duration::from_micros(505_000));
    }

    #[test]
    fn test_of_graceful_wait_if_read_count_becomes_zero_after_10th_sleep() {
        let pc = PhasedCell::<bool>::new(true);
        pc.read_count.fetch_add(1, atomic::Ordering::Release);

        let pc_1 = std::sync::Arc::new(pc);
        let pc_2 = pc_1.clone();

        std::thread::spawn(move || {
            std::thread::sleep(time::Duration::from_micros(990_000)); // < 1000_000
            pc_2.read_count.fetch_sub(1, atomic::Ordering::Release);
        });

        let st = time::Instant::now();
        let r = pc_1.pause(Wait::Graceful {
            timeout: time::Duration::from_secs(1),
        });
        let d = st.elapsed();
        assert!(r.is_ok());
        assert!(d < time::Duration::from_micros(1005_000));
    }

    #[test]
    fn test_of_graceful_wait_if_timeout() {
        let pc = PhasedCell::<bool>::new(true);
        pc.read_count.fetch_add(1, atomic::Ordering::Release);

        let pc_1 = std::sync::Arc::new(pc);
        let pc_2 = pc_1.clone();

        std::thread::spawn(move || {
            std::thread::sleep(time::Duration::from_micros(1005_000)); // > 1000_000
            pc_2.read_count.fetch_sub(1, atomic::Ordering::Release);
        });

        let st = time::Instant::now();
        let r = pc_1.pause(Wait::Graceful {
            timeout: time::Duration::from_secs(1),
        });
        let d = st.elapsed();
        assert!(r.is_err());
        assert!(d < time::Duration::from_micros(1005_000));
    }
}
