use crate::{util, Krate, Path, PathBuf};
use anyhow::Context as _;
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap},
    sync::Arc,
};
use tame_index::index::IndexConfig;
use url::Url;

/// The normal crates.io DL url, note that this is not the one actually advertised
/// by cargo (<https://crates.io/api/v1/crates>) as that is just a redirect to this
/// location, so obviously this will break terribly if crates.io ever changes the
/// actual storage location, but that's unlikely, and is easy to fix if it ever
/// does happen
pub const CRATES_IO_DL: &str = "https://static.crates.io/crates/{crate}/{crate}-{version}.crate";

#[derive(Deserialize)]
pub struct CargoConfig {
    pub registries: Option<HashMap<String, Registry>>,
}

#[derive(Deserialize, Serialize, PartialEq, Eq, Copy, Clone, Debug, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryProtocol {
    #[default]
    Git,
    Sparse,
}

impl std::str::FromStr for RegistryProtocol {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let prot = match s {
            "git" => Self::Git,
            "sparse" => Self::Sparse,
            unknown => anyhow::bail!("unknown protocol '{unknown}'"),
        };

        Ok(prot)
    }
}

#[derive(Eq, Clone, Debug, Serialize, Deserialize)]
pub struct Registry {
    pub index: Url,
    config: Option<IndexConfig>,
    #[serde(default)]
    pub protocol: RegistryProtocol,
    dir_name: String,
}

impl Registry {
    #[inline]
    pub fn new(index: impl AsRef<str>, dl: Option<String>) -> anyhow::Result<Self> {
        let index = Url::parse(index.as_ref())?;
        Self::build(index, dl.map(|dl| IndexConfig { dl, api: None }))
    }

    #[inline]
    pub fn crates_io(protocol: RegistryProtocol) -> Self {
        let index_url = match protocol {
            RegistryProtocol::Git => tame_index::CRATES_IO_INDEX,
            RegistryProtocol::Sparse => tame_index::CRATES_IO_HTTP_INDEX,
        };

        Self::build(
            Url::parse(index_url).unwrap(),
            Some(IndexConfig {
                dl: CRATES_IO_DL.to_owned(),
                api: None,
            }),
        )
        .unwrap()
    }

    #[inline]
    fn build(index: Url, config: Option<IndexConfig>) -> anyhow::Result<Self> {
        let tame_index::utils::UrlDir {
            dir_name,
            canonical,
        } = tame_index::utils::url_to_local_dir(index.as_str())?;
        Ok(Self {
            index,
            config,
            protocol: if canonical.starts_with("sparse+") {
                RegistryProtocol::Sparse
            } else {
                RegistryProtocol::Git
            },
            dir_name,
        })
    }

    /// Gets the download url for the crate
    ///
    /// See <https://doc.rust-lang.org/cargo/reference/registries.html#index-format>
    /// for more info
    pub fn download_url(&self, krate: &Krate) -> String {
        match &self.config {
            Some(ic) => ic.download_url(
                krate.name.as_str().try_into().expect("invalid krate name"),
                &krate.version,
            ),
            None => {
                format!("{}/{}/{}/download", self.index, krate.name, krate.version)
            }
        }
    }

    #[inline]
    pub fn short_name(&self) -> &str {
        &self.dir_name
    }

    #[inline]
    pub fn cache_dir(&self, root: &Path) -> PathBuf {
        let mut cdir = root.join(crate::sync::CACHE_DIR);
        cdir.push(self.short_name());
        cdir
    }

    #[inline]
    pub fn src_dir(&self, root: &Path) -> PathBuf {
        let mut cdir = root.join(crate::sync::SRC_DIR);
        cdir.push(self.short_name());
        cdir
    }

    #[inline]
    pub fn sync_dirs(&self, root: &Path) -> (PathBuf, PathBuf) {
        (self.cache_dir(root), self.src_dir(root))
    }

    #[inline]
    pub fn is_crates_io(&self) -> bool {
        match self.protocol {
            RegistryProtocol::Git => self.index.as_str() == tame_index::CRATES_IO_INDEX,
            RegistryProtocol::Sparse => self.index.as_str() == tame_index::CRATES_IO_HTTP_INDEX,
        }
    }
}

impl PartialEq for Registry {
    fn eq(&self, b: &Self) -> bool {
        self.index.eq(&b.index)
    }
}

impl Ord for Registry {
    fn cmp(&self, b: &Self) -> Ordering {
        self.index.cmp(&b.index)
    }
}

impl PartialOrd for Registry {
    fn partial_cmp(&self, b: &Self) -> Option<Ordering> {
        Some(self.cmp(b))
    }
}

pub fn determine_cargo_root(explicit: Option<&PathBuf>) -> anyhow::Result<PathBuf> {
    let root = match explicit {
        Some(exp) => {
            home::cargo_home_with_cwd(exp.as_std_path()).context("failed to retrieve cargo home")
        }
        None => home::cargo_home().context("failed to retrieve cargo home for cwd"),
    }?;

    Ok(util::path(&root)?.to_owned())
}

#[derive(Deserialize)]
struct LockContents {
    // Note this _could_ be a BTreeSet, but a well-formed Cargo.lock will already
    // be ordered and deduped so no need
    package: Vec<Package>,
}

#[derive(Deserialize, Eq)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    /// Only applies to crates with a registry source, git sources do not have it
    checksum: Option<String>,
}

impl PartialEq for Package {
    fn eq(&self, b: &Self) -> bool {
        self.cmp(b) == Ordering::Equal
    }
}

impl Ord for Package {
    /// This follows (roughly) how cargo implements `Ord` as well
    fn cmp(&self, b: &Self) -> Ordering {
        match self.name.cmp(&b.name) {
            Ordering::Equal => {}
            other => return other,
        }

        match self.version.cmp(&b.version) {
            Ordering::Equal => {}
            other => return other,
        }

        // path dependencies are none
        match (&self.source, &b.source) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(a), Some(b)) => {
                let (a_kind, a_url) = a.split_once('+').expect("valid source id");
                let (b_kind, b_url) = b.split_once('+').expect("valid source id");

                match a_kind.cmp(b_kind) {
                    Ordering::Equal => {}
                    other => return other,
                }

                if a_kind == "registry" {
                    a_url.cmp(b_url)
                } else if a_kind == "git" {
                    let a_can = tame_index::utils::canonicalize_url(a_url).expect("valid url");
                    let b_can = tame_index::utils::canonicalize_url(b_url).expect("valid url");

                    a_can.cmp(&b_can)
                } else {
                    panic!("unexpected package source '{a_kind}'");
                }
            }
        }
    }
}

impl PartialOrd for Package {
    fn partial_cmp(&self, b: &Self) -> Option<Ordering> {
        Some(self.cmp(b))
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug, Serialize, Deserialize)]
pub enum GitFollow {
    Branch(String),
    Tag(String),
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
pub struct RegistrySource {
    pub registry: Arc<Registry>,
    pub chksum: String,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
pub struct GitSource {
    pub url: Url,
    pub rev: GitRev,
    pub ident: String,
    pub follow: Option<GitFollow>,
}

#[derive(Clone, Debug)]
pub struct GitRev {
    /// The full git revision
    pub id: gix::ObjectId,
    /// The short revision, this is used as the identity for checkouts
    short: [u8; 7],
}

impl Eq for GitRev {}
impl PartialEq for GitRev {
    fn eq(&self, o: &Self) -> bool {
        self.id == o.id
    }
}

impl Ord for GitRev {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.id.cmp(&o.id)
    }
}

impl PartialOrd for GitRev {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl GitRev {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let id = gix::ObjectId::from_hex(s.as_bytes()).context("failed to parse revision")?;
        let mut short = [0u8; 7];
        short.copy_from_slice(&s.as_bytes()[..7]);

        Ok(Self { id, short })
    }

    #[inline]
    pub fn short(&self) -> &str {
        #[allow(unsafe_code)]
        // SAFETY: these are only hex characters that we've already validated
        unsafe {
            std::str::from_utf8_unchecked(&self.short)
        }
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
pub enum Source {
    Registry(RegistrySource),
    Git(GitSource),
}

impl Source {
    pub fn from_git_url(url: &Url) -> anyhow::Result<Self> {
        let rev = url.fragment().context("url doesn't contain a revision")?;

        // The revision fragment in the cargo.lock will always be the full
        // sha-1, but we only use the short-id since that is how cargo calculates
        // the local identity of a specific git checkout
        let rev = GitRev::parse(rev)?;

        // There is guaranteed to be exactly one query parameter
        let (key, value) = url
            .query_pairs()
            .next()
            .context("url doesn't contain a query parameter")?;

        let follow = match key.as_ref() {
            // A rev specifier is duplicate info so we just ignore it
            "rev" => None,
            "branch" => Some(GitFollow::Branch(value.into())),
            "tag" => Some(GitFollow::Tag(value.into())),
            _unknown => {
                anyhow::bail!("'{url}' contains an unknown git spec '{key}' with value '{value}'")
            }
        };

        let tame_index::utils::UrlDir {
            dir_name,
            canonical,
        } = tame_index::utils::url_to_local_dir(url.as_str())?;

        Ok(Source::Git(GitSource {
            url: canonical.parse()?,
            ident: dir_name,
            rev,
            follow,
        }))
    }
}

/// Reads all of the custom registries configured in cargo config files.
///
/// Gathers all of the available .cargo/config(.toml) files, then applies
/// them in reverse order, as the more local ones override the ones higher
/// up in the hierarchy
///
/// See <https://doc.rust-lang.org/cargo/reference/config.html>
pub fn read_cargo_config(
    mut cargo_home_path: PathBuf,
    dir: PathBuf,
) -> anyhow::Result<Vec<Registry>> {
    use tracing::{error, info};

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

    let mut dir = dir.canonicalize_utf8()?;

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
                    error!("failed to read cargo config({config_path}): {e}");
                    continue;
                }
            };

            match toml::from_str(&config_contents) {
                Ok(cfg) => cfg,
                Err(e) => {
                    error!("failed to deserialize cargo config({config_path}): {e}");
                    continue;
                }
            }
        };

        if let Some(registries) = config.registries {
            for (name, value) in registries {
                info!("found registry '{name}' in {config_path}");
                if regs.insert(name, value).is_some() {
                    info!("registry overriden");
                }
            }
        }
    }

    // The sparse protocol is now the default as of 1.70, so we need to take that
    // into account, as well as if the default has been overriden by config or env
    // https://doc.rust-lang.org/cargo/reference/config.html#registriescrates-ioprotocol
    if let Some(crates_io) = regs.get_mut("crates-io") {
        *crates_io = Registry::crates_io(crates_io.protocol);
    } else {
        let protocol = if let Ok(protocol) = std::env::var("CARGO_REGISTRIES_CRATES_IO_PROTOCOL") {
            protocol
                .parse()
                .context("'CARGO_REGISTRIES_CRATES_IO_PROTOCOL' is invalid")?
        } else {
            RegistryProtocol::Sparse
        };

        regs.insert("crates-io".to_owned(), Registry::crates_io(protocol));
    }

    // Unfortunately, cargo uses the config.json file located in the indexes
    // root to determine the "dl" property of the registry, and isn't a property
    // that can be set in .cargo/config, but we really don't want to have to
    // fetch the index/cache this property before we can download any crates in
    // the lockfile that are referenced from the lockfile, so instead we try and
    // see if the user has set an environment variable of the form
    // CARGO_FETCHER_<UPPER_NAME>_DL and use that instead, otherwise we fallback
    // to the default that cargo uses, <index>/<crate_name>/<crate_version>/download
    Ok(regs
        .into_iter()
        .map(|(name, mut registry)| {
            if registry.config.is_none() {
                if let Ok(dl) = std::env::var(format!("CARGO_FETCHER_{}_DL", name.to_uppercase())) {
                    info!("Found DL location for registry '{name}'");
                    registry.config = Some(IndexConfig { dl, api: None });
                }
            }

            registry
        })
        .collect())
}

pub fn read_lock_files(
    lock_paths: Vec<PathBuf>,
    registries: Vec<Registry>,
) -> anyhow::Result<(Vec<Krate>, Vec<Arc<Registry>>)> {
    use tracing::{error, info, trace, warn};

    let packages = {
        let all_packages = lock_paths
            .into_par_iter()
            .map(|lock_path| -> anyhow::Result<Vec<Package>> {
                let toml_contents = std::fs::read_to_string(lock_path)?;
                let lock: LockContents = toml::from_str(&toml_contents)?;
                Ok(lock.package)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let mut packages = BTreeSet::<Package>::new();

        for lp in all_packages {
            packages.extend(lp);
        }

        packages
    };

    let mut krates = Vec::with_capacity(packages.len());

    let registries: Vec<_> = registries.into_iter().map(Arc::new).collect();
    let mut regs_to_sync = vec![0u32; registries.len()];

    for pkg in packages {
        let Some(source) = &pkg.source else {
            trace!("skipping 'path' source {}-{}", pkg.name, pkg.version);
            continue;
        };

        let krate = if let Some(reg_src) = source.strip_prefix("registry+") {
            // This will most likely be an extremely short list, so we just do a
            // linear search
            let Some((ind, registry)) = registries
                .iter()
                .enumerate()
                .find(|(_, reg)| {
                    source.ends_with(tame_index::CRATES_IO_INDEX) && reg.is_crates_io() || source.ends_with(reg.index.as_str())
                })
            else {
                warn!(
                    "skipping '{}:{}': unknown registry index '{reg_src}' encountered",
                    pkg.name, pkg.version
                );
                continue;
            };

            regs_to_sync[ind] += 1;

            let Some(chksum) = pkg.checksum else {
                warn!(
                    "skipping '{}:{}': unable to retrieve package checksum",
                    pkg.name, pkg.version,
                );
                continue;
            };

            Krate {
                name: pkg.name,
                version: pkg.version,
                source: Source::Registry(RegistrySource {
                    registry: registry.clone(),
                    chksum,
                }),
            }
        } else {
            let url = match Url::parse(source) {
                Ok(u) => u,
                Err(e) => {
                    error!(
                        "failed to parse url for '{}:{}': {e}",
                        pkg.name, pkg.version
                    );
                    continue;
                }
            };

            match Source::from_git_url(&url) {
                Ok(src) => Krate {
                    name: pkg.name,
                    version: pkg.version,
                    source: src,
                },
                Err(e) => {
                    error!(
                        "unable to use git url '{url}' for '{}:{}': {e}",
                        pkg.name, pkg.version
                    );
                    continue;
                }
            }
        };

        krates.push(krate);
    }

    Ok((
        krates,
        registries
            .into_iter()
            .zip(regs_to_sync)
            .filter_map(|(reg, count)| {
                if count > 0 {
                    Some(reg)
                } else {
                    info!("no sources using registry '{}'", reg.index);
                    None
                }
            })
            .collect(),
    ))
}

#[cfg(test)]
mod test {
    use super::*;

    // Ensures that krates are deduplicated correctly when loading multiple
    // lockfiles
    #[test]
    fn merges_lockfiles() {
        let (krates, regs) = read_lock_files(
            vec!["tests/multi_one.lock".into(), "tests/multi_two.lock".into()],
            vec![Registry::crates_io(RegistryProtocol::Sparse)],
        )
        .unwrap();

        let crates_io = regs[0].clone();

        let source = Source::Registry(RegistrySource {
            registry: crates_io.clone(),
            chksum: String::new(),
        });

        let expected = [
            Krate {
                name: "autometrics-macros".to_owned(),
                version: "0.4.1".to_owned(),
                source: source.clone(),
            },
            Krate {
                name: "axum".to_owned(),
                version: "0.6.17".to_owned(),
                source: source.clone(),
            },
            Krate {
                name: "axum".to_owned(),
                version: "0.6.18".to_owned(),
                source: source.clone(),
            },
            Krate {
                name: "axum-core".to_owned(),
                version: "0.3.4".to_owned(),
                source: source.clone(),
            },
            Krate {
                name: "axum-extra".to_owned(),
                version: "0.7.4".to_owned(),
                source: source.clone(),
            },
            Krate {
                name: "axum-live-view".to_owned(),
                version: "0.1.0".to_owned(),
                source: Source::from_git_url(&Url::parse("https://github.com/EmbarkStudios/axum-live-view?branch=main#165e11655aa0094388df1905da8758d7a4f60e3c").unwrap()).unwrap(),
            },
            Krate {
                name: "axum-live-view-macros".to_owned(),
                version: "0.1.0".to_owned(),
                source: Source::from_git_url(&Url::parse("https://github.com/EmbarkStudios/axum-live-view?branch=main#165e11655aa0094388df1905da8758d7a4f60e3c").unwrap()).unwrap(),
            },
            Krate {
                name: "axum-macros".to_owned(),
                version: "0.3.7".to_owned(),
                source: source.clone(),
            },
            Krate {
                name: "backtrace".to_owned(),
                version: "0.3.67".to_owned(),
                source,
            },
        ];

        assert_eq!(krates.len(), expected.len());

        for (actual, expected) in krates.into_iter().zip(expected.into_iter()) {
            assert_eq!(
                (actual.name, actual.version),
                (expected.name, expected.version)
            );
        }
    }
}
