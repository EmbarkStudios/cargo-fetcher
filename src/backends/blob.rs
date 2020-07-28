use crate::Krate;
use anyhow::{Context, Error};
use azure_sdk_core::prelude::*;
use azure_sdk_storage_blob::prelude::*;
use azure_sdk_storage_core::{client, key_client::KeyClient};
use bytes::Bytes;

#[derive(Debug)]
pub struct BLOBBackend {
    prefix: String,
    client: KeyClient,
    container: String,
}

impl BLOBBackend {
    pub async fn new(
        loc: crate::BlobLocation<'_>,
        account: String,
        master_key: String,
    ) -> Result<Self, Error> {
        let client = client::with_access_key(&account, &master_key);
        Ok(Self {
            prefix: loc.prefix.to_owned(),
            container: loc.container.to_owned(),
            client,
        })
    }

    #[inline]
    fn make_key(&self, krate: &Krate) -> String {
        format!("{}{}", self.prefix, krate.cloud_id())
    }
}

#[async_trait::async_trait]
impl crate::Backend for BLOBBackend {
    async fn fetch(&self, krate: &Krate) -> Result<Bytes, Error> {
        let response = self
            .client
            .get_blob()
            .with_container_name(&self.container)
            .with_blob_name(&self.make_key(krate))
            .finalize()
            .await
            .context("failed to fetch object")?;
        Ok(response.data.into())
    }

    async fn upload(&self, source: Bytes, krate: &Krate) -> Result<usize, Error> {
        let len = source.len();
        let digest = md5::compute(&source[..]);
        self.client
            .put_block_blob()
            .with_container_name(&self.container)
            .with_blob_name(&self.make_key(krate))
            .with_content_type("application/x-tar")
            .with_body(&source[..])
            .with_content_md5(&digest[..])
            .finalize()
            .await
            .context("failed to upload object")?;
        Ok(len)
    }

    async fn list(&self) -> Result<Vec<String>, Error> {
        let list = self
            .client
            .list_blobs()
            .with_container_name(&self.container)
            .with_include_copy()
            .with_include_deleted()
            .with_include_metadata()
            .with_include_snapshots()
            .with_include_uncommitted_blobs()
            .finalize()
            .await
            .context("failed to list objects")?;
        Ok(list
            .incomplete_vector
            .vector
            .into_iter()
            .filter(|o| o.name.starts_with(&self.prefix))
            .map(|o| o.name)
            .collect())
    }

    async fn updated(&self, krate: &Krate) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error> {
        let b = self
            .client
            .get_blob()
            .with_container_name(&self.container)
            .with_blob_name(&self.make_key(krate))
            .finalize()
            .await
            .context("failed to get index blob")?;
        Ok(b.blob.last_modified)
    }

    fn set_prefix(&mut self, prefix: &str) {
        self.prefix = prefix.to_owned();
    }
}
