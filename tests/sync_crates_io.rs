use anyhow::Context;
use cargo_fetcher as cf;
use cf::{Krate, Registry, Source};

mod tutil;
use tutil as util;

#[tokio::test(threaded_scheduler)]
async fn all_missing() {
    let fs_root = tempfile::TempDir::new().expect("failed to create tempdir");
    let registry = Registry::new(
        "https://github.com/rust-lang/crates.io-index".to_owned(),
        None,
        Some("https://crates.io/api/v1/crates".to_owned()),
        None,
    );
    let registries = vec![registry.clone()];
    let mut fs_ctx = util::fs_ctx(fs_root.path().to_owned(), registries).await;

    let missing_root = tempfile::TempDir::new().expect("failed to crate tempdir");
    fs_ctx.root_dir = missing_root.path().to_owned();

    fs_ctx.krates = vec![
        Krate {
            name: "ansi_term".to_owned(),
            version: "0.11.0".to_owned(),
            source: Source::Registry(
                registry.clone(),
                "ee49baf6cb617b853aa8d93bf420db2383fab46d314482ca2803b40d5fde979b".to_owned(),
            ),
        },
        Krate {
            name: "base64".to_owned(),
            version: "0.10.1".to_owned(),
            source: Source::Registry(
                registry.clone(),
                "0b25d992356d2eb0ed82172f5248873db5560c4721f564b13cb5193bda5e668e".to_owned(),
            ),
        },
        Krate {
            name: "uuid".to_owned(),
            version: "0.7.4".to_owned(),
            source: Source::Registry(
                registry.clone(),
                "90dbc611eb48397705a6b0f6e917da23ae517e4d127123d2cf7674206627d32a".to_owned(),
            ),
        },
    ];

    cf::mirror::crates(&fs_ctx)
        .await
        .expect("failed to mirror crates");
    fs_ctx.prep_sync_dirs().expect("create base dirs");
    assert_eq!(
        cf::sync::crates(&fs_ctx)
            .await
            .expect("synced 3 crates")
            .good,
        3,
    );

    tracing::info!("synced crates");

    // Ensure the unmutated crates are in the cache directory
    {
        let cache_root = fs_ctx.root_dir.join(cf::sync::CACHE_DIR);

        for krate in &fs_ctx.krates {
            let bytes = {
                let path = cache_root.join(format!("{}-{}.crate", krate.name, krate.version));

                std::fs::read(&path)
                    .with_context(|| format!("{:#} {}", krate, path.display()))
                    .expect("can't read")
            };

            match krate.source {
                Source::Registry(_, ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                // Source::CratesIo(ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                //     .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        let src_root = fs_ctx.root_dir.join(cf::sync::SRC_DIR);

        for krate in &fs_ctx.krates {
            let path = src_root.join(format!("{}-{}/Cargo.toml", krate.name, krate.version));
            assert!(path.exists(), "didn't find unpacked {}", path.display());
        }
    }
}

#[tokio::test(threaded_scheduler)]
async fn some_missing() {
    let fs_root = tempfile::TempDir::new().expect("failed to create tempdir");
    let registry = Registry::new(
        "https://github.com/rust-lang/crates.io-index".to_owned(),
        None,
        Some("https://crates.io/api/v1/crates".to_owned()),
        None,
    );
    let registries = vec![registry.clone()];
    let mut fs_ctx = util::fs_ctx(fs_root.path().to_owned(), registries).await;

    let missing_root = tempfile::TempDir::new().expect("failed to crate tempdir");
    fs_ctx.root_dir = missing_root.path().to_owned();

    fs_ctx.krates = vec![
        Krate {
            name: "ansi_term".to_owned(),
            version: "0.11.0".to_owned(),
            source: Source::Registry(
                registry.clone(),
                "ee49baf6cb617b853aa8d93bf420db2383fab46d314482ca2803b40d5fde979b".to_owned(),
            ),
        },
        Krate {
            name: "base64".to_owned(),
            version: "0.10.1".to_owned(),
            source: Source::Registry(
                registry.clone(),
                "0b25d992356d2eb0ed82172f5248873db5560c4721f564b13cb5193bda5e668e".to_owned(),
            ),
        },
        Krate {
            name: "uuid".to_owned(),
            version: "0.7.4".to_owned(),
            source: Source::Registry(
                registry.clone(),
                "90dbc611eb48397705a6b0f6e917da23ae517e4d127123d2cf7674206627d32a".to_owned(),
            ),
        },
    ];

    cf::mirror::crates(&fs_ctx)
        .await
        .expect("failed to mirror crates");
    fs_ctx.prep_sync_dirs().expect("create base dirs");

    let stored = fs_ctx.krates.clone();
    fs_ctx.krates = vec![stored[2].clone()];
    assert_eq!(
        cf::sync::crates(&fs_ctx)
            .await
            .expect("synced 1 crate")
            .good,
        1
    );

    // Ensure the unmutated crates are in the cache directory
    {
        let cache_root = fs_ctx.root_dir.join(cf::sync::CACHE_DIR);

        for krate in &fs_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match krate.source {
                // Source::CratesIo(ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                //     .expect("failed to validate checksum"),
                Source::Registry(_, ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        let src_root = fs_ctx.root_dir.join(cf::sync::SRC_DIR);

        for krate in &fs_ctx.krates {
            assert!(src_root
                .join(format!("{}-{}/Cargo.toml", krate.name, krate.version))
                .exists());
        }
    }

    fs_ctx.krates = stored;
    assert_eq!(
        cf::sync::crates(&fs_ctx)
            .await
            .expect("synced 2 crates")
            .good,
        2
    );

    // Ensure the unmutated crates are in the cache directory
    {
        let cache_root = fs_ctx.root_dir.join(cf::sync::CACHE_DIR);

        for krate in &fs_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match krate.source {
                // Source::CratesIo(ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                //     .expect("failed to validate checksum"),
                Source::Registry(_, ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        let src_root = fs_ctx.root_dir.join(cf::sync::SRC_DIR);

        for krate in &fs_ctx.krates {
            let path = src_root.join(format!("{}-{}/Cargo.toml", krate.name, krate.version));
            assert!(path.exists(), "didn't find unpacked {}", path.display());
        }
    }
}

#[tokio::test(threaded_scheduler)]
async fn none_missing() {
    let fs_root = tempfile::TempDir::new().expect("failed to create tempdir");
    let registry = Registry::new(
        "https://github.com/rust-lang/crates.io-index".to_owned(),
        None,
        Some("https://crates.io/api/v1/crates".to_owned()),
        None,
    );
    let registries = vec![registry.clone()];
    let mut fs_ctx = util::fs_ctx(fs_root.path().to_owned(), registries).await;

    let missing_root = tempfile::TempDir::new().expect("failed to crate tempdir");
    fs_ctx.root_dir = missing_root.path().to_owned();

    fs_ctx.krates = vec![
        Krate {
            name: "ansi_term".to_owned(),
            version: "0.11.0".to_owned(),
            source: Source::Registry(
                registry.clone(),
                "ee49baf6cb617b853aa8d93bf420db2383fab46d314482ca2803b40d5fde979b".to_owned(),
            ),
        },
        Krate {
            name: "base64".to_owned(),
            version: "0.10.1".to_owned(),
            source: Source::Registry(
                registry.clone(),
                "0b25d992356d2eb0ed82172f5248873db5560c4721f564b13cb5193bda5e668e".to_owned(),
            ),
        },
        Krate {
            name: "uuid".to_owned(),
            version: "0.7.4".to_owned(),
            source: Source::Registry(
                registry.clone(),
                "90dbc611eb48397705a6b0f6e917da23ae517e4d127123d2cf7674206627d32a".to_owned(),
            ),
        },
    ];

    cf::mirror::crates(&fs_ctx)
        .await
        .expect("failed to mirror crates");
    fs_ctx.prep_sync_dirs().expect("create base dirs");

    assert_eq!(
        cf::sync::crates(&fs_ctx)
            .await
            .expect("synced 3 crate")
            .good,
        3
    );

    // Ensure the unmutated crates are in the cache directory
    {
        let cache_root = fs_ctx.root_dir.join(cf::sync::CACHE_DIR);

        for krate in &fs_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match krate.source {
                // Source::CratesIo(ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                //     .expect("failed to validate checksum"),
                Source::Registry(_, ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        let src_root = fs_ctx.root_dir.join(cf::sync::SRC_DIR);

        for krate in &fs_ctx.krates {
            assert!(src_root
                .join(format!("{}-{}/Cargo.toml", krate.name, krate.version))
                .exists());
        }
    }

    assert_eq!(
        cf::sync::crates(&fs_ctx)
            .await
            .expect("synced 0 crates")
            .total_bytes,
        0
    );

    // Ensure the unmutated crates are in the cache directory
    {
        let cache_root = fs_ctx.root_dir.join(cf::sync::CACHE_DIR);

        for krate in &fs_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match krate.source {
                // Source::CratesIo(ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                //     .expect("failed to validate checksum"),
                Source::Registry(_, ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        let src_root = fs_ctx.root_dir.join(cf::sync::SRC_DIR);

        for krate in &fs_ctx.krates {
            let path = src_root.join(format!("{}-{}/Cargo.toml", krate.name, krate.version));
            assert!(path.exists(), "didn't find unpacked {}", path.display());
        }
    }
}
