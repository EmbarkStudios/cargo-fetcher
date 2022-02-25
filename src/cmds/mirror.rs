use anyhow::Error;
use cf::{mirror, Ctx};
use std::time::Duration;
use tracing::{error, info};

#[derive(clap::Parser)]
pub struct Args {
    #[clap(
        short,
        default_value = "1d",
        parse(try_from_str = parse_duration),
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

fn parse_duration(src: &str) -> Result<Duration, Error> {
    let suffix_pos = src.find(char::is_alphabetic).unwrap_or(src.len());

    let num: u64 = src[..suffix_pos].parse()?;
    let suffix = if suffix_pos == src.len() {
        "d"
    } else {
        &src[suffix_pos..]
    };

    let duration = match suffix {
        "s" | "S" => Duration::from_secs(num),
        "m" | "M" => Duration::from_secs(num * 60),
        "h" | "H" => Duration::from_secs(num * 60 * 60),
        "d" | "D" => Duration::from_secs(num * 60 * 60 * 24),
        s => return Err(anyhow::anyhow!("unknown duration suffix '{}'", s)),
    };

    Ok(duration)
}
