use crate::{util, Krate};
use anyhow::{Context, Error};
use serde::{Deserialize, Serialize};
use std::{
    convert::TryFrom,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
};
use url::Url;

pub const CRATES_IO_URL: &str = "https://github.com/rust-lang/crates.io-index";
/// The normal crates.io DL url, note that this is not the one actually advertised
/// by cargo (<https://crates.io/api/v1/crates>) as that is just a redirect to this
/// location, so obviously this will break terribly if crates.io ever changes the
/// actual storage location, but that's unlikely, and is easy to fix if it ever
/// does happen
pub const CRATES_IO_DL: &str = "https://static.crates.io/crates/{crate}/{crate}-{version}.crate";

#[derive(Deserialize)]
pub struct CargoConfig {
    pub registries: Option<std::collections::HashMap<String, Registry>>,
}

#[derive(Eq, Clone, Debug, Serialize, Deserialize)]
pub struct Registry {
    pub index: Url,
    dl: Option<String>,
}

impl Registry {
    pub fn new(index: impl AsRef<str>, dl: Option<String>) -> Result<Self, Error> {
        let index = Url::parse(index.as_ref())?;
        Ok(Self { index, dl })
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
        format!("{}-{}", ident, hash)
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
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            index: Url::parse(CRATES_IO_URL).unwrap(),
            dl: Some(CRATES_IO_DL.to_owned()),
        }
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

pub fn determine_cargo_root(explicit: Option<&PathBuf>) -> Result<PathBuf, Error> {
    match explicit {
        Some(exp) => home::cargo_home_with_cwd(exp).context("failed to retrieve cargo home"),
        None => home::cargo_home().context("failed to retrieve cargo home for cwd"),
    }
}

#[derive(Deserialize)]
struct LockContents {
    package: Vec<Package>,
    /// V2 lock format doesn't have a metadata section
    #[serde(default)]
    metadata: std::collections::BTreeMap<String, String>,
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
    pub fn from_git_url(url: &Url) -> Result<Self, Error> {
        let rev = match url.query_pairs().find(|(k, _)| k == "rev") {
            Some((_, rev)) => {
                if rev.len() < 7 {
                    anyhow::bail!("revision specifier {} is too short", rev);
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
) -> Result<Vec<Registry>, Error> {
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

    let mut regs = std::collections::HashMap::new();

    regs.insert("crates-io".to_owned(), Registry::default());

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
                info!("found registry '{}' in {}", name, config_path.display());
                if regs.insert(name, value).is_some() {
                    info!("registry overriden");
                }
            }
        }
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
                    info!("Found DL location for registry '{}'", name);
                    registry.dl = Some(dl);
                }
            }

            registry
        })
        .collect())
}

pub fn read_lock_file<P: AsRef<std::path::Path>>(
    lock_path: P,
    registries: Vec<Registry>,
) -> Result<(Vec<Krate>, Vec<Arc<Registry>>), Error> {
    use std::fmt::Write;
    use tracing::{error, info, trace, warn};

    let mut locks: LockContents = {
        let toml_contents = std::fs::read_to_string(lock_path)?;
        toml::from_str(&toml_contents)?
    };

    let mut lookup = String::with_capacity(128);
    let mut krates = Vec::with_capacity(locks.package.len());

    let registries: Vec<_> = registries.into_iter().map(Arc::new).collect();
    let mut regs_to_sync = vec![0u32; registries.len()];

    for pkg in locks.package {
        let source = match pkg.source.as_ref() {
            Some(s) => s,
            None => {
                trace!("skipping 'path' source {}-{}", pkg.name, pkg.version);
                continue;
            }
        };

        if let Some(reg_src) = source.strip_prefix("registry+") {
            // This will most likely be an extremely short list, so we just do a
            // linear search
            let registry = match registries
                .iter()
                .enumerate()
                .find(|(_, reg)| source.ends_with(reg.index.as_str()))
            {
                Some((ind, reg)) => {
                    regs_to_sync[ind] += 1;
                    reg
                }
                None => {
                    warn!(
                        "skipping '{}:{}': unknown registry index '{}' encountered",
                        pkg.name, pkg.version, reg_src
                    );
                    continue;
                }
            };

            let chksum = match pkg.checksum {
                Some(chksum) => chksum,
                None => {
                    lookup.clear();
                    let _ = write!(
                        &mut lookup,
                        "checksum {} {} ({})",
                        pkg.name, pkg.version, source
                    );

                    match locks.metadata.remove(&lookup) {
                        Some(chksum) => chksum,
                        None => {
                            warn!(
                                "skipping '{}:{}': unable to retrieve package checksum",
                                pkg.name, pkg.version,
                            );
                            continue;
                        }
                    }
                }
            };

            krates.push(Krate {
                name: pkg.name,
                version: pkg.version,
                source: Source::Registry {
                    registry: registry.clone(),
                    chksum,
                },
            });
        } else {
            // We support exactly one form of git sources, rev specififers
            // eg. git+https://github.com/EmbarkStudios/rust-build-helper?rev=9135717#91357179ba2ce6ec7e430a2323baab80a8f7d9b3
            let url = match Url::parse(source) {
                Ok(u) => u,
                Err(e) => {
                    error!(
                        "failed to parse url for '{}:{}': {}",
                        pkg.name, pkg.version, e
                    );
                    continue;
                }
            };

            match Source::from_git_url(&url) {
                Ok(src) => {
                    krates.push(Krate {
                        name: pkg.name,
                        version: pkg.version,
                        source: src,
                    });
                }
                Err(e) => {
                    error!(
                        "unable to use git url {} for '{}:{}': {}",
                        url, pkg.name, pkg.version, e
                    );
                }
            }
        }
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
    use super::Arc;

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
        let crates_io = Arc::new(super::Registry::default());

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
                "https://dl.cloudsmith.io/ohhi/embark/rust/cargo/index.git".to_owned(),
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
            "https://complex.io/ohhi/embark/rust/cargo/index.git".to_owned(),
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
}
