use crate::{fetch, Ctx, Krate, Registry, Source};
use anyhow::Error;
use std::time::Duration;
use tracing::{debug, error, info};

pub struct RegistrySet {
    pub registry: std::sync::Arc<Registry>,
    pub krates: Vec<String>,
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn registry_indices(
    ctx: &crate::Ctx,
    max_stale: Duration,
    registries: Vec<RegistrySet>,
) -> usize {
    #[allow(unsafe_code)]
    // SAFETY: we don't forget the future :p
    unsafe {
        async_scoped::TokioScope::scope_and_collect(|s| {
            for rset in registries {
                s.spawn(async {
                    match registry_index(ctx, max_stale, rset).await {
                        Ok(size) => size,
                        Err(err) => {
                            error!("{err:#}");
                            0
                        }
                    }
                });
            }
        })
        .await
        .1
        .into_iter()
        .map(|res| res.unwrap())
        .sum()
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn registry_index(
    ctx: &crate::Ctx,
    max_stale: Duration,
    rset: RegistrySet,
) -> Result<usize, Error> {
    let ident = rset.registry.short_name().to_owned();

    // Create a fake krate for the index, we don't have to worry about clashing
    // since we use a `.` which is not an allowed character in crate names
    let krate = Krate {
        name: ident.clone(),
        version: "2.0.0".to_owned(),
        source: Source::Git(crate::cargo::GitSource {
            url: rset.registry.index.clone(),
            ident,
            rev: crate::cargo::GitRev::parse("feedc0de00000000000000000000000000000000").unwrap(),
            follow: None,
        }),
    };

    // Retrieve the metadata for the last updated registry entry, and update
    // only it if it's stale
    if let Ok(Some(last_updated)) = ctx.backend.updated(krate.cloud_id(false)).await {
        let now = time::OffsetDateTime::now_utc();

        if now - last_updated < max_stale {
            info!(
                "the registry ({}) was last updated {last_updated}, skipping update as it is less than {max_stale:?} old",
                rset.registry.index
            );
            return Ok(0);
        }
    }

    let index = fetch::registry(
        &ctx.client,
        &rset.registry,
        rset.krates.into_iter().collect(),
    )
    .await?;

    debug!(
        size = index.len(),
        "{} index downloaded", rset.registry.index
    );

    let span = tracing::debug_span!("upload");
    let _us = span.enter();
    ctx.backend.upload(index, krate.cloud_id(false)).await
}

pub async fn crates(ctx: &Ctx) -> Result<usize, Error> {
    debug!("checking existing crates...");
    let mut names = ctx.backend.list().await?;

    names.sort();

    let mut to_mirror = Vec::with_capacity(names.len());
    for krate in &ctx.krates {
        let cid = krate.cloud_id(false).to_string();
        if names
            .binary_search_by(|name| name.as_str().cmp(&cid))
            .is_err()
        {
            to_mirror.push(krate.clone());
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

    #[allow(unsafe_code)]
    // SAFETY: we don't forget the future :p
    let total_bytes = unsafe {
        async_scoped::TokioScope::scope_and_collect(|s| {
            for krate in to_mirror {
                s.spawn(async move {
                    let span = tracing::info_span!("mirror", %krate);
                    let _ms = span.enter();

                    let fetch_res = {
                        let span = tracing::debug_span!("fetch");
                        let _ms = span.enter();
                        fetch::from_registry(client, &krate).await
                    };

                    match fetch_res {
                        Ok(krate_data) => {
                            debug!(size = krate_data.len(), "fetched");

                            {
                                let span = tracing::debug_span!("upload");
                                let _us = span.enter();

                                match krate_data {
                                    fetch::KratePackage::Registry(buffer) => {
                                        match backend.upload(buffer, krate.cloud_id(false)).await {
                                            Ok(len) => len,
                                            Err(err) => {
                                                error!("failed to upload crate tarball: {err:#}");
                                                0
                                            }
                                        }
                                    }
                                    fetch::KratePackage::Git(gs) => {
                                        let db = gs.db;
                                        let co = krate.clone();
                                        let checkout = gs.checkout;
                                        let db_backend = backend.clone();

                                        let db_fut = tokio::task::spawn(async move {
                                            match db_backend.upload(db, krate.cloud_id(false)).await {
                                                Ok(l) => l,
                                                Err(err) => {
                                                    error!("failed to upload git db: {err:#}");
                                                    0
                                                }
                                            }
                                        });

                                        let co_backend = backend.clone();
                                        let co_fut = tokio::task::spawn(async move {
                                            if let Some(buffer) = checkout {
                                                match co_backend.upload(buffer, co.cloud_id(true)).await {
                                                    Ok(l) => l,
                                                    Err(err) => {
                                                        error!("failed to upload git checkout: {err:#}");
                                                        0
                                                    }
                                                }
                                            } else {
                                                0
                                            }
                                        });

                                        let (db, co) = tokio::join!(db_fut, co_fut);
                                        db.unwrap() + co.unwrap()
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            error!(krate = %krate, "failed to retrieve: {err:#}");
                            0
                        }
                    }
                });
            }
        })
        .await
        .1
        .into_iter()
        .map(|res| res.unwrap())
        .sum()
    };

    Ok(total_bytes)
}
