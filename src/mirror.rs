use cargo_fetcher::{fetch, upload, util, Krate};
use failure::Error;
use log::{error, info};
use std::{convert::TryFrom, time::Duration};
use tame_gcs::objects::{self, ListOptional, ListResponse, Object};

fn parse_duration(src: &str) -> Result<Duration, Error> {
    let suffix_pos = src.find(char::is_alphabetic).unwrap_or_else(|| src.len());

    let num: u64 = src[..suffix_pos].parse()?;
    let suffix = if suffix_pos == src.len() {
        "h"
    } else {
        &src[suffix_pos..]
    };

    let duration = match suffix {
        "s" | "S" => Duration::from_secs(num),
        "m" | "M" => Duration::from_secs(num * 60),
        "h" | "H" => Duration::from_secs(num * 60 * 60),
        "d" | "D" => Duration::from_secs(num * 60 * 60 * 24),
        s => return Err(failure::format_err!("unknown duration suffix '{}'", s)),
    };

    Ok(duration)
}

#[derive(structopt::StructOpt)]
pub struct Args {
    #[structopt(
        short,
        long = "max-stale",
        default_value = "1d",
        parse(try_from_str = "parse_duration"),
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

fn get_updated(
    ctx: &crate::Context<'_>,
    krate: &Krate,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error> {
    let obj_name = format!("{}{}", ctx.prefix, krate.gcs_id());
    let index_obj_name = tame_gcs::ObjectName::try_from(obj_name)?;

    let get_req = Object::get(
        &(&ctx.gcs_bucket, &index_obj_name),
        Some(objects::GetObjectOptional {
            standard_params: tame_gcs::common::StandardQueryParameters {
                fields: Some("updated"),
                ..Default::default()
            },
            ..Default::default()
        }),
    )?;

    let (parts, _) = get_req.into_parts();

    let uri = parts.uri.to_string();
    let builder = ctx.client.get(&uri);

    let request = builder.headers(parts.headers).build()?;

    let mut response = ctx.client.execute(request)?.error_for_status()?;

    let response = cargo_fetcher::convert_response(&mut response)?;
    let get_response = objects::GetObjectResponse::try_from(response)?;

    Ok(get_response.metadata.updated)
}

fn mirror_registry_index(ctx: &crate::Context<'_>, max_stale: Duration) -> Result<(), Error> {
    let url = url::Url::parse("git+https://github.com/rust-lang/crates.io-index.git")?;
    let canonicalized = util::canonicalize_url(&url)?;
    let ident = util::ident(&canonicalized);

    let krate = Krate {
        name: "crates.io-index".to_owned(),
        version: "1.0.0".to_owned(),
        source: cargo_fetcher::Source::Git {
            url: canonicalized.clone(),
            ident,
        },
    };

    // Retrieve the metadata for the last updated registry entry, and update
    // only it if it's stale
    if let Ok(last_updated) = get_updated(ctx, &krate) {
        if let Some(last_updated) = last_updated {
            let now = chrono::Utc::now();
            let max_dur = chrono::Duration::from_std(max_stale.clone())?;

            if now - last_updated < max_dur {
                info!(
                    "crates.io-index was last updated {}, skipping update as it less than {:?} old",
                    last_updated, max_stale
                );
                return Ok(());
            }
        }
    }

    let index = fetch::registry(&canonicalized)?;

    upload::to_gcs(&ctx.client, index, &ctx.gcs_bucket, ctx.prefix, &krate)
}

fn mirror_locked_crates(ctx: &crate::Context<'_>) -> Result<(), Error> {
    info!("mirroring {} crates", ctx.krates.len());

    // Get a list of all crates already present in gcs, the list
    // operation can return a maximum of 1000 entries per request,
    // so we may have to send multiple requests to determine all
    // of the available crates
    let mut names = Vec::new();
    let mut page_token: Option<String> = None;

    info!("checking existing stored crates...");
    loop {
        let ls_req = Object::list(
            &ctx.gcs_bucket,
            Some(ListOptional {
                // We only care about a single directory
                delimiter: Some("/"),
                prefix: Some(ctx.prefix),
                page_token: page_token.as_ref().map(|s| s.as_ref()),
                ..Default::default()
            }),
        )?;

        let (parts, _) = ls_req.into_parts();

        let uri = parts.uri.to_string();
        let builder = ctx.client.get(&uri);

        let request = builder.headers(parts.headers).build()?;

        let mut res = ctx.client.execute(request)?;

        let response = cargo_fetcher::convert_response(&mut res)?;
        let list_response = ListResponse::try_from(response)?;

        let name_block: Vec<_> = list_response
            .objects
            .into_iter()
            .filter_map(|obj| obj.name)
            .collect();
        names.push(name_block);

        page_token = list_response.page_token;

        if page_token.is_none() {
            break;
        }
    }

    let mut names: Vec<_> = names.into_iter().flat_map(|v| v.into_iter()).collect();
    names.sort();

    let prefix_len = ctx.prefix.len();
    let mut to_mirror = Vec::with_capacity(names.len());
    for krate in ctx.krates {
        if names
            .binary_search_by(|name| name[prefix_len..].cmp(krate.gcs_id()))
            .is_err()
        {
            to_mirror.push(krate);
        }
    }

    if to_mirror.is_empty() {
        info!("all crates already uploaded");
        return Ok(());
    }

    info!("uploading {} crates...", to_mirror.len());

    use rayon::prelude::*;

    to_mirror
        .par_iter()
        .for_each(|krate| match fetch::from_crates_io(&ctx.client, krate) {
            Err(e) => error!("failed to retrieve {}: {}", krate, e),
            Ok(buffer) => {
                if let Err(e) =
                    upload::to_gcs(&ctx.client, buffer, &ctx.gcs_bucket, ctx.prefix, krate)
                {
                    error!("failed to upload {} to GCS: {}", krate, e);
                }
            }
        });

    Ok(())
}

pub fn cmd(ctx: crate::Context<'_>, args: Args) -> Result<(), Error> {
    rayon::join(
        || {
            if !ctx.include_index {
                return;
            }

            info!("mirroring crates.io index");
            match mirror_registry_index(&ctx, args.max_stale) {
                Ok(_) => info!("successfully mirrored crates.io index"),
                Err(e) => error!("failed to mirror crates.io index: {}", e),
            }
        },
        || match mirror_locked_crates(&ctx) {
            Ok(_) => {
                info!("finished uploading crates");
            }
            Err(e) => error!("failed to mirror crates: {}", e),
        },
    );

    Ok(())
}
