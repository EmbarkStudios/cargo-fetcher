use crate::{util, Krate, Path, PathBuf};
use anyhow::Context as _;
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap},
    convert::TryFrom,
    hash::{Hash, Hasher},
    sync::Arc,
};
use url::Url;

/// The canonical git index location
pub const CRATES_IO_URL: &str = "https://github.com/rust-lang/crates.io-index";
/// The crates.io sparse index HTTP location, note the `sparse+` is intentional
/// as this is used as part of the hash
pub const CRATES_IO_SPARSE_URL: &str = "sparse+https://index.crates.io/";
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
    dl: Option<String>,
    #[serde(default)]
    pub protocol: RegistryProtocol,
}

impl Registry {
    #[inline]
    pub fn new(index: impl AsRef<str>, dl: Option<String>) -> anyhow::Result<Self> {
        let index = Url::parse(index.as_ref())?;
        Ok(Self {
            index,
            dl,
            protocol: Default::default(),
        })
    }

    #[inline]
    pub fn crates_io(protocol: RegistryProtocol) -> Self {
        let index_url = match protocol {
            RegistryProtocol::Git => CRATES_IO_URL,
            RegistryProtocol::Sparse => CRATES_IO_SPARSE_URL,
        };

        Self {
            index: Url::parse(index_url).unwrap(),
            dl: Some(CRATES_IO_DL.to_owned()),
            protocol,
        }
    }

    /// Gets the download url for the crate
    ///
    /// See <https://doc.rust-lang.org/cargo/reference/registries.html#index-format>
    /// for more info
    pub fn download_url(&self, krate: &Krate) -> String {
        match &self.dl {
            Some(dl) => {
                let mut dl = dl.clone();

                while let Some(start) = dl.find("{crate}") {
                    dl.replace_range(start..start + 7, &krate.name);
                }

                while let Some(start) = dl.find("{version}") {
                    dl.replace_range(start..start + 9, &krate.version);
                }

                if dl.contains("{prefix}") || dl.contains("{lowerprefix}") {
                    let prefix = get_crate_prefix(&krate.name);

                    while let Some(start) = dl.find("{prefix}") {
                        dl.replace_range(start..start + 8, &prefix);
                    }

                    if dl.contains("{lowerprefix}") {
                        let prefix = prefix.to_lowercase();

                        while let Some(start) = dl.find("{lowerprefix}") {
                            dl.replace_range(start..start + 13, &prefix);
                        }
                    }
                }

                dl
            }
            None => {
                format!("{}/{}/{}/download", self.index, krate.name, krate.version)
            }
        }
    }

    pub fn short_name(&self) -> String {
        let hash = util::short_hash(self);
        let ident = self.index.host_str().unwrap_or("").to_string();
        format!("{ident}-{hash}")
    }

    pub fn cache_dir(&self, root: &Path) -> PathBuf {
        let mut cdir = root.join(crate::sync::CACHE_DIR);
        cdir.push(self.short_name());
        cdir
    }

    pub fn src_dir(&self, root: &Path) -> PathBuf {
        let mut cdir = root.join(crate::sync::SRC_DIR);
        cdir.push(self.short_name());
        cdir
    }

    pub fn sync_dirs(&self, root: &Path) -> (PathBuf, PathBuf) {
        let ident = self.short_name();

        let mut cdir = root.join(crate::sync::CACHE_DIR);
        cdir.push(&ident);

        let mut sdir = root.join(crate::sync::SRC_DIR);
        sdir.push(&ident);

        (cdir, sdir)
    }

    #[inline]
    pub fn is_crates_io(&self) -> bool {
        match self.protocol {
            RegistryProtocol::Git => self.index.as_str() == CRATES_IO_URL,
            RegistryProtocol::Sparse => self.index.as_str() == CRATES_IO_SPARSE_URL,
        }
    }
}

impl Hash for Registry {
    fn hash<S: Hasher>(&self, into: &mut S) {
        // See src/cargo/core/source/source_id.rs
        let (kind, url): (u64, _) = match self.protocol {
            RegistryProtocol::Git => {
                let canonical: util::Canonicalized = (&self.index).try_into().unwrap();
                (2, canonical.0)
            }
            RegistryProtocol::Sparse => (3, self.index.clone()),
        };
        kind.hash(into);
        url.as_str().hash(into);
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
                let (a_kind, a_url) = a.split_once('+').expect("invalid source id");
                let (b_kind, b_url) = b.split_once('+').expect("invalid source id");

                match a_kind.cmp(b_kind) {
                    Ordering::Equal => {}
                    other => return other,
                }

                if a_kind == "registry" {
                    a_url.cmp(b_url)
                } else if a_kind == "git" {
                    let a_url = Url::parse(a_url).expect("invalid url");
                    let b_url = Url::parse(b_url).expect("invalid url");

                    let a_can: util::Canonicalized =
                        (&a_url).try_into().expect("unable to canonicalize url");
                    let b_can: util::Canonicalized =
                        (&b_url).try_into().expect("unable to canonicalize url");

                    a_can.0.cmp(&b_can.0)
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
pub enum Source {
    Registry {
        registry: Arc<Registry>,
        chksum: String,
    },
    Git {
        url: Url,
        rev: String,
        ident: String,
    },
}

impl Source {
    pub fn from_git_url(url: &Url) -> anyhow::Result<Self> {
        let rev = url.fragment().context("url doesn't contain a revision")?;

        // The revision fragment in the cargo.lock will always be the full
        // sha-1, but we only use the short-id since that is how cargo calculates
        // the local identity of a specific git checkout
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
            let mut ccmd = std::process::Command::new("cargo");
            ccmd.arg("-V").stdout(std::process::Stdio::piped());
            let output = ccmd.output().context("unable to spawn cargo")?;

            anyhow::ensure!(
                output.status.success(),
                "failed to run cargo to get version information"
            );

            let output =
                String::from_utf8(output.stdout).context("cargo output was not valid utf-8")?;
            // cargo <semver> (<hash> <date>)
            let semver = output
                .split(' ')
                .nth(1)
                .context("cargo version output was malformed")?;
            // <major>.<minor>.<patch>
            let minor = semver
                .split('.')
                .nth(1)
                .context("context semver version was malformed")?;
            let minor: u32 = minor
                .parse()
                .context("failed to parse cargo minor version")?;

            if minor < 70 {
                RegistryProtocol::Git
            } else {
                RegistryProtocol::Sparse
            }
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
            if registry.dl.is_none() {
                if let Ok(dl) = std::env::var(format!("CARGO_FETCHER_{}_DL", name.to_uppercase())) {
                    info!("Found DL location for registry '{name}'");
                    registry.dl = Some(dl);
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
                    source.ends_with(CRATES_IO_URL) && reg.is_crates_io() || source.ends_with(reg.index.as_str())
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
                source: Source::Registry {
                    registry: registry.clone(),
                    chksum,
                },
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

/// Converts a crate name into its prefix form
///
/// See <https://doc.rust-lang.org/cargo/reference/registries.html#index-format>
/// for more details
pub fn get_crate_prefix(name: &str) -> String {
    match name.chars().count() {
        0 => unreachable!("things have gone awry"),
        1 => "1".to_owned(),
        2 => "2".to_owned(),
        3 => format!("3/{}", name.chars().next().unwrap()),
        _ => {
            let mut pfx = String::with_capacity(5);

            let mut citer = name.chars();
            pfx.push(citer.next().unwrap());
            pfx.push(citer.next().unwrap());
            pfx.push('/');
            pfx.push(citer.next().unwrap());
            pfx.push(citer.next().unwrap());

            pfx
        }
    }
}

#[cfg(test)]
mod test {
    use super::get_crate_prefix as gcp;
    use super::*;

    macro_rules! krate {
        ($name:expr, $vs:expr, $reg:expr) => {
            crate::Krate {
                name: $name.to_owned(),
                version: $vs.to_owned(),
                source: super::Source::Registry {
                    registry: $reg.clone(),
                    chksum: "".to_owned(),
                },
            }
        };
    }

    #[test]
    fn gets_crate_prefix() {
        assert_eq!(gcp("a"), "1");
        assert_eq!(gcp("ab"), "2");
        assert_eq!(gcp("abc"), "3/a");
        assert_eq!(gcp("Åbc"), "3/Å");
        assert_eq!(gcp("AbCd"), "Ab/Cd");
        assert_eq!(gcp("äBcDe"), "äB/cD");
    }

    #[test]
    fn gets_crates_io_download_url() {
        let crates_io = Arc::new(Registry::crates_io(RegistryProtocol::Sparse));

        assert_eq!(
            crates_io.download_url(&krate!("a", "1.0.0", crates_io)),
            "https://static.crates.io/crates/a/a-1.0.0.crate"
        );
        assert_eq!(
            crates_io.download_url(&krate!("aB", "0.1.0", crates_io)),
            "https://static.crates.io/crates/aB/aB-0.1.0.crate"
        );
        assert_eq!(
            crates_io.download_url(&krate!("aBc", "0.1.0", crates_io)),
            "https://static.crates.io/crates/aBc/aBc-0.1.0.crate"
        );
        assert_eq!(
            crates_io.download_url(&krate!("aBc-123", "0.1.0", crates_io)),
            "https://static.crates.io/crates/aBc-123/aBc-123-0.1.0.crate"
        );
    }

    #[test]
    fn gets_other_download_url() {
        let crates_io = Arc::new(
            super::Registry::new(
                "https://dl.cloudsmith.io/ohhi/embark/rust/cargo/index.git",
                Some(
                    "https://dl.cloudsmith.io/ohhi/embark/rust/cargo/{crate}-{version}.crate"
                        .to_owned(),
                ),
            )
            .unwrap(),
        );

        assert_eq!(
            crates_io.download_url(&krate!("a", "1.0.0", crates_io)),
            "https://dl.cloudsmith.io/ohhi/embark/rust/cargo/a-1.0.0.crate"
        );
        assert_eq!(
            crates_io.download_url(&krate!("aB", "0.1.0", crates_io)),
            "https://dl.cloudsmith.io/ohhi/embark/rust/cargo/aB-0.1.0.crate"
        );
        assert_eq!(
            crates_io.download_url(&krate!("aBc", "0.1.0", crates_io)),
            "https://dl.cloudsmith.io/ohhi/embark/rust/cargo/aBc-0.1.0.crate"
        );
        assert_eq!(
            crates_io.download_url(&krate!("aBc-123", "0.1.0", crates_io)),
            "https://dl.cloudsmith.io/ohhi/embark/rust/cargo/aBc-123-0.1.0.crate"
        );
    }

    #[test]
    fn gets_other_complex_download_url() {
        let crates_io = Arc::new(super::Registry::new(
            "https://complex.io/ohhi/embark/rust/cargo/index.git",
            Some(
                "https://complex.io/ohhi/embark/rust/cargo/{lowerprefix}/{crate}/{crate}/{prefix}-{version}"
                    .to_owned(),
            ),
        ).unwrap());

        assert_eq!(
            crates_io.download_url(&krate!("a", "1.0.0", crates_io)),
            "https://complex.io/ohhi/embark/rust/cargo/1/a/a/1-1.0.0"
        );
        assert_eq!(
            crates_io.download_url(&krate!("aB", "0.1.0", crates_io)),
            "https://complex.io/ohhi/embark/rust/cargo/2/aB/aB/2-0.1.0"
        );
        assert_eq!(
            crates_io.download_url(&krate!("aBc", "0.1.0", crates_io)),
            "https://complex.io/ohhi/embark/rust/cargo/3/a/aBc/aBc/3/a-0.1.0"
        );
        assert_eq!(
            crates_io.download_url(&krate!("aBc-123", "0.1.0", crates_io)),
            "https://complex.io/ohhi/embark/rust/cargo/ab/c-/aBc-123/aBc-123/aB/c--0.1.0"
        );
    }

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

        let expected = [
            Krate {
                name: "autometrics-macros".to_owned(),
                version: "0.4.1".to_owned(),
                source: Source::Registry {
                    registry: crates_io.clone(),
                    chksum: String::new(),
                },
            },
            Krate {
                name: "axum".to_owned(),
                version: "0.6.17".to_owned(),
                source: Source::Registry {
                    registry: crates_io.clone(),
                    chksum: String::new(),
                },
            },
            Krate {
                name: "axum".to_owned(),
                version: "0.6.18".to_owned(),
                source: Source::Registry {
                    registry: crates_io.clone(),
                    chksum: String::new(),
                },
            },
            Krate {
                name: "axum-core".to_owned(),
                version: "0.3.4".to_owned(),
                source: Source::Registry {
                    registry: crates_io.clone(),
                    chksum: String::new(),
                },
            },
            Krate {
                name: "axum-extra".to_owned(),
                version: "0.7.4".to_owned(),
                source: Source::Registry {
                    registry: crates_io.clone(),
                    chksum: String::new(),
                },
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
                source: Source::Registry {
                    registry: crates_io.clone(),
                    chksum: String::new(),
                },
            },
            Krate {
                name: "backtrace".to_owned(),
                version: "0.3.67".to_owned(),
                source: Source::Registry {
                    registry: crates_io,
                    chksum: String::new(),
                },
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
