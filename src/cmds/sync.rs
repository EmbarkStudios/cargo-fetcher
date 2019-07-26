use cf::{sync, Context};
use failure::Error;
use log::{error, info};
use std::path::PathBuf;

#[derive(structopt::StructOpt)]
pub struct Args {
    /// The root path for cargo. This defaults to either
    /// CARGO_HOME or HOME/.cargo.
    #[structopt(short, long = "cargo-root", parse(from_os_str))]
    cargo_root: Option<PathBuf>,
}

pub fn cmd(ctx: Context<'_>, include_index: bool, args: Args) -> Result<(), Error> {
    let root_dir = cf::util::determine_cargo_root(args.cargo_root)?;

    // Create the registry directory as it is the root of multiple other ones
    std::fs::create_dir_all(root_dir.join("registry"))?;

    rayon::join(
        || {
            if !include_index {
                return;
            }

            info!("syncing crates.io index");
            match sync::registry_index(&ctx, &root_dir) {
                Ok(_) => info!("successfully synced crates.io index"),
                Err(e) => error!("failed to sync crates.io index: {}", e),
            }
        },
        || match sync::locked_crates(&ctx, &root_dir) {
            Ok(_) => {
                info!("finished syncing crates");
            }
            Err(e) => error!("failed to sync crates: {}", e),
        },
    );

    Ok(())
}
