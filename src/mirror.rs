use crate::{fetch, util, Ctx, Krate, Source};
use anyhow::Error;
use std::{convert::TryFrom, time::Duration};
use tracing::{debug, error, info};
use tracing_futures::Instrument;

pub const CRATES_IO_URL: &str = "https://github.com/rust-lang/crates.io-index";

pub async fn registry_index(
    backend: crate::Storage,
    max_stale: Duration,
    registry_url: Option<util::Canonicalized>,
) -> Result<usize, Error> {
    let canonicalized = match registry_url {
        Some(u) => u,
        None => {
            let u = url::Url::parse(CRATES_IO_URL)?;
            util::Canonicalized::try_from(&u)?
        }
    };
    let ident = canonicalized.ident();

    let url: url::Url = canonicalized.into();
    let path = Path::new(url.path());
    let name = if path.ends_with(".git") {
        path.file_stem().context("failed to get registry name")?
    } else {
        path.file_name().context("failed to get registry name")?
    };
    // Create a fake krate for the index, we don't have to worry about clashing
    // since we use a `.` which is not an allowed character in crate names
    let krate = Krate {
        name: String::from(
            name.to_str()
                .context("failed conversion from OsStr to String")?,
        ),
        version: "1.0.0".to_owned(),
        source: Source::Git {
            url: canonicalized.as_ref().clone().into(),
            ident,
            rev: String::new(),
        },
    };

    // Retrieve the metadata for the last updated registry entry, and update
    // only it if it's stale
    if let Ok(last_updated) = backend.updated(&krate).await {
        if let Some(last_updated) = last_updated {
            let now = chrono::Utc::now();
            let max_dur = chrono::Duration::from_std(max_stale)?;

            if now - last_updated < max_dur {
                info!(
                    "crates.io-index was last updated {}, skipping update as it less than {:?} old",
                    last_updated, max_stale
                );
                return Ok(0);
            }
        }
    }

    let index = async {
        let res = fetch::registry(canonicalized.as_ref()).await;

        if let Ok(ref buffer) = res {
            debug!(size = buffer.len(), "crates.io index downloaded");
        }

        res
    }
    .instrument(tracing::debug_span!("fetch"))
    .await?;

    backend
        .upload(index, &krate)
        .instrument(tracing::debug_span!("upload"))
        .await
}

pub async fn crates(ctx: &Ctx) -> Result<usize, Error> {
    debug!("checking existing crates...");
    let mut names = ctx.backend.list().await?;

    names.sort();

    let mut to_mirror = Vec::with_capacity(names.len());
    for krate in &ctx.krates {
        let cid = format!("{}", krate.cloud_id());
        if names
            .binary_search_by(|name| name.as_str().cmp(&cid))
            .is_err()
        {
            to_mirror.push(krate);
        }
    }

    // Remove duplicates, eg. when 2 crates are sourced from the same git repository
    to_mirror.sort();
    to_mirror.dedup();

    if to_mirror.is_empty() {
        info!("all crates already uploaded");
        return Ok(0);
    }

    info!(
        "mirroring {} of {} crates",
        to_mirror.len(),
        ctx.krates.len()
    );

    let client = &ctx.client;
    let backend = &ctx.backend;

    use futures::StreamExt;

    let bodies = futures::stream::iter(to_mirror)
        .map(|krate| {
            let client = &client;
            let backend = backend.clone();
            async move {
                let res: Result<usize, String> = match fetch::from_crates_io(&client, &krate).await
                {
                    Err(e) => Err(format!("failed to retrieve {}: {}", krate, e)),
                    Ok(buffer) => {
                        debug!(size = buffer.len(), "fetched");
                        match backend
                            .upload(buffer, &krate)
                            .instrument(tracing::debug_span!("upload"))
                            .await
                        {
                            Err(e) => Err(format!("failed to upload {}: {}", krate, e)),
                            Ok(len) => Ok(len),
                        }
                    }
                };

                res
            }
            .instrument(tracing::debug_span!("mirror", %krate))
        })
        .buffer_unordered(32);

    let total_bytes = bodies
        .fold(0usize, |acc, res| async move {
            match res {
                Ok(len) => acc + len,
                Err(e) => {
                    error!("{:#}", e);
                    acc
                }
            }
        })
        .await;

    Ok(total_bytes)
}
