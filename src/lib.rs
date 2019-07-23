use failure::Error;
use std::{collections::BTreeMap, path::Path};

pub mod fetch;
pub mod upload;

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

pub struct Krate {
    pub name: String,
    pub version: String,
    pub checksum: String,
}

pub fn gather<P: AsRef<Path>>(lock_path: P) -> Result<Vec<Krate>, Error> {
    use std::fmt::Write;

    let mut locks: LockContents = {
        let toml_contents = std::fs::read_to_string(lock_path)?;
        toml::from_str(&toml_contents)?
    };

    let mut lookup = String::with_capacity(128);
    let mut krates = Vec::with_capacity(locks.package.len());

    for p in locks.package {
        if p.source.as_ref().map(|s| s.as_ref())
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        {
            continue;
        }

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
                checksum: chksum,
            })
        }

        lookup.clear();
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

#[cfg(test)]
mod test {
    #[test]
    fn gather_self() {
        let krates = super::gather("Cargo.lock").expect("gathered");

        for krate in krates {
            println!("{} @ {} - {}", krate.name, krate.version, krate.checksum);
        }

        panic!("checking!");
    }
}
