mod git;
mod sparse;

use crate::{cargo::Source, util, Krate};
use anyhow::Context as _;
use bytes::Bytes;
use tracing::{error, warn};

pub(crate) enum KrateSource {
    Registry(Bytes),
    Git(crate::git::GitSource),
}

impl KrateSource {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Registry(bytes) => bytes.len(),
            Self::Git(gs) => gs.db.len() + gs.checkout.as_ref().map_or(0, |s| s.len()),
        }
    }
}

#[tracing::instrument(level = "debug")]
pub(crate) fn from_registry(
    client: &crate::HttpClient,
    krate: &Krate,
) -> anyhow::Result<KrateSource> {
    match &krate.source {
        Source::Git { url, rev, .. } => git::via_git(&url.clone(), rev).map(KrateSource::Git),
        Source::Registry { registry, chksum } => {
            let url = registry.download_url(krate);

            let response = client.get(url).send()?.error_for_status()?;
            let res = util::convert_response(response)?;
            let content = res.into_body();

            util::validate_checksum(&content, chksum)?;

            Ok(KrateSource::Registry(content))
        }
    }
}

#[tracing::instrument(level = "debug", skip(krates))]
pub fn registry(
    registry: &crate::cargo::Registry,
    krates: impl Iterator<Item = String> + Send + 'static,
) -> anyhow::Result<Bytes> {
    // We don't bother to suport older versions of cargo that don't support
    // bare checkouts of registry indexes, as that has been in since early 2017
    // See https://github.com/rust-lang/cargo/blob/0e38712d4d7b346747bf91fb26cce8df6934e178/src/cargo/sources/registry/remote.rs#L61
    // for details on why cargo still does what it does
    let temp_dir = tempfile::tempdir()?;
    let temp_dir_path = util::path(temp_dir.path())?;

    match registry.protocol {
        crate::cargo::RegistryProtocol::Git => {
            let mut init_opts = git2::RepositoryInitOptions::new();
            init_opts.external_template(false);

            let repo = git2::Repository::init_opts(&temp_dir, &init_opts)
                .context("failed to initialize repo")?;

            let url = registry.index.as_str().to_owned();

            {
                let span = tracing::debug_span!("fetch");
                let _fs = span.enter();
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
                })?;

                let write_cache = tracing::span!(tracing::Level::DEBUG, "write-cache-entries",);

                write_cache.in_scope(|| {
                    if let Err(e) = git::write_cache_entries(repo, krates) {
                        error!("Failed to write all .cache entries: {e:#}");
                    }
                });
            }

            // We also write a `.last-updated` file just like cargo so that cargo knows
            // the timestamp of the fetch
            std::fs::File::create(temp_dir.path().join(".last-updated"))
                .context("failed to create .last-updated")?;
        }
        crate::cargo::RegistryProtocol::Sparse => {
            let write_cache = tracing::span!(tracing::Level::DEBUG, "write-cache-entries",);

            write_cache.in_scope(|| {
                if let Err(e) = sparse::write_cache_entries(temp_dir_path, &registry.index, krates)
                {
                    error!("Failed to write all .cache entries: {e:#}");
                }
            });
        }
    }

    util::pack_tar(temp_dir_path)
}
