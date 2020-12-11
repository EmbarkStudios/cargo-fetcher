use crate::{cargo::Source, util, Krate};
use anyhow::{Context, Error};
use bytes::Bytes;
use reqwest::Client;
use std::path::Path;
use tracing::{error, warn};
use tracing_futures::Instrument;

pub(crate) enum KrateSource {
    Registry(Bytes),
    Git(crate::git::GitSource),
}

impl KrateSource {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Registry(bytes) => bytes.len(),
            Self::Git(gs) => gs.db.len() + gs.checkout.as_ref().map(|s| s.len()).unwrap_or(0),
        }
    }
}

pub(crate) async fn from_registry(client: &Client, krate: &Krate) -> Result<KrateSource, Error> {
    async {
        match &krate.source {
            Source::Git { url, rev, .. } => via_git(&url.clone(), rev).await.map(KrateSource::Git),
            Source::Registry { registry, chksum } => {
                let url = registry.download_url(krate);

                let response = client.get(&url).send().await?.error_for_status()?;
                let res = util::convert_response(response).await?;
                let content = res.into_body();

                util::validate_checksum(&content, &chksum)?;

                Ok(KrateSource::Registry(content))
            }
        }
    }
    .instrument(tracing::debug_span!("fetch"))
    .await
}

pub async fn via_git(url: &url::Url, rev: &str) -> Result<crate::git::GitSource, Error> {
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
    tokio::task::spawn_blocking(move || -> Result<(), Error> {
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
            .with_context(|| format!("{} doesn't contain rev '{}'", fetch_url, fetch_rev))?;

        Ok(())
    })
    .instrument(tracing::debug_span!("fetch"))
    .await??;

    let fetch_rev = rev.to_owned();
    let temp_db_path = temp_dir.path().to_owned();
    let checkout = tokio::task::spawn(async move {
        match crate::git::prepare_submodules(
            temp_db_path,
            submodule_dir.path().to_owned(),
            fetch_rev.clone(),
        )
        .instrument(tracing::debug_span!("submodule checkout"))
        .await
        {
            Ok(_) => {
                util::pack_tar(submodule_dir.path())
                    .instrument(tracing::debug_span!("tarballing checkout", rev = %fetch_rev))
                    .await
            }
            Err(e) => Err(e),
        }
    });

    let (db, checkout) = tokio::join!(
        async {
            util::pack_tar(temp_dir.path())
                .instrument(tracing::debug_span!("tarballing db", %url, %rev))
                .await
        },
        checkout,
    );

    Ok(crate::git::GitSource {
        db: db?,
        checkout: checkout?.ok(),
    })
}

pub async fn registry(
    url: &url::Url,
    krates: impl Iterator<Item = String> + Send + 'static,
) -> Result<Bytes, Error> {
    // We don't bother to suport older versions of cargo that don't support
    // bare checkouts of registry indexes, as that has been in since early 2017
    // See https://github.com/rust-lang/cargo/blob/0e38712d4d7b346747bf91fb26cce8df6934e178/src/cargo/sources/registry/remote.rs#L61
    // for details on why cargo still does what it does
    let temp_dir = tempfile::tempdir()?;

    let mut init_opts = git2::RepositoryInitOptions::new();
    //init_opts.bare(true);
    init_opts.external_template(false);

    let repo =
        git2::Repository::init_opts(&temp_dir, &init_opts).context("failed to initialize repo")?;

    let url = url.as_str().to_owned();

    // We need to ship off the fetching to a blocking thread so we don't anger tokio
    tokio::task::spawn_blocking(move || -> Result<(), Error> {
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

        let write_cache = tracing::span!(
            tracing::Level::DEBUG,
            "write-cache-entries",
            registry = url.as_str()
        );

        write_cache.in_scope(|| {
            if let Err(e) = write_cache_entries(repo, krates) {
                error!("Failed to write all .cache entries: {:#}", e);
            }
        });

        Ok(())
    })
    .instrument(tracing::debug_span!("fetch"))
    .await??;

    // We also write a `.last-updated` file just like cargo so that cargo knows
    // the timestamp of the fetch
    std::fs::File::create(temp_dir.path().join(".last-updated"))
        .context("failed to create .last-updated")?;

    util::pack_tar(temp_dir.path())
        .instrument(tracing::debug_span!("tarball"))
        .await
}

/// Writes .cache entries in the registry's directory for all of the specified
/// crates. Cargo will write these entries itself if they don't exist the first
/// time it tries to access the crate's metadata, but this noticeably increases
/// initial fetch times. (see src/cargo/sources/registry/index.rs)
fn write_cache_entries(
    repo: git2::Repository,
    krates: impl Iterator<Item = String>,
) -> Result<(), Error> {
    // the path to the repository itself for bare repositories.
    let cache = if repo.is_bare() {
        repo.path().join(".cache")
    } else {
        repo.path().parent().unwrap().join(".cache")
    };

    std::fs::create_dir_all(&cache)?;

    // Every .cache entry encodes the sha1 it was created at in the beginning
    // so that cargo knows when an entry is out of date with the current HEAD
    let head_commit = {
        let branch = repo
            .find_branch("origin/master", git2::BranchType::Remote)
            .context("failed to find 'master' branch")?;
        branch
            .get()
            .target()
            .context("unable to find commit for 'master' branch")?
    };
    let head_commit_str = head_commit.to_string();

    let tree = repo
        .find_commit(head_commit)
        .context("failed to find HEAD commit")?
        .tree()
        .context("failed to get commit tree")?;

    // These can get rather large, so be generous
    let mut buffer = Vec::with_capacity(32 * 1024);

    for krate in krates {
        // cargo always normalizes paths to lowercase
        let lkrate = krate.to_lowercase();
        let mut rel_path = crate::cargo::get_crate_prefix(&lkrate);
        rel_path.push('/');
        rel_path.push_str(&lkrate);

        let path = &Path::new(&rel_path);

        buffer.clear();
        if let Err(e) = write_summary(path, &repo, &tree, head_commit_str.as_bytes(), &mut buffer) {
            warn!(
                "unable to create cache entry for crate '{}': {:#}",
                krate, e
            );
            continue;
        }

        let cache_path = cache.join(rel_path);

        if let Err(e) = std::fs::create_dir_all(cache_path.parent().unwrap()) {
            warn!(
                "failed to create parent .cache directories for crate '{}': {:#}",
                krate, e
            );
            continue;
        }

        if let Err(e) = std::fs::write(&cache_path, &buffer) {
            warn!(
                "failed to write .cache entry for crate '{}': {:#}",
                krate, e
            );
        }
    }

    Ok(())
}

fn write_summary<'blob>(
    path: &Path,
    repo: &'blob git2::Repository,
    tree: &git2::Tree<'blob>,
    version: &[u8],
    buffer: &mut Vec<u8>,
) -> Result<(), Error> {
    fn split<'a>(haystack: &'a [u8]) -> impl Iterator<Item = &'a [u8]> + 'a {
        struct Split<'a> {
            haystack: &'a [u8],
        }

        impl<'a> Iterator for Split<'a> {
            type Item = &'a [u8];

            fn next(&mut self) -> Option<&'a [u8]> {
                if self.haystack.is_empty() {
                    return None;
                }
                let (ret, remaining) = match memchr::memchr(b'\n', self.haystack) {
                    Some(pos) => (&self.haystack[..pos], &self.haystack[pos + 1..]),
                    None => (self.haystack, &[][..]),
                };
                self.haystack = remaining;
                Some(ret)
            }
        }

        Split { haystack }
    }

    let entry = tree
        .get_path(path)
        .context("failed to get entry for path")?;
    let object = entry
        .to_object(repo)
        .context("failed to get object for entry")?;
    let blob = object.as_blob().context("object is not a blob")?;

    // Writes the binary summary for the crate to a buffer, see
    // src/cargo/sources/registry/index.rs for details
    const CURRENT_CACHE_VERSION: u8 = 1;

    buffer.push(CURRENT_CACHE_VERSION);
    buffer.extend_from_slice(version);
    buffer.push(0);

    for (version, data) in split(blob.content()).filter_map(|line| {
        std::str::from_utf8(line).ok().and_then(|lstr| {
            // We need to get the version, as each entry in the .cache
            // entry is a tuple of the version and the summary
            lstr.find("\"vers\":\"")
                .and_then(|ind| {
                    lstr[ind + 8..]
                        .find('"')
                        .map(|nind| ind + 8..nind + ind + 8)
                })
                .map(|version_range| (&line[version_range], line))
        })
    }) {
        buffer.extend_from_slice(version);
        buffer.push(0);
        buffer.extend_from_slice(data);
        buffer.push(0);
    }

    Ok(())
}
