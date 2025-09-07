// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::{Phase, PhasedError, PhasedErrorKind};

use std::{any, fmt};

impl PhasedError {
    pub(crate) fn new(phase: Phase, kind: PhasedErrorKind, message: &str) -> Self {
        Self {
            phase,
            kind,
            message: message.to_string(),
        }
    }
}

impl fmt::Debug for PhasedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {{ phase: {}, kind: {:?}, message: {:?} }}",
            any::type_name::<PhasedError>(),
            self.phase,
            self.kind,
            self.message,
        )
    }
}

#[cfg(test)]
mod tests_of_phased_error {
    use super::*;
    use crate::WaitStrategy;
    use std::time;

    #[test]
    fn test_new() {
        let e = PhasedError::new(
            Phase::Setup,
            PhasedErrorKind::TransitionToReadFailed,
            "Fail to do something.",
        );

        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(e.kind, PhasedErrorKind::TransitionToReadFailed);
        assert_eq!(e.message, "Fail to do something.");
    }

    #[test]
    fn test_debug() {
        let e = PhasedError::new(
            Phase::Setup,
            PhasedErrorKind::TransitionToCleanupTimeout(WaitStrategy::GracefulWait {
                first: time::Duration::from_secs(1),
                interval: time::Duration::from_millis(10),
                timeout: time::Duration::from_secs(5),
            }),
            "Fail to do something.",
        );

        assert_eq!(
            format!("{e:?}"),
            "setup_read_cleanup::PhasedError { phase: Setup, kind: TransitionToCleanupTimeout(GracefulWait { first: 1s, interval: 10ms, timeout: 5s }), message: \"Fail to do something.\" }"
        );
    }
}
