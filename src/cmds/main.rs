// crate-specific exceptions:
#![allow(clippy::exit)]

extern crate cargo_fetcher as cf;

use anyhow::{anyhow, Context, Error};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tracing_subscriber::filter::LevelFilter;
use url::Url;

mod mirror;
mod sync;

#[inline]
fn parse_duration_s(src: &str) -> Result<Duration, Error> {
    parse_duration(src, "s")
}

fn parse_duration(src: &str, def: &str) -> Result<Duration, Error> {
    let suffix_pos = src.find(char::is_alphabetic).unwrap_or(src.len());

    let num: u64 = src[..suffix_pos].parse()?;
    let suffix = if suffix_pos == src.len() {
        def
    } else {
        &src[suffix_pos..]
    };

    let duration = match suffix {
        "s" | "S" => Duration::from_secs(num),
        "m" | "M" => Duration::from_secs(num * 60),
        "h" | "H" => Duration::from_secs(num * 60 * 60),
        "d" | "D" => Duration::from_secs(num * 60 * 60 * 24),
        s => return Err(anyhow::anyhow!("unknown duration suffix '{}'", s)),
    };

    Ok(duration)
}

#[derive(clap::Subcommand)]
enum Command {
    /// Uploads any crates in the lockfile that aren't already present
    /// in the cloud storage location
    #[clap(name = "mirror")]
    Mirror(mirror::Args),
    /// Downloads missing crates to the local cargo locations and unpacks
    /// them
    #[clap(name = "sync")]
    Sync(sync::Args),
}

#[inline]
fn parse_level(s: &str) -> Result<LevelFilter, Error> {
    s.parse::<LevelFilter>()
        .map_err(|_err| anyhow!("failed to parse level '{}'", s))
}

#[derive(clap::Parser)]
#[clap(
    author,
    version,
    about,
    long_about = "cargo plugin to quickly fetch crate sources from cloud or local storage"
)]
struct Opts {
    /// Path to a service account credentials file used to obtain
    /// oauth2 tokens. By default uses GOOGLE_APPLICATION_CREDENTIALS
    /// environment variable.
    #[clap(
        short,
        long,
        env = "GOOGLE_APPLICATION_CREDENTIALS",
        parse(from_os_str)
    )]
    credentials: Option<PathBuf>,
    /// A url to a cloud storage bucket and prefix path at which to store
    /// or retrieve archives
    #[clap(short, long)]
    url: Url,
    /// Path to the lockfile used for determining what crates to operate on
    #[clap(short, long, default_value = "Cargo.lock", parse(from_os_str))]
    lock_file: PathBuf,
    #[clap(
        short = 'L',
        long,
        default_value = "info",
        parse(try_from_str = parse_level),
        long_help = "The log level for messages, only log messages at or above the level will be emitted.

Possible values:
* off
* error
* warn
* info (default)
* debug
* trace"
    )]
    log_level: LevelFilter,
    /// Output log messages as json
    #[clap(long)]
    json: bool,
    /// A snapshot of the registry index is also included when mirroring or syncing
    #[clap(short, long)]
    include_index: bool,
    #[clap(
        short,
        default_value = "30s",
        parse(try_from_str = parse_duration_s),
        long_help = "The maximum duration of a single crate request

Times may be specified with no suffix (default seconds), or one of:
* (s)econds
* (m)inutes
* (h)ours
* (d)ays

"
    )]
    timeout: Duration,
    #[clap(subcommand)]
    cmd: Command,
}

fn init_backend(
    loc: cf::CloudLocation<'_>,
    _credentials: Option<PathBuf>,
    _timeout: Duration,
) -> Result<Arc<dyn cf::Backend + Sync + Send>, Error> {
    match loc {
        #[cfg(feature = "gcs")]
        cf::CloudLocation::Gcs(gcs) => {
            let cred_path = _credentials.context("GCS credentials not specified")?;

            let gcs = cf::backends::gcs::GcsBackend::new(gcs, &cred_path, _timeout)?;
            Ok(Arc::new(gcs))
        }
        #[cfg(not(feature = "gcs"))]
        cf::CloudLocation::Gcs(_) => anyhow::bail!("GCS backend not enabled"),
        #[cfg(feature = "s3")]
        cf::CloudLocation::S3(loc) => {
            // Special case local testing
            let make_bucket = loc.bucket == "testing" && loc.host.contains("localhost");

            let s3 = cf::backends::s3::S3Backend::new(loc, _timeout)?;

            if make_bucket {
                s3.make_bucket().context("failed to create test bucket")?;
            }

            Ok(Arc::new(s3))
        }
        #[cfg(not(feature = "s3"))]
        cf::CloudLocation::S3(_) => anyhow::bail!("S3 backend not enabled"),
        #[cfg(feature = "fs")]
        cf::CloudLocation::Fs(loc) => Ok(Arc::new(cf::backends::fs::FSBackend::new(loc)?)),
        #[cfg(not(feature = "fs"))]
        cf::CloudLocation::Fs(_) => anyhow::bail!("filesystem backend not enabled"),
        #[cfg(feature = "blob")]
        cf::CloudLocation::Blob(loc) => Ok(Arc::new(cf::backends::blob::BlobBackend::new(
            loc, _timeout,
        )?)),
        #[cfg(not(feature = "blob"))]
        cf::CloudLocation::Blob(_) => anyhow::bail!("blob backend not enabled"),
    }
}

fn real_main() -> Result<(), Error> {
    use clap::Parser;
    let args = Opts::parse_from({
        std::env::args().enumerate().filter_map(|(i, a)| {
            if i == 1 && a == "fetcher" {
                None
            } else {
                Some(a)
            }
        })
    });

    let mut env_filter = tracing_subscriber::EnvFilter::from_default_env();

    // If a user specifies a log level, we assume it only pertains to cargo_fetcher,
    // if they want to trace other crates they can use the RUST_LOG env approach
    env_filter = env_filter.add_directive(format!("cargo_fetcher={}", args.log_level).parse()?);

    let subscriber = tracing_subscriber::FmtSubscriber::builder().with_env_filter(env_filter);

    if args.json {
        tracing::subscriber::set_global_default(subscriber.json().finish())
            .context("failed to set default subscriber")?;
    } else {
        tracing::subscriber::set_global_default(subscriber.finish())
            .context("failed to set default subscriber")?;
    };

    let cloud_location = cf::util::CloudLocationUrl::from_url(args.url.clone())?;
    let location = cf::util::parse_cloud_location(&cloud_location)?;
    let backend = init_backend(location, args.credentials, args.timeout)?;

    // Note that unlike cargo (since we require a Cargo.lock), we don't use the
    // current directory as the root when resolving cargo configurations, but
    // rather the directory in which the lockfile is located
    let root_dir = if args.lock_file.is_relative() {
        let mut root_dir = std::env::current_dir()
            .context("unable to acquire current directory")?
            .join(&args.lock_file);
        root_dir.pop();
        root_dir
    } else {
        let mut root_dir = args.lock_file.clone();
        root_dir.pop();
        root_dir
    };

    let cargo_root = cf::cargo::determine_cargo_root(Some(&root_dir))
        .context("failed to determine $CARGO_HOME")?;

    let registries = cf::read_cargo_config(cargo_root.clone(), root_dir)?;

    let (krates, registries) = cf::cargo::read_lock_file(args.lock_file, registries)
        .context("failed to get crates from lock file")?;

    match args.cmd {
        Command::Mirror(margs) => {
            let ctx = cf::Ctx::new(None, backend, krates, registries)
                .context("failed to create context")?;
            mirror::cmd(ctx, args.include_index, margs)
        }
        Command::Sync(sargs) => {
            let ctx = cf::Ctx::new(Some(cargo_root), backend, krates, registries)
                .context("failed to create context")?;
            sync::cmd(ctx, args.include_index, sargs)
        }
    }
}

fn main() {
    match real_main() {
        Ok(_) => {}
        Err(e) => {
            tracing::error!("{:#}", e);
            std::process::exit(1);
        }
    }
}
