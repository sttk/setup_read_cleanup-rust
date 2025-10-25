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
        write!(f, "phase: {:?}, kind: {:?}", self.phase, self.kind)?;
        if let Some(source) = &self.source {
            write!(f, ", source: {:?}", source)?;
        }
        write!(f, " }}")
    }
}

impl fmt::Display for PhasedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {:?}", self.phase, self.kind)
    }
}

impl error::Error for PhasedError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn error::Error + 'static))
    }
}

#[cfg(test)]
mod tests_of_phased_error {
    use super::*;
    use std::error::Error;

    #[test]
    fn test_new() {
        let e = PhasedError::new(
            Phase::Setup,
            PhasedErrorKind::CannotCallUnlessPhaseRead("method".to_string()),
        );

        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("method".to_string())
        );
        assert!(e.source().is_none());
    }

    #[test]
    fn test_with_source() {
        let e0 = std::io::Error::new(std::io::ErrorKind::Interrupted, "oh no!");
        let e = PhasedError::with_source(
            Phase::Setup,
            PhasedErrorKind::CannotCallUnlessPhaseRead("method".to_string()),
            e0,
        );

        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("method".to_string())
        );
        match e.source() {
            Some(ee) => match ee.downcast_ref::<std::io::Error>() {
                Some(ioe) => {
                    assert_eq!(ioe.kind(), std::io::ErrorKind::Interrupted);
                }
                None => panic!(),
            },
            None => panic!(),
        }
    }

    #[test]
    fn test_debug() {
        let e = PhasedError::new(
            Phase::Setup,
            PhasedErrorKind::CannotCallUnlessPhaseRead("method".to_string()),
        );
        assert_eq!(format!("{:?}", e), "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallUnlessPhaseRead(\"method\") }");

        let e0 = std::io::Error::new(std::io::ErrorKind::Interrupted, "oh no!");
        let e = PhasedError::with_source(
            Phase::Setup,
            PhasedErrorKind::CannotCallUnlessPhaseRead("method".to_string()),
            e0,
        );
        assert_eq!(format!("{:?}", e), "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallUnlessPhaseRead(\"method\"), source: Custom { kind: Interrupted, error: \"oh no!\" } }");
    }

    #[test]
    fn test_display() {
        let e = PhasedError::new(
            Phase::Setup,
            PhasedErrorKind::CannotCallUnlessPhaseRead("method".to_string()),
        );
        assert_eq!(
            format!("{}", e),
            "[Setup] CannotCallUnlessPhaseRead(\"method\")"
        );

        let e0 = std::io::Error::new(std::io::ErrorKind::Interrupted, "oh no!");
        let e = PhasedError::with_source(
            Phase::Setup,
            PhasedErrorKind::CannotCallUnlessPhaseRead("method".to_string()),
            e0,
        );
        assert_eq!(
            format!("{}", e),
            "[Setup] CannotCallUnlessPhaseRead(\"method\")"
        );
    }
}
