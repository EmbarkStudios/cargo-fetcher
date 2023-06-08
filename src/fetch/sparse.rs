use crate::Path;
use anyhow::Context as _;
use rayon::prelude::{IntoParallelIterator, ParallelIterator};

pub(super) fn write_cache_entries(
    dir: &Path,
    index: &url::Url,
    krates: impl Iterator<Item = String>,
) -> anyhow::Result<()> {
    use reqwest::header;
    // tell crates.io we're aware of the sparse protocol...assuming crates.io for
    // now since it's the only one currently (AFAIK)that supports this protocol
    let mut headers = header::HeaderMap::new();
    headers.insert(
        "cargo-protocol",
        header::HeaderValue::from_static("version=1"),
    );
    // All index entries are just files with lines of JSON
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("text/plain"),
    );
    // We need to accept both identity and gzip, as otherwise cloudfront will
    // always respond to requests with strong etag's
    headers.insert(
        header::ACCEPT_ENCODING,
        header::HeaderValue::from_static("gzip,identity"),
    );
    let client = crate::HttpClient::builder()
        .default_headers(headers)
        .http2_prior_knowledge()
        .build()?;

    let url = index.as_str();
    let base = url
        .strip_prefix("sparse+")
        .context("index is not a sparse registry")?;

    let krates: Vec<_> = krates.collect();

    // Note we don't treat any failures here as fatal, cargo can fix the index
    // entries if they are invalid/missing
    let (config, cache) = rayon::join(
        || -> anyhow::Result<()> {
            // cargo expects this file, we don't care about it though
            let res = client.get(format!("{base}config.json")).send()?;
            let data = res.bytes()?;
            std::fs::write(dir.join("config.json"), data)?;
            Ok(())
        },
        || -> anyhow::Result<()> {
            let cache_path = dir.join(".cache");
            std::fs::create_dir_all(&cache_path)?;
            krates.into_par_iter().for_each(|krate| {
                let write = || -> anyhow::Result<()> {
                    let lkrate = krate.to_lowercase();
                    let mut rel_path = crate::cargo::get_crate_prefix(&lkrate);
                    rel_path.push('/');
                    rel_path.push_str(&lkrate);

                    let res = client.get(format!("{base}{rel_path}")).send()?;

                    // Get the index version, this allows cargo to know the last
                    // update to the crate, and means it can send it along in
                    // any future requests and skip fetching the index entry again
                    // if it is up to date
                    let index_version = || -> Option<String> {
                        let hdrs = res.headers();
                        // Prefer etag, same as cargo
                        let (key, value) = if let Some(etag) = hdrs.get(header::ETAG) {
                            (header::ETAG, etag)
                        } else if let Some(lm) = hdrs.get(header::LAST_MODIFIED) {
                            (header::LAST_MODIFIED, lm)
                        } else {
                            return None;
                        };

                        let value = value.to_str().ok()?;

                        Some(format!("{key}: {value}"))
                    };

                    let index_version = index_version().unwrap_or_else(|| "Unknown".to_owned());
                    let body = res.bytes()?;

                    let mut entry = Vec::new();
                    let num_versions =
                        super::write_summary(index_version.as_bytes(), &body, &mut entry);
                    tracing::debug!(krate, "wrote entries for {num_versions} versions");

                    let entry_path = cache_path.join(rel_path);
                    std::fs::create_dir_all(entry_path.parent().unwrap()).with_context(|| {
                        format!(
                            "failed to create directory '{}'",
                            entry_path.parent().unwrap()
                        )
                    })?;
                    std::fs::write(&entry_path, entry)
                        .with_context(|| format!("failed to write '{entry_path}'"))?;

                    Ok(())
                };

                if let Err(err) = write() {
                    tracing::error!(err = ?err, krate, "failed to write sparse cache entry");
                }
            });

            Ok(())
        },
    );

    if let Err(err) = config {
        tracing::error!(err = ?err, "failed to write config.json");
    }

    if let Err(err) = cache {
        tracing::error!(err = ?err, "failed to write cache entries");
    }

    Ok(())
}
