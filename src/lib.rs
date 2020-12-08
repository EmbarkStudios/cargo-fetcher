#![warn(clippy::all)]
#![warn(rust_2018_idioms)]

use anyhow::{Context, Error};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    collections::HashMap,
    convert::{From, Into, TryFrom},
    fmt,
    hash::Hash,
    hash::Hasher,
    path::{Path, PathBuf},
    sync::Arc,
};
use tracing::{error, info, trace};
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

#[derive(Eq, Clone, Debug, Serialize, Deserialize)]
pub struct Registry {
    index: String,
    token: Option<String>,
    dl: Option<String>,
    api: Option<String>,
}

impl Registry {
    pub fn new(
        index: String,
        token: Option<String>,
        dl: Option<String>,
        api: Option<String>,
    ) -> Registry {
        Self {
            index,
            token,
            dl,
            api,
        }
    }
}

impl Registry {
    // https://github.com/rust-lang/cargo/blob/master/src/cargo/sources/registry/mod.rs#L403-L407 blame f1e26ed3238f933fda177f06e913a70d8929dd6d
    pub fn short_name(&self) -> Result<String, Error> {
        let hash = util::short_hash(self);
        let ident = Url::parse(&self.index)
            .context(format!("failed parse {} into url::Url", self.index))?
            .host_str()
            .unwrap_or("")
            .to_string();
        Ok(format!("{}-{}", ident, hash))
    }
}

impl Hash for Registry {
    fn hash<S: Hasher>(&self, into: &mut S) {
        // https://github.com/rust-lang/cargo/blob/master/src/cargo/core/source/source_id.rs#L536 blame 6f29fb76fcb9a3acc5068a9b39708837ef9eb47d
        2usize.hash(into);
        // https://github.com/rust-lang/cargo/blob/master/src/cargo/core/source/source_id.rs#L542 blame b691f1e4c5dd7449c3ab3cf1da3a061a2f3d5599
        self.index.hash(into);
    }
}

impl PartialEq for Registry {
    fn eq(&self, b: &Self) -> bool {
        self.index.eq(&b.index)
    }
}

impl Ord for Registry {
    fn cmp(&self, b: &Self) -> std::cmp::Ordering {
        self.index.cmp(&b.index)
    }
}

impl PartialOrd for Registry {
    fn partial_cmp(&self, b: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(b))
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

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug, Serialize, Deserialize)]
pub enum Source {
    Registry(Registry, String),
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

        let url: Url = canonicalized.into();
        Ok(Source::Git {
            url,
            ident,
            rev: rev.to_owned(),
        })
    }

    pub(crate) fn is_git(&self) -> bool {
        !matches!(self, Source::Registry(_, _))
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
            Source::Git { .. } => "git",
            Source::Registry(..) => "registry",
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
    pub registries: Vec<Registry>,
    pub root_dir: PathBuf,
}

impl Ctx {
    pub fn new(
        root_dir: Option<PathBuf>,
        backend: Storage,
        krates: Vec<Krate>,
        registries: Vec<Registry>,
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

/// Reads all of the custom registries configured in cargo config files.
///
/// Gathers all of the available .cargo/config(.toml) files, then applies
/// them in reverse order, as the more local ones override the ones higher
/// up in the hierarchy
///
/// See https://doc.rust-lang.org/cargo/reference/config.html
pub fn read_cargo_config(mut cargo_home_path: PathBuf, dir: PathBuf) -> Result<Vec<Registry>, Error> {
    let mut configs = Vec::new();

    fn read_config_dir(dir: &mut PathBuf) -> Option<PathBuf> {
        // Check for config before config.toml, same as cargo does
        dir.push("config");

        if !dir.exists() {
            dir.set_extension("toml");
        }

        if dir.exists() {
            let ret = dir.clone();
            dir.pop();
            Some(ret)
        } else {
            dir.pop();
            None
        }
    }

    let mut dir = dir.canonicalize()?;

    for _ in 0..dir.ancestors().count() {
        dir.push(".cargo");

        if !dir.exists() {
            dir.pop();
            dir.pop();
            continue;
        }

        if let Some(config) = read_config_dir(&mut dir) {
            configs.push(config);
        }

        dir.pop();
        dir.pop();
    }

    if let Some(home_config) = read_config_dir(&mut cargo_home_path) {
        configs.push(home_config);
    }

    let mut regs = HashMap::new();

    for config_path in configs.iter().rev() {
        let config: CargoConfig = {
            let config_contents = match std::fs::read_to_string(config_path) {
                Ok(s) => s,
                Err(e) => {
                    error!(
                        "failed to read cargo config({}): {}",
                        config_path.display(),
                        e
                    );
                    continue;
                }
            };

            match toml::from_str(&config_contents) {
                Ok(cfg) => cfg,
                Err(e) => {
                    error!(
                        "failed to deserialize cargo config({}): {}",
                        config_path.display(),
                        e
                    );
                    continue;
                }
            }
        };

        if let Some(registries) = config.registries {
            for (name, value) in registries {
                if regs.insert(name, value).is_some() {
                    info!("registry overriden");
                }
            }
        }
    }

    Ok(regs.into_iter().map(|(_, v)| v).collect())
}

pub fn read_lock_file<P: AsRef<Path>>(
    lock_path: P,
    registries: Vec<Registry>,
) -> Result<(Vec<Krate>, Vec<Registry>), Error> {
    use std::fmt::Write;

    let mut locks: LockContents = {
        let toml_contents = std::fs::read_to_string(lock_path)?;
        toml::from_str(&toml_contents)?
    };

    let mut lookup = String::with_capacity(128);
    let mut krates = Vec::with_capacity(locks.package.len());

    let mut registries_to_sync: Vec<Registry> = Vec::with_capacity(krates.len());
    for p in locks.package {
        let source = match p.source.as_ref() {
            Some(s) => s,
            None => {
                trace!("skipping 'path' source {}-{}", p.name, p.version);
                continue;
            }
        };

        if source.starts_with("registry+") {
            let registry = if source.ends_with(util::CRATES_IO_URL) {
                match registries.binary_search_by(|r| source.ends_with(&r.index).cmp(&true)) {
                    Ok(i) => registries[i].clone(),
                    Err(_) => Registry::new(
                        util::CRATES_IO_URL.to_owned(),
                        None,
                        Some(util::CRATES_IO_DL.to_owned()),
                        None,
                    ),
                }
            } else {
                match registries.binary_search_by(|r| source.ends_with(&r.index).cmp(&true)) {
                    Ok(i) => registries[i].clone(),
                    Err(_) => {
                        return Err(anyhow::Error::msg(format!(
                            "failed to find the registry {}",
                            source
                        )))
                    }
                }
            };
            registries_to_sync.push(registry.clone());
            match p.checksum {
                Some(chksum) => krates.push(Krate {
                    name: p.name,
                    version: p.version,
                    source: Source::Registry(registry, chksum),
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
                            source: Source::Registry(registry.clone(), chksum),
                        })
                    }
                    lookup.clear();
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
    registries_to_sync.sort();
    registries_to_sync.dedup();

    Ok((krates, registries_to_sync))
}
