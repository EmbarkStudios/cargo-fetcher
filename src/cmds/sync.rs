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

    let index = tokio::task::spawn(async move {
        if !include_index {
            return;
        }

        info!("syncing crates.io index");
        match sync::registry_index(root, backend).await {
            Ok(_) => info!("successfully synced crates.io index"),
            Err(e) => error!("failed to sync crates.io index: {}", e),
        }
    });

    let (index, _sync) = tokio::join!(index, async move {
        match sync::locked_crates(&ctx).await {
            Ok(_) => {
                info!("finished syncing crates");
            }
            Err(e) => error!("failed to sync crates: {}", e),
        }
    });

    index?;

    Ok(())
}
