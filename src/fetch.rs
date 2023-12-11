use crate::{cargo::Source, util, Krate};
use anyhow::Context as _;
use bytes::Bytes;
use tracing::warn;

pub(crate) enum KratePackage {
    Registry(Bytes),
    Git(crate::git::GitPackage),
}

impl KratePackage {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Registry(bytes) => bytes.len(),
            Self::Git(gs) => gs.db.len() + gs.checkout.as_ref().map_or(0, |s| s.len()),
        }
    }
}

#[tracing::instrument(level = "debug")]
pub(crate) async fn from_registry(
    client: &crate::HttpClient,
    krate: &Krate,
) -> anyhow::Result<KratePackage> {
    match &krate.source {
        Source::Git(gs) => {
            let gs = gs.clone();
            tokio::task::spawn_blocking(move || crate::git::clone(&gs).map(KratePackage::Git))
                .await
                .unwrap()
        }
        Source::Registry(rs) => {
            let url = rs.registry.download_url(krate);

            // Depending on how many crates we are mirroring, we can be sending
            // hundreds of concurrent requests to crates.io...and hit
            // https://github.com/seanmonstar/reqwest/issues/1748
            let res = loop {
                let res = client.get(&url).send().await;

                match res {
                    Err(err) if err.is_connect() || err.is_timeout() || err.is_request() => {
                        continue
                    }
                    Err(err) => return Err(err.into()),
                    Ok(res) => break res,
                }
            };

            let response = res.error_for_status()?;
            let res = util::convert_response(response).await?;
            let content = res.into_body();

            util::validate_checksum(&content, &rs.chksum)?;

            Ok(KratePackage::Registry(content))
        }
    }
}

#[tracing::instrument(level = "debug", skip(krates))]
pub async fn registry(
    client: &crate::HttpClient,
    registry: &crate::cargo::Registry,
    krates: Vec<String>,
) -> anyhow::Result<Bytes> {
    use tame_index::index;

    // We don't bother to support older versions of cargo that don't support
    // bare checkouts of registry indexes, as that has been in since early 2017
    // See https://github.com/rust-lang/cargo/blob/0e38712d4d7b346747bf91fb26cce8df6934e178/src/cargo/sources/registry/remote.rs#L61
    // for details on why cargo still does what it does
    let temp_dir = tempfile::tempdir()?;
    let temp_dir_path = util::path(temp_dir.path())?;

    let index_url = registry.index.as_str().to_owned();

    let write_cache = tracing::span!(tracing::Level::DEBUG, "write-cache-entries");

    let location = index::IndexLocation {
        // note this is a bit of a misnomer, it could be the crates.io registry
        url: index::IndexUrl::NonCratesIo(index_url.clone().into()),
        root: index::IndexPath::Exact(temp_dir_path.to_owned()),
    };

    // Writes .cache entries in the registry's directory for all of the specified
    // crates.
    //
    // Cargo will write these entries itself if they don't exist the first time it
    // tries to access the crate's metadata in the case of git, but this noticeably
    // increases initial fetch times. (see src/cargo/sources/registry/index.rs)
    //
    // For sparse indices, the cache entries are the _only_ local state, and if
    // not present means every missing crate needs to be fetched, without the
    // possibility of the local cache entry being up to date according to the
    // etag/modified time of the remote
    match registry.protocol {
        crate::cargo::RegistryProtocol::Git => {
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let rgi = {
                    let span = tracing::debug_span!("fetch");
                    let _fs = span.enter();

                    tame_index::index::RemoteGitIndex::new(
                        tame_index::index::GitIndex::new(location)
                            .context("unable to open git index")?,
                        &tame_index::index::FileLock::unlocked(),
                    )
                    .context("failed to fetch")?
                };

                write_cache.in_scope(|| {
                    // As with git2, gix::Repository is not thread safe, we _could_
                    // read blobs in serial then write in parallel, but that's not really
                    // worth it for a few hundred crates (probably), but see
                    // https://github.com/frewsxcv/rust-crates-index/blob/a9b60653efb72d9e6be98c4f8fe56194475cbd3f/src/git/mod.rs#L316-L360
                    // for a way this could be done in the future
                    let unlocked = &tame_index::index::FileLock::unlocked();
                    for name in krates {
                        let Ok(name) = name.as_str().try_into() else {
                            warn!("crate name '{name}' is invalid");
                            continue;
                        };
                        if let Err(err) =
                            rgi.krate(name, true /* write the cache entry */, unlocked)
                        {
                            warn!("unable to write .cache entry: {err:#}");
                        }
                    }
                });

                Ok(())
            })
            .await
            .unwrap()?;
        }
        crate::cargo::RegistryProtocol::Sparse => {
            let index = index::AsyncRemoteSparseIndex::new(
                index::SparseIndex::new(location)?,
                client.clone(),
            );

            #[allow(unsafe_code)]
            // SAFETY: we don't forget the future :p
            unsafe {
                async_scoped::TokioScope::scope_and_collect(|s| {
                    s.spawn(async {
                        // We don't particularly care if an individual crate fails here
                        // since the index will be healed by cargo, but still good to
                        // know if something was amiss
                        for (name, res) in index
                            .krates(
                                krates.into_iter().collect(),
                                true,
                                None,
                                &tame_index::index::FileLock::unlocked(),
                            )
                            .await
                        {
                            match res {
                                Ok(Some(_)) => {}
                                Ok(None) => {
                                    warn!("index entry for '{name}' was not found");
                                }
                                Err(err) => {
                                    warn!("unable to write .cache entry for '{name}': {err:#}");
                                }
                            }
                        }
                    });

                    s.spawn(async {
                        let write_config = async {
                            let url =
                                format!("{}config.json", index_url.split_once('+').unwrap().1);

                            let res = loop {
                                let res = client.get(&url).send().await;

                                match res {
                                    Err(err)
                                        if err.is_connect()
                                            || err.is_timeout()
                                            || err.is_request() =>
                                    {
                                        continue
                                    }
                                    Err(err) => Err(err)
                                        .context("failed to send request for config.json")?,
                                    Ok(res) => break res,
                                }
                            };

                            let config_body = res
                                .bytes()
                                .await
                                .context("failed to read config.json response body")?;

                            std::fs::write(temp_dir.path().join("config.json"), &config_body)
                                .context("failed to write config.json")
                        };

                        if let Err(err) = write_config.await {
                            warn!("unable to write config.json: {err:#}");
                        }
                    });
                })
                .await;
            }
        }
    };

    util::pack_tar(temp_dir_path)
}
