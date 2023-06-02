use crate::{util, Krate, Registry, Source};
use anyhow::{Context, Error};
//use indicatif as ia;
use std::{io::Write, path::PathBuf};
use tracing::{debug, error, info, warn};

pub const INDEX_DIR: &str = "registry/index";
pub const CACHE_DIR: &str = "registry/cache";
pub const SRC_DIR: &str = "registry/src";
pub const GIT_DB_DIR: &str = "git/db";
pub const GIT_CO_DIR: &str = "git/checkouts";

pub fn registry_indices(
    root_dir: PathBuf,
    backend: crate::Storage,
    registries: Vec<std::sync::Arc<Registry>>,
) {
    let root_dir = &root_dir;

    use rayon::prelude::*;

    registries.into_par_iter().for_each(|registry| {
        if let Err(err) = registry_index(root_dir, backend.clone(), registry) {
            error!("{:#}", err);
        }
    });
}

#[tracing::instrument(skip(backend))]
pub fn registry_index(
    root_dir: &Path,
    backend: crate::Storage,
    registry: std::sync::Arc<Registry>,
) -> Result<(), Error> {
    let ident = registry.short_name();

    let index_path = root_dir.join(INDEX_DIR).join(ident.clone());
    std::fs::create_dir_all(&index_path).context("failed to create index dir")?;

    // Just skip the index if the git directory already exists,
    // as a patch on top of an existing repo via git fetch is
    // presumably faster
    if let Ok(repo) = git2::Repository::open(&index_path) {
        info!("registry index already exists, fetching instead");

        let url = registry.index.as_str().to_owned();

        let fetch_res = {
            let span = tracing::debug_span!("fetch");
            let _sf = span.enter();

            let git_config =
                git2::Config::open_default().context("Failed to open default git config")?;

            crate::git::with_fetch_options(&git_config, &url, &mut |mut opts| {
                repo.remote_anonymous(&url)?
                    .fetch(
                        &[
                            "refs/heads/master:refs/remotes/origin/master",
                            "HEAD:refs/remotes/origin/HEAD",
                        ],
                        Some(&mut opts),
                        None,
                    )
                    .context("Failed to fetch")
            })
        };

        // We need to ship off the fetching to a blocking thread so we don't anger tokio
        match fetch_res {
            Ok(_) => {
                // Write a file to the directory to let cargo know when it was updated
                std::fs::File::create(index_path.join(".last-updated"))
                    .context("failed to crate .last-updated")?;
                return Ok(());
            }
            Err(err_out) => {
                error!(
                    "failed to pull registry index, removing it and updating manually: {}",
                    err_out
                );
                remove_dir_all::remove_dir_all(&index_path)?;
            }
        }
    }

    let krate = Krate {
        name: ident.clone(),
        version: "1.0.0".to_owned(),
        source: Source::Git {
            url: registry.index.clone(),
            ident,
            rev: "feedc0de".to_owned(),
        },
    };

    let index_data = backend.fetch(&krate)?;

    if let Err(e) = util::unpack_tar(index_data, util::Encoding::Zstd, &index_path) {
        error!(err = ?e, "failed to unpack crates.io-index");
    }

    Ok(())
}

#[tracing::instrument(skip(src))]
fn sync_git(
    db_dir: PathBuf,
    co_dir: PathBuf,
    krate: &Krate,
    src: crate::git::GitSource,
    rev: &str,
) -> Result<(), Error> {
    let db_path = db_dir.join(format!("{}", krate.local_id()));

    // Always just blow away and do a sync from the remote tar
    if db_path.exists() {
        remove_dir_all::remove_dir_all(&db_path).context("failed to remove existing DB path")?;
    }

    let crate::git::GitSource { db, checkout } = src;

    let unpack_path = db_path.clone();
    util::unpack_tar(db, util::Encoding::Zstd, &unpack_path)?;

    let co_path = co_dir.join(format!("{}/{}", krate.local_id(), rev));

    // If we get here, it means there wasn't a .cargo-ok in the dir, even if the
    // rest of it is checked out and ready, so blow it away just in case as we are
    // doing a clone/checkout from a local bare repository rather than a remote one
    if co_path.exists() {
        debug!("removing checkout dir {} for {}", co_path.display(), krate);
        remove_dir_all::remove_dir_all(&co_path)
            .with_context(|| format!("unable to remove {}", co_path.display()))?;
    }

    // If we have a checkout tarball, use that, as it will include submodules,
    // otherwise do a checkout
    match checkout {
        Some(checkout) => {
            util::unpack_tar(checkout, util::Encoding::Zstd, &co_path)?;
        }
        None => {
            // Do a checkout of the bare clone
            crate::git::checkout(db_path, co_path.clone(), rev.to_owned())?;
        }
    }

    let ok = co_path.join(".cargo-ok");
    // The non-git .cargo-ok has "ok" in it, however the git ones do not
    std::fs::File::create(&ok).with_context(|| ok.display().to_string())?;

    Ok(())
}

use std::path::Path;

#[tracing::instrument(level = "debug", skip(data))]
fn sync_package(
    cache_dir: &Path,
    src_dir: &Path,
    krate: &Krate,
    data: bytes::Bytes,
    chksum: &str,
) -> Result<(), Error> {
    util::validate_checksum(&data, chksum)?;

    let packed_krate_path = cache_dir.join(format!("{}", krate.local_id()));

    let pack_data = data.clone();
    let packed_path = packed_krate_path;

    let (pack_write, unpack) = rayon::join(
        // Spawn a worker thread to write the original pack file to disk as we don't
        // particularly care when it is done
        || -> Result<(), Error> {
            let s = tracing::debug_span!("pack_write");
            let _ = s.enter();
            let mut f = std::fs::File::create(&packed_path)?;

            let _ = f.set_len(pack_data.len() as u64);
            f.write_all(&pack_data)?;
            f.sync_all()?;

            debug!(bytes = pack_data.len(), path = ?packed_path, "wrote pack file to disk");
            Ok(())
        },
        || -> Result<(), Error> {
            let mut src_path = src_dir.join(format!("{}", krate.local_id()));

            // Remove the .crate extension
            src_path.set_extension("");
            let ok = src_path.join(".cargo-ok");

            if !ok.exists() {
                if src_path.exists() {
                    debug!("cleaning src/");
                    if let Err(e) = remove_dir_all::remove_dir_all(&src_path) {
                        error!(err = ?e, "failed to remove src/");
                        return Err(e.into());
                    }
                }

                // Crate tarballs already include the top level directory internally,
                // so unpack in the top-level source directory
                if let Err(e) =
                    util::unpack_tar(data, util::Encoding::Gzip, src_path.parent().unwrap())
                {
                    error!(err = ?e, "failed to unpack to src/");
                    return Err(e);
                }

                // Create the .cargo-ok file so that cargo doesn't suspect a thing
                if let Err(e) = util::write_ok(&ok) {
                    // If this happens, cargo will just resync and recheckout the repo most likely
                    warn!(err = ?e, "failed to write .cargo-ok");
                }
            }

            Ok(())
        },
    );

    if let Err(err) = pack_write {
        error!(?err, path = ?packed_path, "failed to write tarball to disk");
    }

    if let Err(err) = unpack {
        error!(?err, "failed to unpack tarball to disk");
    }

    Ok(())
}

fn get_missing_git_sources<'krate>(
    ctx: &'krate crate::Ctx,
    git_co_dir: &Path,
    to_sync: &mut Vec<&'krate Krate>,
) {
    for (rev, ident, krate) in ctx.krates.iter().filter_map(|k| match &k.source {
        Source::Git { rev, ident, .. } => Some((rev, ident, k)),
        Source::Registry { .. } => None,
    }) {
        let path = git_co_dir.join(format!("{ident}/{rev}/.cargo-ok"));

        if !path.exists() {
            to_sync.push(krate);
        }
    }
}

fn get_missing_registry_sources<'krate>(
    ctx: &'krate crate::Ctx,
    registry: &Registry,
    cache_dir: &Path,
    to_sync: &mut Vec<&'krate Krate>,
) -> Result<(), Error> {
    let cache_iter = std::fs::read_dir(cache_dir)?;

    let mut cached_crates: Vec<String> = cache_iter
        .filter_map(|entry| {
            entry
                .ok()
                .and_then(|entry| entry.file_name().to_str().map(|s| s.to_owned()))
        })
        .collect();

    cached_crates.sort();

    let mut krate_name = String::with_capacity(128);

    for krate in ctx.krates.iter().filter(|k| *k == registry) {
        use std::fmt::Write;
        write!(&mut krate_name, "{}", krate.local_id()).unwrap();

        if cached_crates.binary_search(&krate_name).is_err() {
            to_sync.push(krate);
        }

        krate_name.clear();
    }

    Ok(())
}

#[derive(Debug)]
pub struct Summary {
    pub total_bytes: usize,
    pub bad: u32,
    pub good: u32,
}

pub fn crates(ctx: &crate::Ctx) -> Result<Summary, Error> {
    info!("synchronizing {} crates...", ctx.krates.len());

    let root_dir = &ctx.root_dir;
    let git_db_dir = root_dir.join(GIT_DB_DIR);
    let git_co_dir = root_dir.join(GIT_CO_DIR);

    std::fs::create_dir_all(&git_db_dir).context("failed to create git/db/")?;
    std::fs::create_dir_all(&git_co_dir).context("failed to create git/checkouts/")?;

    info!("checking local cache for missing crates...");
    let mut git_sync = Vec::new();
    get_missing_git_sources(ctx, &git_co_dir, &mut git_sync);

    let mut registry_sync = Vec::new();
    for registry in &ctx.registries {
        let (cache_dir, src_dir) = registry.sync_dirs(root_dir);
        std::fs::create_dir_all(&cache_dir).context("failed to create registry/cache")?;
        std::fs::create_dir_all(src_dir).context("failed to create registry/src")?;

        get_missing_registry_sources(ctx, registry, &cache_dir, &mut registry_sync)?;
    }

    // Remove duplicates, eg. when 2 crates are sourced from the same git repository
    git_sync.sort();
    git_sync.dedup();

    // probably shouldn't be needed, but why not
    registry_sync.sort();
    registry_sync.dedup();

    if git_sync.is_empty() && registry_sync.is_empty() {
        info!("all crates already available on local disk");
        return Ok(Summary {
            total_bytes: 0,
            good: 0,
            bad: 0,
        });
    }

    info!(
        "synchronizing {} missing crates...",
        git_sync.len() + registry_sync.len()
    );

    let sync = |to_sync: Vec<&Krate>| -> Summary {
        use rayon::prelude::*;

        to_sync
            .into_par_iter()
            .map(|krate| {
                let span = tracing::debug_span!("sync", %krate);
                let _ss = span.enter();

                let backend = ctx.backend.clone();

                let git_db_dir = git_db_dir.clone();
                let git_co_dir = git_co_dir.clone();

                match &krate.source {
                    Source::Registry { registry, chksum } => {

                        match {
                            let span = tracing::debug_span!("download");
                            let _ds = span.enter();
                            backend.fetch(krate)
                        } {
                            Ok(krate_data) => {
                                let len = krate_data.len();
                                let (cache_dir, src_dir) = registry.sync_dirs(root_dir);
                                if let Err(e) =
                                    sync_package(&cache_dir, &src_dir, krate, krate_data, chksum)
                                {
                                    error!(err = ?e, "failed to splat package");
                                    return Err(e);
                                }

                                Ok(len)
                            }
                            Err(err) => {
                                error!(err = ?err, krate = %krate, cloud = %krate.cloud_id(), "failed to download");
                                Err(err)
                            }
                        }
                    }
                    Source::Git { rev, .. } => {
                        let (krate_data, checkout) = rayon::join(|| {
                            let span = tracing::debug_span!("download");
                            let _ds = span.enter();
                            backend.fetch(krate)
                        }, || {
                            let mut checkout_id = krate.clone();

                            if let Source::Git { rev, .. } = &mut checkout_id.source {
                                rev.push_str("-checkout");
                            }

                            {
                                let span = tracing::debug_span!("download_checkout");
                                let _ds = span.enter();
                                backend.fetch(&checkout_id).ok()
                            }
                        });

                        let krate_data = match krate_data {
                            Ok(krate_data) => {
                                krate_data
                            }
                            Err(err) => {
                                error!(err = ?err, krate = %krate, cloud = %krate.cloud_id(), "failed to download");
                                return Err(err);
                            }
                        };

                        let mut len = krate_data.len();

                        if let Some(co) = &checkout {
                            len += co.len();
                        }

                        let git_source = crate::git::GitSource {
                            db: krate_data,
                            checkout,
                        };

                        match sync_git(git_db_dir, git_co_dir, krate, git_source, rev) {
                            Ok(_) => {
                                Ok(len)
                            }
                            Err(err) => {
                                error!(err = ?err, "failed to splat git repo");
                                Err(err)
                            }
                        }
                    }
                }
            })
            .fold(
                || Summary {
                    total_bytes: 0,
                    bad: 0,
                    good: 0,
                },
                |mut acc, res| {
                    match res {
                        Ok(len) => {
                            acc.good += 1;
                            acc.total_bytes += len;
                        }
                        Err(_) => {
                            acc.bad += 1;
                        }
                    }

                    acc
                },
            )
            .reduce(
                || Summary {
                    total_bytes: 0,
                    bad: 0,
                    good: 0,
                },
                |a, b| Summary {
                    total_bytes: a.total_bytes + b.total_bytes,
                    bad: a.bad + b.bad,
                    good: a.good + b.good,
                },
            )
    };

    let (gs, rs) = rayon::join(|| sync(git_sync), || sync(registry_sync));

    Ok(Summary {
        total_bytes: gs.total_bytes + rs.total_bytes,
        bad: gs.bad + rs.bad,
        good: gs.good + rs.good,
    })
}
