use crate::{util, Path};
use anyhow::Context as _;
use tracing::warn;

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
            .with_context(|| format!("'{fetch_url}' doesn't contain rev '{fetch_url}'"))?;
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

/// Writes .cache entries in the registry's directory for all of the specified
/// crates.
///
/// Cargo will write these entries itself if they don't exist the first time it
/// tries to access the crate's metadata, but this noticeably increases initial
/// fetch times. (see src/cargo/sources/registry/index.rs)
pub(super) fn write_cache_entries(
    repo: git2::Repository,
    krates: impl Iterator<Item = String>,
) -> anyhow::Result<()> {
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

        {
            let write_cache = tracing::span!(tracing::Level::DEBUG, "summary", %krate);
            let _s = write_cache.enter();

            match write_summary(path, &repo, &tree, head_commit_str.as_bytes(), &mut buffer) {
                Ok(num_versions) => tracing::debug!("wrote entries for {num_versions} versions"),
                Err(e) => {
                    warn!("unable to create cache entry for crate: {e:#}");
                    continue;
                }
            }
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
) -> anyhow::Result<usize> {
    let entry = tree
        .get_path(path.as_std_path())
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

    let mut version_count = 0;

    for (version, data) in iter_index_entries(blob.content()) {
        buffer.extend_from_slice(version);
        buffer.push(0);
        buffer.extend_from_slice(data);
        buffer.push(0);

        version_count += 1;
    }

    Ok(version_count)
}

fn iter_index_entries(blob: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    fn split_blob(haystack: &[u8]) -> impl Iterator<Item = &[u8]> {
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

    split_blob(blob).filter_map(|line| {
        std::str::from_utf8(line).ok().and_then(|lstr| {
            // We need to get the version, as each entry in the .cache
            // entry is a tuple of the version and the summary
            lstr.find("\"vers\":")
                .map(|ind| ind + 7)
                .and_then(|ind| lstr[ind..].find('"').map(|bind| ind + bind + 1))
                .and_then(|bind| {
                    lstr[bind..]
                        .find('"')
                        .map(|eind| (&line[bind..bind + eind], line))
                })
        })
    })
}

#[cfg(test)]
mod test {
    use super::iter_index_entries;

    #[test]
    fn parses_unpretty() {
        const BLOB: &[u8] = include_bytes!("../../tests/unpretty-wasi");
        let expected = [
            "0.0.0",
            "0.3.0",
            "0.4.0",
            "0.5.0",
            "0.6.0",
            "0.7.0",
            "0.9.0+wasi-snapshot-preview1",
            "0.10.0+wasi-snapshot-preview1",
        ];

        assert_eq!(expected.len(), iter_index_entries(BLOB).count());

        for (exp, (actual, _)) in expected.iter().zip(iter_index_entries(BLOB)) {
            assert_eq!(exp.as_bytes(), actual);
        }
    }

    #[test]
    fn parses_pretty() {
        const BLOB: &[u8] = include_bytes!("../../tests/pretty-crate");
        let expected = ["0.2.0", "0.3.0", "0.3.1", "0.4.0", "0.5.0"];

        assert_eq!(expected.len(), iter_index_entries(BLOB).count());

        for (exp, (actual, _)) in expected.iter().zip(iter_index_entries(BLOB)) {
            assert_eq!(exp.as_bytes(), actual);
        }
    }
}
