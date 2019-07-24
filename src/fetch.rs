use crate::{Krate, Source};
use bytes::{BufMut, Bytes, BytesMut};
use failure::Error;
use log::debug;
use reqwest::Client;
use std::convert::TryFrom;
use tame_gcs::{objects::Object, BucketName, ObjectName};

// We just treat versions as opaque strings
pub fn from_crates_io(client: &Client, krate: &Krate) -> Result<Bytes, Error> {
    match &krate.source {
        Source::CratesIo(_) => {
            let url = format!(
                "https://static.crates.io/crates/{}/{}-{}.crate",
                krate.name, krate.name, krate.version
            );

            let mut response = client.get(&url).send()?.error_for_status()?;
            let res = crate::convert_response(&mut response)?;
            Ok(res.into_body())
        }
        Source::Git { .. } => via_git(&krate),
    }
}

pub fn from_gcs(
    client: &Client,
    krate: &Krate,
    bucket: &BucketName<'_>,
    prefix: &str,
) -> Result<Bytes, Error> {
    let object_name = format!("{}{}", prefix, krate.gcs_id());
    let object_name = ObjectName::try_from(object_name.as_ref())?;

    let dl_req = Object::download(&(bucket, &object_name), None)?;

    let (parts, _) = dl_req.into_parts();

    let uri = parts.uri.to_string();
    let builder = client.get(&uri);

    let request = builder.headers(parts.headers).build()?;

    let mut response = client.execute(request)?.error_for_status()?;
    let res = crate::convert_response(&mut response)?;
    Ok(res.into_body())
}

pub fn via_git(krate: &Krate) -> Result<Bytes, Error> {
    use failure::bail;

    match &krate.source {
        Source::Git { url, ident } => {
            // Create a temporary directory to clone the repo into
            let temp_dir = tempfile::tempdir()?;

            debug!("cloning {}-{}", krate.name, krate.version);
            let output = std::process::Command::new("git")
                .arg("clone")
                .arg("--bare")
                .arg(url.as_str())
                .arg(temp_dir.path())
                .output()?;

            if !output.status.success() {
                let err_out = String::from_utf8(output.stderr)?;
                bail!(
                    "failed to clone {}-{}: {}",
                    krate.name,
                    krate.version,
                    err_out
                );
            }

            // Ensure that the revision required in the lockfile is actually present
            let rev = &ident[ident.len() - 7..];
            let has_revision = std::process::Command::new("git")
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
                    "git repo {} for {}-{} does not contain revision {}",
                    url,
                    krate.name,
                    krate.version,
                    rev
                );
            }

            // If we don't allocate adequate space in our output buffer, things
            // go very poorly for everyone involved
            let mut estimated_size = 0;
            const TAR_HEADER_SIZE: u64 = 512;
            for entry in walkdir::WalkDir::new(&temp_dir)
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
            archiver.append_dir_all(".", temp_dir.path())?;
            archiver.finish()?;

            let zstd_encoder = archiver.into_inner()?;
            let buf_writer = zstd_encoder.finish()?;
            let out_buffer = buf_writer.into_inner();

            // This is obviously super rough, but at least gives some inkling
            // of our compression ratio
            debug!(
                "estimated compression ratio {:.2}% for {}-{}",
                (out_buffer.len() as f64 / estimated_size as f64) * 100f64,
                krate.name,
                krate.version
            );

            Ok(out_buffer.freeze())
        }
        Source::CratesIo(_) => bail!("{}-{} is not a git source", krate.name, krate.version),
    }
}
// let mut cmd = process("git");
//     cmd.arg("fetch")
//         .arg("--tags") // fetch all tags
//         .arg("--force") // handle force pushes
//         .arg("--update-head-ok") // see discussion in #2078
//         .arg(url.to_string())
//         .arg(refspec)
//         // If cargo is run by git (for example, the `exec` command in `git
//         // rebase`), the GIT_DIR is set by git and will point to the wrong
//         // location (this takes precedence over the cwd). Make sure this is
//         // unset so git will look at cwd for the repo.
//         .env_remove("GIT_DIR")
//         // The reset of these may not be necessary, but I'm including them
//         // just to be extra paranoid and avoid any issues.
//         .env_remove("GIT_WORK_TREE")
//         .env_remove("GIT_INDEX_FILE")
//         .env_remove("GIT_OBJECT_DIRECTORY")
//         .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
//         .cwd(repo.path());
