use anyhow::Error;
use cf::{mirror, Ctx};
use log::{error, info};
use std::time::Duration;

#[derive(structopt::StructOpt)]
pub struct Args {
    #[structopt(
        short,
        long = "max-stale",
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

pub(crate) async fn cmd(ctx: Ctx, include_index: bool, args: Args) -> Result<(), Error> {
    let backend = ctx.backend.clone();

    let index = tokio::task::spawn(async move {
        if !include_index {
            return;
        }

        info!("mirroring crates.io index");
        match mirror::registry_index(backend, args.max_stale).await {
            Ok(_) => info!("successfully mirrored crates.io index"),
            Err(e) => error!("failed to mirror crates.io index: {}", e),
        }
    });

    let (index, _mirror) = tokio::join!(index, async move {
        match mirror::locked_crates(&ctx).await {
            Ok(_) => {
                info!("finished uploading crates");
            }
            Err(e) => error!("failed to mirror crates: {}", e),
        }
    });

    index?;

    Ok(())
}

fn parse_duration(src: &str) -> Result<Duration, Error> {
    let suffix_pos = src.find(char::is_alphabetic).unwrap_or_else(|| src.len());

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
