use crate::Krate;
use anyhow::{Context, Error};
use rusoto_s3::{S3Client, S3};

pub struct S3Backend {
    client: S3Client,
    bucket: String,
    prefix: String,
}

impl S3Backend {
    pub fn new(loc: crate::S3Location<'_>) -> Result<Self, Error> {
        let region = rusoto_core::Region::Custom {
            name: loc.region.to_owned(),
            endpoint: loc.host.to_owned(),
        };

        let client = S3Client::new(region);

        Ok(Self {
            client,
            prefix: loc.prefix.to_owned(),
            bucket: loc.bucket.to_owned(),
        })
    }

    #[inline]
    fn make_key(&self, krate: &Krate) -> String {
        format!("{}{}", self.prefix, krate.cloud_id())
    }

    #[cfg(feature = "s3_test")]
    pub fn make_bucket(&self) -> Result<(), Error> {
        let bucket_request = rusoto_s3::CreateBucketRequest {
            bucket: self.bucket.clone(),
            ..Default::default()
        };

        self.client.create_bucket(bucket_request).sync()?;

        Ok(())
    }
}

impl crate::Backend for S3Backend {
    fn fetch(&self, krate: &Krate) -> Result<bytes::Bytes, Error> {
        let get_request = rusoto_s3::GetObjectRequest {
            bucket: self.bucket.clone(),
            key: self.make_key(krate),
            ..Default::default()
        };

        let get_output = self
            .client
            .get_object(get_request)
            .sync()
            .context("failed to retrieve object")?;

        let stream = get_output.body.context("failed to retrieve body")?;

        use futures::{Future, Stream};
        Ok(stream.concat2().wait().context("failed to read body")?)
    }

    fn upload(&self, source: bytes::Bytes, krate: &Krate) -> Result<(), Error> {
        let put_request = rusoto_s3::PutObjectRequest {
            bucket: self.bucket.clone(),
            key: self.make_key(krate),
            body: Some(source.to_vec().into()),
            ..Default::default()
        };

        self.client
            .put_object(put_request)
            .sync()
            .context("failed to upload object")?;

        Ok(())
    }

    fn list(&self) -> Result<Vec<String>, Error> {
        let list_request = rusoto_s3::ListObjectsV2Request {
            bucket: self.bucket.clone(),
            ..Default::default()
        };

        let list_objects_response = self
            .client
            .list_objects_v2(list_request)
            .sync()
            .context("failed to list objects")?;

        let objects = list_objects_response.contents.unwrap_or_else(|| Vec::new());

        let len = self.prefix.len();

        Ok(objects
            .into_iter()
            .filter_map(|obj| obj.key.map(|k| k[len..].to_owned()))
            .collect())
    }

    fn updated(&self, krate: &Krate) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error> {
        let get_request = rusoto_s3::GetObjectRequest {
            bucket: self.bucket.clone(),
            key: self.make_key(krate),
            ..Default::default()
        };

        // Uhh...so it appears like S3 doesn't have a way of just getting metdata, it also
        // always retrieves the actual object? WTF
        let get_output = self
            .client
            .get_object(get_request)
            .sync()
            .context("failed to get object")?;

        let last_modified = get_output
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
