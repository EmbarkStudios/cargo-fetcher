use crate::{fetch, upload, util, Ctx, Krate, Source};
use anyhow::Error;
use log::{error, info};
use std::{convert::TryFrom, time::Duration};

#[cfg(feature = "gcs")]
fn get_updated_gcs(
    ctx: &Ctx<'_>,
    loc: &crate::GcsLocation<'_>,
    krate: &Krate,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error> {
    use tame_gcs::{
        objects::{self, Object},
        ObjectName,
    };

    let obj_name = ctx.location.path(krate);
    let index_obj_name = ObjectName::try_from(obj_name)?;

    let get_req = Object::get(
        &(&loc.bucket, &index_obj_name),
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

    let response = util::convert_response(&mut response)?;
    let get_response = objects::GetObjectResponse::try_from(response)?;

    Ok(get_response.metadata.updated)
}

#[cfg(feature = "s3")]
fn get_updated_s3(
    ctx: &Ctx<'_>,
    loc: &crate::S3Location<'_>,
    krate: &Krate,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error> {
    let region = rusoto_core::Region::Custom {
        name: loc.region.to_owned(),
        endpoint: loc.host.to_owned(),
    };

    let s3_client = rusoto_s3::S3Client::new(region);

    let obj_name = ctx.location.path(krate);

    let get_request = rusoto_s3::GetObjectRequest {
        bucket: loc.bucket.to_owned(),
        key: obj_name.to_owned(),
        ..Default::default()
    };

    use rusoto_s3::S3;
    let get_output = s3_client
        .get_object(get_request)
        .sync()
        .expect("Failed to get object");

    let last_modified =
        chrono::DateTime::parse_from_rfc3339(get_output.last_modified.unwrap().as_str())
            .expect("Error in date string")
            .with_timezone(&chrono::Utc);

    Ok(Some(last_modified))
}

fn get_updated(
    ctx: &Ctx<'_>,
    krate: &Krate,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error> {
    match &ctx.location {
        #[cfg(feature = "gcs")]
        crate::CloudLocation::Gcs(loc) => get_updated_gcs(ctx, loc, krate),
        #[cfg(feature = "s3")]
        crate::CloudLocation::S3(loc) => get_updated_s3(ctx, loc, krate),
    }
}

pub fn registry_index(ctx: &Ctx<'_>, max_stale: Duration) -> Result<(), Error> {
    let url = url::Url::parse("git+https://github.com/rust-lang/crates.io-index.git")?;
    let canonicalized = util::Canonicalized::try_from(&url)?;
    let ident = canonicalized.ident();

    // Create a fake krate for the index, we don't have to worry about clashing
    // since we use a `.` which is not an allowed character in crate names
    let krate = Krate {
        name: "crates.io-index".to_owned(),
        version: "1.0.0".to_owned(),
        source: Source::Git {
            url: canonicalized.as_ref().clone(),
            ident,
        },
    };

    // Retrieve the metadata for the last updated registry entry, and update
    // only it if it's stale
    if let Ok(last_updated) = get_updated(ctx, &krate) {
        if let Some(last_updated) = last_updated {
            let now = chrono::Utc::now();
            let max_dur = chrono::Duration::from_std(max_stale)?;

            if now - last_updated < max_dur {
                info!(
                    "crates.io-index was last updated {}, skipping update as it less than {:?} old",
                    last_updated, max_stale
                );
                return Ok(());
            }
        }
    }

    let index = fetch::registry(canonicalized.as_ref())?;

    upload::to_cloud(&ctx, index, &krate)
}

#[cfg(feature = "gcs")]
fn list_gcs_crates(ctx: &Ctx<'_>, loc: &crate::GcsLocation<'_>) -> Result<Vec<String>, Error> {
    use tame_gcs::objects::{ListOptional, ListResponse, Object};

    // Get a list of all crates already present in gcs, the list
    // operation can return a maximum of 1000 entries per request,
    // so we may have to send multiple requests to determine all
    // of the available crates
    let mut names = Vec::new();
    let mut page_token: Option<String> = None;

    loop {
        let ls_req = Object::list(
            &loc.bucket,
            Some(ListOptional {
                // We only care about a single directory
                delimiter: Some("/"),
                prefix: Some(loc.prefix),
                page_token: page_token.as_ref().map(|s| s.as_ref()),
                ..Default::default()
            }),
        )?;

        let (parts, _) = ls_req.into_parts();

        let uri = parts.uri.to_string();
        let builder = ctx.client.get(&uri);

        let request = builder.headers(parts.headers).build()?;

        let mut res = ctx.client.execute(request)?;

        let response = util::convert_response(&mut res)?;
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

    let len = loc.prefix.len();

    Ok(names
        .into_iter()
        .flat_map(|v| v.into_iter().map(|p| p[len..].to_owned()))
        .collect())
}

#[cfg(feature = "s3")]
fn list_s3_crates(ctx: &Ctx<'_>, loc: &crate::S3Location<'_>) -> Result<Vec<String>, Error> {
    let region = rusoto_core::Region::Custom {
        name: loc.region.to_owned(),
        endpoint: loc.host.to_owned(),
    };

    let s3_client = rusoto_s3::S3Client::new(region);

    let list_request = rusoto_s3::ListObjectsV2Request {
        bucket: loc.bucket.to_owned(),
        ..Default::default()
    };

    use rusoto_s3::S3;
    let list_objects_response = s3_client
        .list_objects_v2(list_request)
        .sync()
        .expect("Failed to list objects");

    let objects = list_objects_response.contents.unwrap();

    Ok(objects
        .into_iter()
        .map(|object| object.key.unwrap())
        .collect())
}

pub fn locked_crates(ctx: &Ctx<'_>) -> Result<(), Error> {
    info!("mirroring {} crates", ctx.krates.len());

    info!("checking existing stored crates...");
    let mut names: Vec<String> = match &ctx.location {
        #[cfg(feature = "gcs")]
        crate::CloudLocation::Gcs(loc) => list_gcs_crates(ctx, loc)?,
        #[cfg(feature = "s3")]
        crate::CloudLocation::S3(loc) => list_s3_crates(ctx, loc)?,
    };

    names.sort();

    let mut to_mirror = Vec::with_capacity(names.len());
    for krate in ctx.krates {
        if names
            .binary_search_by(|name| name.as_str().cmp(krate.cloud_id()))
            .is_err()
        {
            to_mirror.push(krate);
        }
    }

    // Remove duplicates, eg. when 2 crates are sourced from the same git repository
    to_mirror.sort();
    to_mirror.dedup();

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
                if let Err(e) = upload::to_cloud(&ctx, buffer, krate) {
                    error!("failed to upload {} to GCS: {}", krate, e);
                }
            }
        });

    Ok(())
}
