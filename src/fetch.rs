use crate::{cargo::Source, util, Krate};
use anyhow::Context as _;
use bytes::Bytes;
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
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
pub fn via_git(url: &url::Url, rev: &str) -> anyhow::Result<crate::git::GitSource> {
    // Create a temporary directory to fetch the repo into
    let temp_dir = tempfile::tempdir()?;
    // Create another temporary directory where we *may* checkout submodules into
    let submodule_dir = tempfile::tempdir()?;

    let mut init_opts = git2::RepositoryInitOptions::new();
    init_opts.bare(true);
    init_opts.external_template(false);

    let repo =
        git2::Repository::init_opts(&temp_dir, &init_opts).context("failed to initialize repo")?;

    let fetch_url = url.as_str().to_owned();
    let fetch_rev = rev.to_owned();

    // We need to ship off the fetching to a blocking thread so we don't anger tokio
    {
        let span = tracing::debug_span!("fetch");
        let _fs = span.enter();

        let git_config =
            git2::Config::open_default().context("Failed to open default git config")?;

        crate::git::with_fetch_options(&git_config, &fetch_url, &mut |mut opts| {
            opts.download_tags(git2::AutotagOption::All);
            repo.remote_anonymous(&fetch_url)?
                .fetch(
                    &[
                        "refs/heads/*:refs/remotes/origin/*",
                        "HEAD:refs/remotes/origin/HEAD",
                    ],
                    Some(&mut opts),
                    None,
                )
                .context("Failed to fetch")
        })?;

        // Ensure that the repo actually contains the revision we need
        repo.revparse_single(&fetch_rev)
            .with_context(|| format!("'{fetch_url}' doesn't contain rev '{fetch_rev}'"))?;
    }

    let fetch_rev = rev.to_owned();
    let temp_db_path = util::path(temp_dir.path())?;
    let sub_dir_path = util::path(submodule_dir.path())?;

    let (checkout, db) = rayon::join(
        || -> anyhow::Result<_> {
            crate::git::prepare_submodules(
                temp_db_path.to_owned(),
                sub_dir_path.to_owned(),
                fetch_rev.clone(),
            )?;

            util::pack_tar(sub_dir_path)
        },
        || -> anyhow::Result<_> { util::pack_tar(temp_db_path) },
    );

    Ok(crate::git::GitSource {
        db: db?,
        checkout: checkout.ok(),
    })
}

#[tracing::instrument(level = "debug")]
pub(crate) fn from_registry(
    client: &crate::HttpClient,
    krate: &Krate,
) -> anyhow::Result<KrateSource> {
    match &krate.source {
        Source::Git { url, rev, .. } => via_git(&url.clone(), rev).map(KrateSource::Git),
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

    let krates: Vec<tame_index::KrateName<'_>> = krates
        .iter()
        .filter_map(|krate| match krate.as_str().try_into() {
            Ok(kn) => Some(kn),
            Err(err) => {
                error!("krate name is invalid: {err:#}");
                None
            }
        })
        .collect();

    // Writes .cache entries in the registry's directory for all of the specified
    // crates.
    //
    // Cargo will write these entries itself if they don't exist the first time it
    // tries to access the crate's metadata, but this noticeably increases initial
    // fetch times. (see src/cargo/sources/registry/index.rs)
    match registry.protocol {
        crate::cargo::RegistryProtocol::Git => {
            let repo = {
                let span = tracing::debug_span!("fetch");
                let _fs = span.enter();

                let mut init_opts = git2::RepositoryInitOptions::new();
                init_opts.external_template(false);

                let repo = git2::Repository::init_opts(&temp_dir, &init_opts)
                    .context("failed to initialize repo")?;

                let git_config =
                    git2::Config::open_default().context("Failed to open default git config")?;

                crate::git::with_fetch_options(&git_config, &index_url, &mut |mut opts| {
                    repo.remote_anonymous(&index_url)?
                        .fetch(&["HEAD:refs/remotes/origin/HEAD"], Some(&mut opts), None)
                        .context("Failed to fetch")
                })?;

                repo
            };

            let mut local = index::GitIndex::at_path(temp_dir_path.to_owned(), index_url);

            // Set the HEAD commit so that all cache entries have it serialized
            // for cache invalidation purposes
            let head = repo
                .head()?
                .target()
                .context("HEAD did not point to a commit")?;
            let mut head_oid = [0u8; 20];
            // This will panic if git + git2 ever moves to use something other than sha1
            head_oid.copy_from_slice(head.as_bytes());
            local.set_head_commit(Some(head_oid));

            let tree = repo
                .find_commit(head)
                .context("failed to find HEAD commit")?
                .tree()
                .context("failed to get commit tree")?;

            // Unfortunately git2 repo's by default can't be shared across threads
            // because pointers, so we just take the lazy approach and read the blobs
            // serially
            let blobs: Vec<_> = krates
                .iter()
                .filter_map(|name| {
                    let rel_path = name.relative_path(None);

                    let get_entry = || -> anyhow::Result<_> {
                        let entry = tree
                            .get_path(std::path::Path::new(&rel_path))
                            .context("failed to get entry for path")?;
                        let object = entry
                            .to_object(&repo)
                            .context("failed to get object for entry")?;
                        let blob = object.as_blob().context("object is not a blob")?;
                        Ok(blob.content().to_vec())
                    };

                    match get_entry() {
                        Ok(blob) => Some((name, blob)),
                        Err(err) => {
                            error!("failed to read crate '{name}' metadata: {err:#}");
                            None
                        }
                    }
                })
                .collect();

            // We also write a `.last-updated` file just like cargo so that cargo knows
            // the timestamp of the fetch
            std::fs::File::create(temp_dir.path().join(".last-updated"))
                .context("failed to create .last-updated")?;

            write_cache.in_scope(|| {
                blobs.into_par_iter().for_each(|(name, blob)| {
                    match tame_index::IndexKrate::from_slice(&blob) {
                        Ok(krate) => {
                            if let Err(err) = local.write_to_cache(&krate) {
                                error!("unable to write '{name}' .cache entry: {err:#}");
                            }
                        }
                        Err(err) => {
                            error!("unable to deserialized '{name}' from git blob: {err:#}");
                        }
                    }
                });
            });
        }
        crate::cargo::RegistryProtocol::Sparse => {
            let index = index::RemoteSparseIndex::new(
                index::SparseIndex::at_path(temp_dir_path.to_owned(), index_url),
                client.clone(),
            );

            write_cache.in_scope(|| {
                krates.into_par_iter().for_each(|name| {
                    if let Err(err) = index.krate(name, true /* write the cache entry */) {
                        error!("unable to write .cache entry: {err:#}");
                    }
                });
            });
        }
    };

    util::pack_tar(temp_dir_path)
}
