#![warn(clippy::all)]
#![warn(rust_2018_idioms)]

use anyhow::Error;
use std::{
    convert::From,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};
use tracing::warn;
pub use url::Url;

pub mod backends;
pub mod cargo;
mod fetch;
pub mod mirror;
pub mod sync;
pub mod util;

pub use cargo::{read_cargo_config, Registry, Source};

#[derive(Eq, Clone, Debug)]
pub struct Krate {
    pub name: String,
    pub version: String, // We just treat versions as opaque strings
    pub source: Source,
}

// impl tracing::Value for Krate {
//     fn record(&self, key: &tracing::field::Field, visitor: &mut dyn tracing::field::Visit) {
//         visitor.record_debug(key, self)
//     }
// }

impl Ord for Krate {
    fn cmp(&self, b: &Self) -> std::cmp::Ordering {
        self.source.cmp(&b.source)
    }
}

impl PartialOrd for Krate {
    fn partial_cmp(&self, b: &Self) -> Option<std::cmp::Ordering> {
        self.source.partial_cmp(&b.source)
    }
}

impl PartialEq for Krate {
    fn eq(&self, b: &Self) -> bool {
        self.source.eq(&b.source)
    }
}

impl Krate {
    pub fn cloud_id(&self) -> CloudId<'_> {
        CloudId { inner: self }
    }

    pub fn local_id(&self) -> LocalId<'_> {
        LocalId { inner: self }
    }
}

impl fmt::Display for Krate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let typ = match &self.source {
            Source::Git { .. } => "git",
            Source::Registry { .. } => "registry",
        };

        write!(f, "{}-{}({})", self.name, self.version, typ)
    }
}

pub struct LocalId<'a> {
    inner: &'a Krate,
}

impl<'a> fmt::Display for LocalId<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner.source {
            Source::Git { ident, .. } => write!(f, "{}", &ident),
            Source::Registry { .. } => {
                write!(f, "{}-{}.crate", self.inner.name, self.inner.version)
            }
        }
    }
}

pub struct CloudId<'a> {
    inner: &'a Krate,
}

impl<'a> fmt::Display for CloudId<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner.source {
            Source::Git { ident, rev, .. } => write!(f, "{}-{}", ident, rev),
            // TODO: havn't make sure
            Source::Registry { chksum, .. } => write!(f, "{}", chksum),
        }
    }
}

#[allow(dead_code)]
pub struct GcsLocation<'a> {
    bucket: &'a str,
    prefix: &'a str,
}

#[allow(dead_code)]
pub struct S3Location<'a> {
    pub bucket: &'a str,
    pub region: &'a str,
    pub host: &'a str,
    pub prefix: &'a str,
}

pub struct FilesystemLocation<'a> {
    pub path: &'a Path,
}

pub struct BlobLocation<'a> {
    pub prefix: &'a str,
    pub container: &'a str,
}

pub enum CloudLocation<'a> {
    Gcs(GcsLocation<'a>),
    S3(S3Location<'a>),
    Fs(FilesystemLocation<'a>),
    Blob(BlobLocation<'a>),
}

pub type Storage = Arc<dyn Backend + Sync + Send>;

pub struct Ctx {
    pub client: reqwest::Client,
    pub backend: Storage,
    pub krates: Vec<Krate>,
    pub registries: Vec<Arc<Registry>>,
    pub root_dir: PathBuf,
}

impl Ctx {
    pub fn new(
        root_dir: Option<PathBuf>,
        backend: Storage,
        krates: Vec<Krate>,
        registries: Vec<Arc<Registry>>,
    ) -> Result<Self, Error> {
        Ok(Self {
            client: reqwest::Client::builder().build()?,
            backend,
            krates,
            registries,
            root_dir: root_dir.unwrap_or_else(|| PathBuf::from(".")),
        })
    }

    pub fn prep_sync_dirs(&self) -> Result<(), Error> {
        // Create the registry and git directories as they are the root of multiple other ones
        std::fs::create_dir_all(self.root_dir.join("registry"))?;
        std::fs::create_dir_all(self.root_dir.join("git"))?;

        Ok(())
    }
}

impl fmt::Debug for Ctx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "krates: {}", self.krates.len())
    }
}

#[async_trait::async_trait]
pub trait Backend: fmt::Debug {
    async fn fetch(&self, krate: &Krate) -> Result<bytes::Bytes, Error>;
    async fn upload(&self, source: bytes::Bytes, krate: &Krate) -> Result<usize, Error>;
    async fn list(&self) -> Result<Vec<String>, Error>;
    async fn updated(&self, krate: &Krate) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error>;
    fn set_prefix(&mut self, prefix: &str);
}
