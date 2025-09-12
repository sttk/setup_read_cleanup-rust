# [setup_read_cleanup][repo-url] [![crates.io][cratesio-img]][cratesio-url] [![doc.rs][docrs-img]][docrs-url] [![CI Status][ci-img]][ci-url] [![MIT License][mit-img]][mit-url]

A library for safely transitioning through the three phases of shared resource access: setup, read, and cleanup.

This crate provides `PhasedLock` struct, which controls the updatability of internal data through three phases.
The three phases are `Setup`, `Read`, and `Cleanup`.
The internal data can be updated with exclusive control during the `Setup` and `Cleanup` phases.
The `Read` phase allows the internal data to be treated as read-only without exclusive control.

In this `PhasedLock`, an atomic variable is used to determine the phase.
Atomic variables control the strictness of read and write ordering through `Ordering` specifications; however, stricter settings incur higher costs.
Taking this into account, this crate provide two pairs of methods: `read_fast` and `phase_fast`, which prioritize speed over strictness, and `read_gracefully` and `phase_exact`, which prioritize strictness over speed.

In the transition from the `Read` to the `Cleanup` phase, this crate provides a "graceful wait" mechanism that waits for operations on data acquired within the `Read` phase to finish.
After acquiring the data using the `PhasedLock#read_gracefully` method, you call the `PhasedLock#finish_reading_gracefully` method to tell the `PhasedLock` that processing the data is complete.
By making the transition to the `Cleanup` phase wait until `PhasedLock#finish_reading_gracefully` is executed in a way that pairs with `PhasedLock#read_gracefully` in each multiple thread, the internal data can be updated safely.

## Installation

In Cargo.toml, write this crate as a dependency:

```toml
[dependencies]
setup_read_cleanup = "0.0.0"
```

## Usage

This crate provides a mechanism for safely managing shared data through three distinct phases: Setup, Read, and Cleanup.
This is ideal for scenarios where a resource is constructed, becomes read-only for a period, and is then prepared for destruction or reconstruction.

### 1. Defining a Phased Lock and Data Structure

First, define the data structure that `PhasedLock` will protect.
For this example, we'll initialize a `PhasedLock` instance as a static variable for global access.
The generic type `T` must implement `Send + Sync` to ensure safe sharing across threads.

```rust
use setup_read_cleanup::PhasedLock;
use std::{error, fmt};

// Define your data structure and a custom error type.
struct MyData {
    num: i32,
    str: Option<String>,
}

#[derive(Debug)]
struct MyError;
impl fmt::Display for MyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "MyError") }
}
impl error::Error for MyError {}

// For this example, we create a static PhasedLock instance for global access.
static PHASED_LOCK: PhasedLock<MyData> = PhasedLock::new(MyData {
    num: 0,
    str: None,
});
```

### 2. The Setup Phase: Data Initialization 

A `PhasedLock` instance starts in the Setup phase by default.
In this phase, you can use the `lock_for_update()` method to gain exclusive access and initialize your data.

```rust
// In the Setup phase, we use lock_for_update() to get exclusive access.
{
    let mut data = PHASED_LOCK.lock_for_update().unwrap();
    data.num = 123;
    data.str = Some("hello".to_string());
} // The lock is automatically released when the guard goes out of scope.
```

This method returns a mutable guard that provides a safe, exclusive lock, preventing data races during initialization.

### 3. Transitioning to the Read Phase

Once the data is set up, call `transition_to_read()` to move to the Read phase.
This method is only callable from the Setup phase and requires a closure to perform any final, read-only operations before the transition completes.

```rust
// Transition to the Read phase, performing a final read-only check.
PHASED_LOCK.transition_to_read(|_data| Ok::<(), MyError>(())).unwrap();
```

After this transition, the data is ready for concurrent, read-only access by multiple threads.

### 4. The Read Phase: Concurrent Reading

In the Read phase, you can concurrently read the data.
For this, the crate provides a "graceful wait" mechanism to ensure a safe transition later on.
Use `read_gracefully()` to acquire a read-only guard and register your reader.

After you have finished processing the data, you must call `finish_reading_gracefully()` to inform the lock that you are done.

```rust
use std::thread;

// ... (code continued from above) ...

// In the Read phase, we read the data concurrently.
let handles: Vec<_> = (0..3).map(|i| {
    thread::spawn(move || {
        let data = PHASED_LOCK.read_gracefully().unwrap();
        println!("Thread {}: num={}, str={:?}", i, data.num, data.str);

        // It is crucial to call this method to signal that reading is complete.
        PHASED_LOCK.finish_reading_gracefully().unwrap();
    })
}).collect();

for h in handles {
    h.join().unwrap();
}
```

This pattern is essential for safe transitions, as it allows the lock to track active readers.

If you can guarantee that the Read phase will definitely finish before transitioning to the Cleanup phase, you can use the faster `read_fast` method. (This does not have a corresponding finish notification method, and the graceful wait will not work.)

### 5. Transitioning to the Cleanup Phase

To move to the Cleanup phase, call `transition_to_cleanup()`.
The `WaitStrategy::GracefulWait` option ensures that the lock will wait for all readers registered via `read_gracefully()` to finish before proceeding.

```rust
use setup_read_cleanup::WaitStrategy;
use std::time::Duration;

// ... (code continued from above) ...

// Transition to the Cleanup phase. This call will block until all readers are finished,
// with a maximum wait time of 10 seconds.
PHASED_LOCK.transition_to_cleanup(WaitStrategy::GracefulWait {
    first: Duration::from_millis(100),
    interval: Duration::from_millis(50),
    timeout: Duration::from_secs(10),
}).unwrap();
```

For a faster but less safe transition, you can use `WaitStrategy::NoWait`, which does not wait for active readers.

### 6. The Cleanup Phase: Final Updates

The Cleanup phase allows for exclusive data updates, just like the Setup phase.
You can use this phase to tear down or reset the data.

```rust
// In the Cleanup phase, we reset the data.
{
    let mut data = PHASED_LOCK.lock_for_update().unwrap();
    data.num = 0;
    data.str = None;
} // The lock is released when the guard goes out of scope.
```

## Supported Rust versions

This crate supports Rust 1.63.0 or later.

```sh
% ./build.sh msrv
  [Meta]   cargo-msrv 0.18.4

Compatibility Check #1: Rust 1.73.0
  [OK]     Is compatible

Compatibility Check #2: Rust 1.64.0
  [OK]     Is compatible

Compatibility Check #3: Rust 1.60.0
  [FAIL]   Is incompatible

Compatibility Check #4: Rust 1.62.1
  [FAIL]   Is incompatible

Compatibility Check #5: Rust 1.63.0
  [OK]     Is compatible

Result:
   Considered (min … max):   Rust 1.56.1 … Rust 1.89.0
   Search method:            bisect
   MSRV:                     1.63.0
   Target:                   x86_64-apple-darwin
```

## License

Copyright (C) 2025 Takayuki Sato

This program is free software under MIT License.<br>
See the file LICENSE in this distribution for more details.


[repo-url]: https://github.com/sttk/setup_read_cleanup-rust
[cratesio-img]: https://img.shields.io/badge/crates.io-ver.0.0.0-fc8d62?logo=rust
[cratesio-url]: https://crates.io/crates/setup_read_cleanup
[docrs-img]: https://img.shields.io/badge/doc.rs-setup_read_cleanup-66c2a5?logo=docs.rs
[docrs-url]: https://docs.rs/setup_read_cleanup
[ci-img]: https://github.com/sttk/setup_read_cleanup-rust/actions/workflows/rust.yml/badge.svg?branch=main
[ci-url]: https://github.com/sttk/setup_read_cleanup-rust/actions?query=branch%3Amain
[mit-img]: https://img.shields.io/badge/license-MIT-green.svg
[mit-url]: https://opensource.org/licenses/MIT
