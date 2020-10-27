# 🎁 cargo-fetcher

[![Embark](https://img.shields.io/badge/embark-open%20source-blueviolet.svg)](https://embark.dev)
[![Embark](https://img.shields.io/badge/discord-ark-%237289da.svg?logo=discord)](https://discord.gg/dAuKfZS)
[![Crates.io](https://img.shields.io/crates/v/cargo-fetcher.svg)](https://crates.io/crates/cargo-fetcher)
[![Docs](https://docs.rs/cargo-fetcher/badge.svg)](https://docs.rs/cargo-fetcher)
[![dependency status](https://deps.rs/repo/github/EmbarkStudios/cargo-fetcher/status.svg)](https://deps.rs/repo/github/EmbarkStudios/cargo-fetcher)
[![Build Status](https://github.com/EmbarkStudios/cargo-fetcher/workflows/CI/badge.svg)](https://github.com/EmbarkStudios/cargo-fetcher/actions?workflow=CI)

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

## Features
### `gcs`

The `gcs` feature enables the use of [Google Cloud Storage](https://cloud.google.com/storage/) as a backend.

* Must provide a url to the `-u | --url` parameter with the [gsutil](https://cloud.google.com/storage/docs/gsutil#syntax) syntax `gs://<bucket_name>(/<prefix>)?`
* Must provide [GCP service account](https://cloud.google.com/iam/docs/service-accounts) credentials either with `--credentials` or via the `GOOGLE_APPLICATION_CREDENTIALS` environment variable

### `s3`

The `s3` feature enables the use of [Amazon S3](https://aws.amazon.com/s3/) as a backend.

* Must provide a url to the `-u | --url` parameter, it must of the form `http(s)?://<bucket>.s3(-<region>).<host>(/<prefix>)?`
* Must provide AWS credentials by the default mechanism(s) described [here](https://github.com/rusoto/rusoto/blob/master/AWS-CREDENTIALS.md)

### `fs`

The `fs` feature enables use of a folder on a local disk to store crates to and fetch crates from.

* Must provide a url to the `-u | --url` parameter with the `file:` scheme

### `blob`

The `blob` feature enables the use of [Azure Blob storage](https://azure.microsoft.com/services/storage/blobs/) as a backend.

* Must provide a url to the `-u | --url` parameter, it must of the form `blob://<container_name>(/<prefix>)?`
* Must provide [Azure Storage Account](https://docs.microsoft.com/en-us/azure/storage/common/storage-account-overview) via the environment variables `STORAGE_ACCOUNT` and `STORAGE_MASTER_KEY` described [here](https://docs.microsoft.com/azure/storage/common/storage-account-keys-manage?tabs=azure-portal).

## Examples

This is an example from our CI for an internal project.

### Dependencies

* 424 crates.io crates: cached - 38MB, unpacked - 214MB
* 13 crates source from 10 git repositories: db - 27MB, checked out - 38MB

### Scenario

The following CI jobs are run in parallel, each in a Kubernetes Job running on GKE. The container base is roughly the same as the official [rust](https://hub.docker.com/_/rust):1.39.0-slim image.

* Build modules for WASM 
* Build modules for native
* Build host client for native

~ wait for all jobs to finish ~

* Run the tests for both the WASM and native modules from the host client

### Before

All 3 build jobs take around **1m2s** each to do `cargo fetch --target x86_64-unknown-linux-gnu`

### After

All 3 build jobs take **3-4s** each to do `cargo fetcher --include-index mirror` followed by **5-7s** to
do `cargo fetch --target x86_64-unknown-linux-gnu`.

## Contributing

[![Contributor Covenant](https://img.shields.io/badge/contributor%20covenant-v1.4-ff69b4.svg)](../CODE_OF_CONDUCT.md)

We welcome community contributions to this project.

Please read our [Contributor Guide](CONTRIBUTING.md) for more information on how to get started.

## License

Licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
