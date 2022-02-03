use anyhow::{Context, Error};
use http::HeaderValue;
use http::Uri;
use std::str::FromStr;

impl<B> TryFrom<http::Response<B>> for super::PropertiesResponse {
    type Error = Error;
    fn try_from(response: http::Response<B>) -> Result<Self, Error> {
        Ok(Self {
            last_modified: response
                .headers()
                .get("Last-Modified")
                .context("failed to read Last-Modified in headers")?
                .to_str()?
                .to_owned(),
        })
    }
}

impl super::Blob {
    pub fn properties(
        &self,
        file_name: &str,
        timefmt: &str,
    ) -> Result<http::Request<std::io::Empty>, Error> {
        let action = super::Actions::Properties;
        let now = timefmt;

        let mut req_builder = http::Request::builder();
        let mut uri = self.container_uri();
        uri.push('/');
        uri.push_str(file_name);
        let sign = self.sign(
            &super::Actions::Properties,
            Uri::from_str(&uri)?.path(),
            timefmt,
            0,
        );
        let formatedkey = format!(
            "SharedKey {}:{}",
            &self.account,
            sign?,
            // self.sign(&super::Actions::Properties, file_name, timefmt, 0)?
        );
        let hm = req_builder.headers_mut().context("context")?;
        hm.insert("Authorization", HeaderValue::from_str(&formatedkey)?);
        hm.insert("x-ms-date", HeaderValue::from_str(now)?);
        hm.insert("x-ms-version", HeaderValue::from_str(&self.version_value)?);
        let request = req_builder
            .method(http::Method::from(&action))
            .uri(uri)
            .body(std::io::empty())?;
        Ok(request)
    }
}
