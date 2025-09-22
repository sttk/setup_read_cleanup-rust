// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::locking::{Cleanup, Setup};

pub(crate) const U8_TO_LOCKING_SETUP: u8 = 0b00_00_00_11u8;
pub(crate) const U8_TO_LOCKING_CLEANUP: u8 = 0b00_00_11_00u8;

pub(crate) const LOCKING_SETUP_UNLOCK: u8 = 0b00_00_00_01u8;

#[cfg(feature = "setup_read_cleanup-blocking")]
pub(crate) const LOCKING_SETUP_BLOCKING: u8 = 0b00_00_00_10u8;

#[cfg(feature = "setup_read_cleanup-on-tokio")]
pub(crate) const LOCKING_SETUP_NON_BLOCKING: u8 = 0b00_00_00_11u8;

pub(crate) const LOCKING_CLEANUP_UNLOCK: u8 = 0b00_00_01_00u8;

#[cfg(feature = "setup_read_cleanup-blocking")]
pub(crate) const LOCKING_CLEANUP_BLOCKING: u8 = 0b00_00_10_00u8;

#[cfg(feature = "setup_read_cleanup-on-tokio")]
pub(crate) const LOCKING_CLEANUP_NON_BLOCKING: u8 = 0b00_00_11_00u8;

pub(crate) const fn u8_to_locking_of_setup(bits: u8) -> Setup {
    let locking = bits & U8_TO_LOCKING_SETUP;
    match locking {
        LOCKING_SETUP_UNLOCK => Setup::Unlock,

        #[cfg(feature = "setup_read_cleanup-blocking")]
        LOCKING_SETUP_BLOCKING => Setup::Blocking,

        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        LOCKING_SETUP_NON_BLOCKING => Setup::NonBlocking,

        _ => Setup::Unlock,
    }
}

#[inline]
pub(crate) const fn set_locking_of_setup_to_u8(bits: u8, locking_of_setup: u8) -> u8 {
    (bits & !U8_TO_LOCKING_SETUP) | (locking_of_setup & U8_TO_LOCKING_SETUP)
}

pub(crate) const fn u8_to_locking_of_cleanup(bits: u8) -> Cleanup {
    let locking = bits & U8_TO_LOCKING_CLEANUP;
    match locking {
        LOCKING_CLEANUP_UNLOCK => Cleanup::Unlock,

        #[cfg(feature = "setup_read_cleanup-blocking")]
        LOCKING_CLEANUP_BLOCKING => Cleanup::Blocking,

        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        LOCKING_CLEANUP_NON_BLOCKING => Cleanup::NonBlocking,

        _ => Cleanup::Unlock,
    }
}

#[inline]
pub(crate) const fn set_locking_of_cleanup_to_u8(bits: u8, locking_of_cleanup: u8) -> u8 {
    (bits & !U8_TO_LOCKING_CLEANUP) | (locking_of_cleanup & U8_TO_LOCKING_CLEANUP)
}

#[cfg(test)]
mod tests_of_locking {
    use super::*;

    #[test]
    fn test_of_u8_to_locking_of_setup() {
        assert_eq!(u8_to_locking_of_setup(LOCKING_SETUP_UNLOCK), Setup::Unlock);

        #[cfg(feature = "setup_read_cleanup-blocking")]
        assert_eq!(
            u8_to_locking_of_setup(LOCKING_SETUP_BLOCKING),
            Setup::Blocking
        );

        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            u8_to_locking_of_setup(LOCKING_SETUP_NON_BLOCKING),
            Setup::NonBlocking
        );

        assert_eq!(u8_to_locking_of_setup(0b00_00_00_00), Setup::Unlock);

        assert_eq!(u8_to_locking_of_setup(0b11_11_11_01), Setup::Unlock);

        #[cfg(feature = "setup_read_cleanup-blocking")]
        assert_eq!(u8_to_locking_of_setup(0b11_11_11_10), Setup::Blocking);

        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(u8_to_locking_of_setup(0b11_11_11_11), Setup::NonBlocking);

        assert_eq!(u8_to_locking_of_setup(0b11_11_11_00), Setup::Unlock);
    }

    #[test]
    fn test_of_set_locking_of_setup_to_u8() {
        assert_eq!(
            set_locking_of_setup_to_u8(0b00_00_00_00, LOCKING_SETUP_UNLOCK),
            0b00_00_00_01
        );
        assert_eq!(
            set_locking_of_setup_to_u8(0b11_11_11_11, LOCKING_SETUP_UNLOCK),
            0b11_11_11_01
        );

        #[cfg(feature = "setup_read_cleanup-blocking")]
        assert_eq!(
            set_locking_of_setup_to_u8(0b00_00_00_00, LOCKING_SETUP_BLOCKING),
            0b00_00_00_10
        );
        #[cfg(feature = "setup_read_cleanup-blocking")]
        assert_eq!(
            set_locking_of_setup_to_u8(0b11_11_11_11, LOCKING_SETUP_BLOCKING),
            0b11_11_11_10
        );

        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            set_locking_of_setup_to_u8(0b00_00_00_00, LOCKING_SETUP_NON_BLOCKING),
            0b00_00_00_11
        );
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            set_locking_of_setup_to_u8(0b11_11_11_11, LOCKING_SETUP_NON_BLOCKING),
            0b11_11_11_11
        );
    }

    #[test]
    fn test_of_u8_to_locking_of_cleanup() {
        assert_eq!(
            u8_to_locking_of_cleanup(LOCKING_CLEANUP_UNLOCK),
            Cleanup::Unlock
        );

        #[cfg(feature = "setup_read_cleanup-blocking")]
        assert_eq!(
            u8_to_locking_of_cleanup(LOCKING_CLEANUP_BLOCKING),
            Cleanup::Blocking
        );

        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            u8_to_locking_of_cleanup(LOCKING_CLEANUP_NON_BLOCKING),
            Cleanup::NonBlocking
        );

        assert_eq!(u8_to_locking_of_cleanup(0b00_00_00_00), Cleanup::Unlock);

        assert_eq!(u8_to_locking_of_cleanup(0b11_11_01_11), Cleanup::Unlock);

        #[cfg(feature = "setup_read_cleanup-blocking")]
        assert_eq!(u8_to_locking_of_cleanup(0b11_11_10_11), Cleanup::Blocking);

        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            u8_to_locking_of_cleanup(0b11_11_11_11),
            Cleanup::NonBlocking
        );

        assert_eq!(u8_to_locking_of_cleanup(0b11_11_00_11), Cleanup::Unlock);
    }

    #[test]
    fn test_of_set_locking_of_cleanup_to_u8() {
        assert_eq!(
            set_locking_of_cleanup_to_u8(0b00_00_00_00, LOCKING_CLEANUP_UNLOCK),
            0b00_00_01_00
        );
        assert_eq!(
            set_locking_of_cleanup_to_u8(0b11_11_11_11, LOCKING_CLEANUP_UNLOCK),
            0b11_11_01_11
        );

        #[cfg(feature = "setup_read_cleanup-blocking")]
        assert_eq!(
            set_locking_of_cleanup_to_u8(0b00_00_00_00, LOCKING_CLEANUP_BLOCKING),
            0b00_00_10_00
        );
        #[cfg(feature = "setup_read_cleanup-blocking")]
        assert_eq!(
            set_locking_of_cleanup_to_u8(0b11_11_11_11, LOCKING_CLEANUP_BLOCKING),
            0b11_11_10_11
        );

        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            set_locking_of_cleanup_to_u8(0b00_00_00_00, LOCKING_CLEANUP_NON_BLOCKING),
            0b00_00_11_00
        );
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            set_locking_of_cleanup_to_u8(0b11_11_11_11, LOCKING_CLEANUP_NON_BLOCKING),
            0b11_11_11_11
        );
    }
}
