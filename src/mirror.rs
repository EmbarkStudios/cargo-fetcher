use crate::{fetch, util, Ctx, Krate, Source};
use anyhow::Error;
use log::{error, info};
use std::{convert::TryFrom, time::Duration};

pub async fn registry_index(backend: crate::Storage, max_stale: Duration) -> Result<(), Error> {
    let url = url::Url::parse("git+https://github.com/rust-lang/crates.io-index.git")?;
    let canonicalized = util::Canonicalized::try_from(&url)?;
    let ident = canonicalized.ident();

    // Create a fake krate for the index, we don't have to worry about clashing
    // since we use a `.` which is not an allowed character in crate names
    let krate = Krate {
        name: "crates.io-index".to_owned(),
        version: "1.0.0".to_owned(),
        source: Source::Git {
            url: canonicalized.as_ref().clone(),
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
                return Ok(());
            }
        }
    }

    let index = fetch::registry(canonicalized.as_ref()).await?;

    backend.upload(index, &krate).await
}

pub async fn locked_crates(ctx: Ctx) -> Result<(), Error> {
    info!("mirroring {} crates", ctx.krates.len());

    info!("checking existing stored crates...");
    let mut names = ctx.backend.list().await?;

    names.sort();

    let mut to_mirror = Vec::with_capacity(names.len());
    for krate in ctx.krates {
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
        return Ok(());
    }

    info!("uploading {} crates...", to_mirror.len());

    let client = ctx.client;
    let backend = ctx.backend;

    let mut handles = Vec::with_capacity(to_mirror.len());

    for krate in to_mirror {
        let client = client.clone();
        let backend = backend.clone();

        handles.push(tokio::spawn(async move {
            match fetch::from_crates_io(&client, &krate).await {
                Err(e) => error!("failed to retrieve {}: {}", krate, e),
                Ok(buffer) => {
                    if let Err(e) = backend.upload(buffer, &krate).await {
                        error!("failed to upload {}: {}", krate, e);
                    }
                }
            }
        }));
    }

    futures::future::join_all(handles).await;

    Ok(())
}
