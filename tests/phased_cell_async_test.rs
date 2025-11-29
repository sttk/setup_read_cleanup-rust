#[cfg(test)]
#[cfg(feature = "setup_read_cleanup-on-tokio")]
mod integration_tests_of_phased_cell_async {
    use setup_read_cleanup::{PhasedCellAsync, PhasedErrorKind};
    use std::{error, fmt};
    use tokio::task;

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

    static PHASED_CELL: PhasedCellAsync<MyStruct> = PhasedCellAsync::new(MyStruct::new());

    #[tokio::test]
    async fn test() {
        let mut join_handlers = Vec::<task::JoinHandle<_>>::new();
        for i in 0..3 {
            let handler = tokio::spawn(async move {
                match PHASED_CELL.lock_async().await {
                    Ok(mut data) => {
                        data.add("A-".to_string() + &i.to_string());
                    }
                    Err(e) => panic!("{e:?}"),
                }
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).await {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        let mut join_handlers = Vec::<task::JoinHandle<_>>::new();
        for i in 0..3 {
            let handler = tokio::spawn(async move {
                if let Err(e) = PHASED_CELL
                    .transition_to_read_async(|data| {
                        data.add("B-".to_string() + &i.to_string());
                        Box::pin(async { Ok::<(), MyError>(()) })
                    })
                    .await
                {
                    assert!(
                        e.kind() == PhasedErrorKind::DuringTransitionToRead
                            || e.kind() == PhasedErrorKind::PhaseIsAlreadyRead,
                    )
                }
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).await {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        let mut join_handlers = Vec::<task::JoinHandle<_>>::new();
        for _ in 0..10 {
            let handler = tokio::spawn(async move {
                let data = PHASED_CELL.read_relaxed().unwrap();
                assert!(data.vec[0].starts_with("A-"));
                assert!(data.vec[1].starts_with("A-"));
                assert!(data.vec[2].starts_with("A-"));
                assert!(data.vec[3].starts_with("B-"));
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).await {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        let mut join_handlers = Vec::<task::JoinHandle<_>>::new();
        for i in 0..3 {
            let handler = tokio::spawn(async move {
                if let Err(e) = PHASED_CELL
                    .transition_to_cleanup_async(|data| {
                        data.add("C-".to_string() + &i.to_string());
                        Box::pin(async { Ok::<(), MyError>(()) })
                    })
                    .await
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
            let _ = match join_handlers.remove(0).await {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        let mut join_handlers = Vec::<task::JoinHandle<_>>::new();
        for i in 0..4 {
            let handler = tokio::spawn(async move {
                match PHASED_CELL.lock_async().await {
                    Ok(mut data) => {
                        data.add("D-".to_string() + &i.to_string());
                    }
                    Err(e) => panic!("{e:?}"),
                }
            });
            join_handlers.push(handler);
        }
        while join_handlers.len() > 0 {
            let _ = match join_handlers.remove(0).await {
                Ok(_) => Ok::<(), MyError>(()),
                Err(e) => panic!("{e:?}"),
            };
        }

        match PHASED_CELL.lock_async().await {
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
