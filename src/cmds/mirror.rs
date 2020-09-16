use anyhow::Error;
use cf::{mirror, Ctx};
use std::time::Duration;
use tracing::{error, info};
use tracing_futures::Instrument;

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

    let local = tokio::task::LocalSet::new();
    let regs = ctx.registries.to_vec();
    let index = local.run_until(async move {
        if !include_index {
            return;
        }

        if let Err(e) = tokio::task::spawn_local(async move {
            match mirror::registries_index(backend, args.max_stale, regs)
                .instrument(tracing::info_span!("index"))
                .await
            {
                Ok(_) => {
                    info!("successfully mirrored all registries index");
                }
                Err(e) => error!("failed to mirror registries index: {:#}", e),
            }
        })
        .await
        {
            error!("failed to spawn index mirror task: {:#}", e);
        }
    });

    let (_index, _mirror) = tokio::join!(index, async move {
        match mirror::crates(&ctx).await {
            Ok(_) => {
                info!("finished uploading crates");
            }
            Err(e) => error!("failed to mirror crates: {:#}", e),
        }
    });

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
