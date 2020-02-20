use crate::{util, Krate, Source};
use anyhow::{bail, Context, Error};
use bytes::Bytes;
use reqwest::Client;
use std::path::Path;
use tokio::process::Command;
use tracing::debug;
use tracing_attributes::instrument;

#[instrument]
pub async fn from_crates_io(client: &Client, krate: &Krate) -> Result<Bytes, Error> {
    match &krate.source {
        Source::CratesIo(chksum) => {
            let url = format!(
                "https://static.crates.io/crates/{}/{}-{}.crate",
                krate.name, krate.name, krate.version
            );

            let response = client.get(&url).send().await?.error_for_status()?;
            let res = util::convert_response(response).await?;
            let content = res.into_body();

            util::validate_checksum(&content, &chksum)?;

            Ok(content)
        }
        Source::Git { .. } => via_git(&krate).await,
    }
}

#[instrument]
pub async fn via_git(krate: &Krate) -> Result<Bytes, Error> {
    match &krate.source {
        Source::Git { url, rev, .. } => {
            // Create a temporary directory to clone the repo into
            let temp_dir = tempfile::tempdir()?;

            debug!("cloning {}", krate);

            let init = Command::new("git")
                .arg("init")
                .arg("--template")
                .arg("")
                .arg("--bare")
                .arg(temp_dir.path())
                .output()
                .await?;

            if !init.status.success() {
                let err_out = String::from_utf8(init.stderr)?;
                bail!("failed to init git repo {}: {}", krate, err_out);
            }

            let mut fetch = Command::new("git");
            fetch
                .arg("fetch")
                .arg("--tags") // fetch all tags
                .arg("--force") // handle force pushes
                .arg("--update-head-ok") // see discussion in #2078
                .arg(url.as_str())
                .arg("refs/heads/*:refs/heads/*")
                // If cargo is run by git (for example, the `exec` command in `git
                // rebase`), the GIT_DIR is set by git and will point to the wrong
                // location (this takes precedence over the cwd). Make sure this is
                // unset so git will look at cwd for the repo.
                .env_remove("GIT_DIR")
                // The reset of these may not be necessary, but I'm including them
                // just to be extra paranoid and avoid any issues.
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_OBJECT_DIRECTORY")
                .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
                .current_dir(temp_dir.path());

            let fetch = fetch.output().await?;

            if !fetch.status.success() {
                let err_out = String::from_utf8(fetch.stderr)?;
                bail!("failed to fetch git repo {}: {}", krate, err_out);
            }

            // Ensure that the revision required in the lockfile is actually present
            let has_revision = Command::new("git")
                .arg("cat-file")
                .arg("-t")
                .arg(rev)
                .current_dir(temp_dir.path())
                .output()
                .await?;

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

            tracing::debug_span!("tarballing", %url, %rev)
                .in_scope(|| tarball(temp_dir.path()))
                .await
        }
        Source::CratesIo(_) => bail!("{} is not a git source", krate),
    }
}

#[instrument]
pub async fn update_bare(krate: &Krate, path: &Path) -> Result<(), Error> {
    let rev = match &krate.source {
        Source::Git { rev, .. } => rev,
        _ => bail!("not a git source"),
    };

    // Check if we already have the required revision and can skip the fetch
    // altogether
    let has_revision = Command::new("git")
        .arg("cat-file")
        .arg("-t")
        .arg(rev)
        .current_dir(&path)
        .output()
        .await?;

    if has_revision.status.success() {
        return Ok(());
    }

    let output = Command::new("git")
        .arg("fetch")
        .current_dir(&path)
        .output()
        .await?;

    if !output.status.success() {
        let err_out = String::from_utf8(output.stderr)?;
        anyhow::bail!("failed to fetch: {}", err_out);
    }

    // Ensure that the revision required in the lockfile is actually present
    let has_revision = Command::new("git")
        .arg("cat-file")
        .arg("-t")
        .arg(rev)
        .current_dir(&path)
        .output()
        .await?;

    if !has_revision.status.success()
        || String::from_utf8(has_revision.stdout)
            .ok()
            .as_ref()
            .map(|s| s.as_ref())
            != Some("commit\n")
    {
        anyhow::bail!("git repo for {} does not contain revision {}", krate, rev);
    }

    Ok(())
}

#[instrument]
pub async fn registry(url: &url::Url) -> Result<Bytes, Error> {
    // See https://github.com/rust-lang/cargo/blob/0e38712d4d7b346747bf91fb26cce8df6934e178/src/cargo/sources/registry/remote.rs#L61
    // for why we go through the whole repo init process + fetch instead of just a bare clone
    let temp_dir = tempfile::tempdir()?;

    let output = Command::new("git")
        .arg("init")
        .arg("--template=''") // Ensure we don't get any templates
        .current_dir(&temp_dir)
        .output()
        .await
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
        .await
        .context("git-fetch")?;

    if !output.status.success() {
        bail!("failed to fetch registry index");
    }

    // We also write a `.last-updated` file just like cargo so that cargo knows
    // the timestamp of the fetch
    std::fs::File::create(temp_dir.path().join(".last-updated"))
        .context("failed to create .last-updated")?;

    tarball(temp_dir.path()).await
}

#[instrument]
async fn tarball(path: &std::path::Path) -> Result<Bytes, Error> {
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

            // Add write permissions to all files, this is to
            // get around an issue where unpacking tar files on
            // Windows will result in errors if there are read-only
            // directories
            let mut perms = md.permissions();
            perms.set_readonly(false);
            std::fs::set_permissions(entry.path(), perms)?;
        }
    }

    use std::{
        io,
        pin::Pin,
        task::{Context, Poll},
    };

    struct Writer<W: io::Write> {
        encoder: zstd::Encoder<W>,
    }

    impl<W> futures::io::AsyncWrite for Writer<W>
    where
        W: io::Write + Send + std::marker::Unpin,
    {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            let w = self.get_mut(); //unsafe { self.get_unchecked_mut() };
            Poll::Ready(io::Write::write(&mut w.encoder, buf))
        }

        fn poll_write_vectored(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bufs: &[io::IoSlice<'_>],
        ) -> Poll<io::Result<usize>> {
            let w = unsafe { self.get_unchecked_mut() };
            Poll::Ready(io::Write::write_vectored(&mut w.encoder, bufs))
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            let w = unsafe { self.get_unchecked_mut() };
            Poll::Ready(io::Write::flush(&mut w.encoder))
        }

        fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.poll_flush(cx)
        }
    }

    //let out_buffer = BytesMut::with_capacity(estimated_size as usize);
    //let buf_writer = out_buffer.writer();

    let out_buffer = Vec::with_capacity(estimated_size as usize);
    let buf_writer = std::io::Cursor::new(out_buffer);

    let zstd_encoder = zstd::Encoder::new(buf_writer, 9)?;

    let mut archiver = async_tar::Builder::new(Writer {
        encoder: zstd_encoder,
    });
    archiver.append_dir_all(".", path).await?;
    archiver.finish().await?;

    let writer = archiver.into_inner().await?;
    let buf_writer = writer.encoder.finish()?;
    let out_buffer = buf_writer.into_inner();

    Ok(Bytes::from(out_buffer))
}
