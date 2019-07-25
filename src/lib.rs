use failure::Error;
use std::{collections::BTreeMap, fmt, io::Read, path::Path};
use url::Url;

pub mod fetch;
pub mod upload;
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

pub enum Source {
    CratesIo(String),
    Git { url: Url, ident: String },
}

pub struct Krate {
    pub name: String,
    pub version: String,
    pub source: Source,
}

impl Krate {
    pub fn gcs_id(&self) -> &str {
        match &self.source {
            Source::CratesIo(chksum) => chksum,
            Source::Git { ident, .. } => ident,
        }
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
            Source::Git { ident, .. } => write!(f, "{}", &ident[..ident.len() - 8]),
        }
    }
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

            let rev = match url.query_pairs().find(|(k, _)| k == "rev") {
                Some((_, rev)) => {
                    if rev.len() < 7 {
                        log::error!(
                            "skipping {}-{}: revision length was too short",
                            p.name,
                            p.version
                        );
                        continue;
                    } else {
                        rev
                    }
                }
                None => {
                    log::warn!("skipping {}-{}: revision not specified", p.name, p.version);
                    continue;
                }
            };

            // This will handle
            // 1. 7 character short_id
            // 2. Full 40 character sha-1
            // 3. 7 character short_id#sha-1
            let rev = &rev[..7];

            let canonicalized = match util::canonicalize_url(&url) {
                Ok(i) => i,
                Err(e) => {
                    log::warn!("skipping {}-{}: {}", p.name, p.version, e);
                    continue;
                }
            };

            let ident = util::ident(&canonicalized);

            krates.push(Krate {
                name: p.name,
                version: p.version,
                source: Source::Git {
                    url: canonicalized,
                    ident: format!("{}-{}", ident, rev),
                },
            })
        }
    }

    Ok(krates)
}

pub fn convert_response(
    res: &mut reqwest::Response,
) -> Result<tame_gcs::http::Response<bytes::Bytes>, Error> {
    use bytes::BufMut;

    let body = bytes::BytesMut::with_capacity(res.content_length().unwrap_or(4 * 1024) as usize);
    let mut writer = body.writer();
    res.copy_to(&mut writer)?;
    let body = writer.into_inner();

    let mut builder = tame_gcs::http::Response::builder();

    builder.status(res.status()).version(res.version());

    let headers = builder
        .headers_mut()
        .ok_or_else(|| failure::format_err!("failed to convert response headers"))?;

    headers.extend(
        res.headers()
            .into_iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );

    Ok(builder.body(body.freeze())?)
}

pub fn unpack_tar<R: Read, P: AsRef<Path>>(stream: R, dir: P) -> Result<R, (R, Error)> {
    let mut archive_reader = tar::Archive::new(stream);

    let dir = dir.as_ref();

    if let Err(e) = archive_reader.unpack(dir) {
        // Attempt to remove anything that may have been written so that we
        // _hopefully_ don't actually mess up cargo
        if dir.exists() {
            if let Err(e) = remove_dir_all::remove_dir_all(dir) {
                log::error!(
                    "error trying to remove contents of {}: {}",
                    dir.display(),
                    e
                );
            }
        }

        return Err((
            archive_reader.into_inner(),
            failure::format_err!("failed to unpack: {}", e),
        ));
    }

    Ok(archive_reader.into_inner())
}
