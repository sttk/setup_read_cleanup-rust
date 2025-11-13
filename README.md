# [setup_read_cleanup][repo-url] [![crates.io][cratesio-img]][cratesio-url] [![doc.rs][docrs-img]][docrs-url] [![CI Status][ci-img]][ci-url] [![MIT License][mit-img]][mit-url]

A library for managing data through distinct lifecycle phases: `Setup` -> `Read` -> `Cleanup`.

This crate provides "phased cells", a collection of smart pointer types that enforce a specific lifecycle for the data they manage. This is useful for data that is initialized once, read by multiple threads or tasks for a period, and then explicitly destroyed.

## Core Concepts

The lifecycle is divided into three phases:

-   **`Setup`**: The initial phase. The data can be mutably accessed for initialization.
-   **`Read`**: The operational phase. The data can only be accessed immutably. This phase is optimized for concurrent, lock-free reads.
-   **`Cleanup`**: The final phase. The data can be mutably accessed again for deconstruction or resource cleanup.

## Cell Variants

This crate offers several cell variants to suit different concurrency needs:

-   [`PhasedCell`]: The basic cell. It is `Sync`, allowing it to be shared across threads for reading. However, mutable access via `get_mut_unlocked` is not thread-safe and requires the caller to ensure exclusive access.
-   [`PhasedCellSync`]: A thread-safe version that uses a `std::sync::Mutex` to allow for safe concurrent mutable access during the `Setup` and `Cleanup` phases.
-   [`PhasedCellAsync`]: (Requires the `setup_read_cleanup-on-tokio` feature) An `async` version of `PhasedCellSync` that uses a `tokio::sync::Mutex`.

## Graceful Shutdown

(Requires the `setup_read_cleanup-graceful` feature)

The `graceful` module provides wrappers that add graceful shutdown capabilities. When transitioning to the `Cleanup` phase, these cells will wait for a specified duration for all active read operations to complete.

-   [`GracefulPhasedCell`](https://docs.rs/setup_read_cleanup/latest/setup_read_cleanup/graceful/struct.GracefulPhasedCell.html)
-   [`GracefulPhasedCellSync`](https://docs.rs/setup_read_cleanup/latest/setup_read_cleanup/graceful/struct.GracefulPhasedCellSync.html)
-   [`GracefulPhasedCellAsync`](https://docs.rs/setup_read_cleanup/latest/setup_read_cleanup/graceful/struct.GracefulPhasedCellAsync.html) (Requires both features)

## Features

-   `setup_read_cleanup-on-tokio`: Enables the `async` cell variants (`PhasedCellAsync`, `GracefulPhasedCellAsync`) which use `tokio::sync`.
-   `setup_read_cleanup-graceful`: Enables the `graceful` module, which provides cells with graceful shutdown capabilities.

## Examples

Using a `static PhasedCellSync` to initialize data, read it from multiple threads, and then clean it up.

```rust
use setup_read_cleanup::{PhasedCellSync, Phase};
use std::thread;

struct MyData {
    items: Vec<i32>,
}

// Declare a static PhasedCellSync instance
static CELL: PhasedCellSync<MyData> = PhasedCellSync::new(MyData { items: Vec::new() });

fn main() {
    // --- Setup Phase ---
    assert_eq!(CELL.phase(), Phase::Setup);
    {
        let mut data = CELL.lock().unwrap();
        data.items.push(10);
        data.items.push(20);
    } // Lock is released here

    // --- Transition to Read Phase ---
    CELL.transition_to_read(|data| {
        data.items.push(30);
        Ok::<(), std::io::Error>(())
    }).unwrap();
    assert_eq!(CELL.phase(), Phase::Read);

    // --- Read Phase ---
    // Now, multiple threads can read the data concurrently.
    let mut handles = Vec::new();
    for i in 0..3 {
        handles.push(thread::spawn(move || {
            let data = CELL.read().unwrap(); // Access the static CELL
            println!("Thread {} reads: {:?}", i, data.items);
            assert_eq!(data.items, &[10, 20, 30]);
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // --- Transition to Cleanup Phase ---
    CELL.transition_to_cleanup(|data| {
        println!("Cleaning up. Final item: {:?}", data.items.pop());
        Ok::<(), std::io::Error>(())
    }).unwrap();
    assert_eq!(CELL.phase(), Phase::Cleanup);

    println!("Example finished successfully!");
}
``````

## Installation

In Cargo.toml, write this crate as a dependency:

```toml
[dependencies]
setup_read_cleanup = "0.3.1"
```

## Supported Rust versions

This crate supports Rust 1.74.1 or later.

```sh
% ./build.sh msrv
  [Meta]   cargo-msrv 0.18.4

Compatibility Check #1: Rust 1.74.1
  [OK]     Is compatible

Compatibility Check #2: Rust 1.65.0
  [FAIL]   Is incompatible

Compatibility Check #3: Rust 1.69.0
  [FAIL]   Is incompatible

Compatibility Check #4: Rust 1.71.1
  [FAIL]   Is incompatible

Compatibility Check #5: Rust 1.72.1
  [FAIL]   Is incompatible

Compatibility Check #6: Rust 1.73.0
  [FAIL]   Is incompatible

Result:
   Considered (min … max):   Rust 1.56.1 … Rust 1.91.1
   Search method:            bisect
   MSRV:                     1.74.1
   Target:                   x86_64-apple-darwin
```

## License

Copyright (C) 2025 Takayuki Sato

This program is free software under MIT License.<br>
See the file LICENSE in this distribution for more details.


[repo-url]: https://github.com/sttk/setup_read_cleanup-rust
[cratesio-img]: https://img.shields.io/badge/crates.io-ver.0.3.1-fc8d62?logo=rust
[cratesio-url]: https://crates.io/crates/setup_read_cleanup
[docrs-img]: https://img.shields.io/badge/doc.rs-setup_read_cleanup-66c2a5?logo=docs.rs
[docrs-url]: https://docs.rs/setup_read_cleanup
[ci-img]: https://github.com/sttk/setup_read_cleanup-rust/actions/workflows/rust.yml/badge.svg?branch=main
[ci-url]: https://github.com/sttk/setup_read_cleanup-rust/actions?query=branch%3Amain
[mit-img]: https://img.shields.io/badge/license-MIT-green.svg
[mit-url]: https://opensource.org/licenses/MIT
