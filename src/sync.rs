use crate::{util, Krate, Source};
use anyhow::{Context, Error};
use bytes::{Buf, IntoBuf};
use log::{debug, error, info};
use rayon::prelude::*;
use std::{convert::TryFrom, io::Write};

const INDEX_DIR: &str = "registry/index/github.com-1ecc6299db9ec823";
const CACHE_DIR: &str = "registry/cache/github.com-1ecc6299db9ec823";
const SRC_DIR: &str = "registry/src/github.com-1ecc6299db9ec823";
const GIT_DB_DIR: &str = "git/db";
const GIT_CO_DIR: &str = "git/checkouts";

pub fn registry_index(ctx: &crate::Ctx) -> Result<(), Error> {
    let index_path = ctx.root_dir.join(INDEX_DIR);
    std::fs::create_dir_all(&index_path).context("failed to create index dir")?;

    // Just skip the index if the git directory already exists,
    // as a patch on top of an existing repo via git fetch is
    // presumably faster
    if index_path.join(".git").exists() {
        info!("registry index already exists, pulling instead");

        let output = std::process::Command::new("git")
            .arg("pull")
            .current_dir(&index_path)
            .output()?;

        if !output.status.success() {
            let err_out = String::from_utf8(output.stderr)?;
            error!(
                "failed to pull registry index, removing it and updating manually: {}",
                err_out
            );
            remove_dir_all::remove_dir_all(&index_path)?;
        } else {
            // Write a file to the directory to let cargo know when it was updated
            std::fs::File::create(index_path.join(".last-updated"))
                .context("failed to crate .last-updated")?;
            return Ok(());
        }
    }

    let url = url::Url::parse("git+https://github.com/rust-lang/crates.io-index.git")?;
    let canonicalized = util::Canonicalized::try_from(&url)?;
    let ident = canonicalized.ident();

    let krate = Krate {
        name: "crates.io-index".to_owned(),
        version: "1.0.0".to_owned(),
        source: Source::Git {
            url: canonicalized.into(),
            ident,
            rev: String::new(),
        },
    };

    let index_data = ctx.backend.fetch(&krate)?;

    let buf_reader = index_data.into_buf().reader();
    let zstd_decoder = zstd::Decoder::new(buf_reader)?;

    if let Err((_, e)) = util::unpack_tar(zstd_decoder, index_path) {
        error!("failed to unpack crates.io-index: {}", e);
    }

    Ok(())
}

pub fn locked_crates(ctx: &crate::Ctx) -> Result<usize, Error> {
    info!("synchronizing {} crates...", ctx.krates.len());

    let root_dir = &ctx.root_dir;

    let cache_dir = root_dir.join(CACHE_DIR);
    let src_dir = root_dir.join(SRC_DIR);
    let git_db_dir = root_dir.join(GIT_DB_DIR);
    let git_co_dir = root_dir.join(GIT_CO_DIR);

    std::fs::create_dir_all(&cache_dir).context("failed to create registry/cache/")?;
    std::fs::create_dir_all(&src_dir).context("failed to create registry/src/")?;
    std::fs::create_dir_all(&git_db_dir).context("failed to create git/db/")?;
    std::fs::create_dir_all(&git_co_dir).context("failed to create git/checkouts/")?;

    let cache_iter = std::fs::read_dir(&cache_dir)?;

    // TODO: Also check the untarred crates
    info!("checking local cache for missing crates...");

    let mut cached_crates: Vec<String> = cache_iter
        .filter_map(|entry| {
            entry
                .ok()
                .and_then(|entry| entry.file_name().to_str().map(|s| s.to_owned()))
        })
        .collect();

    cached_crates.sort();

    let mut to_sync = Vec::with_capacity(ctx.krates.len());
    let mut krate_name = String::with_capacity(128);

    for krate in ctx.krates.iter().filter(|k| !k.source.is_git()) {
        use std::fmt::Write;
        write!(&mut krate_name, "{}", krate.local_id()).unwrap();

        if cached_crates.binary_search(&krate_name).is_err() {
            to_sync.push(krate);
        }

        krate_name.clear();
    }

    for krate in ctx.krates.iter().filter(|k| k.source.is_git()) {
        match &krate.source {
            Source::Git { rev, ident, .. } => {
                let path = git_co_dir.join(format!("{}/{}/.cargo-ok", ident, rev));

                if !path.exists() {
                    to_sync.push(krate);
                }
            }
            _ => unreachable!(),
        }
    }

    // Remove duplicates, eg. when 2 crates are sourced from the same git repository
    to_sync.sort();
    to_sync.dedup();

    if to_sync.is_empty() {
        info!("all crates already available on local disk");
        return Ok(0);
    }

    info!("synchronizing {} missing crates...", to_sync.len());

    to_sync.par_iter().for_each(|krate| {
        match ctx.backend.fetch(krate) {
            Err(e) => error!("failed to download {}: {}", krate, e),
            Ok(krate_data) => {
                match &krate.source {
                    Source::CratesIo(ref chksum) => {
                        if let Err(e) = util::validate_checksum(&krate_data, &chksum) {
                            error!("failed to validate checksum for {}: {}", krate, e);
                            return;
                        }

                        let packed_krate_path = cache_dir.join(format!("{}", krate.local_id()));

                        match std::fs::File::create(&packed_krate_path) {
                            Ok(mut f) => {
                                let _ = f.set_len(krate_data.len() as u64);

                                if let Err(e) = f.write_all(&krate_data) {
                                    error!("failed to write {} to disk: {}", krate, e);
                                }
                            }
                            Err(e) => {
                                error!("failed to create {}: {}", krate, e);
                            }
                        }

                        // Decompress and splat the tar onto the filesystem
                        let buf_reader = krate_data.into_buf().reader();
                        let gz_decoder = flate2::read::GzDecoder::new(buf_reader);

                        let mut src_path = src_dir.join(format!("{}", krate.local_id()));
                        // Remove the .crate extension
                        src_path.set_extension("");

                        if !src_path.exists() {
                            log::debug!("unpacking {} to src/", krate);
                            if let Err((_, e)) = util::unpack_tar(gz_decoder, src_path) {
                                error!("failed to unpack dependency {}: {}", krate, e);
                            }
                        }
                    }
                    Source::Git { rev, .. } => {
                        let buf_reader = krate_data.into_buf().reader();
                        let zstd_decoder = match zstd::Decoder::new(buf_reader) {
                            Ok(zd) => zd,
                            Err(e) => {
                                error!("failed to create decompressor for {}: {}", krate, e);
                                return;
                            }
                        };

                        let db_path = git_db_dir.join(format!("{}", krate.local_id()));

                        // The path may already exist, so in that case just do a fetch
                        if db_path.exists() {
                            debug!("fetching bare repo for {}", krate);
                            if let Err(e) = crate::fetch::update_bare(krate, &db_path) {
                                error!("failed to fetch crate {}: {}", krate, e);
                                return;
                            }
                        } else if let Err((_, e)) = util::unpack_tar(zstd_decoder, &db_path) {
                            error!("failed to unpack dependency {}: {}", krate, e);
                            return;
                        }

                        let co_path = git_co_dir.join(format!("{}/{}", krate.local_id(), rev));

                        // If we get here, it means there wasn't a .cargo-ok in the dir, even if the
                        // rest of it is checked out and ready, so blow it away just in case as we are
                        // doing a clone/checkout from a local bare repository rather than a remote one
                        if co_path.exists() {
                            debug!("removing checkout dir {} for {}", co_path.display(), krate);
                            if let Err(e) = remove_dir_all::remove_dir_all(&co_path) {
                                error!("failed to remove {}: {}", co_path.display(), e);
                                return;
                            }
                        }

                        // Do a checkout of the bare clone
                        debug!("checking out {} to {}", krate, co_path.display());
                        if util::checkout(&db_path, &co_path, rev).is_ok() {
                            // Tell cargo it totally checked this out itself
                            if let Err(e) = std::fs::File::create(co_path.join(".cargo-ok")) {
                                error!("failed to write .cargo-ok: {}", e);
                            }
                        }
                    }
                }
            }
        }
    });

    Ok(to_sync.len())
}
