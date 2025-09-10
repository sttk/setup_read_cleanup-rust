// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::Phase;

use std::{any, fmt};

pub(crate) const PHASE_SETUP: u8 = 0;
pub(crate) const PHASE_READ: u8 = 1;
pub(crate) const PHASE_CLEANUP: u8 = 2;

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

pub(crate) fn u8_to_phase(phase_code: u8) -> Phase {
    match phase_code {
        PHASE_SETUP => Phase::Setup,
        PHASE_READ => Phase::Read,
        PHASE_CLEANUP => Phase::Cleanup,
        _ => {
            eprintln!(
                "{} is passed an invalid phase code: {}",
                any::type_name::<Phase>(),
                phase_code
            );
            return Phase::Cleanup;
        }
    }
}

#[cfg(test)]
mod tests_of_phase {
    use super::*;

    #[test]
    fn test_debug() {
        assert_eq!(format!("{:?}", Phase::Setup), "Setup");
        assert_eq!(format!("{:?}", Phase::Read), "Read");
        assert_eq!(format!("{:?}", Phase::Cleanup), "Cleanup");
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", Phase::Setup), "Setup");
        assert_eq!(format!("{}", Phase::Read), "Read");
        assert_eq!(format!("{}", Phase::Cleanup), "Cleanup");
    }

    #[test]
    fn test_u8_to_phase() {
        assert_eq!(u8_to_phase(PHASE_SETUP), Phase::Setup);
        assert_eq!(u8_to_phase(PHASE_READ), Phase::Read);
        assert_eq!(u8_to_phase(PHASE_CLEANUP), Phase::Cleanup);
        assert_eq!(u8_to_phase(10), Phase::Cleanup);
    }
}
