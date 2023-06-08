use crate::{fetch, Ctx, Krate, Registry, Source};
use anyhow::Error;
use rayon::prelude::*;
use std::time::Duration;
use tracing::{debug, error, info};

pub struct RegistrySet {
    pub registry: std::sync::Arc<Registry>,
    pub krates: Vec<String>,
}

#[tracing::instrument(level = "debug", skip_all)]
pub fn registry_indices(
    backend: crate::Storage,
    max_stale: Duration,
    registries: Vec<RegistrySet>,
) -> usize {
    registries
        .into_par_iter()
        .map(
            |rset| match registry_index(backend.clone(), max_stale, rset) {
                Ok(size) => size,
                Err(err) => {
                    error!("{err:#}");
                    0
                }
            },
        )
        .sum()
}

#[tracing::instrument(level = "debug", skip_all)]
pub fn registry_index(
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
    if let Ok(Some(last_updated)) = backend.updated(&krate) {
        let now = time::OffsetDateTime::now_utc();

        if now - last_updated < max_stale {
            info!(
                    "the registry ({}) was last updated {last_updated}, skipping update as it is less than {max_stale:?} old",
                    rset.registry.index
                );
            return Ok(0);
        }
    }

    let index = fetch::registry(&rset.registry, rset.krates.into_iter())?;

    debug!(
        size = index.len(),
        "{} index downloaded", rset.registry.index
    );

    let span = tracing::debug_span!("upload");
    let _us = span.enter();
    backend.upload(index, &krate)
}

pub fn crates(ctx: &Ctx) -> Result<usize, Error> {
    debug!("checking existing crates...");
    let mut names = ctx.backend.list()?;

    names.sort();

    let mut to_mirror = Vec::with_capacity(names.len());
    for krate in &ctx.krates {
        let cid = krate.cloud_id().to_string();
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

    let total_bytes = to_mirror
        .into_par_iter()
        .map(|krate| {
            let span = tracing::debug_span!("mirror", %krate);
            let _ms = span.enter();

            match fetch::from_registry(client, krate) {
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

                                {
                                    let span = tracing::debug_span!("upload_checkout");
                                    let _us = span.enter();

                                    match backend.upload(checkout, &checkout_id) {
                                        Err(e) => {
                                            tracing::warn!("failed to upload  {checkout_id}: {e}");
                                        }
                                        Ok(len) => {
                                            checkout_size = Some(len);
                                        }
                                    }
                                }
                            }

                            gs.db
                        }
                    };

                    {
                        let span = tracing::debug_span!("upload");
                        let _us = span.enter();
                        match backend.upload(buffer, krate) {
                            Err(e) => {
                                error!("failed to upload: {e}");
                                0
                            }
                            Ok(len) => len + checkout_size.unwrap_or(0),
                        }
                    }
                }
                Err(e) => {
                    error!("failed to retrieve: {}", e);
                    0
                }
            }
        })
        .sum();

    Ok(total_bytes)
}
