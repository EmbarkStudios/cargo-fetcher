use anyhow::{Context, Error};

pub(crate) fn with_fetch_options(
    git_config: &git2::Config,
    url: &str,
    cb: &mut dyn FnMut(git2::FetchOptions<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    with_authentication(url, git_config, |f| {
        let mut rcb = git2::RemoteCallbacks::new();
        rcb.credentials(f);

        // rcb.transfer_progress(|stats| {
        //     progress
        //         .tick(stats.indexed_objects(), stats.total_objects())
        //         .is_ok()
        // });

        // Create a local anonymous remote in the repository to fetch the
        // url
        let mut opts = git2::FetchOptions::new();
        opts.remote_callbacks(rcb);
        cb(opts)
    })?;
    Ok(())
}

/// Prepare the authentication callbacks for cloning a git repository.
///
/// The main purpose of this function is to construct the "authentication
/// callback" which is used to clone a repository. This callback will attempt to
/// find the right authentication on the system (without user input) and will
/// guide libgit2 in doing so.
///
/// The callback is provided `allowed` types of credentials, and we try to do as
/// much as possible based on that:
///
/// * Prioritize SSH keys from the local ssh agent as they're likely the most
///   reliable. The username here is prioritized from the credential
///   callback, then from whatever is configured in git itself, and finally
///   we fall back to the generic user of `git`.
///
/// * If a username/password is allowed, then we fallback to git2-rs's
///   implementation of the credential helper. This is what is configured
///   with `credential.helper` in git, and is the interface for the macOS
///   keychain, for example.
///
/// * After the above two have failed, we just kinda grapple attempting to
///   return *something*.
///
/// If any form of authentication fails, libgit2 will repeatedly ask us for
/// credentials until we give it a reason to not do so. To ensure we don't
/// just sit here looping forever we keep track of authentications we've
/// attempted and we don't try the same ones again.
fn with_authentication<T, F>(url: &str, cfg: &git2::Config, mut f: F) -> Result<T, Error>
where
    F: FnMut(&mut git2::Credentials<'_>) -> Result<T, Error>,
{
    use std::env;

    let mut cred_helper = git2::CredentialHelper::new(url);
    cred_helper.config(cfg);

    let mut ssh_username_requested = false;
    let mut cred_helper_bad = None;
    let mut ssh_agent_attempts = Vec::new();
    let mut any_attempts = false;
    let mut tried_sshkey = false;
    let mut url_attempt = None;

    let orig_url = url;
    let mut res = f(&mut |url, username, allowed| {
        any_attempts = true;
        if url != orig_url {
            url_attempt = Some(url.to_string());
        }
        // libgit2's "USERNAME" authentication actually means that it's just
        // asking us for a username to keep going. This is currently only really
        // used for SSH authentication and isn't really an authentication type.
        // The logic currently looks like:
        //
        //      let user = ...;
        //      if (user.is_null())
        //          user = callback(USERNAME, null, ...);
        //
        //      callback(SSH_KEY, user, ...)
        //
        // So if we're being called here then we know that (a) we're using ssh
        // authentication and (b) no username was specified in the URL that
        // we're trying to clone. We need to guess an appropriate username here,
        // but that may involve a few attempts. Unfortunately we can't switch
        // usernames during one authentication session with libgit2, so to
        // handle this we bail out of this authentication session after setting
        // the flag `ssh_username_requested`, and then we handle this below.
        if allowed.contains(git2::CredentialType::USERNAME) {
            debug_assert!(username.is_none());
            ssh_username_requested = true;
            return Err(git2::Error::from_str("gonna try usernames later"));
        }

        // An "SSH_KEY" authentication indicates that we need some sort of SSH
        // authentication. This can currently either come from the ssh-agent
        // process or from a raw in-memory SSH key. Cargo only supports using
        // ssh-agent currently.
        //
        // If we get called with this then the only way that should be possible
        // is if a username is specified in the URL itself (e.g., `username` is
        // Some), hence the unwrap() here. We try custom usernames down below.
        if allowed.contains(git2::CredentialType::SSH_KEY) && !tried_sshkey {
            // If ssh-agent authentication fails, libgit2 will keep
            // calling this callback asking for other authentication
            // methods to try. Make sure we only try ssh-agent once,
            // to avoid looping forever.
            tried_sshkey = true;
            let username = username.unwrap();
            debug_assert!(!ssh_username_requested);
            ssh_agent_attempts.push(username.to_string());
            return git2::Cred::ssh_key_from_agent(username);
        }

        // Sometimes libgit2 will ask for a username/password in plaintext. This
        // is where Cargo would have an interactive prompt if we supported it,
        // but we currently don't! Right now the only way we support fetching a
        // plaintext password is through the `credential.helper` support, so
        // fetch that here.
        //
        // If ssh-agent authentication fails, libgit2 will keep calling this
        // callback asking for other authentication methods to try. Check
        // cred_helper_bad to make sure we only try the git credentail helper
        // once, to avoid looping forever.
        if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) && cred_helper_bad.is_none()
        {
            let r = git2::Cred::credential_helper(cfg, url, username);
            cred_helper_bad = Some(r.is_err());
            return r;
        }

        // I'm... not sure what the DEFAULT kind of authentication is, but seems
        // easy to support?
        if allowed.contains(git2::CredentialType::DEFAULT) {
            return git2::Cred::default();
        }

        // Whelp, we tried our best
        Err(git2::Error::from_str("no authentication available"))
    });

    // Ok, so if it looks like we're going to be doing ssh authentication, we
    // want to try a few different usernames as one wasn't specified in the URL
    // for us to use. In order, we'll try:
    //
    // * A credential helper's username for this URL, if available.
    // * This account's username.
    // * "git"
    //
    // We have to restart the authentication session each time (due to
    // constraints in libssh2 I guess? maybe this is inherent to ssh?), so we
    // call our callback, `f`, in a loop here.
    if ssh_username_requested {
        debug_assert!(res.is_err());
        let mut attempts = vec!["git".to_string()];

        if let Ok(s) = env::var("USER").or_else(|_| env::var("USERNAME")) {
            attempts.push(s);
        }
        if let Some(ref s) = cred_helper.username {
            attempts.push(s.clone());
        }

        while let Some(s) = attempts.pop() {
            // We should get `USERNAME` first, where we just return our attempt,
            // and then after that we should get `SSH_KEY`. If the first attempt
            // fails we'll get called again, but we don't have another option so
            // we bail out.
            let mut attempts = 0;
            res = f(&mut |_url, username, allowed| {
                if allowed.contains(git2::CredentialType::USERNAME) {
                    return git2::Cred::username(&s);
                }
                if allowed.contains(git2::CredentialType::SSH_KEY) {
                    debug_assert_eq!(Some(&s[..]), username);
                    attempts += 1;
                    if attempts == 1 {
                        ssh_agent_attempts.push(s.clone());
                        return git2::Cred::ssh_key_from_agent(&s);
                    }
                }
                Err(git2::Error::from_str("no authentication available"))
            });

            // If we made two attempts then that means:
            //
            // 1. A username was requested, we returned `s`.
            // 2. An ssh key was requested, we returned to look up `s` in the
            //    ssh agent.
            // 3. For whatever reason that lookup failed, so we were asked again
            //    for another mode of authentication.
            //
            // Essentially, if `attempts == 2` then in theory the only error was
            // that this username failed to authenticate (e.g., no other network
            // errors happened). Otherwise something else is funny so we bail
            // out.
            if attempts != 2 {
                break;
            }
        }
    }
    let mut err = match res {
        Ok(e) => return Ok(e),
        Err(e) => e,
    };

    // In the case of an authentication failure (where we tried something) then
    // we try to give a more helpful error message about precisely what we
    // tried.
    if any_attempts {
        let mut msg = "failed to authenticate when downloading \
                       repository"
            .to_string();

        if let Some(attempt) = &url_attempt {
            if url != attempt {
                msg.push_str(": ");
                msg.push_str(attempt);
            }
        }
        msg.push('\n');
        if !ssh_agent_attempts.is_empty() {
            let names = ssh_agent_attempts
                .iter()
                .map(|s| format!("`{}`", s))
                .collect::<Vec<_>>()
                .join(", ");
            msg.push_str(&format!(
                "\n* attempted ssh-agent authentication, but \
                 no usernames succeeded: {}",
                names
            ));
        }
        if let Some(failed_cred_helper) = cred_helper_bad {
            if failed_cred_helper {
                msg.push_str(
                    "\n* attempted to find username/password via \
                     git's `credential.helper` support, but failed",
                );
            } else {
                msg.push_str(
                    "\n* attempted to find username/password via \
                     `credential.helper`, but maybe the found \
                     credentials were incorrect",
                );
            }
        }
        msg.push_str("\n\n");
        msg.push_str("if the git CLI succeeds then `net.git-fetch-with-cli` may help here\n");
        msg.push_str("https://doc.rust-lang.org/cargo/reference/config.html#netgit-fetch-with-cli");
        err = err.context(msg);

    // Otherwise if we didn't even get to the authentication phase them we may
    // have failed to set up a connection, in these cases hint on the
    // `net.git-fetch-with-cli` configuration option.
    } else if let Some(e) = err.downcast_ref::<git2::Error>() {
        use git2::ErrorClass;
        match e.class() {
            ErrorClass::Net
            | ErrorClass::Ssl
            | ErrorClass::Submodule
            | ErrorClass::FetchHead
            | ErrorClass::Ssh
            | ErrorClass::Callback
            | ErrorClass::Http => {
                let mut msg = "network failure seems to have happened\n".to_string();
                msg.push_str(
                    "if a proxy or similar is necessary `net.git-fetch-with-cli` may help here\n",
                );
                msg.push_str(
                    "https://doc.rust-lang.org/cargo/reference/config.html#netgit-fetch-with-cli",
                );
                err = err.context(msg);
            }
            _ => {}
        }
    }

    Err(err)
}

pub struct GitSource {
    /// The tarball of the bare repository
    pub db: bytes::Bytes,
    /// The tarball of the checked out repository, including all submodules
    pub checkout: Option<bytes::Bytes>,
}

pub(crate) async fn checkout(
    src: std::path::PathBuf,
    target: std::path::PathBuf,
    rev: String,
) -> Result<git2::Repository, Error> {
    // We require the target directory to be clean
    std::fs::create_dir_all(target.parent().unwrap()).context("failed to create checkout dir")?;
    if target.exists() {
        remove_dir_all::remove_dir_all(&target).context("failed to clean checkout dir")?;
    }

    tokio::task::spawn_blocking(move || {
        let fopts = git2::FetchOptions::new();
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.dry_run(); // we'll do this below during a `reset`

        let src_url =
            url::Url::from_file_path(&src).map_err(|_err| Error::msg("invalid path URL"))?;

        let repo = git2::build::RepoBuilder::new()
            // use hard links and/or copy the database, we're doing a
            // filesystem clone so this'll speed things up quite a bit.
            .clone_local(git2::build::CloneLocal::Local)
            .with_checkout(checkout)
            .fetch_options(fopts)
            .clone(src_url.as_str(), &target)
            .context("failed to clone")?;

        if let Ok(mut cfg) = repo.config() {
            let _ = cfg.set_bool("core.autocrlf", false);
        }

        {
            let object = repo
                .revparse_single(&rev)
                .context("failed to find revision")?;
            repo.reset(&object, git2::ResetType::Hard, None)
                .context("failed to do hard reset")?;
        }

        Ok(repo)
    })
    .await?
}

pub(crate) async fn prepare_submodules(
    src: std::path::PathBuf,
    target: std::path::PathBuf,
    rev: String,
) -> Result<(), Error> {
    let repo = checkout(src, target, rev).await?;

    fn update_submodules(repo: &git2::Repository, git_cfg: &git2::Config) -> Result<(), Error> {
        tracing::info!("update submodules for: {:?}", repo.workdir().unwrap());

        for mut child in repo.submodules()? {
            update_submodule(repo, &mut child, git_cfg).with_context(|| {
                format!(
                    "failed to update submodule '{}'",
                    child.name().unwrap_or("")
                )
            })?;
        }
        Ok(())
    }

    fn update_submodule(
        parent: &git2::Repository,
        child: &mut git2::Submodule<'_>,
        git_cfg: &git2::Config,
    ) -> Result<(), Error> {
        child.init(false).context("failed to init submodule")?;

        let url = child
            .url()
            .with_context(|| format!("non-utf8 url for submodule {:?}", child.path()))?;

        // A submodule which is listed in .gitmodules but not actually
        // checked out will not have a head id, so we should ignore it.
        let head = match child.head_id() {
            Some(head) => head,
            None => {
                tracing::debug!(
                    "skipping submodule '{}' without HEAD",
                    child.name().unwrap_or("")
                );
                return Ok(());
            }
        };

        // If the submodule hasn't been checked out yet, we need to
        // clone it. If it has been checked out and the head is the same
        // as the submodule's head, then we can skip an update and keep
        // recursing.
        let head_and_repo = child.open().and_then(|repo| {
            let target = repo.head()?.target();
            Ok((target, repo))
        });

        let repo = match head_and_repo {
            Ok((head, repo)) => {
                if child.head_id() == head {
                    return update_submodules(&repo, git_cfg);
                }
                repo
            }
            Err(_) => {
                let path = parent.workdir().unwrap().join(child.path());
                let _ = remove_dir_all::remove_dir_all(&path);

                let mut opts = git2::RepositoryInitOptions::new();
                opts.external_template(false);
                opts.bare(false);
                git2::Repository::init_opts(&path, &opts)?
            }
        };

        with_fetch_options(git_cfg, url, &mut |mut fopts| {
            fopts.download_tags(git2::AutotagOption::All);

            repo.remote_anonymous(url)?
                .fetch(
                    &[
                        "refs/heads/*:refs/remotes/origin/*",
                        "HEAD:refs/remotes/origin/HEAD",
                    ],
                    Some(&mut fopts),
                    None,
                )
                .context("failed to fetch")
        })
        .with_context(|| format!("failed to fetch submodule '{}'", child.name().unwrap_or("")))?;

        let obj = repo
            .find_object(head, None)
            .context("failed to find HEAD")?;
        repo.reset(&obj, git2::ResetType::Hard, None)
            .context("failed to reset")?;
        update_submodules(&repo, git_cfg)
    }

    tokio::task::spawn_blocking(move || {
        let git_config =
            git2::Config::open_default().context("Failed to open default git config")?;

        update_submodules(&repo, &git_config)
    })
    .await?
}
