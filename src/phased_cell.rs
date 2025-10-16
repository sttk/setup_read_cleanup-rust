// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::phase::*;
use crate::{Phase, PhasedCell, PhasedError, PhasedErrorKind, Wait};

use std::{any, cell, error, marker, sync::atomic, thread};

unsafe impl<T: Send + Sync> Sync for PhasedCell<T> {}
unsafe impl<T: Send + Sync> Send for PhasedCell<T> {}

impl<T: Send + Sync> PhasedCell<T> {
    pub const fn new(data: T) -> Self {
        Self {
            phase: atomic::AtomicU8::new(PHASE_SETUP),
            read_count: atomic::AtomicUsize::new(0),
            data_cell: cell::UnsafeCell::new(data),
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

        let data = unsafe { &*self.data_cell.get() };
        Ok(data)
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

        let data = unsafe { &*self.data_cell.get() };
        Ok(data)
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

        if let Err(_) = self.read_count.fetch_update(
            atomic::Ordering::AcqRel,
            atomic::Ordering::Acquire,
            |count| {
                if count == 0 {
                    None
                } else {
                    Some(count - 1)
                }
            },
        ) {
            eprintln!(
                "{} is called excessively.",
                Self::method_name("finish_read_gracefully"),
            );
        }

        Ok(())
    }

    pub fn transition_to_cleanup(&self, wait: Wait) -> Result<(), PhasedError> {
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
                let result = self.pause(wait);
                self.change_phase(PHASE_SETUP_TO_CLEANUP, PHASE_CLEANUP);
                match result {
                    Ok(_) => Ok(()),
                    Err(w) => Err(PhasedError::new(
                        u8_to_phase(PHASE_SETUP_TO_CLEANUP),
                        PhasedErrorKind::TransitionToCleanupTimeout(wait),
                    )),
                }
            }
            Ok(PHASE_READ) => {
                let result = self.pause(wait);
                self.change_phase(PHASE_READ_TO_CLEANUP, PHASE_CLEANUP);
                match result {
                    Ok(_) => Ok(()),
                    Err(w) => Err(PhasedError::new(
                        u8_to_phase(PHASE_READ_TO_CLEANUP),
                        PhasedErrorKind::TransitionToCleanupTimeout(wait),
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

    pub fn transition_to_read<F, E>(&self, mut f: F) -> Result<(), PhasedError>
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
            Ok(old_phase_cd) => {
                let data = unsafe { &mut *self.data_cell.get() };
                if let Err(e) = f(data) {
                    self.change_phase(PHASE_SETUP_TO_READ, old_phase_cd);
                    Err(PhasedError::with_source(
                        u8_to_phase(PHASE_SETUP_TO_READ),
                        PhasedErrorKind::FailToRunClosureDuringTransitionToRead,
                        e,
                    ))
                } else {
                    self.change_phase(PHASE_SETUP_TO_READ, PHASE_READ);
                    Ok(())
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

    pub fn get_mut(&self) -> Result<&mut T, PhasedError> {
        let phase = self.phase.load(atomic::Ordering::Acquire);
        if phase != PHASE_SETUP {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseSetup(Self::method_name("get_mut")),
            ));
        } else if phase != PHASE_CLEANUP {
            return Err(PhasedError::new(
                u8_to_phase(phase),
                PhasedErrorKind::CannotCallUnlessPhaseCleanup(Self::method_name("get_mut")),
            ));
        }

        let data = unsafe { &mut *self.data_cell.get() };
        Ok(data)
    }

    #[inline]
    fn method_name(m: &str) -> String {
        format!("{}::{}", any::type_name::<PhasedCell<T>>(), m)
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
mod tests_of_phased_cell {
    use super::*;
    use std::sync::Arc;
    use std::{fmt, time};

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
    impl error::Error for MyError {}

    #[test]
    fn test_setup_read_cleanup_with_wait_zero() {
        let cell = PhasedCell::new(MyStruct::new());
        assert_eq!(cell.phase_fast(), Phase::Setup);
        assert_eq!(cell.phase_exact(), Phase::Setup);
    }
}
