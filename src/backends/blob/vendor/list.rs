use anyhow::{anyhow, Context, Error};
use serde::{Deserialize, Serialize};

impl super::Blob {
    pub fn list(&self, timefmt: &str) -> Result<http::Request<std::io::Empty>, Error> {
        let action = super::Actions::List;
        let now = timefmt;

        let mut req_builder = http::Request::builder();
        let mut uri = self.container_uri();
        uri.push_str("?restype=container&comp=list");
        let uri: http::Uri = uri.parse()?;

        let sign = self.sign(&action, uri.path(), timefmt, 0);
        let formatedkey = format!(
            "SharedKey {}:{}",
            &self.account,
            sign?,
            // self.sign(&action, Uri::from_str(&uri)?.path(), timefmt, 0)?
        );
        let hm = req_builder.headers_mut().context("context")?;
        hm.insert("Authorization", formatedkey.parse()?);
        hm.insert("x-ms-date", now.parse()?);
        hm.insert("x-ms-version", self.version_value.parse()?);
        hm.insert(
            "x-ms-blob-type",
            http::HeaderValue::from_static("BlockBlob"),
        );
        let request = req_builder
            .method(http::Method::from(&action))
            .uri(uri)
            .body(std::io::empty())?;
        Ok(request)
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct EnumerationResults {
    #[serde(rename = "Blobs")]
    pub blobs: Blobs,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Blobs {
    #[serde(rename = "Blob")]
    pub blob: Vec<Blob>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Blob {
    // #[serde(rename(serialize = "Name", deserialize = "Name"))]
    #[serde(rename = "Name")]
    pub name: String,
    // #[serde(rename(serialize = "Properties", deserialize = "Properties"))]
    #[serde(rename = "Properties")]
    pub properties: Properties,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Properties {
    // #[serde(rename(serialize = "Last-Modified", deserialize = "Last-Modified"))]
    #[serde(rename = "Last-Modified")]
    pub last_modified: String,
    // #[serde(rename(serialize = "Content-Length", deserialize = "Content-Length"))]
    #[serde(rename = "Content-Length")]
    pub content_length: usize,
    // #[serde(rename(serialize = "Content-MD5", deserialize = "Content-MD5"))]
    #[serde(rename = "Content-MD5")]
    pub content_md5: String,
}

pub fn parse_list_body(s: &str) -> Result<EnumerationResults, Error> {
    match quick_xml::de::from_str(s) {
        Ok(d) => Ok(d),
        Err(e) => Err(anyhow!("failed to parse list action body. {}", e)),
    }
}
