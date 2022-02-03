use crate::{HttpClient, Krate};
use anyhow::{Context, Error};
use rusty_s3::{
    actions::{CreateBucket, GetObject, ListObjectsV2, PutObject, S3Action},
    Bucket, Credentials,
};
use std::time::Duration;

const ONE_HOUR: Duration = Duration::from_secs(3600);

pub struct S3Backend {
    prefix: String,
    bucket: Bucket,
    credential: Credentials,
    client: HttpClient,
}

impl S3Backend {
    pub fn new(loc: crate::S3Location<'_>, key: String, secret: String) -> Result<Self, Error> {
        let endpoint = format!("https://s3.{}.{}", loc.region, loc.host)
            .parse()
            .context("failed to parse s3 endpoint")?;

        let bucket = Bucket::new(
            endpoint,
            rusty_s3::UrlStyle::VirtualHost,
            loc.bucket.to_owned(),
            loc.region.to_owned(),
        )
        .context("failed to new Bucket")?;
        let credential = Credentials::new(key, secret);
        let client = HttpClient::new();

        Ok(Self {
            prefix: loc.prefix.to_owned(),
            bucket,
            credential,
            client,
        })
    }

    #[inline]
    fn make_key(&self, krate: &Krate) -> String {
        format!("{}{}", self.prefix, krate.cloud_id())
    }

    pub fn make_bucket(&self) -> Result<(), Error> {
        let action = CreateBucket::new(&self.bucket, &self.credential);
        let signed_url = action.sign(ONE_HOUR);
        self.client
            .put(signed_url)
            .send()
            .context("failed io when fetching s3")?
            .error_for_status()?;

        Ok(())
    }
}

impl crate::Backend for S3Backend {
    fn fetch(&self, krate: &Krate) -> Result<bytes::Bytes, Error> {
        let obj = self.make_key(krate);
        let mut action = GetObject::new(&self.bucket, Some(&self.credential), &obj);
        action
            .query_mut()
            .insert("response-cache-control", "no-cache, no-store");
        let signed_url = action.sign(ONE_HOUR);

        let res = self
            .client
            .get(signed_url)
            .send()
            .context("failed io when fetching s3")?
            .error_for_status()?;
        Ok(res.bytes()?)
    }

    fn upload(&self, source: bytes::Bytes, krate: &Krate) -> Result<usize, Error> {
        let len = source.len();
        let obj = self.make_key(krate);
        let action = PutObject::new(&self.bucket, Some(&self.credential), &obj);
        let signed_url = action.sign(ONE_HOUR);
        self.client
            .put(signed_url)
            .body(source)
            .send()
            .context("failed io when uploading s3")?
            .error_for_status()?;
        Ok(len)
    }

    fn list(&self) -> Result<Vec<String>, Error> {
        let action = ListObjectsV2::new(&self.bucket, Some(&self.credential));
        let signed_url = action.sign(ONE_HOUR);
        let resp = self
            .client
            .get(signed_url)
            .send()
            .context("failed io when listing s3")?
            .error_for_status()?;
        let text = resp.text()?;
        let parsed =
            ListObjectsV2::parse_response(&text).context("failed parsing list response")?;
        Ok(parsed.contents.into_iter().map(|obj| obj.key).collect())
    }

    fn updated(&self, krate: &Krate) -> Result<Option<crate::Timestamp>, Error> {
        let mut action = ListObjectsV2::new(&self.bucket, Some(&self.credential));
        action.query_mut().insert("prefix", self.make_key(krate));
        action.query_mut().insert("max-keys", "1");
        let signed_url = action.sign(ONE_HOUR);
        let resp = self
            .client
            .get(signed_url)
            .send()
            .context("failed io when getting updated info")?
            .error_for_status()?;
        let text = resp.text()?;
        let parsed = ListObjectsV2::parse_response(&text).context("faild parsing updated info")?;
        let last_modified = &parsed
            .contents
            .get(0)
            .context(format!("can not get the updated info of {}", krate))?
            .last_modified;

        let last_modified = crate::Timestamp::parse(
            last_modified,
            &time::format_description::well_known::Rfc3339,
        )
        .context("failed to parse last_modified")?
        // This _should_ already be set during parsing?
        .replace_offset(time::UtcOffset::UTC);

        Ok(Some(last_modified))
    }

    fn set_prefix(&mut self, prefix: &str) {
        self.prefix = prefix.to_owned();
    }
}

use std::fmt;

impl fmt::Debug for S3Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("s3")
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .finish()
    }
}
