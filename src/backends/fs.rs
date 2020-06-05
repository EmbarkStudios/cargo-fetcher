use crate::Krate;

use anyhow::Error;
use async_std::{fs, io, stream::StreamExt};
use bytes::Bytes;
use digest::{Digest as DigestTrait, FixedOutput};
use sha2::Sha256;

use std::{convert::Into, fmt, path::PathBuf, str, time};

const FINGERPRINT_SIZE: usize = 32;

#[derive(serde::Serialize, serde::Deserialize, Copy, Clone, Debug)]
struct Fingerprint([u8; FINGERPRINT_SIZE]);

impl Fingerprint {
    fn digest(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::default();
        hasher.input(bytes);
        Self::from_sha256_bytes(&hasher.fixed_result())
    }

    fn from_sha256_bytes(sha256_bytes: &[u8]) -> Self {
        if sha256_bytes.len() != FINGERPRINT_SIZE {
            panic!(
                "Input value was not a fingerprint; has length: {} (must be {})",
                sha256_bytes.len(),
                FINGERPRINT_SIZE,
            );
        }
        let mut fingerprint = [0; FINGERPRINT_SIZE];
        fingerprint.clone_from_slice(&sha256_bytes[0..FINGERPRINT_SIZE]);
        Self(fingerprint)
    }

    fn from_hex_string(hex_string: &str) -> Result<Self, Error> {
        <[u8; FINGERPRINT_SIZE] as hex::FromHex>::from_hex(hex_string)
            .map(Self)
            .map_err(|e| e.into())
    }

    fn to_hex(&self) -> String {
        let mut s = String::new();
        for &byte in &self.0 {
            fmt::Write::write_fmt(&mut s, format_args!("{:02x}", byte)).unwrap();
        }
        s
    }
}

///
/// A key-value store backed by a filesystem directory.
///
#[derive(Debug)]
struct FilesystemDB {
    root: PathBuf,
}

impl FilesystemDB {
    async fn new(root: PathBuf) -> Result<Self, Error> {
        fs::create_dir_all(&root).await?;
        Ok(Self { root })
    }

    async fn lookup_fingerprint(&self, key: Fingerprint) -> Result<Option<Bytes>, Error> {
        let hex = key.to_hex();
        let entry_path = self.root.join(hex);
        match fs::read(&entry_path).await {
            Ok(bytes) => Ok(Some(Bytes::from(bytes))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn lookup<K: Into<Fingerprint>, V: From<Bytes>>(
        &self,
        key: K,
    ) -> Result<Option<V>, Error> {
        let key = key.into();
        let result = self.lookup_fingerprint(key).await?;
        Ok(result.map(|bytes| bytes.into()))
    }

    async fn insert_fingerprint_bytes(
        &self,
        key: Fingerprint,
        value: Bytes,
    ) -> Result<Fingerprint, Error> {
        let hex = key.to_hex();
        let entry_path = self.root.join(hex);
        fs::write(&entry_path, &value).await?;
        Ok(key)
    }

    async fn insert<K: Into<Fingerprint>, V: Into<Bytes>>(
        &self,
        key: K,
        value: V,
    ) -> Result<Fingerprint, Error> {
        let key = key.into();
        self.insert_fingerprint_bytes(key, value.into()).await
    }

    async fn list_keys(&self) -> Result<Vec<Fingerprint>, Error> {
        let mut entries = fs::read_dir(&self.root).await?;
        let mut results: Vec<Fingerprint> = vec![];
        while let Some(res) = entries.next().await {
            let entry = res?;
            let file_name = entry.file_name();
            results.push(Fingerprint::from_hex_string(&file_name.to_string_lossy())?);
        }
        Ok(results)
    }

    async fn modified_time_fingerprint(
        &self,
        key: Fingerprint,
    ) -> Result<Option<time::SystemTime>, Error> {
        let hex = key.to_hex();
        let entry_path = self.root.join(hex);
        let modified_time = match fs::metadata(&entry_path).await {
            Ok(metadata) => metadata.modified()?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(e) => {
                return Err(e.into());
            }
        };
        Ok(Some(modified_time))
    }

    async fn modified_time<K: Into<Fingerprint>>(
        &self,
        key: K,
    ) -> Result<Option<time::SystemTime>, Error> {
        self.modified_time_fingerprint(key.into()).await
    }
}

///
/// A specialization of FilesystemDB that implements a Content-Addressed Storage (CAS) interface.
///
#[derive(Debug)]
struct CASDB {
    db: FilesystemDB,
}

impl CASDB {
    fn new(db: FilesystemDB) -> Self {
        Self { db }
    }

    async fn lookup_cas_fingerprint(&self, key: Fingerprint) -> Result<Option<Bytes>, Error> {
        self.db.lookup_fingerprint(key).await
    }

    async fn lookup_cas<K: Into<Fingerprint>, V: From<Bytes>>(
        &self,
        key: K,
    ) -> Result<Option<V>, Error> {
        let key = key.into();
        let result = self.lookup_cas_fingerprint(key).await?;
        Ok(result.map(|bytes| bytes.into()))
    }

    async fn insert_cas_bytes(&self, value: Bytes) -> Result<Fingerprint, Error> {
        let key = Fingerprint::digest(&value);
        self.db.insert_fingerprint_bytes(key, value).await
    }

    async fn insert_cas<V: Into<Bytes>>(&self, value: V) -> Result<Fingerprint, Error> {
        let bytes = value.into();
        self.insert_cas_bytes(bytes).await
    }

    async fn list_cas_keys(&self) -> Result<Vec<Fingerprint>, Error> {
        self.db.list_keys().await
    }
}

#[derive(Debug)]
pub struct FSBackend {
    krate_lookup: CASDB,
    krate_data: FilesystemDB,
    krate_digest_mapping: FilesystemDB,
    fetch_cache: FilesystemDB,
    // TODO: figure out if this `prefix` boilerplate can be simplified.
    prefix: String,
}

impl FSBackend {
    pub async fn new(loc: crate::FilesystemLocation<'_>) -> Result<Self, Error> {
        let crate::FilesystemLocation { path } = loc;

        let krate_lookup = CASDB::new(FilesystemDB::new(path.join("krate_lookup")).await?);
        let krate_data = FilesystemDB::new(path.join("krate_data")).await?;
        let krate_digest_mapping = FilesystemDB::new(path.join("krate_digest_mapping")).await?;
        let fetch_cache = FilesystemDB::new(path.join("fetch_cache")).await?;

        Ok(Self {
            krate_lookup,
            krate_data,
            krate_digest_mapping,
            fetch_cache,
            prefix: "".to_string(),
        })
    }
}

impl Into<Fingerprint> for Krate {
    fn into(self) -> Fingerprint {
        let krate_json =
            serde_json::to_string(&self).expect("did not expect an error serializing Krate object");
        Fingerprint::digest(krate_json.as_bytes())
    }
}

impl Into<Bytes> for Krate {
    fn into(self) -> Bytes {
        let krate_json =
            serde_json::to_string(&self).expect("did not expect an error serializing Krate object");
        Bytes::copy_from_slice(krate_json.as_bytes())
    }
}

impl From<Bytes> for Krate {
    fn from(bytes: Bytes) -> Self {
        let json_string =
            str::from_utf8(&bytes).expect("failed to convert bytes into json string for Krate");
        let krate: Krate = serde_json::from_str(json_string)
            .expect("failed to deserialize Krate from json string");
        krate
    }
}

#[async_trait::async_trait]
impl crate::Backend for FSBackend {
    async fn fetch(&self, krate: &Krate) -> Result<Bytes, Error> {
        self.krate_data
            .lookup(krate.clone())
            .await?
            .ok_or_else(|| anyhow::Error::msg(format!("krate {:?} not found!", krate)))
    }

    async fn upload(&self, source: Bytes, krate: &Krate) -> Result<usize, Error> {
        // 1. Serialize the krate to json and store that in a separate table than the package
        // contents table. This will be consumed by list().
        self.krate_lookup.insert_cas(krate.clone()).await?;

        // 2. Still using the Krate as the content-addressed key, store the package bytes into the
        // package contents table (in this case, writing a file). This will be consumed by fetch().
        let len = source.len();
        self.krate_data.insert(krate.clone(), source).await?;

        Ok(len)
    }

    async fn list(&self) -> Result<Vec<String>, Error> {
        let all_keys: Vec<Fingerprint> = self.krate_lookup.list_cas_keys().await?;
        let mut all_names: Vec<String> = vec![];
        for key in all_keys.into_iter() {
            let cur_krate: Krate = self
                .krate_lookup
                .lookup_cas(key)
                .await?
                .expect("this key was provided by list_cas_keys()");
            let stripped_name = cur_krate.name[self.prefix.len()..].to_owned();
            all_names.push(stripped_name);
        }
        Ok(all_names)
    }

    async fn updated(&self, krate: &Krate) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error> {
        let result = self.krate_data.modified_time(krate.clone()).await?;
        Ok(result.map(|system_time| {
            let unix_time: u64 = system_time
                .duration_since(time::UNIX_EPOCH)
                .expect("Surely you're not before the unix epoch?")
                .as_secs();
            // TODO: figure out how to initialize a chrono timestamp using a u64 instead of cutting
            // off half the range into an i64.
            let naive_time = chrono::NaiveDateTime::from_timestamp(unix_time as i64, 0);
            chrono::DateTime::<chrono::Utc>::from_utc(naive_time, chrono::Utc)
        }))
    }

    fn set_prefix(&mut self, prefix: &str) {
        self.prefix = prefix.to_owned();
    }
}
