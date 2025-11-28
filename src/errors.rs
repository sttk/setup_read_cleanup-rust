// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use crate::{Phase, PhasedError, PhasedErrorKind};

use std::{any, error, fmt};

pub(crate) const METHOD_READ: &'static str = "read";
pub(crate) const METHOD_READ_RELAXED: &'static str = "read_relaxed";
pub(crate) const METHOD_GET_MUT_UNLOCKED: &'static str = "get_mut_unlocked";
pub(crate) const METHOD_LOCK: &'static str = "lock";
pub(crate) const METHOD_LOCK_ASYNC: &'static str = "lock_async";

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

    /// Gets the phase in which the error occurred.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Gets the kind of error that occurred.
    pub fn kind(&self) -> PhasedErrorKind {
        self.kind
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
            PhasedErrorKind::CannotCallUnlessPhaseRead("read"),
        );

        assert_eq!(e.phase(), Phase::Setup);
        assert_eq!(e.kind(), PhasedErrorKind::CannotCallUnlessPhaseRead("read"));
        assert!(e.source().is_none());
    }

    #[test]
    fn test_with_source() {
        let e0 = std::io::Error::new(std::io::ErrorKind::Interrupted, "oh no!");
        let e = PhasedError::with_source(
            Phase::Setup,
            PhasedErrorKind::CannotCallUnlessPhaseRead("read_relaxed"),
            e0,
        );

        assert_eq!(e.phase(), Phase::Setup);
        assert_eq!(
            e.kind(),
            PhasedErrorKind::CannotCallUnlessPhaseRead("read_relaxed"),
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
            PhasedErrorKind::CannotCallUnlessPhaseRead("method"),
        );
        assert_eq!(format!("{:?}", e), "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallUnlessPhaseRead(\"method\") }");

        let e0 = std::io::Error::new(std::io::ErrorKind::Interrupted, "oh no!");
        let e = PhasedError::with_source(
            Phase::Setup,
            PhasedErrorKind::CannotCallUnlessPhaseRead("lock_async"),
            e0,
        );
        assert_eq!(format!("{:?}", e), "setup_read_cleanup::PhasedError { phase: Setup, kind: CannotCallUnlessPhaseRead(\"lock_async\"), source: Custom { kind: Interrupted, error: \"oh no!\" } }");
    }

    #[test]
    fn test_display() {
        let e = PhasedError::new(
            Phase::Setup,
            PhasedErrorKind::CannotCallUnlessPhaseRead("lock"),
        );
        assert_eq!(
            format!("{}", e),
            "[Setup] CannotCallUnlessPhaseRead(\"lock\")"
        );

        let e0 = std::io::Error::new(std::io::ErrorKind::Interrupted, "oh no!");
        let e = PhasedError::with_source(
            Phase::Setup,
            PhasedErrorKind::CannotCallUnlessPhaseRead("lock"),
            e0,
        );
        assert_eq!(
            format!("{}", e),
            "[Setup] CannotCallUnlessPhaseRead(\"lock\")"
        );
    }
}
