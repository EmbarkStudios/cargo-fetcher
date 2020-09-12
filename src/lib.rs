#![warn(clippy::all)]
#![warn(rust_2018_idioms)]

use crate::util::Canonicalized;
use anyhow::{Context, Error};
use serde::{de::Visitor, ser::SerializeStruct, Deserialize, Deserializer, Serialize, Serializer};
use std::{
    collections::BTreeMap,
    collections::HashMap,
    convert::{From, Into, TryFrom},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};
pub use url::Url;

pub mod backends;
mod fetch;
pub mod mirror;
pub mod sync;
pub mod util;

#[derive(Deserialize)]
struct CargoConfig {
    registries: Option<HashMap<String, Registry>>,
}

#[derive(Ord, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct Registry {
    index: String,
    token: Option<String>,
    dl: Option<String>,
    api: Option<String>,
}

impl PartialEq for Registry {
    fn eq(&self, b: &Self) -> bool {
        self.index.eq(&b.index)
    }
}

impl PartialOrd for Registry {
    fn partial_cmp(&self, b: &Self) -> Option<std::cmp::Ordering> {
        self.index.partial_cmp(&b.index)
    }
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    /// V2 lock format has the package checksum at the package definition
    /// instead of the separate metadata
    checksum: Option<String>,
}

#[derive(Deserialize)]
struct LockContents {
    package: Vec<Package>,
    /// V2 lock format doesn't have a metadata section
    #[serde(default)]
    metadata: BTreeMap<String, String>,
}

///
/// NB: This exists purely to make Source be able to auto-derive Serialize and Deserialize!
///
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
pub struct UrlWrapper(Url);

impl From<Url> for UrlWrapper {
    fn from(url: Url) -> Self {
        Self(url)
    }
}

impl Into<Url> for UrlWrapper {
    fn into(self) -> Url {
        self.0
    }
}

impl Serialize for UrlWrapper {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut obj = serializer.serialize_struct("UrlWrapper", 1)?;
        obj.serialize_field("url", &format!("{}", self.0))?;
        obj.end()
    }
}

// TODO: figure out how to avoid all this boilerplate implementing Deserialize!
impl<'de> Deserialize<'de> for UrlWrapper {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UrlWrapperVisitor;

        impl<'de> Visitor<'de> for UrlWrapperVisitor {
            type Value = UrlWrapper;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("struct UrlWrapper")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                url::Url::parse(v).map(UrlWrapper).map_err(|err| {
                    serde::de::Error::invalid_value(
                        serde::de::Unexpected::Str(&format!("{:?}: {}", v, err)),
                        &"A url",
                    )
                })
            }
        }

        const FIELDS: &[&str] = &["url"];
        deserializer.deserialize_struct("UrlWrapper", FIELDS, UrlWrapperVisitor)
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug, Serialize, Deserialize)]
pub enum Source {
    Registry(Registry, String),
    CratesIo(String),
    Git {
        url: UrlWrapper,
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

        let url: Url = canonicalized.into();
        Ok(Source::Git {
            url: url.into(),
            ident,
            rev: rev.to_owned(),
        })
    }

    pub(crate) fn is_git(&self) -> bool {
        match self {
            Source::CratesIo(_) => false,
            Source::Registry(_, _) => false,
            _ => true,
        }
    }
}

#[derive(Eq, Clone, Debug, Serialize, Deserialize)]
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
            Source::CratesIo(_) => "crates.io",
            Source::Git { .. } => "git",
            Source::Registry(..) => "alternate registry",
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
            // TODO: havn't make sure
            Source::Registry(_, _) => write!(f, "{}-{}.crate", self.inner.name, self.inner.version),
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
            // TODO: havn't make sure
            Source::Registry(_, chksum) => write!(f, "{}", chksum),
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
    pub registries_url: Vec<Canonicalized>,
    pub root_dir: PathBuf,
}

impl Ctx {
    pub fn new(
        root_dir: Option<PathBuf>,
        backend: Storage,
        krates: Vec<Krate>,
        registries_url: Vec<Canonicalized>,
    ) -> Result<Self, Error> {
        Ok(Self {
            client: reqwest::Client::builder().build()?,
            backend,
            krates,
            registries_url,
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

pub fn read_cargo_config<P: AsRef<Path>>(
    config_path: P,
) -> Result<Option<HashMap<String, Registry>>, Error> {
    let config: CargoConfig = {
        let config_contents = std::fs::read_to_string(config_path)?;
        toml::from_str(&config_contents)?
    };
    match config.registries {
        None => Ok(None),
        Some(r) => {
            let mut registries = HashMap::new();
            for (_, reg) in r.into_iter() {
                registries.insert(format!("registry+{}", reg.index), reg);
            }
            Ok(Some(registries))
        }
    }
}

pub fn read_lock_file<P: AsRef<Path>>(
    lock_path: P,
    registries: Option<HashMap<String, Registry>>,
) -> Result<(Vec<Krate>, Vec<Canonicalized>), Error> {
    use std::fmt::Write;
    use tracing::{error, trace};

    let mut locks: LockContents = {
        let toml_contents = std::fs::read_to_string(lock_path)?;
        toml::from_str(&toml_contents)?
    };

    let mut lookup = String::with_capacity(128);
    let mut krates = Vec::with_capacity(locks.package.len());

    let mut registries_url_map: HashMap<String, Canonicalized> = HashMap::new();
    for p in locks.package {
        let source = match p.source.as_ref() {
            Some(s) => s,
            None => {
                trace!("skipping 'path' source {}-{}", p.name, p.version);
                continue;
            }
        };

        if source == "registry+https://github.com/rust-lang/crates.io-index" {
            let c = Canonicalized::try_from(&Url::parse(source)?)?;
            registries_url_map.insert(source.clone(), c);
            match p.checksum {
                Some(chksum) => krates.push(Krate {
                    name: p.name,
                    version: p.version,
                    source: Source::CratesIo(chksum),
                }),
                None => {
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
                }
            }
        } else if source.starts_with("registry+") {
            let c = Canonicalized::try_from(&Url::parse(source)?)?;
            registries_url_map.insert(source.clone(), c);
            let regs = registries.clone().context(format!(
                "failed to find the registry {} in cargo config file",
                source
            ))?;
            let reg = regs.get(source).context(format!(
                "failed to find the registry {} in cargo config file",
                source
            ))?;
            match p.checksum {
                Some(chksum) => krates.push(Krate {
                    name: p.name,
                    version: p.version,
                    source: Source::Registry(reg.clone(), chksum),
                }),
                None => {
                    write!(
                        &mut lookup,
                        "checksum {} {} ({})",
                        p.name, p.version, source
                    )
                    .unwrap();
                    if let Some(chksum) = locks.metadata.remove(&lookup) {
                        krates.push(Krate {
                            name: p.name,
                            version: p.version,
                            source: Source::Registry(reg.clone(), chksum),
                        })
                    }
                }
            }
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
                    error!(
                        "unable to use git url {} for {}-{}: {}",
                        url, p.name, p.version, e
                    );
                }
            }
        }
    }

    let registry_urls = registries_url_map
        .into_iter()
        .map(|(_url, curl)| curl)
        .collect();
    Ok((krates, registry_urls))
}
