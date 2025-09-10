# [setup_read_cleanup][repo-url] [![crates.io][cratesio-img]][cratesio-url] [![doc.rs][docrs-img]][docrs-url] [![CI Status][ci-img]][ci-url] [![MIT License][mit-img]][mit-url]

A library for safely transitioning through the three phases of shared resource access: setup, read, and cleanup.

This crate provides `PhasedLock` struct, which controls the updatability of internal data through three phases.
The three phases are `Setup`, `Read`, and `Cleanup`.
The internal data can be updated with exclusive control during the `Setup` and `Cleanup` phases.
The `Read` phase allows the internal data to be treated as read-only without exclusive control.

In this `PhasedLock`, an atomic variable is used to determine the phase.
Atomic variables control the strictness of read and write ordering through `Ordering` specifications; however, stricter settings incur higher costs.
Taking this into account, this create provide two pairs of methods: `read_fast` and `phase_fast`, which prioritize speed over strictness, and `read_gracefully` and `phase_exact`, which prioritize strictness over speed.

In the transition from the `Read` to the` Cleanup` phase, this crate provide a "graceful wait" mechanism that waits for operations on data acquired within the `Read` phase to finish.
After acquiring the data using the `PhasedLock#read_gracefully` method, you call the `PhasedLock#finish_reading_gracefully` method to tell the `PhasedLock` that processing the data is complete.
By making the transition to the `Cleanup` phase wait until `PhasedLock#finish_reading_lock` is executed in a way that pairs with `PhasedLock#read_gracefully` in each multiple thread, the internal data can be updated safely.

## Installation

In Cargo.toml, write this crate as a dependency:

```toml
[dependencies]
setup_read_cleanup = "0.0.0"
```

## Usage

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
