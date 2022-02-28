<div align="center">

# `🎁 cargo-fetcher`

[![Embark](https://img.shields.io/badge/embark-open%20source-blueviolet.svg)](https://embark.dev)
[![Embark](https://img.shields.io/badge/discord-ark-%237289da.svg?logo=discord)](https://discord.gg/dAuKfZS)
[![Crates.io](https://img.shields.io/crates/v/cargo-fetcher.svg)](https://crates.io/crates/cargo-fetcher)
[![Docs](https://docs.rs/cargo-fetcher/badge.svg)](https://docs.rs/cargo-fetcher)
[![dependency status](https://deps.rs/repo/github/EmbarkStudios/cargo-fetcher/status.svg)](https://deps.rs/repo/github/EmbarkStudios/cargo-fetcher)
[![Build Status](https://github.com/EmbarkStudios/cargo-fetcher/workflows/CI/badge.svg)](https://github.com/EmbarkStudios/cargo-fetcher/actions?workflow=CI)

Alternative to `cargo fetch` for use in CI or other "clean" environments that you want to quickly bootstrap with the necessary crates to compile/test etc your project(s).

</div>

## Why?

* You run many CI jobs in clean and/or containerized environments and you want to quickly fetch cargo registries and crates so that you can spend your compute resources on actually compiling and testing the code, rather than downloading dependencies.

## Why not?

* Other than the `fs` storage backend, the only supported backends are the 3 major cloud storage backends, as it is generally beneficial to store crate and registry information in the same cloud as you are running your CI jobs to take advantage of locality and I/O throughput.
* `cargo-fetcher` should not be used in a typical user environment as it completely disregards various safety mechanisms that are built into cargo, such as file-based locking.
* `cargo-fetcher` assumes it is running in an environment with high network throughput and low latency.

## Supported Storage Backends

### `gcs`

The `gcs` feature enables the use of [Google Cloud Storage](https://cloud.google.com/storage/) as a backend.

* Must provide a url to the `-u | --url` parameter with the [gsutil](https://cloud.google.com/storage/docs/gsutil#syntax) syntax `gs://<bucket_name>(/<prefix>)?`
* Must provide [GCP service account](https://cloud.google.com/iam/docs/service-accounts) credentials either with `--credentials` or via the `GOOGLE_APPLICATION_CREDENTIALS` environment variable

### `s3`

The `s3` feature enables the use of [Amazon S3](https://aws.amazon.com/s3/) as a backend.

* Must provide a url to the `-u | --url` parameter, it must of the form `http(s)?://<bucket>.s3(-<region>).<host>(/<prefix>)?`
* Must provide AWS IAM user via the environment `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` described [here](https://docs.aws.amazon.com/cli/latest/userguide/cli-configure-envvars.html) or run from an ec2 instance with an assumed role as described [here](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/iam-roles-for-amazon-ec2.html).

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

All 3 build jobs take **3-4s** each to do `cargo fetcher --include-index sync`.

## Usage

`cargo-fetcher` has only 2 subcommands. Both of them share a set of options, the important inputs for each backend are described in [Storage Backends](#supported-storage-backends).

In addition to the backend specifics, the only required optional is the path to the `Cargo.lock` lockfile that you are operating on. `cargo-fetcher` requires a lockfile, as otherwise the normal cargo work of generating a lockfile requires having a full registry index locally, which partially defeats the point of this tool.

```text
-l, --lock-file <lock-file>
    Path to the lockfile used for determining what crates to operate on [default: Cargo.lock]
```

### `mirror`

The `mirror` subcommand does the work of downloading crates and registry indexes from their original locations and re-uploading them to your storage backend.

It does have one additional option however, to determine how often it should take snapshots of the registry index(es).

```text
-m, --max-stale <max-stale>
    The duration for which the index will not be replaced after its most recent update.

    Times may be specified with no suffix (default days), or one of:
    * (s)econds
    * (m)inutes
    * (h)ours
    * (d)ays
```

### Custom registries

One wrinkle with mirroring is the presence of custom registries. To handle these, `cargo fetcher` uses the same logic that cargo uses to locate `.cargo/config<.toml>` config files to detect custom registries, however, cargo's config files only contain the metadata needed to fetch and publish to the registry, but the url template for where to download crates from is actually present in a `config.json` file in the root of the registry itself.

Rather than wait for a registry index to be downloaded each time before fetching any crates sourced that registry, `cargo-fetcher` instead allows you to specify the download location yourself via an environment variable, that way it can fully parallelize the fetching of registry indices and crates.

#### Example

```ini
# .cargo/config.toml

[registries]
embark = { index = "<secret url>" }
```

The environment variable is of the form `CARGO_FETCHER_<name>_DL` where name is the same name (upper-cased) of the registry in the configuration file.

```sh
CARGO_FETCHER_EMBARK_DL="https://secret/rust/cargo/{crate}-{version}.crate" cargo fetcher mirror
```

The [format](https://doc.rust-lang.org/cargo/reference/registries.html#index-format) of the URL should be the same as the one in your registry's `config.json` file, if this environment variable is not specified for your registry, the default of `/{crate}/{version}/download` is just appended to the url of the registry.

### `sync`

The `sync` subcommand is the actual replacement for `cargo fetch`, except instead of downloading crates and registries from their normal location, it downloads them from your storage backend, and splats them to disk in the same way that cargo does, so that cargo won't have to do any actual work before it can start building code.

## Contributing

[![Contributor Covenant](https://img.shields.io/badge/contributor%20covenant-v1.4-ff69b4.svg)](../CODE_OF_CONDUCT.md)

We welcome community contributions to this project.

Please read our [Contributor Guide](CONTRIBUTING.md) for more information on how to get started.

## License

Licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
