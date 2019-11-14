use anyhow::Context;
use cargo_fetcher as cf;
use cf::{Krate, Source};

mod util;

#[test]
fn all_missing() {
    let mut s3_ctx = util::s3_ctx("sync-all", "missing/");

    let missing_root = tempfile::TempDir::new().expect("failed to crate tempdir");
    s3_ctx.root_dir = missing_root.path().to_owned();

    s3_ctx.krates = vec![
        Krate {
            name: "ansi_term".to_owned(),
            version: "0.11.0".to_owned(),
            source: Source::CratesIo(
                "ee49baf6cb617b853aa8d93bf420db2383fab46d314482ca2803b40d5fde979b".to_owned(),
            ),
        },
        Krate {
            name: "base64".to_owned(),
            version: "0.10.1".to_owned(),
            source: Source::CratesIo(
                "0b25d992356d2eb0ed82172f5248873db5560c4721f564b13cb5193bda5e668e".to_owned(),
            ),
        },
        Krate {
            name: "uuid".to_owned(),
            version: "0.7.4".to_owned(),
            source: Source::CratesIo(
                "90dbc611eb48397705a6b0f6e917da23ae517e4d127123d2cf7674206627d32a".to_owned(),
            ),
        },
    ];

    cf::mirror::locked_crates(&s3_ctx).expect("failed to mirror crates");
    s3_ctx.prep_sync_dirs().expect("create base dirs");
    assert_eq!(
        cf::sync::locked_crates(&s3_ctx).expect("synced 3 crates"),
        3,
    );

    // Ensure the unmutated crates are in the cache directory
    {
        let cache_root = s3_ctx.root_dir.join(cf::sync::CACHE_DIR);

        for krate in &s3_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match krate.source {
                Source::CratesIo(ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        let src_root = s3_ctx.root_dir.join(cf::sync::SRC_DIR);

        for krate in &s3_ctx.krates {
            let path = src_root.join(format!("{}-{}/Cargo.toml", krate.name, krate.version));
            assert!(path.exists(), "didn't find unpacked {}", path.display());
        }
    }
}

#[test]
fn some_missing() {
    let mut s3_ctx = util::s3_ctx("sync-some", "some_missing/");

    let missing_root = tempfile::TempDir::new().expect("failed to crate tempdir");
    s3_ctx.root_dir = missing_root.path().to_owned();

    s3_ctx.krates = vec![
        Krate {
            name: "ansi_term".to_owned(),
            version: "0.11.0".to_owned(),
            source: Source::CratesIo(
                "ee49baf6cb617b853aa8d93bf420db2383fab46d314482ca2803b40d5fde979b".to_owned(),
            ),
        },
        Krate {
            name: "base64".to_owned(),
            version: "0.10.1".to_owned(),
            source: Source::CratesIo(
                "0b25d992356d2eb0ed82172f5248873db5560c4721f564b13cb5193bda5e668e".to_owned(),
            ),
        },
        Krate {
            name: "uuid".to_owned(),
            version: "0.7.4".to_owned(),
            source: Source::CratesIo(
                "90dbc611eb48397705a6b0f6e917da23ae517e4d127123d2cf7674206627d32a".to_owned(),
            ),
        },
    ];

    cf::mirror::locked_crates(&s3_ctx).expect("failed to mirror crates");
    s3_ctx.prep_sync_dirs().expect("create base dirs");

    let stored = s3_ctx.krates.clone();
    s3_ctx.krates = vec![stored[2].clone()];
    assert_eq!(cf::sync::locked_crates(&s3_ctx).expect("synced 1 crate"), 1);

    // Ensure the unmutated crates are in the cache directory
    {
        let cache_root = s3_ctx.root_dir.join(cf::sync::CACHE_DIR);

        for krate in &s3_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match krate.source {
                Source::CratesIo(ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        let src_root = s3_ctx.root_dir.join(cf::sync::SRC_DIR);

        for krate in &s3_ctx.krates {
            assert!(src_root
                .join(format!("{}-{}/Cargo.toml", krate.name, krate.version))
                .exists());
        }
    }

    s3_ctx.krates = stored;
    assert_eq!(
        cf::sync::locked_crates(&s3_ctx).expect("synced 2 crates"),
        2
    );

    // Ensure the unmutated crates are in the cache directory
    {
        let cache_root = s3_ctx.root_dir.join(cf::sync::CACHE_DIR);

        for krate in &s3_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match krate.source {
                Source::CratesIo(ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        let src_root = s3_ctx.root_dir.join(cf::sync::SRC_DIR);

        for krate in &s3_ctx.krates {
            let path = src_root.join(format!("{}-{}/Cargo.toml", krate.name, krate.version));
            assert!(path.exists(), "didn't find unpacked {}", path.display());
        }
    }
}

#[test]
fn none_missing() {
    let mut s3_ctx = util::s3_ctx("sync-none", "none_missing/");

    let missing_root = tempfile::TempDir::new().expect("failed to crate tempdir");
    s3_ctx.root_dir = missing_root.path().to_owned();

    s3_ctx.krates = vec![
        Krate {
            name: "ansi_term".to_owned(),
            version: "0.11.0".to_owned(),
            source: Source::CratesIo(
                "ee49baf6cb617b853aa8d93bf420db2383fab46d314482ca2803b40d5fde979b".to_owned(),
            ),
        },
        Krate {
            name: "base64".to_owned(),
            version: "0.10.1".to_owned(),
            source: Source::CratesIo(
                "0b25d992356d2eb0ed82172f5248873db5560c4721f564b13cb5193bda5e668e".to_owned(),
            ),
        },
        Krate {
            name: "uuid".to_owned(),
            version: "0.7.4".to_owned(),
            source: Source::CratesIo(
                "90dbc611eb48397705a6b0f6e917da23ae517e4d127123d2cf7674206627d32a".to_owned(),
            ),
        },
    ];

    cf::mirror::locked_crates(&s3_ctx).expect("failed to mirror crates");
    s3_ctx.prep_sync_dirs().expect("create base dirs");

    assert_eq!(cf::sync::locked_crates(&s3_ctx).expect("synced 3 crate"), 3);

    // Ensure the unmutated crates are in the cache directory
    {
        let cache_root = s3_ctx.root_dir.join(cf::sync::CACHE_DIR);

        for krate in &s3_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match krate.source {
                Source::CratesIo(ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        let src_root = s3_ctx.root_dir.join(cf::sync::SRC_DIR);

        for krate in &s3_ctx.krates {
            assert!(src_root
                .join(format!("{}-{}/Cargo.toml", krate.name, krate.version))
                .exists());
        }
    }

    assert_eq!(
        cf::sync::locked_crates(&s3_ctx).expect("synced 0 crates"),
        0
    );

    // Ensure the unmutated crates are in the cache directory
    {
        let cache_root = s3_ctx.root_dir.join(cf::sync::CACHE_DIR);

        for krate in &s3_ctx.krates {
            let bytes =
                std::fs::read(cache_root.join(format!("{}-{}.crate", krate.name, krate.version)))
                    .with_context(|| format!("{:#}", krate))
                    .expect("can't read");

            match krate.source {
                Source::CratesIo(ref chksum) => cf::util::validate_checksum(&bytes, chksum)
                    .expect("failed to validate checksum"),
                _ => unreachable!(),
            }
        }
    }

    // Ensure the crates are unpacked
    {
        let src_root = s3_ctx.root_dir.join(cf::sync::SRC_DIR);

        for krate in &s3_ctx.krates {
            let path = src_root.join(format!("{}-{}/Cargo.toml", krate.name, krate.version));
            assert!(path.exists(), "didn't find unpacked {}", path.display());
        }
    }
}
