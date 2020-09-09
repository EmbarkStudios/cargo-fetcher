use crate::{util, Krate, Source};
use anyhow::{bail, Context, Error};
use bytes::Bytes;
use reqwest::Client;
use std::path::Path;
use tokio::process::Command;
use tracing::{debug, error};
use tracing_futures::Instrument;

pub async fn from_registry(client: &Client, krate: &Krate) -> Result<Bytes, Error> {
    async {
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
            Source::Git { url, rev, .. } => via_git(&url.clone().into(), rev).await,
            Source::Registry(registry, chksum) => {
                let dl = registry
                    .dl
                    .as_ref()
                    .context(format!("failed get dl from registry({})", registry.index))?;
                let url = format!("{}/{}/{}/download", dl, krate.name, krate.version);

                // TODO use token in private registry
                let response = client.get(&url).send().await?.error_for_status()?;
                let res = util::convert_response(response).await?;
                let content = res.into_body();

                util::validate_checksum(&content, &chksum)?;

                Ok(content)
            }
        }
    }
    .instrument(tracing::debug_span!("fetch"))
    .await
}

pub async fn via_git(url: &url::Url, rev: &str) -> Result<Bytes, Error> {
    // Create a temporary directory to clone the repo into
    let temp_dir = tempfile::tempdir()?;

    let init = Command::new("git")
        .arg("init")
        .arg("--template=''")
        .arg("--bare")
        .arg(temp_dir.path())
        .output()
        .await?;

    if !init.status.success() {
        let err = String::from_utf8(init.stderr)?;
        error!(?err, "failed to init git repo");
        bail!("failed to init git repo");
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
        let err = String::from_utf8(fetch.stderr)?;
        error!(?err, "failed to fetch git repo");
        bail!("failed to fetch git repo");
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
        error!(?rev, "revision not found");
        bail!("revision not found");
    }

    util::pack_tar(temp_dir.path())
        .instrument(tracing::debug_span!("tarballing", %url, %rev))
        .await
}

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

    util::pack_tar(temp_dir.path())
        .instrument(tracing::debug_span!("tarball"))
        .await
}
