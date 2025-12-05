// Copyright (C) 2025 Takayuki Sato. All Rights Reserved.
// This program is free software under MIT License.
// See the file LICENSE in this distribution for more details.

use super::{GracefulWaitError, GracefulWaitErrorKind};

use std::{any, fmt};

impl GracefulWaitError {
    pub(crate) fn new(kind: GracefulWaitErrorKind) -> Self {
        Self { kind }
    }
}

impl fmt::Debug for GracefulWaitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {{ ", any::type_name::<GracefulWaitError>())?;
        write!(f, "kind: {:?}", self.kind)?;
        write!(f, " }}")
    }
}

impl fmt::Display for GracefulWaitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.kind)
    }
}

impl std::error::Error for GracefulWaitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

#[cfg(test)]
mod tests_of_graceful_wait_error {
    use super::*;
    use std::error::Error;

    #[test]
    fn test_new() {
        let e = GracefulWaitError::new(GracefulWaitErrorKind::TimedOut(
            std::time::Duration::from_secs(1),
        ));

        assert_eq!(
            e.kind,
            GracefulWaitErrorKind::TimedOut(std::time::Duration::from_secs(1)),
        );
        assert!(e.source().is_none());
    }

    #[test]
    fn test_debug() {
        let e = GracefulWaitError::new(GracefulWaitErrorKind::TimedOut(
            std::time::Duration::from_secs(12),
        ));
        assert_eq!(
            format!("{:?}", e),
            "setup_read_cleanup::graceful::GracefulWaitError { kind: TimedOut(12s) }"
        );
    }

    #[test]
    fn test_display() {
        let e = GracefulWaitError::new(GracefulWaitErrorKind::TimedOut(
            std::time::Duration::from_secs(12),
        ));
        assert_eq!(format!("{}", e), "TimedOut(12s)");
    }
}
