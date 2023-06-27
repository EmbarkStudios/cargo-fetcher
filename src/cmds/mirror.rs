use anyhow::Error;
use cf::{mirror, Ctx};
use tracing::{error, info};

#[derive(clap::Parser)]
pub struct Args {
    #[clap(
        short,
        default_value = "1d",
        long_help = "The duration for which the index will not be replaced after its most recent update.

Times may be specified with no suffix (default seconds), or one of:
* (s)econds
* (m)inutes
* (h)ours
* (d)ays

"
    )]
    max_stale: crate::Dur,
}

pub(crate) fn cmd(ctx: Ctx, include_index: bool, args: Args) -> Result<(), Error> {
    let regs = ctx.registry_sets();

    rayon::join(
        || {
            if !include_index {
                return;
            }

            mirror::registry_indices(&ctx, args.max_stale.0, regs);
            info!("finished uploading registry indices");
        },
        || match mirror::crates(&ctx) {
            Ok(_) => info!("finished uploading crates"),
            Err(e) => error!("failed to mirror crates: {:#}", e),
        },
    );

    Ok(())
}
