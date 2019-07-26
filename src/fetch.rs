use crate::{util, Context, Krate, Source};
use bytes::{BufMut, Bytes, BytesMut};
use failure::{bail, Error, ResultExt};
use log::debug;
use reqwest::Client;
use std::process::Command;
use tame_gcs::objects::Object;

pub fn from_crates_io(client: &Client, krate: &Krate) -> Result<Bytes, Error> {
    match &krate.source {
        Source::CratesIo(chksum) => {
            let url = format!(
                "https://static.crates.io/crates/{}/{}-{}.crate",
                krate.name, krate.name, krate.version
            );

            let mut response = client.get(&url).send()?.error_for_status()?;
            let res = util::convert_response(&mut response)?;
            let content = res.into_body();

            util::validate_checksum(&content, &chksum)?;

            Ok(content)
        }
        Source::Git { .. } => via_git(&krate),
    }
}

pub fn from_gcs(ctx: &Context<'_>, krate: &Krate) -> Result<Bytes, Error> {
    let dl_req = Object::download(&(&ctx.gcs_bucket, &ctx.object_name(&krate)?), None)?;

    let (parts, _) = dl_req.into_parts();

    let uri = parts.uri.to_string();
    let builder = ctx.client.get(&uri);

    let request = builder.headers(parts.headers).build()?;

    let mut response = ctx.client.execute(request)?.error_for_status()?;
    let res = util::convert_response(&mut response)?;
    let content = res.into_body();

    if let Source::CratesIo(ref chksum) = krate.source {
        util::validate_checksum(&content, &chksum)?;
    }

    Ok(content)
}

pub fn via_git(krate: &Krate) -> Result<Bytes, Error> {
    match &krate.source {
        Source::Git { url, ident } => {
            // Create a temporary directory to clone the repo into
            let temp_dir = tempfile::tempdir()?;

            debug!("cloning {}", krate);
            let output = Command::new("git")
                .arg("clone")
                .arg("--bare")
                .arg(url.as_str())
                .arg(temp_dir.path())
                .output()?;

            if !output.status.success() {
                let err_out = String::from_utf8(output.stderr)?;
                bail!("failed to clone {}: {}", krate, err_out);
            }

            // Ensure that the revision required in the lockfile is actually present
            let rev = &ident[ident.len() - 7..];
            let has_revision = Command::new("git")
                .arg("cat-file")
                .arg("-t")
                .arg(rev)
                .current_dir(temp_dir.path())
                .output()?;

            if !has_revision.status.success()
                || String::from_utf8(has_revision.stdout)
                    .ok()
                    .as_ref()
                    .map(|s| s.as_ref())
                    != Some("commit\n")
            {
                bail!(
                    "git repo {} for {} does not contain revision {}",
                    url,
                    krate,
                    rev
                );
            }

            tarball(temp_dir.path())
        }
        Source::CratesIo(_) => bail!("{} is not a git source", krate),
    }
}

pub fn registry(url: &url::Url) -> Result<Bytes, Error> {
    // See https://github.com/rust-lang/cargo/blob/0e38712d4d7b346747bf91fb26cce8df6934e178/src/cargo/sources/registry/remote.rs#L61
    // for why we go through the whole repo init process + fetch instead of just a bare clone
    let temp_dir = tempfile::tempdir()?;

    let output = Command::new("git")
        .arg("init")
        .arg("--template=''") // Ensure we don't get any templates
        .current_dir(&temp_dir)
        .output()
        .context("git-init")?;

    if !output.status.success() {
        bail!("failed to initialize registry index repo");
    }

    debug!("fetching crates.io index");
    let output = Command::new("git")
        .arg("fetch")
        .arg(url.as_str())
        .arg("refs/heads/master:refs/remotes/origin/master")
        .current_dir(temp_dir.path())
        .output()
        .context("git-fetch")?;

    if !output.status.success() {
        bail!("failed to fetch registry index");
    }

    // We also write a `.last-updated` file just like cargo so that cargo knows
    // the timestamp of the fetch
    std::fs::File::create(temp_dir.path().join(".last-updated"))
        .context("failed to create .last-updated")?;

    tarball(temp_dir.path())
}

fn tarball(path: &std::path::Path) -> Result<Bytes, Error> {
    // If we don't allocate adequate space in our output buffer, things
    // go very poorly for everyone involved
    let mut estimated_size = 0;
    const TAR_HEADER_SIZE: u64 = 512;
    for entry in walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        estimated_size += TAR_HEADER_SIZE;
        if let Ok(md) = entry.metadata() {
            estimated_size += md.len();
        }
    }

    let out_buffer = BytesMut::with_capacity(estimated_size as usize);
    let buf_writer = out_buffer.writer();

    let zstd_encoder = zstd::Encoder::new(buf_writer, 9)?;

    let mut archiver = tar::Builder::new(zstd_encoder);
    archiver.append_dir_all(".", path)?;
    archiver.finish()?;

    let zstd_encoder = archiver.into_inner()?;
    let buf_writer = zstd_encoder.finish()?;
    let out_buffer = buf_writer.into_inner();

    // This is obviously super rough, but at least gives some inkling
    // of our compression ratio
    // debug!(
    //     "estimated compression ratio {:.2}% for {}",
    //     (out_buffer.len() as f64 / estimated_size as f64) * 100f64,
    //     krate
    // );

    Ok(out_buffer.freeze())
}
