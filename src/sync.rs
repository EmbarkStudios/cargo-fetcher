use crate::{util, Krate, Source};
use anyhow::{Context, Error};
use bytes::buf::BufExt;
use log::{debug, error, info};
use std::{convert::TryFrom, io::Write, path::PathBuf};

pub const INDEX_DIR: &str = "registry/index/github.com-1ecc6299db9ec823";
pub const CACHE_DIR: &str = "registry/cache/github.com-1ecc6299db9ec823";
pub const SRC_DIR: &str = "registry/src/github.com-1ecc6299db9ec823";
pub const GIT_DB_DIR: &str = "git/db";
pub const GIT_CO_DIR: &str = "git/checkouts";

pub async fn registry_index(root_dir: PathBuf, backend: crate::Storage) -> Result<(), Error> {
    let index_path = root_dir.join(INDEX_DIR);
    std::fs::create_dir_all(&index_path).context("failed to create index dir")?;

    // Just skip the index if the git directory already exists,
    // as a patch on top of an existing repo via git fetch is
    // presumably faster
    if index_path.join(".git").exists() {
        info!("registry index already exists, fetching instead");

        let output = tokio::process::Command::new("git")
            .arg("fetch")
            .current_dir(&index_path)
            .output()
            .await?;

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

    let index_data = backend.fetch(&krate).await?;

    let buf_reader = index_data.reader();
    let zstd_decoder = zstd::Decoder::new(buf_reader)?;

    if let Err((_, e)) = util::unpack_tar(zstd_decoder, index_path) {
        error!("failed to unpack crates.io-index: {}", e);
    }

    Ok(())
}

async fn sync_git(
    db_dir: PathBuf,
    co_dir: PathBuf,
    krate: &Krate,
    data: bytes::Bytes,
    rev: &str,
) -> Result<(), Error> {
    let buf_reader = data.reader();
    let zstd_decoder = zstd::Decoder::new(buf_reader)?;

    let db_path = db_dir.join(format!("{}", krate.local_id()));

    // The path may already exist, so in that case just do a fetch
    if db_path.exists() {
        debug!("fetching bare repo for {}", krate);
        crate::fetch::update_bare(krate, &db_path)
            .await
            .with_context(|| format!("unable to fetch into {}", db_path.display()))?;
    } else {
        util::unpack_tar(zstd_decoder, &db_path)
            .map_err(|(_, e)| e)
            .with_context(|| format!("unable to unpack tar into {}", db_path.display()))?;
    }

    let co_path = co_dir.join(format!("{}/{}", krate.local_id(), rev));

    // If we get here, it means there wasn't a .cargo-ok in the dir, even if the
    // rest of it is checked out and ready, so blow it away just in case as we are
    // doing a clone/checkout from a local bare repository rather than a remote one
    if co_path.exists() {
        debug!("removing checkout dir {} for {}", co_path.display(), krate);
        remove_dir_all::remove_dir_all(&co_path)
            .with_context(|| format!("unable to remove {}", co_path.display()))?;
    }

    // Do a checkout of the bare clone
    debug!("checking out {} to {}", krate, co_path.display());
    util::checkout(&db_path, &co_path, rev)?;
    let ok = co_path.join(".cargo-ok");
    std::fs::File::create(&ok).with_context(|| ok.display().to_string())?;

    Ok(())
}

pub async fn locked_crates(ctx: &crate::Ctx) -> Result<usize, Error> {
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

    let sync_package = |krate: &Krate, data: bytes::Bytes, chksum: &str| -> Result<(), Error> {
        util::validate_checksum(&data, chksum)?;

        let packed_krate_path = cache_dir.join(format!("{}", krate.local_id()));

        {
            let mut f = std::fs::File::create(&packed_krate_path)
                .with_context(|| packed_krate_path.display().to_string())?;
            let _ = f.set_len(data.len() as u64);
            f.write_all(&data)?;
        }

        // Decompress and splat the tar onto the filesystem
        let buf_reader = data.reader();
        let gz_decoder = flate2::read::GzDecoder::new(buf_reader);

        let mut src_path = src_dir.join(format!("{}", krate.local_id()));
        // Remove the .crate extension
        src_path.set_extension("");
        let ok = src_path.join(".cargo-ok");

        if !ok.exists() {
            if src_path.exists() {
                log::debug!("cleaning src/ dir for {}", krate);
                remove_dir_all::remove_dir_all(&src_path)
                    .with_context(|| format!("unable to remove {}", src_path.display()))?;
            }

            log::debug!("unpacking {} to src/", krate);

            // Crate tarballs already include the top level directory internally,
            // so unpack in the top-level source directory
            util::unpack_tar(gz_decoder, &src_dir).map_err(|(_, e)| e)?;

            // Create the .cargo-ok file so that cargo doesn't suspect a thing
            std::fs::File::create(&ok).with_context(|| ok.display().to_string())?;
        }

        Ok(())
    };

    let num_syncs = to_sync.len();

    use futures::StreamExt;

    let bodies = futures::stream::iter(to_sync)
        .map(|krate| {
            let backend = ctx.backend.clone();

            let git_db_dir = git_db_dir.clone();
            let git_co_dir = git_co_dir.clone();

            async move {
                match backend.fetch(krate).await {
                    Err(e) => Err(format!("failed to download {}: {}", krate, e)),
                    Ok(krate_data) => match &krate.source {
                        Source::CratesIo(ref chksum) => sync_package(krate, krate_data, chksum)
                            .map_err(|e| {
                                format!("unable to synchronize (crates.io) {}: {}", krate, e)
                            }),
                        Source::Git { rev, .. } => {
                            sync_git(git_db_dir, git_co_dir, krate, krate_data, rev)
                                .await
                                .map_err(|e| {
                                    format!("unable to synchronize (git) {}: {}", krate, e)
                                })
                        }
                    },
                }
            }
        })
        .buffer_unordered(32);

    bodies
        .for_each(|res| async move {
            match res {
                Ok(_) => {}
                Err(e) => {
                    error!("{}", e);
                }
            }
        })
        .await;

    Ok(num_syncs)
}
