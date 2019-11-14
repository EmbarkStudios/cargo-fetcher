use anyhow::Error;
use cf::{sync, Ctx};
use log::{error, info};
use std::path::PathBuf;

#[derive(structopt::StructOpt)]
pub struct Args {
    /// The root path for cargo. This defaults to either
    /// CARGO_HOME or HOME/.cargo.
    #[structopt(short, long = "cargo-root", parse(from_os_str))]
    pub cargo_root: Option<PathBuf>,
}

pub fn cmd(ctx: Ctx, include_index: bool, _args: Args) -> Result<(), Error> {
    ctx.prep_sync_dirs()?;

    rayon::join(
        || {
            if !include_index {
                return;
            }

            info!("syncing crates.io index");
            match sync::registry_index(&ctx) {
                Ok(_) => info!("successfully synced crates.io index"),
                Err(e) => error!("failed to sync crates.io index: {}", e),
            }
        },
        || match sync::locked_crates(&ctx) {
            Ok(_) => {
                info!("finished syncing crates");
            }
            Err(e) => error!("failed to sync crates: {}", e),
        },
    );

    Ok(())
}
