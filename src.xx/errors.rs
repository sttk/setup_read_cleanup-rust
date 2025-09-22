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
        write!(f, "phased: {:?}, kind: {:?}", self.phase, self.kind)?;
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
    fn test_with_source() {
        let e0 = std::io::Error::new(std::io::ErrorKind::Interrupted, "oh no!");
        let e = PhasedError::with_source(
            Phase::Setup,
            PhasedErrorKind::TransitionToCleanupTimeout(WaitStrategy::GracefulWait {
                timeout: time::Duration::from_secs(2),
            }),
            e0,
        );

        assert_eq!(e.phase, Phase::Setup);
        if let PhasedErrorKind::TransitionToCleanupTimeout(ws) = e.kind {
            if let WaitStrategy::GracefulWait { timeout } = ws {
                assert_eq!(timeout, time::Duration::from_secs(2));
            } else {
                panic!();
            }
        } else {
            panic!();
        }
        if let Some(e1) = e.source {
            assert_eq!(
                format!("{e1:?}"),
                "Custom { kind: Interrupted, error: \"oh no!\" }"
            );
        } else {
            panic!();
        }
    }

    #[test]
    fn test_debug_without_source() {
        let e = PhasedError::new(Phase::Read, PhasedErrorKind::TransitionToCleanupFailed);
        assert_eq!(
            format!("{e:?}"),
            "setup_read_cleanup::PhasedError { phased: Read, kind: TransitionToCleanupFailed }"
        );
    }

    #[test]
    fn test_debug_with_source() {
        let e0 = std::io::Error::new(std::io::ErrorKind::Interrupted, "oh no!");
        let e = PhasedError::with_source(
            Phase::Setup,
            PhasedErrorKind::TransitionToCleanupTimeout(WaitStrategy::GracefulWait {
                timeout: time::Duration::from_secs(2),
            }),
            e0,
        );
        assert_eq!(format!("{e:?}"), "setup_read_cleanup::PhasedError { phased: Setup, kind: TransitionToCleanupTimeout(GracefulWait { timeout: 2s }), source: Custom { kind: Interrupted, error: \"oh no!\" } }");
    }
}
