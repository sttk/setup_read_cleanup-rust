// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::{
    locking::{Cleanup, Read, Setup},
    Phase,
};

pub(crate) const U8_TO_PHASE: u8 = 0b00_0_00_111u8;
pub(crate) const U8_TO_LOCKING_SETUP: u8 = 0b11_0_00_000u8;
pub(crate) const U8_TO_LOCKING_READ: u8 = 0b00_1_00_000u8;
pub(crate) const U8_TO_LOCKING_CLEANUP: u8 = 0b00_0_11_000u8;

pub(crate) const PHASE_SETUP: u8 = 0b00_0_00_000u8;
pub(crate) const PHASE_READ: u8 = 0b00_0_00_001u8;
pub(crate) const PHASE_CLEANUP: u8 = 0b00_0_00_010u8;
pub(crate) const PHASE_SETUP_TO_READ: u8 = 0b00_0_00_011u8;
pub(crate) const PHASE_READ_TO_CLEANUP: u8 = 0b00_0_00_100u8;

pub(crate) const LOCKING_SETUP_UNLOCK: u8 = 0b00_0_00_000u8;
pub(crate) const LOCKING_SETUP_BLOCKING: u8 = 0b01_0_00_000u8;
#[cfg(feature = "setup_read_cleanup-on-tokio")]
pub(crate) const LOCKING_SETUP_NON_BLOCKING: u8 = 0b10_0_00_000u8;

pub(crate) const LOCKING_READ_FAST: u8 = 0b00_0_00_000u8;
pub(crate) const LOCKING_READ_GRACEFUL: u8 = 0b00_1_00_000u8;

pub(crate) const LOCKING_CLEANUP_UNLOCK: u8 = 0b00_0_00_000u8;
pub(crate) const LOCKING_CLEANUP_BLOCKING: u8 = 0b00_0_01_000u8;
#[cfg(feature = "setup_read_cleanup-on-tokio")]
pub(crate) const LOCKING_CLEANUP_NON_BLOCKING: u8 = 0b00_0_10_000u8;

pub(crate) const fn u8_to_phase(flags: u8) -> Phase {
    let phase_code = flags & U8_TO_PHASE;
    match phase_code {
        PHASE_SETUP => Phase::Setup,
        PHASE_READ => Phase::Read,
        PHASE_CLEANUP => Phase::Cleanup,
        PHASE_SETUP_TO_READ => Phase::Setup,
        PHASE_READ_TO_CLEANUP => Phase::Cleanup,
        _ => Phase::Cleanup,
    }
}

#[inline]
pub(crate) const fn set_phase_code_to_u8(flags: u8, phase_code: u8) -> u8 {
    (flags & !U8_TO_PHASE) | (phase_code & U8_TO_PHASE)
}

pub(crate) const fn u8_to_locking_of_setup(flags: u8) -> Setup {
    let locking = flags & U8_TO_LOCKING_SETUP;
    match locking {
        LOCKING_SETUP_UNLOCK => Setup::Unlock,
        LOCKING_SETUP_BLOCKING => Setup::Blocking,
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        LOCKING_SETUP_NON_BLOCKING => Setup::NonBlocking,
        _ => Setup::Blocking,
    }
}

#[inline]
pub(crate) const fn set_locking_of_setup_to_u8(flags: u8, locking_of_setup: u8) -> u8 {
    (flags & !U8_TO_LOCKING_SETUP) | (locking_of_setup & U8_TO_LOCKING_SETUP)
}

pub(crate) const fn u8_to_locking_of_read(flags: u8) -> Read {
    let locking = flags & U8_TO_LOCKING_READ;
    match locking {
        LOCKING_READ_FAST => Read::Fast,
        LOCKING_READ_GRACEFUL => Read::Graceful,
        _ => Read::Fast,
    }
}

#[inline]
pub(crate) const fn set_locking_of_read_to_u8(flags: u8, locking_of_read: u8) -> u8 {
    (flags & !U8_TO_LOCKING_READ) | (locking_of_read & U8_TO_LOCKING_READ)
}

pub(crate) const fn u8_to_locking_of_cleanup(flags: u8) -> Cleanup {
    let locking = flags & U8_TO_LOCKING_CLEANUP;
    match locking {
        LOCKING_CLEANUP_UNLOCK => Cleanup::Unlock,
        LOCKING_CLEANUP_BLOCKING => Cleanup::Blocking,
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        LOCKING_CLEANUP_NON_BLOCKING => Cleanup::NonBlocking,
        _ => Cleanup::Blocking,
    }
}

#[inline]
pub(crate) const fn set_locking_of_cleanup_to_u8(flags: u8, locking_of_cleanup: u8) -> u8 {
    (flags & !U8_TO_LOCKING_CLEANUP) | (locking_of_cleanup & U8_TO_LOCKING_CLEANUP)
}

#[cfg(test)]
mod tests_of_phase {
    use super::*;

    #[test]
    fn test_of_u8_to_phase() {
        assert_eq!(u8_to_phase(PHASE_SETUP), Phase::Setup);
        assert_eq!(u8_to_phase(PHASE_READ), Phase::Read);
        assert_eq!(u8_to_phase(PHASE_CLEANUP), Phase::Cleanup);
        assert_eq!(u8_to_phase(PHASE_SETUP_TO_READ), Phase::Setup);
        assert_eq!(u8_to_phase(PHASE_READ_TO_CLEANUP), Phase::Cleanup);

        assert_eq!(u8_to_phase(0b00_0_00_101), Phase::Cleanup);
        assert_eq!(u8_to_phase(0b00_0_00_110), Phase::Cleanup);
        assert_eq!(u8_to_phase(0b00_0_00_111), Phase::Cleanup);

        assert_eq!(u8_to_phase(0b01_1_10_000), Phase::Setup);
        assert_eq!(u8_to_phase(0b10_0_01_001), Phase::Read);
        assert_eq!(u8_to_phase(0b11_1_10_010), Phase::Cleanup);
        assert_eq!(u8_to_phase(0b01_1_10_011), Phase::Setup);
        assert_eq!(u8_to_phase(0b10_0_01_100), Phase::Cleanup);
        assert_eq!(u8_to_phase(0b11_1_10_101), Phase::Cleanup);
        assert_eq!(u8_to_phase(0b11_1_11_110), Phase::Cleanup);
        assert_eq!(u8_to_phase(0b11_1_11_111), Phase::Cleanup);
    }

    #[test]
    fn test_of_set_phase_code_to_u8() {
        assert_eq!(
            set_phase_code_to_u8(0b00_0_00_000, PHASE_SETUP),
            0b00_0_00_000
        );
        assert_eq!(
            set_phase_code_to_u8(0b11_1_11_111, PHASE_SETUP),
            0b11_1_11_000
        );
        assert_eq!(
            set_phase_code_to_u8(0b00_0_00_000, PHASE_READ),
            0b00_0_00_001
        );
        assert_eq!(
            set_phase_code_to_u8(0b11_1_11_111, PHASE_READ),
            0b11_1_11_001
        );
        assert_eq!(
            set_phase_code_to_u8(0b00_0_00_000, PHASE_CLEANUP),
            0b00_0_00_010
        );
        assert_eq!(
            set_phase_code_to_u8(0b11_1_11_111, PHASE_CLEANUP),
            0b11_1_11_010
        );
        assert_eq!(
            set_phase_code_to_u8(0b00_0_00_000, PHASE_SETUP_TO_READ),
            0b00_0_00_011
        );
        assert_eq!(
            set_phase_code_to_u8(0b11_1_11_111, PHASE_SETUP_TO_READ),
            0b11_1_11_011
        );
        assert_eq!(
            set_phase_code_to_u8(0b00_0_00_000, PHASE_READ_TO_CLEANUP),
            0b00_0_00_100
        );
        assert_eq!(
            set_phase_code_to_u8(0b11_1_11_111, PHASE_READ_TO_CLEANUP),
            0b11_1_11_100
        );
    }

    #[test]
    fn test_phase_roundtrip() {
        // An arbitrary initial flag value.
        let initial_flags = 0b10101010;
        // Mask for non-phase flags.
        let non_phase_mask = 0b11_1_11_000;

        // Test roundtrip for PHASE_SETUP
        let flags_after_set_setup = set_phase_code_to_u8(initial_flags, PHASE_SETUP);
        assert_eq!(u8_to_phase(flags_after_set_setup), Phase::Setup);
        // Assert that other flags are not modified.
        assert_eq!(
            flags_after_set_setup & non_phase_mask,
            initial_flags & non_phase_mask
        );

        // Test roundtrip for PHASE_READ
        let flags_after_set_read = set_phase_code_to_u8(initial_flags, PHASE_READ);
        assert_eq!(u8_to_phase(flags_after_set_read), Phase::Read);
        // Assert that other flags are not modified.
        assert_eq!(
            flags_after_set_read & non_phase_mask,
            initial_flags & non_phase_mask
        );

        // Test roundtrip for PHASE_CLEANUP
        let flags_after_set_cleanup = set_phase_code_to_u8(initial_flags, PHASE_CLEANUP);
        assert_eq!(u8_to_phase(flags_after_set_cleanup), Phase::Cleanup);
        // Assert that other flags are not modified.
        assert_eq!(
            flags_after_set_cleanup & non_phase_mask,
            initial_flags & non_phase_mask
        );
    }
}

#[cfg(test)]
mod tests_of_locking_setup {
    use super::*;

    #[test]
    fn test_of_u8_to_locking_of_setup() {
        assert_eq!(u8_to_locking_of_setup(LOCKING_SETUP_UNLOCK), Setup::Unlock);
        assert_eq!(
            u8_to_locking_of_setup(LOCKING_SETUP_BLOCKING),
            Setup::Blocking
        );
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            u8_to_locking_of_setup(LOCKING_SETUP_NON_BLOCKING),
            Setup::NonBlocking
        );
        #[cfg(not(feature = "setup_read_cleanup-on-tokio"))]
        assert_eq!(u8_to_locking_of_setup(0b10_0_00_000), Setup::Blocking);

        assert_eq!(u8_to_locking_of_setup(0b11_0_00_000), Setup::Blocking);

        assert_eq!(u8_to_locking_of_setup(0b00_1_01_101), Setup::Unlock);
        assert_eq!(u8_to_locking_of_setup(0b01_0_10_010), Setup::Blocking);

        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(u8_to_locking_of_setup(0b10_1_11_111), Setup::NonBlocking);
        #[cfg(not(feature = "setup_read_cleanup-on-tokio"))]
        assert_eq!(u8_to_locking_of_setup(0b10_1_11_111), Setup::Blocking);

        assert_eq!(u8_to_locking_of_setup(0b11_1_10_101), Setup::Blocking);
    }

    #[test]
    fn test_of_locking_of_setup_to_u8() {
        assert_eq!(
            set_locking_of_setup_to_u8(0b00_0_00_000, LOCKING_SETUP_UNLOCK),
            0b00_0_00_000
        );
        assert_eq!(
            set_locking_of_setup_to_u8(0b11_1_11_111, LOCKING_SETUP_UNLOCK),
            0b00_1_11_111
        );
        assert_eq!(
            set_locking_of_setup_to_u8(0b00_0_00_000, LOCKING_SETUP_BLOCKING),
            0b01_0_00_000
        );
        assert_eq!(
            set_locking_of_setup_to_u8(0b11_1_11_111, LOCKING_SETUP_BLOCKING),
            0b01_1_11_111
        );

        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            set_locking_of_setup_to_u8(0b00_0_00_000, LOCKING_SETUP_NON_BLOCKING),
            0b10_0_00_000
        );
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            set_locking_of_setup_to_u8(0b11_1_11_111, LOCKING_SETUP_NON_BLOCKING),
            0b10_1_11_111
        );
    }
}

#[cfg(test)]
mod tests_of_locking_read {
    use super::*;

    #[test]
    fn test_of_u8_to_locking_of_read() {
        assert_eq!(u8_to_locking_of_read(LOCKING_READ_FAST), Read::Fast);
        assert_eq!(u8_to_locking_of_read(LOCKING_READ_GRACEFUL), Read::Graceful);

        assert_eq!(u8_to_locking_of_read(0b01_0_01_001), Read::Fast);
        assert_eq!(u8_to_locking_of_read(0b10_1_10_101), Read::Graceful);
    }

    #[test]
    fn test_of_locking_of_read_to_u8() {
        assert_eq!(
            set_locking_of_read_to_u8(0b00_0_00_000, LOCKING_READ_FAST),
            0b00_0_00_000
        );
        assert_eq!(
            set_locking_of_read_to_u8(0b00_0_00_000, LOCKING_READ_GRACEFUL),
            0b00_1_00_000
        );

        assert_eq!(
            set_locking_of_read_to_u8(0b11_1_11_111, LOCKING_READ_FAST),
            0b11_0_11_111
        );
        assert_eq!(
            set_locking_of_read_to_u8(0b11_1_11_111, LOCKING_READ_GRACEFUL),
            0b11_1_11_111
        );
    }
}

#[cfg(test)]
mod tests_of_locking_cleanup {
    use super::*;

    #[test]
    fn test_of_u8_to_locking_of_cleanup() {
        assert_eq!(
            u8_to_locking_of_cleanup(LOCKING_CLEANUP_UNLOCK),
            Cleanup::Unlock
        );
        assert_eq!(
            u8_to_locking_of_cleanup(LOCKING_CLEANUP_BLOCKING),
            Cleanup::Blocking
        );

        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            u8_to_locking_of_cleanup(LOCKING_CLEANUP_NON_BLOCKING),
            Cleanup::NonBlocking
        );
        #[cfg(not(feature = "setup_read_cleanup-on-tokio"))]
        assert_eq!(u8_to_locking_of_cleanup(0b00_0_10_000), Cleanup::Blocking);

        assert_eq!(u8_to_locking_of_cleanup(0b00_0_11_000), Cleanup::Blocking);

        assert_eq!(u8_to_locking_of_cleanup(0b10_1_00_101), Cleanup::Unlock);
        assert_eq!(u8_to_locking_of_cleanup(0b01_0_01_010), Cleanup::Blocking);

        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            u8_to_locking_of_cleanup(0b11_1_10_111),
            Cleanup::NonBlocking
        );
        #[cfg(not(feature = "setup_read_cleanup-on-tokio"))]
        assert_eq!(u8_to_locking_of_cleanup(0b11_1_10_111), Cleanup::Blocking);

        assert_eq!(u8_to_locking_of_cleanup(0b11_0_11_101), Cleanup::Blocking);
    }

    #[test]
    fn test_of_locking_of_cleanup_to_u8() {
        assert_eq!(
            set_locking_of_cleanup_to_u8(0b00_0_00_000, LOCKING_CLEANUP_UNLOCK),
            0b00_0_00_000
        );
        assert_eq!(
            set_locking_of_cleanup_to_u8(0b00_0_00_000, LOCKING_CLEANUP_BLOCKING),
            0b00_0_01_000
        );
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            set_locking_of_cleanup_to_u8(0b00_0_00_000, LOCKING_CLEANUP_NON_BLOCKING),
            0b00_0_10_000
        );

        assert_eq!(
            set_locking_of_cleanup_to_u8(0b11_1_11_111, LOCKING_CLEANUP_UNLOCK),
            0b11_1_00_111
        );
        assert_eq!(
            set_locking_of_cleanup_to_u8(0b11_1_11_111, LOCKING_CLEANUP_BLOCKING),
            0b11_1_01_111
        );
        #[cfg(feature = "setup_read_cleanup-on-tokio")]
        assert_eq!(
            set_locking_of_cleanup_to_u8(0b11_1_11_111, LOCKING_CLEANUP_NON_BLOCKING),
            0b11_1_10_111
        );
    }
}
