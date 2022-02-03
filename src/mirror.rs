use crate::{fetch, Ctx, Krate, Registry, Source};
use anyhow::Error;
use futures::StreamExt;
use std::time::Duration;
use tracing::{debug, error, info};
use tracing_futures::Instrument;

pub struct RegistrySet {
    pub registry: std::sync::Arc<Registry>,
    pub krates: Vec<String>,
}

pub async fn registry_indices(
    backend: crate::Storage,
    max_stale: Duration,
    registries: Vec<RegistrySet>,
) -> Result<usize, Error> {
    let bytes = futures::stream::iter(registries)
        .map(|rset| {
            let backend = backend.clone();
            let index = rset.registry.index.clone();
            async move {
                let res: Result<usize, Error> = registry_index(backend, max_stale, rset)
                    .instrument(tracing::debug_span!("upload registry"))
                    .await;
                res
            }
            .instrument(tracing::debug_span!("mirror registries", %index))
        })
        .buffer_unordered(32);

    let total_bytes = bytes
        .fold(0usize, |acc, res| async move {
            match res {
                Ok(a) => a + acc,
                Err(e) => {
                    error!("{:#}", e);
                    acc
                }
            }
        })
        .await;

    Ok(total_bytes)
}

pub async fn registry_index(
    backend: crate::Storage,
    max_stale: Duration,
    rset: RegistrySet,
) -> Result<usize, Error> {
    let ident = rset.registry.short_name();

    // Create a fake krate for the index, we don't have to worry about clashing
    // since we use a `.` which is not an allowed character in crate names
    let krate = Krate {
        name: ident.clone(),
        version: "1.0.0".to_owned(),
        source: Source::Git {
            url: rset.registry.index.clone(),
            ident,
            rev: "feedc0de".to_owned(),
        },
    };

    // Retrieve the metadata for the last updated registry entry, and update
    // only it if it's stale
    if let Ok(Some(last_updated)) = backend.updated(&krate).await {
        let now = time::OffsetDateTime::now_utc();

        if now - last_updated < max_stale {
            info!(
                    "the registry ({}) was last updated {}, skipping update as it is less than {:?} old",
                    rset.registry.index, last_updated, max_stale
                );
            return Ok(0);
        }
    }

    let index = async {
        let res = fetch::registry(&rset.registry.index, rset.krates.into_iter()).await;

        if let Ok(ref buffer) = res {
            debug!(
                size = buffer.len(),
                "{} index downloaded", rset.registry.index
            );
        }

        res
    }
    .instrument(tracing::debug_span!("fetch_index"))
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

    let bodies = futures::stream::iter(to_mirror)
        .map(|krate| {
            let client = &client;
            let backend = backend.clone();
            async move {
                let res: Result<usize, String> = match fetch::from_registry(client, krate).await {
                    Err(e) => Err(format!("failed to retrieve {}: {}", krate, e)),
                    Ok(krate_data) => {
                        debug!(size = krate_data.len(), "fetched");

                        let mut checkout_size = None;

                        let buffer = match krate_data {
                            fetch::KrateSource::Registry(buffer) => buffer,
                            fetch::KrateSource::Git(gs) => {
                                if let Some(checkout) = gs.checkout {
                                    // We synthesize a slightly different krate id so that we can
                                    // store both (and also not have to change every backend)
                                    let mut checkout_id = krate.clone();

                                    if let Source::Git { rev, .. } = &mut checkout_id.source {
                                        rev.push_str("-checkout");
                                    }

                                    match backend
                                        .upload(checkout, &checkout_id)
                                        .instrument(tracing::debug_span!("upload"))
                                        .await
                                    {
                                        Err(e) => {
                                            tracing::warn!(
                                                "failed to upload  {}: {}",
                                                checkout_id,
                                                e
                                            );
                                        }
                                        Ok(len) => {
                                            checkout_size = Some(len);
                                        }
                                    }
                                }

                                gs.db
                            }
                        };

                        match backend
                            .upload(buffer, krate)
                            .instrument(tracing::debug_span!("upload"))
                            .await
                        {
                            Err(e) => Err(format!("failed to upload {}: {}", krate, e)),
                            Ok(len) => Ok(len + checkout_size.unwrap_or(0)),
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
