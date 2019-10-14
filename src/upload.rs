use crate::{Ctx, Krate};
use anyhow::Error;

pub fn to_cloud(ctx: &Ctx<'_>, source: bytes::Bytes, krate: &Krate) -> Result<(), Error> {
    match &ctx.location {
        #[cfg(feature = "gcs")]
        crate::CloudLocation::Gcs(loc) => to_gcs(ctx, &loc.bucket, source, krate),
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
