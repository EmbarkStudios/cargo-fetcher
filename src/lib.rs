#![warn(clippy::all)]
#![warn(rust_2018_idioms)]

use anyhow::Error;
use std::{
    collections::BTreeMap,
    convert::TryFrom,
    fmt,
    path::{Path, PathBuf},
};

pub mod backends;
mod fetch;
pub mod mirror;
pub mod sync;
pub mod util;

#[derive(serde::Deserialize)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
}

#[derive(serde::Deserialize)]
struct LockContents {
    package: Vec<Package>,
    metadata: BTreeMap<String, String>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    CratesIo(String),
    Git {
        url: Url,
        rev: String,
        ident: String,
    },
}

impl Source {
    pub fn from_git_url(url: &Url) -> Result<Self, Error> {
        let rev = match url.query_pairs().find(|(k, _)| k == "rev") {
            Some((_, rev)) => {
                if rev.len() < 7 {
                    anyhow::bail!("revision specififer {} is too short", rev);
                } else {
                    rev
                }
            }
            None => {
                anyhow::bail!("url doesn't contain a revision specifier");
            }
        };

        // This will handle
        // 1. 7 character short_id
        // 2. Full 40 character sha-1
        // 3. 7 character short_id#sha-1
        let rev = &rev[..7];

        let canonicalized = util::Canonicalized::try_from(url)?;
        let ident = canonicalized.ident();

        Ok(Source::Git {
            url: canonicalized.into(),
            ident,
            rev: rev.to_owned(),
        })
    }

    pub(crate) fn is_git(&self) -> bool {
        match self {
            Source::CratesIo(_) => false,
            _ => true,
        }
    }
}

#[derive(Ord, Eq)]
pub struct Krate {
    pub name: String,
    pub version: String, // We just treat versions as opaque strings
    pub source: Source,
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
            Source::CratesIo(_) => "crates.io",
            Source::Git { .. } => "git",
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
            Source::CratesIo(_) => write!(f, "{}-{}.crate", self.inner.name, self.inner.version),
            Source::Git { ident, .. } => write!(f, "{}", &ident),
        }
    }
}

pub struct CloudId<'a> {
    inner: &'a Krate,
}

impl<'a> fmt::Display for CloudId<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner.source {
            Source::CratesIo(chksum) => write!(f, "{}", chksum),
            Source::Git { ident, rev, .. } => write!(f, "{}-{}", ident, rev),
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

pub enum CloudLocation<'a> {
    Gcs(GcsLocation<'a>),
    S3(S3Location<'a>),
}

pub struct Ctx {
    pub client: reqwest::Client,
    pub backend: Box<dyn Backend + Sync>,
    pub krates: Vec<Krate>,
    pub root_dir: PathBuf,
}

impl Ctx {
    pub fn new(
        root_dir: Option<PathBuf>,
        backend: Box<dyn Backend + Sync>,
        krates: Vec<Krate>,
    ) -> Result<Self, Error> {
        Ok(Self {
            client: reqwest::Client::builder().gzip(false).build()?,
            backend,
            krates,
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

pub trait Backend {
    fn fetch(&self, krate: &Krate) -> Result<bytes::Bytes, Error>;
    fn upload(&self, source: bytes::Bytes, krate: &Krate) -> Result<(), Error>;
    fn list(&self) -> Result<Vec<String>, Error>;
    fn updated(&self, krate: &Krate) -> Result<Option<chrono::DateTime<chrono::Utc>>, Error>;
}

pub fn gather<P: AsRef<Path>>(lock_path: P) -> Result<Vec<Krate>, Error> {
    use log::{debug, error};
    use std::fmt::Write;

    let mut locks: LockContents = {
        let toml_contents = std::fs::read_to_string(lock_path)?;
        toml::from_str(&toml_contents)?
    };

    let mut lookup = String::with_capacity(128);
    let mut krates = Vec::with_capacity(locks.package.len());

    for p in locks.package {
        let source = match p.source.as_ref() {
            Some(s) => s,
            None => {
                debug!("skipping 'path' source {}-{}", p.name, p.version);
                continue;
            }
        };

        if source == "registry+https://github.com/rust-lang/crates.io-index" {
            write!(
                &mut lookup,
                "checksum {} {} (registry+https://github.com/rust-lang/crates.io-index)",
                p.name, p.version
            )
            .unwrap();

            if let Some(chksum) = locks.metadata.remove(&lookup) {
                krates.push(Krate {
                    name: p.name,
                    version: p.version,
                    source: Source::CratesIo(chksum),
                })
            }

            lookup.clear();
        } else {
            // We support exactly one form of git sources, rev specififers
            // eg. git+https://github.com/EmbarkStudios/rust-build-helper?rev=9135717#91357179ba2ce6ec7e430a2323baab80a8f7d9b3
            let url = match Url::parse(source) {
                Ok(u) => u,
                Err(e) => {
                    error!("failed to parse url for {}-{}: {}", p.name, p.version, e);
                    continue;
                }
            };

            match Source::from_git_url(&url) {
                Ok(src) => {
                    krates.push(Krate {
                        name: p.name,
                        version: p.version,
                        source: src,
                    });
                }
                Err(e) => {
                    log::error!(
                        "unable to use git url {} for {}-{}: {}",
                        url,
                        p.name,
                        p.version,
                        e
                    );
                }
            }
        }
    }

    Ok(krates)
}
