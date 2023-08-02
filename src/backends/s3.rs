use crate::{CloudId, HttpClient};
use anyhow::{Context as _, Result};
use rusty_s3::{
    actions::{CreateBucket, GetObject, ListObjectsV2, PutObject, S3Action},
    credentials::Ec2SecurityCredentialsMetadataResponse,
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
    pub fn new(loc: crate::S3Location<'_>, timeout: std::time::Duration) -> Result<Self> {
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

        let client = HttpClient::builder()
            .use_rustls_tls()
            .timeout(timeout)
            .build()?;
        let credential = Credentials::from_env()
            .map_or_else(|| ec2_credentials(&client).ok(), Some)
            .context("Either set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or run from an ec2 instance with an assumed IAM role")?;

        Ok(Self {
            prefix: loc.prefix.to_owned(),
            bucket,
            credential,
            client,
        })
    }

    #[inline]
    fn make_key(&self, id: CloudId<'_>) -> String {
        format!("{}{id}", self.prefix)
    }

    pub fn make_bucket(&self) -> Result<()> {
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
    fn fetch(&self, id: CloudId<'_>) -> Result<bytes::Bytes> {
        let obj = self.make_key(id);
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

    fn upload(&self, source: bytes::Bytes, id: CloudId<'_>) -> Result<usize> {
        let len = source.len();
        let obj = self.make_key(id);
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

    fn list(&self) -> Result<Vec<String>> {
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

    fn updated(&self, id: CloudId<'_>) -> Result<Option<crate::Timestamp>> {
        let mut action = ListObjectsV2::new(&self.bucket, Some(&self.credential));
        action.query_mut().insert("prefix", self.make_key(id));
        action.query_mut().insert("max-keys", "1");
        let signed_url = action.sign(ONE_HOUR);
        let resp = self
            .client
            .get(signed_url)
            .send()
            .context("failed to send request for update info")?
            .error_for_status()?;
        let text = resp.text()?;
        let parsed = ListObjectsV2::parse_response(&text).context("failed parsing updated info")?;
        let last_modified = &parsed
            .contents
            .get(0)
            .context("could not locate update info")?
            .last_modified;

        let last_modified = crate::Timestamp::parse(
            last_modified,
            &time::format_description::well_known::Rfc3339,
        )
        .context("failed to parse last_modified timestamp")?
        // This _should_ already be set during parsing?
        .replace_offset(time::UtcOffset::UTC);

        Ok(Some(last_modified))
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

const AWS_IMDS_CREDENTIALS: &str =
    "http://169.254.169.254/latest/meta-data/iam/security-credentials";

fn ec2_credentials(client: &HttpClient) -> Result<Credentials> {
    let resp = client
        .get(AWS_IMDS_CREDENTIALS)
        .send()
        .context("failed to get role name")?
        .error_for_status()?;

    let role_name = resp.text()?;
    let resp = client
        .get(format!("{AWS_IMDS_CREDENTIALS}/{role_name}"))
        .send()
        .context("failed to get role name")?
        .error_for_status()?;

    let json = resp.text()?;
    let ec2_creds = Ec2SecurityCredentialsMetadataResponse::deserialize(&json)?;
    Ok(ec2_creds.into_credentials())
}
