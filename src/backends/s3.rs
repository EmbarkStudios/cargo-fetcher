use crate::Krate;
use anyhow::{Context, Error};
use reqwest::Client;
use rusoto_s3::{S3Client, S3};
use rusty_s3::actions::{GetObject, ListObjectsV2, PutObject, S3Action};
use rusty_s3::{Bucket, Credentials};
use std::time::Duration;

const ONE_HOUR: Duration = Duration::from_secs(3600);

pub struct S3Backend {
    client: S3Client,
    bucket: String,
    prefix: String,
    bucket_rusty_s3: Bucket,
    credential: Credentials,
    client_reqwest: Client,
}

impl S3Backend {
    pub fn new(loc: crate::S3Location<'_>) -> Result<Self, Error> {
        let endpoint = format!("https://s3.{}.{}", loc.region, loc.host).parse()?;
        let path_style = false;
        let bucket = Bucket::new(endpoint, path_style, loc.bucket.into(), loc.region.into())
            .context("Can not new Bucket obj")?;
        let key = "AKIA6BO3PLN4ZB5CWIHE";
        let secret = "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";
        let credential = Credentials::new(key.into(), secret.into());
        let client_reqwest = Client::new();

        println!(
            "endpoint: {}",
            format!("https://s3.{}.{}", loc.region, loc.host)
        );
        let region = rusoto_core::Region::Custom {
            name: loc.region.to_owned(),
            endpoint: format!("https://s3.{}.{}", loc.region, loc.host),
        };

        let client = S3Client::new(region);

        Ok(Self {
            client,
            prefix: loc.prefix.to_owned(),
            bucket: loc.bucket.to_owned(),
            bucket_rusty_s3: bucket,
            credential,
            client_reqwest,
        })
    }

    #[inline]
    fn make_key(&self, krate: &Krate) -> String {
        format!("{}{}", self.prefix, krate.cloud_id())
    }

    pub async fn make_bucket(&self) -> Result<(), Error> {
        let bucket_request = rusoto_s3::CreateBucketRequest {
            bucket: self.bucket.clone(),
            ..Default::default()
        };

        // Can "fail" if bucket already exists
        let _ = self.client.create_bucket(bucket_request).await;

        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::Backend for S3Backend {
    async fn fetch(&self, krate: &Krate) -> Result<bytes::Bytes, Error> {
        let obj = self.make_key(krate);
        let mut action = GetObject::new(&self.bucket_rusty_s3, Some(&self.credential), &obj);
        action
            .query_mut()
            .insert("response-cache-control", "no-cache, no-store");
        let signed_url = action.sign(ONE_HOUR);

        let res = self
            .client_reqwest
            .get(signed_url)
            .send()
            .await?
            .error_for_status()?;
        Ok(res.bytes().await?)
    }

    async fn upload(&self, source: bytes::Bytes, krate: &Krate) -> Result<usize, Error> {
        let len = source.len();
        let obj = self.make_key(krate);
        let action = PutObject::new(&self.bucket_rusty_s3, Some(&self.credential), &obj);
        let signed_url = action.sign(ONE_HOUR);
        self.client_reqwest
            .put(signed_url)
            .body(source)
            .send()
            .await?
            .error_for_status()?;
        Ok(len)
    }

    async fn list(&self) -> Result<Vec<String>, Error> {
        let action = ListObjectsV2::new(&self.bucket_rusty_s3, Some(&self.credential));
        let signed_url = action.sign(ONE_HOUR);
        let resp = self
            .client_reqwest
            .get(signed_url)
            .send()
            .await?
            .error_for_status()?;
        let text = resp.text().await?;
        let parsed = ListObjectsV2::parse_response(&text)?;
        Ok(parsed
            .contents
            .into_iter()
            .filter_map(|obj| Some(obj.key))
            .collect())
    }

    async fn updated(&self, krate: &Krate) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error> {
        let head_request = rusoto_s3::HeadObjectRequest {
            bucket: self.bucket.clone(),
            key: self.make_key(krate),
            ..Default::default()
        };

        let head_output = self
            .client
            .head_object(head_request)
            .await
            .context("failed to get head object")?;

        let last_modified = head_output
            .last_modified
            .context("last_modified not available for object")?;

        let last_modified = chrono::DateTime::parse_from_rfc3339(&last_modified)
            .context("failed to parse last_modified")?
            .with_timezone(&chrono::Utc);

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
