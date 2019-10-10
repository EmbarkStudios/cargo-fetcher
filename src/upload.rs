use crate::{Ctx, Krate};
use anyhow::Error;
use tame_gcs::objects::{InsertObjectOptional, Object};

pub fn to_gcs(ctx: &Ctx<'_>, source: bytes::Bytes, krate: &Krate) -> Result<(), Error> {
    use bytes::{Buf, IntoBuf};

    let content_len = source.len() as u64;

    let insert_req = Object::insert_simple(
        &(&ctx.gcs_bucket, &ctx.object_name(&krate)?),
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
