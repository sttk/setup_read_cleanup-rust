#[cfg(test)]
mod integration_tests_of_graceful_phased_cell_sync {
    use setup_read_cleanup::{graceful::GracefulPhasedCellSync, PhasedErrorKind};
    use std::{error, fmt, thread, time};

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

    static PHASED_CELL: GracefulPhasedCellSync<MyStruct> =
        GracefulPhasedCellSync::new(MyStruct::new());

    #[test]
    fn test() {
        let mut join_handlers = Vec::<thread::JoinHandle<_>>::new();
        for i in 0..3 {
            let handler = thread::spawn(move || match PHASED_CELL.lock() {
                Ok(mut data) => {
                    data.add("A-".to_string() + &i.to_string());
                }
                Err(e) => panic!("{e:?}"),
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).join() {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        let mut join_handlers = Vec::<thread::JoinHandle<_>>::new();
        for i in 0..3 {
            let handler = thread::spawn(move || {
                if let Err(e) = PHASED_CELL.transition_to_read(|data| {
                    data.add("B-".to_string() + &i.to_string());
                    Ok::<(), MyError>(())
                }) {
                    assert!(
                        e.kind() == PhasedErrorKind::PhaseIsAlreadyRead
                            || e.kind() == PhasedErrorKind::DuringTransitionToRead
                    );
                }
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).join() {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        let mut join_handlers = Vec::<thread::JoinHandle<_>>::new();
        for i in 0..10 {
            let handler = thread::spawn(move || {
                let data = PHASED_CELL.read_relaxed().unwrap();
                assert!(data.vec[0].starts_with("A-"));
                assert!(data.vec[1].starts_with("A-"));
                assert!(data.vec[2].starts_with("A-"));
                assert!(data.vec[3].starts_with("B-"));
                thread::sleep(time::Duration::from_millis(i * 100));
                PHASED_CELL.finish_reading();
            });
            join_handlers.push(handler);
        }
        thread::sleep(time::Duration::from_millis(100));

        let mut join_handlers = Vec::<thread::JoinHandle<_>>::new();
        for i in 0..3 {
            let handler = thread::spawn(move || {
                if let Err(e) =
                    PHASED_CELL.transition_to_cleanup(time::Duration::from_secs(5), |data| {
                        data.add("C-".to_string() + &i.to_string());
                        Ok::<(), MyError>(())
                    })
                {
                    assert!(
                        e.kind() == PhasedErrorKind::PhaseIsAlreadyCleanup
                            || e.kind() == PhasedErrorKind::DuringTransitionToCleanup
                    );
                }
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).join() {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        let mut join_handlers = Vec::<thread::JoinHandle<_>>::new();
        for i in 0..4 {
            let handler = thread::spawn(move || match PHASED_CELL.lock() {
                Ok(mut data) => {
                    data.add("D-".to_string() + &i.to_string());
                }
                Err(e) => panic!("{e:?}"),
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).join() {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        match PHASED_CELL.lock() {
            Ok(mut data) => {
                assert!(data.vec[0].starts_with("A-"));
                assert!(data.vec[1].starts_with("A-"));
                assert!(data.vec[2].starts_with("A-"));
                assert!(data.vec[3].starts_with("B-"));
                assert!(data.vec[4].starts_with("C-"));
                assert!(data.vec[5].starts_with("D-"));
                assert!(data.vec[6].starts_with("D-"));
                assert!(data.vec[7].starts_with("D-"));
                assert!(data.vec[8].starts_with("D-"));
                data.clear();
                assert_eq!(&data.vec, &[] as &[String]);
            }
            Err(e) => panic!("{e:?}"),
        }
    }
}
