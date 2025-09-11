// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::{Phase, PhasedError, PhasedErrorKind};

use std::{any, error, fmt};

impl PhasedError {
    pub(crate) fn new(phase: Phase, kind: PhasedErrorKind) -> Self {
        Self {
            phase,
            kind,
            source: None,
        }
    }

    pub(crate) fn with_source<E>(phase: Phase, kind: PhasedErrorKind, e: E) -> Self
    where
        E: error::Error + Send + Sync + 'static,
    {
        Self {
            phase,
            kind,
            source: Some(Box::new(e)),
        }
    }
}

impl fmt::Debug for PhasedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {{ ", any::type_name::<PhasedError>())?;
        write!(f, "phase: {}, kind: {:?}", self.phase, self.kind)?;
        if let Some(source) = &self.source {
            write!(f, ", source: {:?}", source)?;
        }
        write!(f, " }}")
    }
}

#[cfg(test)]
mod tests_of_phased_error {
    use super::*;
    use crate::WaitStrategy;
    use std::time;

    #[test]
    fn test_new() {
        let e = PhasedError::new(Phase::Setup, PhasedErrorKind::TransitionToReadFailed);

        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(e.kind, PhasedErrorKind::TransitionToReadFailed);
        assert!(e.source.is_none());
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
        );

        assert_eq!(
            format!("{e:?}"),
            "setup_read_cleanup::PhasedError { phase: Setup, kind: TransitionToCleanupTimeout(GracefulWait { first: 1s, interval: 10ms, timeout: 5s }) }"
        );
    }
}
