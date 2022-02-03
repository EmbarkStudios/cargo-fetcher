use anyhow::Error;
use cf::{sync, Ctx};
use tracing::{error, info};

#[derive(clap::Parser)]
pub struct Args {}

pub(crate) fn cmd(ctx: Ctx, include_index: bool, _args: Args) -> Result<(), Error> {
    ctx.prep_sync_dirs()?;

    let root = ctx.root_dir.clone();
    let backend = ctx.backend.clone();
    let registries = ctx.registries.clone();

    let (_index, sync) = rayon::join(
        || {
            if !include_index {
                return;
            }

            info!("syncing registries index");
            sync::registry_indices(root, backend, registries);
        },
        || sync::crates(&ctx),
    );

    match sync {
        Ok(summary) => {
            info!(
                bytes = summary.total_bytes,
                succeeded = summary.good,
                failed = summary.bad,
                "synced crates"
            );
        }
        Err(e) => error!(err = ?e, "failed to sync crates"),
    }

    Ok(())
}
