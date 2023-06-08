use anyhow::Context as _;
use rayon::prelude::{IntoParallelIterator, ParallelIterator};

pub(super) fn write_cache_entries(
    dir: &crate::Path,
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
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("text/plain"),
    );
    let client = crate::HttpClient::builder()
        .default_headers(headers)
        .http2_prior_knowledge()
        .build()?;

    let url = index.as_str();
    let base = url
        .strip_prefix("sparse+")
        .context("index is not a sparse registry")?;

    // Note we don't treat any failures here as fatal, cargo can fix the index
    // entries if they are invalid/missing
    rayon::join(
        || -> anyhow::Result<()> {
            // cargo expects this file, we don't care about it though
            let res = client.get(format!("{base}config.json")).send()?;
            let data = res.bytes()?;
            std::fs::write(dir.join("config.json"), data)?;
            Ok(())
        },
        || -> anyhow::Result<()> {
            let cache_path = dir.join(".cache");
            std::fs::create_dir(&cache_path)?;
            let krates: Vec<_> = krates.collect();
            krates.into_par_iter().for_each(|krate| {
                let write = || -> anyhow::Result<()> {
                    let lkrate = krate.to_lowercase();
                    let mut rel_path = crate::cargo::get_crate_prefix(&lkrate);
                    rel_path.push('/');
                    rel_path.push_str(&lkrate);
    
                    let res = client.get(format!("{base}{rel_path}")).send()?;
                    
                };

                if let Err(err) = write() {
                    tracing::error!(err = ?err, krate = krate, "failed to write sparse cache entry");
                }
            });

            Ok(())
        },
    );

    Ok(())
}
