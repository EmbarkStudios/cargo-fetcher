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

/// Returns a tuple of the version and JSON manifest for every crate version in
/// a cargo index summary
fn iter_index_entries(blob: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    /// Returns an iterator over the specified blob split by a `\n`
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

fn write_summary(version: &[u8], blob: &[u8], buffer: &mut Vec<u8>) -> usize {
    // Writes the binary summary for the crate to a buffer, see
    // src/cargo/sources/registry/index.rs for details
    const CURRENT_CACHE_VERSION: u8 = 3;

    /// The maximum schema version of the `v` field in the index this version of
    /// cargo understands. See [`IndexPackage::v`] for the detail.
    const INDEX_V_MAX: u32 = 2;

    // Reserve enough room in the vector for the header and all of the versions
    let versions_capacity: usize = iter_index_entries(blob)
        .map(
            |(vers, data)| vers.len() + data.len() + 2, /* 2 nulls */
        )
        .sum();
    buffer.reserve_exact(
        std::mem::size_of::<u8>() // cache version
        + std::mem::size_of::<u32>() // index_v_max
        + version.len() // version identifier for entry (git rev, etag, etc)
        + 1 // null
        + versions_capacity, // size of all version entries
    );

    buffer.push(CURRENT_CACHE_VERSION);
    buffer.extend_from_slice(&u32::to_le_bytes(INDEX_V_MAX));
    buffer.extend_from_slice(version);
    buffer.push(0);

    let mut version_count = 0;

    for (version, data) in iter_index_entries(blob) {
        buffer.extend_from_slice(version);
        buffer.push(0);
        buffer.extend_from_slice(data);
        buffer.push(0);

        version_count += 1;
    }

    version_count
}

#[cfg(test)]
mod test {
    use super::iter_index_entries;
    const BLOB: &[u8] = include_bytes!("../tests/unpretty-wasi.txt");
    const WASI_VERSIONS: &[&str] = &[
        "0.0.0",
        "0.3.0",
        "0.4.0",
        "0.5.0",
        "0.6.0",
        "0.7.0",
        "0.9.0+wasi-snapshot-preview1",
        "0.10.0+wasi-snapshot-preview1",
    ];

    #[test]
    fn parses_unpretty() {
        assert_eq!(WASI_VERSIONS.len(), iter_index_entries(BLOB).count());

        for (exp, (actual, _)) in WASI_VERSIONS.iter().zip(iter_index_entries(BLOB)) {
            assert_eq!(exp.as_bytes(), actual);
        }
    }

    #[test]
    fn parses_pretty() {
        const BLOB_PRETTY: &[u8] = include_bytes!("../tests/pretty-crate.txt");
        let expected = ["0.2.0", "0.3.0", "0.3.1", "0.4.0", "0.5.0"];

        assert_eq!(expected.len(), iter_index_entries(BLOB_PRETTY).count());

        for (exp, (actual, _)) in expected.iter().zip(iter_index_entries(BLOB_PRETTY)) {
            assert_eq!(exp.as_bytes(), actual);
        }
    }

    #[test]
    fn writes_summary() {
        const VERSION: &str = "etag: 9b9907cdecafa38556de200edda50a12";
        let mut output = Vec::new();
        super::write_summary(VERSION.as_bytes(), BLOB, &mut output);

        const WASI_SIZES: &[usize] = &[157, 157, 157, 157, 479, 672, 691, 692];

        let header = 1 + 4 + VERSION.len() + 1;
        let versions: usize = WASI_VERSIONS
            .iter()
            .zip(WASI_SIZES.iter())
            .map(|(vers, size)| vers.len() + *size + 2)
            .sum();

        assert_eq!(output.len(), header + versions);
    }
}
