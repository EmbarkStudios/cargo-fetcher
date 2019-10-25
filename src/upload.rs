use crate::{Ctx, Krate};
use anyhow::Error;

pub fn to_cloud(ctx: &Ctx<'_>, source: bytes::Bytes, krate: &Krate) -> Result<(), Error> {
    match &ctx.location {
        #[cfg(feature = "gcs")]
        crate::CloudLocation::Gcs(loc) => to_gcs(ctx, &loc.bucket, source, krate),
        #[cfg(feature = "s3")]
        crate::CloudLocation::S3(loc) => {
            to_s3(ctx, &loc.host, &loc.region, &loc.bucket, source, krate)
        }
    }
}

#[cfg(feature = "gcs")]
fn to_gcs(
    ctx: &Ctx<'_>,
    bucket: &tame_gcs::BucketName<'_>,
    source: bytes::Bytes,
    krate: &Krate,
) -> Result<(), Error> {
    use bytes::{Buf, IntoBuf};
    use std::convert::TryFrom;
    use tame_gcs::objects::{InsertObjectOptional, Object};

    let content_len = source.len() as u64;

    let insert_req = Object::insert_simple(
        &(
            bucket,
            &tame_gcs::ObjectName::try_from(ctx.location.path(&krate))?,
        ),
        source,
        content_len,
        Some(InsertObjectOptional {
            content_type: Some("application/x-tar"),
            ..Default::default()
        }),
    )?;

    let (parts, body) = insert_req.into_parts();

    let uri = parts.uri.to_string();
    let builder = ctx.client.post(&uri);

    let request = builder
        .headers(parts.headers)
        .body(reqwest::Body::new(body.into_buf().reader()))
        .build()?;

    ctx.client.execute(request)?.error_for_status()?;
    Ok(())
}

#[cfg(feature = "s3")]
pub fn to_s3(
    ctx: &Ctx<'_>,
    host: &str,
    region: &str,
    bucket: &str,
    source: bytes::Bytes,
    krate: &Krate,
) -> Result<(), Error> {
    let region = rusoto_core::Region::Custom {
        name: region.to_owned(),
        endpoint: host.to_owned(),
    };

    let s3_client = rusoto_s3::S3Client::new(region);

    let object_name = ctx.location.path(krate);

    let put_request = rusoto_s3::PutObjectRequest {
        bucket: bucket.to_owned(),
        key: object_name.to_owned(),
        body: Some(source.to_vec().into()),
        ..Default::default()
    };

    use rusoto_s3::S3;
    s3_client
        .put_object(put_request)
        .sync()
        .expect("Failed to put object");
    Ok(())
}
