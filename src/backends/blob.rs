use crate::{
    util::{self, send_request_with_retry},
    CloudId, HttpClient,
};
use anyhow::{Context as _, Result};
use bytes::Bytes;

mod vendor;
use vendor as blob;

#[derive(Debug)]
pub struct BlobBackend {
    prefix: String,
    instance: blob::Blob,
    client: HttpClient,
}

impl BlobBackend {
    pub fn new(loc: crate::BlobLocation<'_>, timeout: std::time::Duration) -> Result<Self> {
        let account =
            std::env::var("STORAGE_ACCOUNT").context("Set env variable STORAGE_ACCOUNT first!")?;
        let master_key = std::env::var("STORAGE_MASTER_KEY")
            .context("Set env variable STORAGE_MASTER_KEY first!")?;

        let instance = blob::Blob::new(&account, &master_key, loc.container, false);
        let client = HttpClient::builder()
            .use_rustls_tls()
            .timeout(timeout)
            .build()?;

        Ok(Self {
            prefix: loc.prefix.to_owned(),
            instance,
            client,
        })
    }

    #[inline]
    fn make_key(&self, id: CloudId<'_>) -> String {
        format!("{}{id}", self.prefix)
    }
}

const FMT: &[time::format_description::FormatItem<'_>] = time::macros::format_description!(
    "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
);

#[inline]
fn utc_now_to_str() -> String {
    time::OffsetDateTime::now_utc().format(&FMT).unwrap()
}

#[async_trait::async_trait]
impl crate::Backend for BlobBackend {
    async fn fetch(&self, id: CloudId<'_>) -> Result<Bytes> {
        let dl_req = self
            .instance
            .download(&self.make_key(id), &utc_now_to_str())?;

        let res = send_request_with_retry(&self.client, util::convert_request(dl_req))
            .await?
            .error_for_status()?;

        Ok(res.bytes().await?)
    }

    async fn upload(&self, source: Bytes, id: CloudId<'_>) -> Result<usize> {
        let content_len = source.len() as u64;
        let insert_req = self
            .instance
            .insert(&self.make_key(id), source, &utc_now_to_str())?;

        send_request_with_retry(&self.client, insert_req.try_into()?)
            .await?
            .error_for_status()?;

        Ok(content_len as usize)
    }

    async fn list(&self) -> Result<Vec<String>> {
        let list_req = self.instance.list(&utc_now_to_str())?;

        let response = send_request_with_retry(&self.client, util::convert_request(list_req))
            .await?
            .error_for_status()?;

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

    async fn updated(&self, id: CloudId<'_>) -> Result<Option<crate::Timestamp>> {
        let request = self
            .instance
            .properties(&self.make_key(id), &utc_now_to_str())?;

        let response = send_request_with_retry(&self.client, util::convert_request(request))
            .await?
            .error_for_status()?;

        let properties =
            blob::PropertiesResponse::try_from(util::convert_response(response).await?)?;

        // Ensure the offset is UTC, the azure datetime format is truly terrible
        let last_modified = crate::Timestamp::parse(&properties.last_modified, &FMT)?
            .replace_offset(time::UtcOffset::UTC);

        Ok(Some(last_modified))
    }
}
