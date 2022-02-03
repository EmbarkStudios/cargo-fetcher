use anyhow::Context;
use cargo_fetcher as cf;
use cf::{Krate, Registry, Source};

mod tutil;
use tutil as util;

#[test]
fn all_missing() {
    let fs_root = tempfile::TempDir::new().expect("failed to create tempdir");
    let registry = std::sync::Arc::new(Registry::default());
    let registries = vec![registry.clone()];
    let mut fs_ctx = util::fs_ctx(fs_root.path().to_owned(), registries);

    let missing_root = tempfile::TempDir::new().expect("failed to crate tempdir");
    fs_ctx.root_dir = missing_root.path().to_owned();

    fs_ctx.krates = vec![
        Krate {
            name: "ansi_term".to_owned(),
            version: "0.11.0".to_owned(),
            source: Source::Registry {
                registry: registry.clone(),
                chksum: "ee49baf6cb617b853aa8d93bf420db2383fab46d314482ca2803b40d5fde979b"
                    .to_owned(),
            },
        },
        Krate {
            name: "base64".to_owned(),
            version: "0.10.1".to_owned(),
            source: Source::Registry {
                registry: registry.clone(),
                chksum: "0b25d992356d2eb0ed82172f5248873db5560c4721f564b13cb5193bda5e668e"
                    .to_owned(),
            },
        },
        Krate {
            name: "uuid".to_owned(),
            version: "0.7.4".to_owned(),
            source: Source::Registry {
                registry,
                chksum: "90dbc611eb48397705a6b0f6e917da23ae517e4d127123d2cf7674206627d32a"
                    .to_owned(),
            },
        },
    ];

    cf::mirror::crates(&fs_ctx).expect("failed to mirror crates");
    fs_ctx.prep_sync_dirs().expect("create base dirs");
    assert_eq!(cf::sync::crates(&fs_ctx).expect("synced 3 crates").good, 3,);

    let (cache_root, src_root) = util::get_sync_dirs(&fs_ctx);

    // Ensure the unmutated crates are in the cache directory
    {
        for krate in &fs_ctx.krates {
            let bytes = {
                let path = cache_root.join(format!("{}-{}.crate", krate.name, krate.version));

                std::fs::read(&path)
                    .with_context(|| format!("{:#} {}", krate, path.display()))
                    .expect("can't read")
            };

            match &krate.source {
                Source::Registry { chksum, .. } => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        for krate in &fs_ctx.krates {
            let path = src_root.join(format!("{}-{}/Cargo.toml", krate.name, krate.version));
            assert!(path.exists(), "didn't find unpacked {}", path.display());
        }
    }
}

#[test]
fn some_missing() {
    let fs_root = tempfile::TempDir::new().expect("failed to create tempdir");
    let registry = std::sync::Arc::new(Registry::default());
    let mut fs_ctx = util::fs_ctx(fs_root.path().to_owned(), vec![registry.clone()]);

    let missing_root = tempfile::TempDir::new().expect("failed to crate tempdir");
    fs_ctx.root_dir = missing_root.path().to_owned();

    fs_ctx.krates = vec![
        Krate {
            name: "ansi_term".to_owned(),
            version: "0.11.0".to_owned(),
            source: Source::Registry {
                registry: registry.clone(),
                chksum: "ee49baf6cb617b853aa8d93bf420db2383fab46d314482ca2803b40d5fde979b"
                    .to_owned(),
            },
        },
        Krate {
            name: "base64".to_owned(),
            version: "0.10.1".to_owned(),
            source: Source::Registry {
                registry: registry.clone(),
                chksum: "0b25d992356d2eb0ed82172f5248873db5560c4721f564b13cb5193bda5e668e"
                    .to_owned(),
            },
        },
        Krate {
            name: "uuid".to_owned(),
            version: "0.7.4".to_owned(),
            source: Source::Registry {
                registry,
                chksum: "90dbc611eb48397705a6b0f6e917da23ae517e4d127123d2cf7674206627d32a"
                    .to_owned(),
            },
        },
    ];

    tracing::info!("mirroring crates");

    // Download and store the crates in the local fs backend
    cf::mirror::crates(&fs_ctx).expect("failed to mirror crates");

    fs_ctx.prep_sync_dirs().expect("create base dirs");

    // Sync just the base64 crate to the local store
    let stored = fs_ctx.krates.clone();
    fs_ctx.krates = vec![stored[2].clone()];
    assert_eq!(cf::sync::crates(&fs_ctx).expect("synced 1 crate").good, 1);

    let (cache_root, src_root) = util::get_sync_dirs(&fs_ctx);

    // Ensure the unmutated crates are in the cache directory
    {
        for krate in &fs_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match &krate.source {
                Source::Registry { chksum, .. } => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        for krate in &fs_ctx.krates {
            assert!(src_root
                .join(format!("{}-{}/Cargo.toml", krate.name, krate.version))
                .exists());
        }
    }

    // Sync all of the crates, except since we've already synced base64, we should
    // only receive the other 2
    fs_ctx.krates = stored;
    assert_eq!(cf::sync::crates(&fs_ctx).expect("synced 2 crates").good, 2);

    // Ensure the unmutated crates are in the cache directory
    {
        for krate in &fs_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match &krate.source {
                Source::Registry { chksum, .. } => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        for krate in &fs_ctx.krates {
            let path = src_root.join(format!("{}-{}/Cargo.toml", krate.name, krate.version));
            assert!(path.exists(), "didn't find unpacked {}", path.display());
        }
    }
}

#[test]
fn none_missing() {
    let fs_root = tempfile::TempDir::new().expect("failed to create tempdir");
    let registry = std::sync::Arc::new(Registry::default());
    let registries = vec![registry.clone()];
    let mut fs_ctx = util::fs_ctx(fs_root.path().to_owned(), registries);

    let missing_root = tempfile::TempDir::new().expect("failed to crate tempdir");
    fs_ctx.root_dir = missing_root.path().to_owned();

    fs_ctx.krates = vec![
        Krate {
            name: "ansi_term".to_owned(),
            version: "0.11.0".to_owned(),
            source: Source::Registry {
                registry: registry.clone(),
                chksum: "ee49baf6cb617b853aa8d93bf420db2383fab46d314482ca2803b40d5fde979b"
                    .to_owned(),
            },
        },
        Krate {
            name: "base64".to_owned(),
            version: "0.10.1".to_owned(),
            source: Source::Registry {
                registry: registry.clone(),
                chksum: "0b25d992356d2eb0ed82172f5248873db5560c4721f564b13cb5193bda5e668e"
                    .to_owned(),
            },
        },
        Krate {
            name: "uuid".to_owned(),
            version: "0.7.4".to_owned(),
            source: Source::Registry {
                registry,
                chksum: "90dbc611eb48397705a6b0f6e917da23ae517e4d127123d2cf7674206627d32a"
                    .to_owned(),
            },
        },
    ];

    cf::mirror::crates(&fs_ctx).expect("failed to mirror crates");
    fs_ctx.prep_sync_dirs().expect("create base dirs");

    assert_eq!(cf::sync::crates(&fs_ctx).expect("synced 3 crate").good, 3);

    let (cache_root, src_root) = util::get_sync_dirs(&fs_ctx);

    // Ensure the unmutated crates are in the cache directory
    {
        for krate in &fs_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match &krate.source {
                Source::Registry { chksum, .. } => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        for krate in &fs_ctx.krates {
            assert!(src_root
                .join(format!("{}-{}/Cargo.toml", krate.name, krate.version))
                .exists());
        }
    }

    assert_eq!(
        cf::sync::crates(&fs_ctx)
            .expect("synced 0 crates")
            .total_bytes,
        0
    );

    // Ensure the unmutated crates are in the cache directory
    {
        for krate in &fs_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match &krate.source {
                Source::Registry { chksum, .. } => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        for krate in &fs_ctx.krates {
            let path = src_root.join(format!("{}-{}/Cargo.toml", krate.name, krate.version));
            assert!(path.exists(), "didn't find unpacked {}", path.display());
        }
    }
}
