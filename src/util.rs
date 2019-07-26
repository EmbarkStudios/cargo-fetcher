use failure::{format_err, Error};
#[allow(deprecated)]
use std::{
    hash::{Hash, Hasher, SipHasher},
    path::{Path, PathBuf},
};
use url::Url;

pub fn to_hex(num: u64) -> String {
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

pub fn hash_u64<H: Hash>(hashable: H) -> u64 {
    #[allow(deprecated)]
    let mut hasher = SipHasher::new_with_keys(0, 0);
    hashable.hash(&mut hasher);
    hasher.finish()
}

pub fn short_hash<H: Hash>(hashable: &H) -> String {
    to_hex(hash_u64(hashable))
}

pub fn ident(url: &Url) -> String {
    let ident = url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or("");

    let ident = if ident == "" { "_empty" } else { ident };

    format!("{}-{}", ident, short_hash(&url))
}

// Some hacks and heuristics for making equivalent URLs hash the same.
pub fn canonicalize_url(url: &Url) -> Result<Url, Error> {
    let mut url = url.clone();

    // cannot-be-a-base-urls (e.g., `github.com:rust-lang-nursery/rustfmt.git`)
    // are not supported.
    if url.cannot_be_a_base() {
        failure::bail!(
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

    Ok(url)
}

pub fn determine_cargo_root(explicit: Option<PathBuf>) -> Result<PathBuf, Error> {
    let root_dir = explicit
        .or_else(|| std::env::var_os("CARGO_HOME").map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|hd| hd.join(".cargo")));

    let root_dir = root_dir.ok_or_else(|| format_err!("unable to determine cargo root"))?;

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
        return Err(format_err!(
            "cargo root {} does not seem to contain the cargo binary",
            root_dir.display()
        ));
    }

    Ok(root_dir)
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
        .ok_or_else(|| format_err!("failed to convert response headers"))?;

    headers.extend(
        res.headers()
            .into_iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );

    Ok(builder.body(body.freeze())?)
}

pub fn unpack_tar<R: std::io::Read, P: AsRef<Path>>(stream: R, dir: P) -> Result<R, (R, Error)> {
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
            format_err!("failed to unpack: {}", e),
        ));
    }

    Ok(archive_reader.into_inner())
}
