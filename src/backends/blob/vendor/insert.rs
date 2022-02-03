use std::str::FromStr;

use anyhow::{Context, Error};
use http::HeaderValue;
use http::Uri;

impl super::Blob {
    pub fn insert(
        &self,
        file_name: &str,
        source: bytes::Bytes,
        timefmt: &str,
    ) -> Result<http::Request<bytes::Bytes>, Error> {
        let action = super::Actions::Insert;
        let now = timefmt;

        let mut uri = self.container_uri();
        uri.push('/');
        uri.push_str(file_name);
        let sign = self.sign(&action, Uri::from_str(&uri)?.path(), timefmt, source.len());
        let formatedkey = format!("SharedKey {}:{}", self.account, sign?);
        let mut req_builder = http::Request::builder();
        let hm = req_builder.headers_mut().context("context")?;
        hm.insert("Authorization", HeaderValue::from_str(&formatedkey)?);
        hm.insert("x-ms-date", HeaderValue::from_str(now)?);
        hm.insert("x-ms-version", HeaderValue::from_str(&self.version_value)?);
        hm.insert("x-ms-blob-type", HeaderValue::from_str("BlockBlob")?);
        let request = req_builder
            .method(http::Method::from(&action))
            .uri(uri)
            .body(source)?;
        Ok(request)
    }
}
