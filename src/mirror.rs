use failure::Error;
use log::{error, info};
use std::convert::TryFrom;
use tame_gcs::objects::{ListOptional, ListResponse, Object};

#[derive(structopt::StructOpt)]
pub struct Args {}

pub fn cmd(ctx: crate::Context<'_>, _args: Args) -> Result<(), Error> {
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

    to_mirror.par_iter().for_each(|krate| {
        match cargo_fetcher::fetch::from_crates_io(&ctx.client, krate) {
            Err(e) => error!("failed to retrieve {}: {}", krate, e),
            Ok(buffer) => {
                if let Err(e) = cargo_fetcher::upload::to_gcs(
                    &ctx.client,
                    buffer,
                    &ctx.gcs_bucket,
                    ctx.prefix,
                    krate,
                ) {
                    error!(
                        "failed to upload {} to GCS: {}",
                        krate, e
                    );
                }
            }
        }
    });

    info!("finished uploading crates");
    Ok(())
}
