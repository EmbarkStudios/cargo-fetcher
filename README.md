# 🎁 cargo-fetcher

[![Build Status](https://travis-ci.com/EmbarkStudios/cargo-fetcher.svg?branch=master)](https://travis-ci.com/EmbarkStudios/cargo-fetcher)
[![Crates.io](https://img.shields.io/crates/v/cargo-fetcher.svg)](https://crates.io/crates/cargo-fetcher)
[![Docs](https://docs.rs/cargo-fetcher/badge.svg)](https://docs.rs/cargo-fetcher)
[![Contributor Covenant](https://img.shields.io/badge/contributor%20covenant-v1.4%20adopted-ff69b4.svg)](CODE_OF_CONDUCT.md)
[![Embark](https://img.shields.io/badge/embark-open%20source-blueviolet.svg)](http://embark.games)

Alternative to `cargo fetch` for use in CI or other "clean" environments that you want to quickly bootstrap
with the necessary crates to compile/test etc your project(s).

## Why?

* You run CI jobs inside of [GCP](https://cloud.google.com/) and you want faster crates.io and git downloads so that
your compute resources can be spent on the things that you actually care about.

## Why not?

* You don't run CI inside of GCP. Currently `cargo-fetcher` only supports storing crates/git snapshots
inside of [GCS](https://cloud.google.com/storage/) which means they can be located closer to the compute resources your CI is running on. PRs are of course welcome for adding additional storage backends though!
* `cargo-fetcher` should not be used in a typical user environment as it completely disregards various
safety mechanisms that are built into cargo, such as file-based locking.
* You project doesn't have a `Cargo.lock` file. `cargo-fetcher` only works with `Cargo.lock` files (due to how `cargo fetch` works), so library crates without checked in lock files won't be able to use `cargo-fetcher`.

## Examples

This is an example from our CI for an internal project.

### Dependencies

* 424 crates.io crates: cached - 38MB, unpacked - 214MB
* 13 crates source from 10 git repositories: db - 27MB, checked out - 38MB

### Scenario

The following CI jobs are run in parallel, each in a Kubernetes Job running on GKE. The container base is roughly the same as the official [rust](https://hub.docker.com/_/rust):1.36.0-slim image.

* Build modules for WASM 
* Build modules for native
* Build host client for native

~ wait for all jobs to finish ~

* Run the tests for both the WASM and native modules from the host client

### Before

All 3 build jobs take around **1m2s** each to do `cargo fetch --target x86_64-unknown-linux-gnu`

### After

All 3 build jobs take **2-3s** each to do `cargo fetcher --include-index mirror` followed by **6s** to
do `cargo fetch --target x86_64-unknown-linux-gnu`. The **6s** can probably be [further reduced](https://github.com/EmbarkStudios/cargo-fetcher/issues/1).

## Contributing

We welcome community contributions to this project.

Please read our [Contributor Guide](CONTRIBUTING.md) for more information on how to get started.

## License

Licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
