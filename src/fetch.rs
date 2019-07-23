use crate::Krate;
use bytes::Bytes;
use failure::Error;
use reqwest::Client;
use std::convert::TryFrom;
use tame_gcs::{objects::Object, BucketName, ObjectName};

// We just treat versions as opaque strings
pub fn from_crates_io(client: &Client, krate: &Krate) -> Result<Bytes, Error> {
    let url = format!(
        "https://static.crates.io/crates/{}/{}-{}.crate",
        krate.name, krate.name, krate.version
    );

    let mut response = client.get(&url).send()?.error_for_status()?;
    let res = crate::convert_response(&mut response)?;
    Ok(res.into_body())
}

pub fn from_gcs(
    client: &Client,
    krate: &Krate,
    bucket: &BucketName<'_>,
    prefix: &str,
) -> Result<Bytes, Error> {
    let object_name = format!("{}{}", prefix, krate.checksum);
    let object_name = ObjectName::try_from(object_name.as_ref())?;

    let dl_req = Object::download(&(bucket, &object_name), None)?;

    let (parts, _) = dl_req.into_parts();

    let uri = parts.uri.to_string();
    let builder = client.get(&uri);

    let request = builder.headers(parts.headers).build()?;

    let mut response = client.execute(request)?.error_for_status()?;
    let res = crate::convert_response(&mut response)?;
    Ok(res.into_body())
}
