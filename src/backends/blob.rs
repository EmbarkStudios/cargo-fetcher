use crate::{backends::gcs::convert_response, HttpClient, Krate};
use anyhow::{Context, Error};
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
    pub fn new(loc: crate::BlobLocation<'_>, timeout: std::time::Duration) -> Result<Self, Error> {
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
    fn make_key(&self, krate: &Krate) -> String {
        format!("{}{}", self.prefix, krate.cloud_id())
    }
}

const FMT: &[time::format_description::FormatItem<'_>] = time::macros::format_description!(
    "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
);

#[inline]
fn utc_now_to_str() -> String {
    time::OffsetDateTime::now_utc().format(&FMT).unwrap()
}

impl crate::Backend for BlobBackend {
    fn fetch(&self, krate: &Krate) -> Result<Bytes, Error> {
        let dl_req = self
            .instance
            .download(&self.make_key(krate), &utc_now_to_str())?;
        let (parts, _) = dl_req.into_parts();

        let uri = parts.uri.to_string();
        let builder = self.client.get(&uri);

        let request = builder.headers(parts.headers).build()?;

        let response = self.client.execute(request)?.error_for_status()?;
        let res = convert_response(response)?;
        let content = res.into_body();

        Ok(content)
    }

    fn upload(&self, source: Bytes, krate: &Krate) -> Result<usize, Error> {
        let content_len = source.len() as u64;
        let insert_req = self
            .instance
            .insert(&self.make_key(krate), source, &utc_now_to_str())?;
        let (parts, body) = insert_req.into_parts();

        let uri = parts.uri.to_string();
        let builder = self.client.put(&uri);

        let request = builder.headers(parts.headers).body(body).build()?;

        self.client.execute(request)?.error_for_status()?;

        Ok(content_len as usize)
    }

    fn list(&self) -> Result<Vec<String>, Error> {
        let list_req = self.instance.list(&utc_now_to_str())?;

        let (parts, _) = list_req.into_parts();

        let uri = parts.uri.to_string();
        let builder = self.client.get(&uri);

        let request = builder.headers(parts.headers).build()?;
        let response = self.client.execute(request)?.error_for_status()?;
        let resp_body = response.text().context("failed to get list response")?;
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

    fn updated(&self, krate: &Krate) -> Result<Option<crate::Timestamp>, Error> {
        let request = self
            .instance
            .properties(&self.make_key(krate), &utc_now_to_str())?;
        let (parts, _) = request.into_parts();

        let uri = parts.uri.to_string();
        let builder = self.client.head(&uri);

        let request = builder.headers(parts.headers).build()?;

        let response = self.client.execute(request)?.error_for_status()?;
        let properties = blob::PropertiesResponse::try_from(convert_response(response)?)?;

        // Ensure the offset is UTC, the azure datetime format is truly terrible
        let last_modified = crate::Timestamp::parse(&properties.last_modified, &FMT)?
            .replace_offset(time::UtcOffset::UTC);

        Ok(Some(last_modified))
    }

    fn set_prefix(&mut self, prefix: &str) {
        self.prefix = prefix.to_owned();
    }
}
