use anyhow::Error;
use cf::{mirror, Ctx};
use std::time::Duration;
use tracing::{error, info};

#[derive(clap::Parser)]
pub struct Args {
    #[clap(
        short,
        default_value = "1d",
        parse(try_from_str = parse_duration_d),
        long_help = "The duration for which the index will not be replaced after its most recent update.

Times may be specified with no suffix (default days), or one of:
* (s)econds
* (m)inutes
* (h)ours
* (d)ays

"
    )]
    max_stale: Duration,
}

pub(crate) fn cmd(ctx: Ctx, include_index: bool, args: Args) -> Result<(), Error> {
    let backend = ctx.backend.clone();
    let regs = ctx.registry_sets();

    rayon::join(
        || {
            if !include_index {
                return;
            }

            mirror::registry_indices(backend, args.max_stale, regs);
            info!("finished uploading registry indices");
        },
        || match mirror::crates(&ctx) {
            Ok(_) => info!("finished uploading crates"),
            Err(e) => error!("failed to mirror crates: {:#}", e),
        },
    );

    Ok(())
}

#[inline]
fn parse_duration_d(src: &str) -> Result<Duration, Error> {
    crate::parse_duration(src, "d")
}
