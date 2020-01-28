use crate::Krate;
use anyhow::{Context, Error};
use rusoto_s3::{S3Client, S3};
use tokio::task::spawn_blocking;

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

#[async_trait::async_trait]
impl crate::Backend for S3Backend {
    async fn fetch(&self, krate: &Krate) -> Result<bytes::Bytes, Error> {
        let get_request = rusoto_s3::GetObjectRequest {
            bucket: self.bucket.clone(),
            key: self.make_key(krate),
            ..Default::default()
        };

        let client = self.client.clone();
        let get_output = spawn_blocking(move || client.get_object(get_request).sync())
            .await
            .context("blocking error")?
            .context("failed to retrieve object")?;

        let len = get_output.content_length.unwrap_or(1024) as usize;
        let stream = get_output.body.context("failed to retrieve body")?;

        let buffer = spawn_blocking(move || -> Result<bytes::Bytes, std::io::Error> {
            use std::io::Read;

            let mut buffer = bytes::BytesMut::with_capacity(len);
            let mut reader = stream.into_blocking_read();

            let mut chunk = [0u8; 8 * 1024];
            loop {
                let read = reader.read(&mut chunk)?;

                if read > 0 {
                    buffer.extend_from_slice(&chunk);
                } else {
                    break;
                }
            }

            Ok(buffer.freeze())
        })
        .await
        .context("blocking error")?
        .context("read error")?;

        Ok(buffer)
    }

    async fn upload(&self, source: bytes::Bytes, krate: &Krate) -> Result<(), Error> {
        let put_request = rusoto_s3::PutObjectRequest {
            bucket: self.bucket.clone(),
            key: self.make_key(krate),
            body: Some(source.to_vec().into()),
            ..Default::default()
        };

        let client = self.client.clone();

        spawn_blocking(move || client.put_object(put_request).sync())
            .await
            .context("join error")?
            .context("failed to upload object")?;

        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>, Error> {
        let list_request = rusoto_s3::ListObjectsV2Request {
            bucket: self.bucket.clone(),
            ..Default::default()
        };

        let client = self.client.clone();
        let list_objects_response =
            spawn_blocking(move || client.list_objects_v2(list_request).sync())
                .await
                .context("blocking error")?
                .context("failed to list objects")?;

        let objects = list_objects_response.contents.unwrap_or_else(Vec::new);

        let len = self.prefix.len();

        Ok(objects
            .into_iter()
            .filter_map(|obj| obj.key.map(|k| k[len..].to_owned()))
            .collect())
    }

    async fn updated(&self, krate: &Krate) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error> {
        let get_request = rusoto_s3::GetObjectRequest {
            bucket: self.bucket.clone(),
            key: self.make_key(krate),
            ..Default::default()
        };

        // Uhh...so it appears like S3 doesn't have a way of just getting metdata, it also
        // always retrieves the actual object? WTF
        let client = self.client.clone();
        let get_output = spawn_blocking(move || client.get_object(get_request).sync())
            .await
            .context("blocking error")?
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
