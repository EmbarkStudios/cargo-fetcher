use crate::Krate;
use failure::Error;
use reqwest::Client;
use std::convert::TryFrom;
use tame_gcs::{
    objects::{InsertObjectOptional, Object},
    BucketName, ObjectName,
};

pub fn to_gcs(
    client: &Client,
    source: bytes::Bytes,
    bucket: &BucketName<'_>,
    prefix: &str,
    krate: &Krate,
) -> Result<(), Error> {
    use bytes::{Buf, IntoBuf};

    let object_name = format!("{}{}", prefix, krate.gcs_id());
    let object_name = ObjectName::try_from(object_name.as_ref())?;

    let content_len = source.len() as u64;

    let insert_req = Object::insert_simple(
        &(bucket, &object_name),
        source,
        content_len,
        Some(InsertObjectOptional {
            content_type: Some("application/x-tar"),
            ..Default::default()
        }),
    )?;

    let (parts, body) = insert_req.into_parts();

    let uri = parts.uri.to_string();
    let builder = client.post(&uri);

    let request = builder
        .headers(parts.headers)
        .body(reqwest::Body::new(body.into_buf().reader()))
        .build()?;

    client.execute(request)?.error_for_status()?;
    Ok(())
}
