#[cfg(all(test, feature = "graceful"))]
mod integration_tests_of_phased_cell {
    use setup_read_cleanup::graceful::GracefulPhasedCell;
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

    static PHASED_CELL: GracefulPhasedCell<MyStruct> = GracefulPhasedCell::new(MyStruct::new());

    #[test]
    fn test() {
        for i in 0..3 {
            match PHASED_CELL.get_mut_unlocked() {
                Ok(data) => {
                    data.add("A-".to_string() + &i.to_string());
                }
                Err(e) => panic!("{e:?}"),
            }
        }

        if let Err(e) = PHASED_CELL.transition_to_read(|data| {
            data.add("B".to_string());
            Ok::<(), MyError>(())
        }) {
            panic!("{e:?}");
        }

        let mut join_handlers = Vec::<thread::JoinHandle<_>>::new();
        for i in 0..10 {
            let handler = thread::spawn(move || {
                let data = PHASED_CELL.read_relaxed().unwrap();
                assert_eq!(data.vec.as_slice().join(", "), "A-0, A-1, A-2, B",);
                thread::sleep(time::Duration::from_millis(i * 100));
                PHASED_CELL.finish_reading();
            });
            join_handlers.push(handler);
        }

        thread::sleep(time::Duration::from_millis(100));

        if let Err(e) = PHASED_CELL.transition_to_cleanup(time::Duration::from_secs(5), |data| {
            data.add("C".to_string());
            Ok::<(), MyError>(())
        }) {
            panic!("{e:?}");
        }

        for i in 0..4 {
            match PHASED_CELL.get_mut_unlocked() {
                Ok(data) => {
                    data.add("D-".to_string() + &i.to_string());
                }
                Err(e) => panic!("{e:?}"),
            }
        }

        match PHASED_CELL.get_mut_unlocked() {
            Ok(data) => {
                assert_eq!(
                    &data.vec,
                    &[
                        "A-0".to_string(),
                        "A-1".to_string(),
                        "A-2".to_string(),
                        "B".to_string(),
                        "C".to_string(),
                        "D-0".to_string(),
                        "D-1".to_string(),
                        "D-2".to_string(),
                        "D-3".to_string(),
                    ]
                );
                data.clear();
                assert_eq!(&data.vec, &[] as &[String]);
            }
            Err(e) => panic!("{e:?}"),
        }
    }
}
