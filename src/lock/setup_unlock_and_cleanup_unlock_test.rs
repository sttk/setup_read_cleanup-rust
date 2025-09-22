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
fn test_setup_read_cleanup_simply() {
    let lock = PhasedLock::new(MyStruct::new(), Setup::Unlock, Cleanup::Unlock);
    assert_eq!(lock.phase_fast(), Phase::Setup);
    assert_eq!(lock.phase_exact(), Phase::Setup);

    if let Ok(data) = lock.get_mut_for_setup() {
        data.add("Hello".to_string());
    } else {
        panic!();
    }

    if let Err(e) = lock.read_fast() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_fast".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_gracefully() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_gracefully".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallOnPhaseSetup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::finish_reading_gracefully".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.get_mut_for_cleanup() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseCleanup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_cleanup".to_string())
        );
    } else {
        panic!();
    }

    // Setup -> Read
    if let Err(e) = lock.transition_to_read(|data| {
        data.add("World".to_string());
        Ok::<(), MyError>(())
    }) {
        panic!("{e:?}");
    }
    if let Err(e) = lock.transition_to_read(|data| {
        data.add("XXX".to_string());
        Ok::<(), MyError>(())
    }) {
        assert_eq!(e.phase, Phase::Read);
        assert_eq!(e.kind, PhasedErrorKind::PhaseIsAlreadyRead);
    } else {
        panic!();
    }

    assert_eq!(lock.phase_fast(), Phase::Read);
    assert_eq!(lock.phase_exact(), Phase::Read);

    if let Err(e) = lock.get_mut_for_setup() {
        assert_eq!(e.phase, Phase::Read);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseSetup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_setup".to_string())
        );
    } else {
        panic!();
    }

    match lock.read_fast() {
        Ok(data) => {
            //data.add("World".to_string()); // ERROR: `data` is a `&` reference, so the data it refers to cannot be borrowed as mutable

            assert_eq!(data.vec, &["Hello".to_string(), "World".to_string()]);
        }
        Err(e) => panic!("{e:?}"),
    }

    match lock.read_gracefully() {
        Ok(data) => {
            //data.add("World".to_string()); // ERROR: `data` is a `&` reference, so the data it refers to cannot be borrowed as mutable

            assert_eq!(data.vec, &["Hello".to_string(), "World".to_string()]);
        }
        Err(e) => panic!("{e:?}"),
    }

    match lock.read_gracefully() {
        Ok(data) => {
            //data.add("World".to_string()); // ERROR: `data` is a `&` reference, so the data it refers to cannot be borrowed as mutable

            assert_eq!(data.vec, &["Hello".to_string(), "World".to_string()]);
        }
        Err(e) => panic!("{e:?}"),
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        panic!("{e:?}");
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        panic!("{e:?}");
    }

    if let Err(e) = lock.get_mut_for_cleanup() {
        assert_eq!(e.phase, Phase::Read);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseCleanup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_cleanup".to_string())
        );
    } else {
        panic!();
    }

    // Read -> Cleanup
    if let Err(e) = lock.transition_to_cleanup(WaitStrategy::NoWait) {
        panic!("{e:?}");
    }

    assert_eq!(lock.phase_fast(), Phase::Cleanup);
    assert_eq!(lock.phase_exact(), Phase::Cleanup);

    if let Err(e) = lock.get_mut_for_setup() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseSetup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_setup".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_fast() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_fast".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_fast() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_fast".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_gracefully() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_gracefully".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        panic!("{e:?}");
    }

    if let Ok(data) = lock.get_mut_for_cleanup() {
        data.clear();
        assert_eq!(data.vec, &[] as &[String]);
    } else {
        panic!();
    }

    if let Err(e) = lock.transition_to_cleanup(WaitStrategy::NoWait) {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(e.kind, PhasedErrorKind::PhaseIsAlreadyCleanup);
    } else {
        panic!();
    }
}

#[test]
fn test_setup_read_cleanup_fixed_wait() {
    let lock = PhasedLock::new(MyStruct::new(), Setup::Unlock, Cleanup::Unlock);
    assert_eq!(lock.phase_fast(), Phase::Setup);
    assert_eq!(lock.phase_exact(), Phase::Setup);

    if let Ok(data) = lock.get_mut_for_setup() {
        data.add("Hello".to_string());
    } else {
        panic!();
    }

    if let Err(e) = lock.read_fast() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_fast".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_gracefully() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_gracefully".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallOnPhaseSetup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::finish_reading_gracefully".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.get_mut_for_cleanup() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseCleanup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_cleanup".to_string())
        );
    } else {
        panic!();
    }

    // Setup -> Read
    if let Err(e) = lock.transition_to_read(|data| {
        data.add("World".to_string());
        Ok::<(), MyError>(())
    }) {
        panic!("{e:?}");
    }

    assert_eq!(lock.phase_fast(), Phase::Read);
    assert_eq!(lock.phase_exact(), Phase::Read);

    if let Err(e) = lock.get_mut_for_setup() {
        assert_eq!(e.phase, Phase::Read);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseSetup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_setup".to_string())
        );
    } else {
        panic!();
    }

    match lock.read_fast() {
        Ok(data) => {
            // data.add("World".to_string()); // ERROR: `data` is a `&` reference, so the data it refers to cannot be borrowed as mutable

            assert_eq!(data.vec, &["Hello".to_string(), "World".to_string()]);
        }
        Err(e) => panic!("{e:?}"),
    }

    match lock.read_gracefully() {
        Ok(data) => {
            //data.add("World".to_string()); // ERROR: `data` is a `&` reference, so the data it refers to cannot be borrowed as mutable

            assert_eq!(data.vec, &["Hello".to_string(), "World".to_string()]);
        }
        Err(e) => panic!("{e:?}"),
    }

    match lock.read_gracefully() {
        Ok(data) => {
            //data.add("World".to_string()); // ERROR: `data` is a `&` reference, so the data it refers to cannot be borrowed as mutable

            assert_eq!(data.vec, &["Hello".to_string(), "World".to_string()]);
        }
        Err(e) => panic!("{e:?}"),
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        panic!("{e:?}");
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        panic!("{e:?}");
    }

    if let Err(e) = lock.get_mut_for_cleanup() {
        assert_eq!(e.phase, Phase::Read);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseCleanup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_cleanup".to_string())
        );
    } else {
        panic!();
    }

    // Read -> Cleanup
    if let Err(e) =
        lock.transition_to_cleanup(WaitStrategy::FixedWait(time::Duration::from_secs(1)))
    {
        panic!("{e:?}");
    }

    assert_eq!(lock.phase_fast(), Phase::Cleanup);
    assert_eq!(lock.phase_exact(), Phase::Cleanup);

    if let Err(e) = lock.get_mut_for_setup() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseSetup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_setup".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_fast() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_fast".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_fast() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_fast".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_gracefully() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_gracefully".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        panic!("{e:?}");
    }

    if let Ok(data) = lock.get_mut_for_cleanup() {
        data.clear();
        assert_eq!(data.vec, &[] as &[String]);
    } else {
        panic!();
    }

    if let Err(e) = lock.transition_to_cleanup(WaitStrategy::FixedWait(
        time::Duration::from_secs(1),
    )) {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(e.kind, PhasedErrorKind::PhaseIsAlreadyCleanup);
    } else {
        panic!();
    }
}

#[test]
fn test_setup_read_cleanup_graceful_wait() {
    let lock = PhasedLock::new(MyStruct::new(), Setup::Unlock, Cleanup::Unlock);
    assert_eq!(lock.phase_fast(), Phase::Setup);
    assert_eq!(lock.phase_exact(), Phase::Setup);

    if let Ok(data) = lock.get_mut_for_setup() {
        data.add("Hello".to_string());
    } else {
        panic!();
    }

    if let Err(e) = lock.read_fast() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_fast".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_gracefully() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_gracefully".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallOnPhaseSetup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::finish_reading_gracefully".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.get_mut_for_cleanup() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseCleanup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_cleanup".to_string())
        );
    } else {
        panic!();
    }

    // Setup -> Read
    if let Err(e) = lock.transition_to_read(|data| {
        data.add("World".to_string());
        Ok::<(), MyError>(())
    }) {
        panic!("{e:?}");
    }

    assert_eq!(lock.phase_fast(), Phase::Read);
    assert_eq!(lock.phase_exact(), Phase::Read);

    if let Err(e) = lock.get_mut_for_setup() {
        assert_eq!(e.phase, Phase::Read);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseSetup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_setup".to_string())
        );
    } else {
        panic!();
    }

    match lock.read_fast() {
        Ok(data) => {
            // data.add("World".to_string()); // ERROR: `data` is a `&` reference, so the data it refers to cannot be borrowed as mutable

            assert_eq!(data.vec, &["Hello".to_string(), "World".to_string()]);
        }
        Err(e) => panic!("{e:?}"),
    }

    match lock.read_gracefully() {
        Ok(data) => {
            //data.add("World".to_string()); // ERROR: `data` is a `&` reference, so the data it refers to cannot be borrowed as mutable

            assert_eq!(data.vec, &["Hello".to_string(), "World".to_string()]);
        }
        Err(e) => panic!("{e:?}"),
    }

    match lock.read_gracefully() {
        Ok(data) => {
            //data.add("World".to_string()); // ERROR: `data` is a `&` reference, so the data it refers to cannot be borrowed as mutable

            assert_eq!(data.vec, &["Hello".to_string(), "World".to_string()]);
        }
        Err(e) => panic!("{e:?}"),
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        panic!("{e:?}");
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        panic!("{e:?}");
    }

    if let Err(e) = lock.get_mut_for_cleanup() {
        assert_eq!(e.phase, Phase::Read);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseCleanup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_cleanup".to_string())
        );
    } else {
        panic!();
    }

    // Read -> Cleanup
    if let Err(e) = lock.transition_to_cleanup(WaitStrategy::GracefulWait {
        timeout: time::Duration::from_secs(1),
    }) {
        panic!("{e:?}");
    }

    assert_eq!(lock.phase_fast(), Phase::Cleanup);
    assert_eq!(lock.phase_exact(), Phase::Cleanup);

    if let Err(e) = lock.get_mut_for_setup() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseSetup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_setup".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_fast() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_fast".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_fast() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_fast".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_gracefully() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_gracefully".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        panic!("{e:?}");
    }

    if let Ok(data) = lock.get_mut_for_cleanup() {
        data.clear();
        assert_eq!(data.vec, &[] as &[String]);
    } else {
        panic!();
    }

    if let Err(e) = lock.transition_to_cleanup(WaitStrategy::GracefulWait {
        timeout: time::Duration::from_secs(1),
    }) {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(e.kind, PhasedErrorKind::PhaseIsAlreadyCleanup);
    } else {
        panic!();
    }
}

#[test]
fn test_setup_cleanup() {
    let lock = PhasedLock::new(MyStruct::new(), Setup::Unlock, Cleanup::Unlock);
    assert_eq!(lock.phase_fast(), Phase::Setup);
    assert_eq!(lock.phase_exact(), Phase::Setup);

    if let Ok(data) = lock.get_mut_for_setup() {
        data.add("Hello".to_string());
    } else {
        panic!();
    }

    if let Err(e) = lock.read_fast() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_fast".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_gracefully() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_gracefully".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallOnPhaseSetup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::finish_reading_gracefully".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.get_mut_for_cleanup() {
        assert_eq!(e.phase, Phase::Setup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseCleanup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_cleanup".to_string())
        );
    } else {
        panic!();
    }

    // Setup -> Cleanup
    if let Err(e) = lock.transition_to_cleanup(WaitStrategy::GracefulWait {
        timeout: time::Duration::from_secs(1),
    }) {
        panic!("{e:?}");
    }

    assert_eq!(lock.phase_fast(), Phase::Cleanup);
    assert_eq!(lock.phase_exact(), Phase::Cleanup);

    if let Err(e) = lock.get_mut_for_setup() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseSetup("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::get_mut_for_setup".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_fast() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_fast".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_fast() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_fast".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.read_gracefully() {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::CannotCallUnlessPhaseRead("setup_read_cleanup::PhasedLock<setup_read_cleanup::lock::setup_unlock_and_cleanup_unlock_test::MyStruct>::read_gracefully".to_string())
        );
    } else {
        panic!();
    }

    if let Err(e) = lock.finish_reading_gracefully() {
        panic!("{e:?}");
    }

    match lock.get_mut_for_cleanup() {
        Ok(data) => {
            data.clear();
            assert_eq!(data.vec, &[] as &[String]);
        }
        Err(e) => {
            panic!("{e:?}");
        }
    }
}

#[test]
fn test_setup_read_cleanup_graceful_wait_in_multi_threads() {
    let lock = PhasedLock::new(MyStruct::new(), Setup::Unlock, Cleanup::Unlock);

    if let Ok(data) = lock.get_mut_for_setup() {
        data.add("Hello".to_string());
        assert_eq!(data.vec, &["Hello".to_string()]);
    } else {
        panic!();
    }

    // Setup -> Read
    if let Err(e) = lock.transition_to_read(|data| {
        data.add("World".to_string());
        Ok::<(), MyError>(())
    }) {
        panic!("{e:?}");
    }

    let lock = Arc::new(lock);

    for _i in 0..10 {
        let lock_clone = Arc::clone(&lock);
        let _handler = std::thread::spawn(move || {
            let data = lock_clone.read_gracefully().unwrap();
            let s = data.vec.as_slice().join(", ");
            //println!("{}. {}", _i, s);
            assert_eq!(s, "Hello, World");
            std::thread::sleep(std::time::Duration::from_secs(1));
            let _r = lock_clone.finish_reading_gracefully();
        });
    }

    std::thread::sleep(std::time::Duration::from_secs(1));

    // Read -> Cleanup
    if let Err(e) = lock.transition_to_cleanup(WaitStrategy::GracefulWait {
        timeout: time::Duration::from_secs(5),
    }) {
        panic!("{e:?}");
    }

    if let Ok(data) = lock.get_mut_for_cleanup() {
        data.clear();
        assert_eq!(data.vec, &[] as &[String]);
    } else {
        panic!();
    }
}

#[test]
fn test_setup_read_cleanup_graceful_wait_in_multi_threads_and_timeout() {
    let lock = PhasedLock::new(MyStruct::new(), Setup::Unlock, Cleanup::Unlock);

    if let Ok(data) = lock.get_mut_for_setup() {
        data.add("Hello".to_string());
        assert_eq!(data.vec, &["Hello".to_string()]);
    } else {
        panic!();
    }

    // Setup -> Read
    if let Err(e) = lock.transition_to_read(|data| {
        data.add("World".to_string());
        Ok::<(), MyError>(())
    }) {
        panic!("{e:?}");
    }

    let lock = Arc::new(lock);

    for _i in 0..10 {
        let lock_clone = Arc::clone(&lock);
        let _handler = std::thread::spawn(move || {
            let data = lock_clone.read_gracefully().unwrap();
            //println!("{}. {}", _i, data.vec.as_slice().join(", "));
            assert_eq!(data.vec.as_slice().join(", "), "Hello, World");
            std::thread::sleep(std::time::Duration::from_secs(3));
            let _r = lock_clone.finish_reading_gracefully();
        });
    }

    std::thread::sleep(std::time::Duration::from_secs(1));

    // Read -> Cleanup
    if let Err(e) = lock.transition_to_cleanup(WaitStrategy::GracefulWait {
        timeout: time::Duration::from_secs(1),
    }) {
        assert_eq!(e.phase, Phase::Cleanup);
        assert_eq!(
            e.kind,
            PhasedErrorKind::TransitionToCleanupTimeout(WaitStrategy::GracefulWait {
                timeout: std::time::Duration::from_secs(1)
            })
        );
    } else {
        panic!();
    }

    if let Ok(data) = lock.get_mut_for_cleanup() {
        data.clear();
        assert_eq!(data.vec, &[] as &[String]);
    } else {
        panic!();
    }
}
