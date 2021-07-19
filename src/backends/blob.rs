use crate::backends::gcs::convert_response;
use crate::Krate;
use anyhow::{Context, Error};
use bloblock::blob;
use bytes::Bytes;
use chrono::Utc;
use reqwest::Client;
use std::convert::TryFrom;

#[derive(Debug)]
pub struct BlobBackend {
    prefix: String,
    instance: blob::Blob,
    client: Client,
    container: String,
}

impl BlobBackend {
    pub async fn new(
        loc: crate::BlobLocation<'_>,
        account: String,
        master_key: String,
    ) -> Result<Self, Error> {
        let instance = blob::Blob::new(&account, &master_key, loc.container, false);
        let client = reqwest::Client::new();
        Ok(Self {
            prefix: loc.prefix.to_owned(),
            container: loc.container.to_owned(),
            instance,
            client,
        })
    }

    #[inline]
    fn make_key(&self, krate: &Krate) -> String {
        format!("{}{}", self.prefix, krate.cloud_id())
    }
}

#[async_trait::async_trait]
impl crate::Backend for BlobBackend {
    async fn fetch(&self, krate: &Krate) -> Result<Bytes, Error> {
        let dl_req = self.instance.download(
            &self.make_key(krate),
            &Utc::now().format("%a, %d %b %Y %T GMT").to_string(),
        )?;
        let (parts, _) = dl_req.into_parts();

        let uri = parts.uri.to_string();
        let builder = self.client.get(&uri);

        let request = builder.headers(parts.headers).build()?;

        let response = self.client.execute(request).await?.error_for_status()?;
        let res = convert_response(response).await?;
        let content = res.into_body();

        Ok(content)
    }

    async fn upload(&self, source: Bytes, krate: &Krate) -> Result<usize, Error> {
        let content_len = source.len() as u64;
        let insert_req = self.instance.insert(
            &self.make_key(krate),
            source,
            &Utc::now().format("%a, %d %b %Y %T GMT").to_string(),
        )?;
        let (parts, body) = insert_req.into_parts();

        let uri = parts.uri.to_string();
        let builder = self.client.put(&uri);

        let request = builder.headers(parts.headers).body(body).build()?;

        self.client.execute(request).await?.error_for_status()?;

        Ok(content_len as usize)
    }

    async fn list(&self) -> Result<Vec<String>, Error> {
        let list_req = self
            .instance
            .list(&Utc::now().format("%a, %d %b %Y %T GMT").to_string())?;

        let (parts, _) = list_req.into_parts();

        let uri = parts.uri.to_string();
        let builder = self.client.get(&uri);

        let request = builder.headers(parts.headers).build()?;
        let response = self.client.execute(request).await?.error_for_status()?;
        let resp_body = response
            .text()
            .await
            .context("failed to get list response")?;
        let resp_body = resp_body.trim_start_matches('\u{feff}');
        let resp = blob::parse_list_body(resp_body)?;
        let a = resp
            .blobs
            .blob
            .into_iter()
            .map(|b| b.name)
            .collect::<Vec<String>>();
        Ok(a)
    }

    async fn updated(&self, krate: &Krate) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error> {
        let request = self.instance.properties(
            &self.make_key(krate),
            &Utc::now().format("%a, %d %b %Y %T GMT").to_string(),
        )?;
        let (parts, _) = request.into_parts();

        let uri = parts.uri.to_string();
        let builder = self.client.head(&uri);

        let request = builder.headers(parts.headers).build()?;

        let response = self.client.execute(request).await?.error_for_status()?;
        let properties = blob::PropertiesResponse::try_from(convert_response(response).await?)?;
        let a = properties.last_modified;
        let a = chrono::DateTime::parse_from_str(&a, "%a, %d %b %Y %T GMT")?;
        Ok(Some(a.into()))
    }

    fn set_prefix(&mut self, prefix: &str) {
        self.prefix = prefix.to_owned();
    }
}
