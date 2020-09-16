use anyhow::Error;
use cf::{sync, Ctx};
use std::path::PathBuf;
use tracing::{error, info};

#[derive(structopt::StructOpt)]
pub struct Args {
    /// The root path for cargo. This defaults to either
    /// CARGO_HOME or HOME/.cargo.
    #[structopt(short, long = "cargo-root", parse(from_os_str))]
    pub cargo_root: Option<PathBuf>,
}

pub(crate) async fn cmd(ctx: Ctx, include_index: bool, _args: Args) -> Result<(), Error> {
    ctx.prep_sync_dirs()?;

    let root = ctx.root_dir.clone();
    let backend = ctx.backend.clone();
    let registries = ctx.registries.clone();

    let index = tokio::task::spawn(async move {
        if !include_index {
            return;
        }

        info!("syncing registries index");
        match sync::registries_index(root, backend, registries).await {
            Ok(_) => info!("successfully synced registries index"),
            Err(e) => error!(err = ?e, "failed to sync registries index"),
        }
    });

    let (index, _sync) = tokio::join!(index, async move {
        match sync::crates(&ctx).await {
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
    });

    index?;

    Ok(())
}
