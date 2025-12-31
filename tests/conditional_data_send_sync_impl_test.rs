// This test file verifies two key aspects of the PhasedCell and GracefulPhasedCell
// after the conditional implementation refactoring:
//
// 1. **Flexibility:** PhasedCells can now hold non-`Send` or non-`Sync` data
//    (like `Rc<T>` or `Cell<T>`) for single-threaded use cases.
// 2. **Concurrency:** The original multi-threaded capabilities for `Send + Sync` data
//    remain fully functional, and thread-safety is enforced at compile-time
//    if one attempts to share a cell holding non-`Sync` data.

use setup_read_cleanup::{Phase, PhasedCell};

#[cfg(feature = "setup_read_cleanup-graceful")]
use setup_read_cleanup::graceful::GracefulPhasedCell;

use std::rc::Rc;
use std::sync::Arc;
use std::thread;

#[cfg(feature = "setup_read_cleanup-graceful")]
use std::time::Duration;

#[cfg(test)]
mod tests_of_phased_cell {
    use super::*;

    #[test]
    fn test_single_threaded_with_non_sync_data() {
        let cell = PhasedCell::new(Rc::new(String::from("hello")));

        cell.get_mut_unlocked()
            .map(|rc_str| *Rc::get_mut(rc_str).unwrap() = String::from("setup"))
            .unwrap();

        cell.transition_to_read(|rc_str: &mut Rc<String>| {
            *Rc::get_mut(rc_str).unwrap() = String::from("read");
            Ok::<(), std::io::Error>(())
        })
        .unwrap();

        let data = cell.read().unwrap();
        assert_eq!(**data, "read");
        let _ = data;

        cell.transition_to_cleanup(|rc_str: &mut Rc<String>| {
            *Rc::get_mut(rc_str).unwrap() = String::from("cleanup");
            Ok::<(), std::io::Error>(())
        })
        .unwrap();
        assert_eq!(cell.phase(), Phase::Cleanup);

        let mut mut_data_cleanup = cell.get_mut_unlocked().unwrap();
        *Rc::get_mut(&mut mut_data_cleanup).unwrap() = String::from("final");
        let _ = mut_data_cleanup;

        let final_data = cell.get_mut_unlocked().unwrap();
        assert_eq!(*Rc::get_mut(final_data).unwrap(), "final");
        let _ = final_data;
    }

    #[test]
    fn test_multi_threaded_with_sync_data() {
        let cell_instance = PhasedCell::new(String::from("initial"));

        cell_instance
            .get_mut_unlocked()
            .map(|s| *s = String::from("setup_value"))
            .unwrap();
        assert_eq!(*cell_instance.get_mut_unlocked().unwrap(), "setup_value");

        let cell = Arc::new(cell_instance);

        cell.transition_to_read(|s: &mut String| {
            *s = String::from("read_value");
            Ok::<(), std::io::Error>(())
        })
        .unwrap();

        let handles: Vec<_> = (0..5)
            .map(|_| {
                let cell_clone: Arc<PhasedCell<String>> = Arc::clone(&cell);
                thread::spawn(move || {
                    let data = cell_clone.read().unwrap();
                    assert_eq!(*data, "read_value");
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        cell.transition_to_cleanup(|s: &mut String| {
            *s = String::from("cleanup_value");
            Ok::<(), std::io::Error>(())
        })
        .unwrap();
        assert_eq!(cell.phase(), Phase::Cleanup);

        let cleanup_data = cell.get_mut_unlocked().unwrap();
        *cleanup_data = String::from("final_value");
        assert_eq!(*cleanup_data, "final_value");
        let _ = cleanup_data;
    }
}

#[cfg(feature = "setup_read_cleanup-graceful")]
#[cfg(test)]
mod tests_of_graceful_phased_cell {
    use super::*;

    #[test]
    fn test_single_threaded_with_non_sync_data() {
        let cell = GracefulPhasedCell::new(Rc::new(vec![10, 20]));

        cell.get_mut_unlocked()
            .map(|rc_vec| *Rc::get_mut(rc_vec).unwrap() = vec![1, 2, 3])
            .unwrap();
        assert_eq!(**cell.get_mut_unlocked().unwrap(), vec![1, 2, 3]);

        cell.transition_to_read(|_v: &mut Rc<Vec<i32>>| Ok::<(), std::io::Error>(()))
            .unwrap();

        let data = cell.read().unwrap();
        assert_eq!(**data, vec![1, 2, 3]);
        cell.finish_reading();

        cell.transition_to_cleanup(Duration::ZERO, |rc_vec: &mut Rc<Vec<i32>>| {
            *Rc::get_mut(rc_vec).unwrap() = vec![4, 5, 6];
            Ok::<(), std::io::Error>(())
        })
        .unwrap();
        assert_eq!(cell.phase(), Phase::Cleanup);

        let mut mut_data_cleanup = cell.get_mut_unlocked().unwrap();
        *Rc::get_mut(&mut mut_data_cleanup).unwrap() = vec![7, 8, 9];
        let _ = mut_data_cleanup;

        let final_data = cell.get_mut_unlocked().unwrap();
        assert_eq!(**Rc::get_mut(final_data).unwrap(), vec![7, 8, 9]);
        let _ = final_data;
    }

    #[cfg(feature = "setup_read_cleanup-graceful")]
    #[test]
    fn test_multi_threaded_with_sync_data() {
        let cell_instance = GracefulPhasedCell::new(String::from("initial graceful"));

        cell_instance
            .get_mut_unlocked()
            .map(|s| *s = String::from("setup_graceful_value"))
            .unwrap();
        assert_eq!(
            *cell_instance.get_mut_unlocked().unwrap(),
            "setup_graceful_value"
        );

        let cell = Arc::new(cell_instance);

        cell.transition_to_read(|s: &mut String| {
            *s = String::from("read_graceful_value");
            Ok::<(), std::io::Error>(())
        })
        .unwrap();

        let handles: Vec<_> = (0..5)
            .map(|_| {
                let cell_clone: Arc<GracefulPhasedCell<String>> = Arc::clone(&cell);
                thread::spawn(move || {
                    let data = cell_clone.read().unwrap();
                    assert_eq!(*data, "read_graceful_value");
                    cell_clone.finish_reading();
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        cell.transition_to_cleanup(Duration::ZERO, |s: &mut String| {
            *s = String::from("cleanup_graceful_value");
            Ok::<(), std::io::Error>(())
        })
        .unwrap();
        assert_eq!(cell.phase(), Phase::Cleanup);

        let cleanup_data = cell.get_mut_unlocked().unwrap();
        *cleanup_data = String::from("final_graceful_value");
        assert_eq!(*cleanup_data, "final_graceful_value");
        let _ = cleanup_data;
    }
}
