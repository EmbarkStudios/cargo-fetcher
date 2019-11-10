use anyhow::{anyhow, bail, Context, Error};
#[allow(deprecated)]
use std::{
    hash::{Hash, Hasher, SipHasher},
    path::{Path, PathBuf},
};
use url::Url;

fn to_hex(num: u64) -> String {
    const CHARS: &[u8] = b"0123456789abcdef";

    let bytes = &[
        num as u8,
        (num >> 8) as u8,
        (num >> 16) as u8,
        (num >> 24) as u8,
        (num >> 32) as u8,
        (num >> 40) as u8,
        (num >> 48) as u8,
        (num >> 56) as u8,
    ];

    let mut output = vec![0u8; 16];

    let mut ind = 0;

    for &byte in bytes {
        output[ind] = CHARS[(byte >> 4) as usize];
        output[ind + 1] = CHARS[(byte & 0xf) as usize];

        ind += 2;
    }

    String::from_utf8(output).expect("valid utf-8 hex string")
}

fn hash_u64<H: Hash>(hashable: H) -> u64 {
    #[allow(deprecated)]
    let mut hasher = SipHasher::new_with_keys(0, 0);
    hashable.hash(&mut hasher);
    hasher.finish()
}

fn short_hash<H: Hash>(hashable: &H) -> String {
    to_hex(hash_u64(hashable))
}

pub struct Canonicalized(Url);

impl Canonicalized {
    pub(crate) fn ident(&self) -> String {
        // This is the same identity function used by cargo
        let ident = self
            .0
            .path_segments()
            .and_then(|mut s| s.next_back())
            .unwrap_or("");

        let ident = if ident == "" { "_empty" } else { ident };

        format!("{}-{}", ident, short_hash(&self.0))
    }
}

impl AsRef<Url> for Canonicalized {
    fn as_ref(&self) -> &Url {
        &self.0
    }
}

impl Into<Url> for Canonicalized {
    fn into(self) -> Url {
        self.0
    }
}

impl std::convert::TryFrom<&Url> for Canonicalized {
    type Error = Error;

    fn try_from(url: &Url) -> Result<Self, Self::Error> {
        // This is the same canonicalization that cargo does, except the URLs
        // they use don't have any query params or fragments, even though
        // they do occur in Cargo.lock files

        let mut url = url.clone();

        // cannot-be-a-base-urls (e.g., `github.com:rust-lang-nursery/rustfmt.git`)
        // are not supported.
        if url.cannot_be_a_base() {
            bail!(
                "invalid url `{}`: cannot-be-a-base-URLs are not supported",
                url
            )
        }

        // Strip a trailing slash.
        if url.path().ends_with('/') {
            url.path_segments_mut().unwrap().pop_if_empty();
        }

        // HACK: for GitHub URLs specifically, just lower-case
        // everything. GitHub treats both the same, but they hash
        // differently, and we're gonna be hashing them. This wants a more
        // general solution, and also we're almost certainly not using the
        // same case conversion rules that GitHub does. (See issue #84.)
        if url.host_str() == Some("github.com") {
            url.set_scheme("https").unwrap();
            let path = url.path().to_lowercase();
            url.set_path(&path);
        }

        // Repos can generally be accessed with or without `.git` extension.
        let needs_chopping = url.path().ends_with(".git");
        if needs_chopping {
            let last = {
                let last = url.path_segments().unwrap().next_back().unwrap();
                last[..last.len() - 4].to_owned()
            };
            url.path_segments_mut().unwrap().pop().push(&last);
        }

        // Ensure there are no fragments, eg sha-1 revision specifiers
        url.set_fragment(None);
        // Strip off any query params, they aren't relevant for the hash
        url.set_query(None);

        Ok(Self(url))
    }
}

pub fn determine_cargo_root(explicit: Option<PathBuf>) -> Result<PathBuf, Error> {
    let root_dir = explicit
        .or_else(|| std::env::var_os("CARGO_HOME").map(PathBuf::from))
        .or_else(|| {
            app_dirs2::data_root(app_dirs2::AppDataType::UserConfig)
                .map(|hd| hd.join(".cargo"))
                .ok()
        });

    let root_dir = root_dir.ok_or_else(|| anyhow!("unable to determine cargo root"))?;

    // There should always be a bin/cargo(.exe) relative to the root directory, at a minimum
    // there are probably ways to have setups where this doesn't hold true, but this is simple
    // and can be fixed later
    let cargo_path = {
        let mut cpath = root_dir.join("bin/cargo");

        if cfg!(target_os = "windows") {
            cpath.set_extension("exe");
        }

        cpath
    };

    if !cargo_path.exists() {
        return Err(anyhow!(
            "cargo root {} does not seem to contain the cargo binary",
            root_dir.display()
        ));
    }

    Ok(root_dir)
}

pub fn convert_response(
    res: &mut reqwest::Response,
) -> Result<http::Response<bytes::Bytes>, Error> {
    use bytes::BufMut;

    let body = bytes::BytesMut::with_capacity(res.content_length().unwrap_or(4 * 1024) as usize);
    let mut writer = body.writer();
    res.copy_to(&mut writer)?;
    let body = writer.into_inner();

    let mut builder = http::Response::builder();

    builder.status(res.status()).version(res.version());

    let headers = builder
        .headers_mut()
        .ok_or_else(|| anyhow!("failed to convert response headers"))?;

    headers.extend(
        res.headers()
            .into_iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );

    Ok(builder.body(body.freeze())?)
}

pub(crate) fn unpack_tar<R: std::io::Read, P: AsRef<Path>>(
    stream: R,
    dir: P,
) -> Result<R, (R, Error)> {
    let mut archive_reader = tar::Archive::new(stream);
    archive_reader.set_preserve_permissions(false);

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
            anyhow!("failed to unpack: {:#?}", e),
        ));
    }

    Ok(archive_reader.into_inner())
}

// All of cargo's checksums are currently SHA256
pub(crate) fn validate_checksum(buffer: &[u8], expected: &str) -> Result<(), Error> {
    if expected.len() != 64 {
        bail!(
            "hex checksum length is {} instead of expected 64",
            expected.len()
        );
    }

    let content_digest = ring::digest::digest(&ring::digest::SHA256, buffer);
    let digest = content_digest.as_ref();

    for (ind, exp) in expected.as_bytes().chunks(2).enumerate() {
        let mut cur;

        match exp[0] {
            b'A'..=b'F' => cur = exp[0] - b'A' + 10,
            b'a'..=b'f' => cur = exp[0] - b'a' + 10,
            b'0'..=b'9' => cur = exp[0] - b'0',
            c => bail!("invalid byte in expected checksum string {}", c),
        }

        cur <<= 4;

        match exp[1] {
            b'A'..=b'F' => cur |= exp[1] - b'A' + 10,
            b'a'..=b'f' => cur |= exp[1] - b'a' + 10,
            b'0'..=b'9' => cur |= exp[1] - b'0',
            c => bail!("invalid byte in expected checksum string {}", c),
        }

        if digest[ind] != cur {
            bail!("checksum mismatch, expected {}", expected);
        }
    }

    Ok(())
}

fn parse_s3_url(url: &Url) -> Result<crate::S3Location<'_>, Error> {
    let host = url.host().context("url has no host")?;

    let host_dns = match host {
        url::Host::Domain(h) => h,
        _ => anyhow::bail!("host name is an IP"),
    };

    // TODO: Support localhost without bucket and region, for testing

    // We only support virtual-hosted-style references as path style is being deprecated
    // mybucket.s3-us-west-2.amazonaws.com
    // https://aws.amazon.com/blogs/aws/amazon-s3-path-deprecation-plan-the-rest-of-the-story/
    if host_dns.contains("s3") {
        let mut bucket = None;
        let mut region = None;
        let mut host = None;

        for part in host_dns.split('.') {
            if part.is_empty() {
                anyhow::bail!("malformed host name detected");
            }

            if bucket.is_none() {
                bucket = Some(part);
                continue;
            }

            if part.starts_with("s3") && region.is_none() {
                let rgn = &part[2..];

                if rgn.starts_with('-') {
                    region = Some((&rgn[1..], part.len()));
                } else {
                    region = Some(("us-east-1", part.len()));
                }
            } else if region.is_none() {
                bucket = Some(&host_dns[..bucket.as_ref().unwrap().len() + 1 + part.len()]);
            } else if host.is_none() {
                host = Some(
                    &host_dns[2 // for the 2 dots
                        + bucket.as_ref().unwrap().len()
                        + region.as_ref().unwrap().1..],
                );
                break;
            }
        }

        let bucket = bucket.context("bucket not specified")?;
        let region = region.context("region not specified")?.0;
        let host = host.context("host not specified")?;

        let loc = crate::S3Location {
            bucket,
            region,
            host,
            prefix: if !url.path().is_empty() {
                &url.path()[1..]
            } else {
                url.path()
            },
        };

        Ok(loc)
    } else {
        anyhow::bail!("not an s3 url");
    }
}

pub fn parse_cloud_location(url: &Url) -> Result<crate::CloudLocation<'_>, Error> {
    match url.scheme() {
        #[cfg(feature = "gcs")]
        "gs" => {
            let bucket = url.domain().context("url doesn't contain a bucket")?;
            // Remove the leading slash that url gives us
            let path = if !url.path().is_empty() {
                &url.path()[1..]
            } else {
                url.path()
            };

            let loc = crate::GcsLocation {
                bucket,
                prefix: path,
            };

            Ok(crate::CloudLocation::Gcs(loc))
        }
        #[cfg(not(feature = "gcs"))]
        "gs" => {
            anyhow::bail!("GCS support was not enabled, you must compile with the 'gcs' feature")
        }
        "http" | "https" => {
            let s3 = parse_s3_url(url).context("failed to parse s3 url")?;

            if cfg!(feature = "s3") {
                Ok(crate::CloudLocation::S3(s3))
            } else {
                anyhow::bail!("S3 support was not enabled, you must compile with the 's3' feature")
            }
        }
        scheme => anyhow::bail!("the scheme '{}' is not supported", scheme),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::convert::TryFrom;

    #[test]
    fn canonicalizes_urls() {
        let url = Url::parse("git+https://github.com/EmbarkStudios/cpal.git?rev=d59b4de#d59b4decf72a96932a1482cc27fe4c0b50c40d32").unwrap();
        let canonicalized = Canonicalized::try_from(&url).unwrap();

        assert_eq!(
            "https://github.com/embarkstudios/cpal",
            canonicalized.as_ref().as_str()
        );
    }

    #[test]
    fn idents_urls() {
        let url = Url::parse("git+https://github.com/gfx-rs/genmesh?rev=71abe4d").unwrap();
        let canonicalized = Canonicalized::try_from(&url).unwrap();
        let ident = canonicalized.ident();

        assert_eq!(ident, "genmesh-401fe503e87439cc");

        let url = Url::parse("git+https://github.com/EmbarkStudios/cpal?rev=d59b4de#d59b4decf72a96932a1482cc27fe4c0b50c40d32").unwrap();
        let canonicalized = Canonicalized::try_from(&url).unwrap();
        let ident = canonicalized.ident();

        assert_eq!(ident, "cpal-a7ffd7cabefac714");
    }

    #[test]
    fn validates_checksums() {
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";

        validate_checksum(b"hello world", expected).unwrap();
    }

    #[test]
    fn parses_s3_virtual_hosted_style() {
        let url = Url::parse("http://johnsmith.net.s3.amazonaws.com/homepage.html").unwrap();
        let loc = parse_s3_url(&url).unwrap();

        assert_eq!(loc.bucket, "johnsmith.net");
        assert_eq!(loc.region, "us-east-1");
        assert_eq!(loc.host, "amazonaws.com");
        assert_eq!(loc.prefix, "homepage.html");

        let url =
            Url::parse("http://johnsmith.eu.s3-eu-west-1.amazonaws.com/homepage.html").unwrap();
        let loc = parse_s3_url(&url).unwrap();

        assert_eq!(loc.bucket, "johnsmith.eu");
        assert_eq!(loc.region, "eu-west-1");
        assert_eq!(loc.host, "amazonaws.com");
        assert_eq!(loc.prefix, "homepage.html");

        let url = Url::parse("http://mybucket.s3-us-west-2.amazonaws.com/some_prefix/").unwrap();
        let loc = parse_s3_url(&url).unwrap();

        assert_eq!(loc.bucket, "mybucket");
        assert_eq!(loc.region, "us-west-2");
        assert_eq!(loc.host, "amazonaws.com");
        assert_eq!(loc.prefix, "some_prefix/");

        let url = Url::parse("http://mybucket.with.many.dots.in.it.s3.amazonaws.com/some_prefix/")
            .unwrap();
        let loc = parse_s3_url(&url).unwrap();

        assert_eq!(loc.bucket, "mybucket.with.many.dots.in.it");
        assert_eq!(loc.region, "us-east-1");
        assert_eq!(loc.host, "amazonaws.com");
        assert_eq!(loc.prefix, "some_prefix/");
    }
}
