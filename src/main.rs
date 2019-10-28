extern crate cargo_fetcher as cf;
use anyhow::{anyhow, Context, Error};
use log::error;
use reqwest::Client;
use std::path::PathBuf;
use structopt::StructOpt;
use url::Url;

mod cmds;

use cmds::{mirror, sync};

#[derive(StructOpt)]
enum Command {
    /// Uploads any crates in the lockfile that aren't already present
    /// in the cloud storage location
    #[structopt(name = "mirror")]
    Mirror(mirror::Args),
    /// Downloads missing crates to the local cargo locations and unpacks
    /// them
    #[structopt(name = "sync")]
    Sync(sync::Args),
}

fn parse_level(s: &str) -> Result<log::LevelFilter, Error> {
    s.parse::<log::LevelFilter>()
        .map_err(|_| anyhow!("failed to parse level '{}'", s))
}

#[derive(StructOpt)]
struct Opts {
    /// Path to a service account credentials file used to obtain
    /// oauth2 tokens. By default uses GOOGLE_APPLICATION_CREDENTIALS
    /// environment variable.
    #[structopt(
        short,
        long,
        env = "GOOGLE_APPLICATION_CREDENTIALS",
        parse(from_os_str)
    )]
    credentials: Option<PathBuf>,
    /// A url to a cloud storage bucket and prefix path at which to store
    /// or retrieve archives
    #[structopt(short = "u", long = "url")]
    url: Url,
    /// Path to the lockfile used for determining what crates to operate on
    #[structopt(
        short,
        long = "lock-file",
        default_value = "Cargo.lock",
        parse(from_os_str)
    )]
    lock_file: PathBuf,
    #[structopt(
        short = "L",
        long = "log-level",
        default_value = "info",
        parse(try_from_str = parse_level),
        long_help = "The log level for messages, only log messages at or above the level will be emitted.

Possible values:
* off
* critical
* error
* warning
* info
* debug
* trace"
    )]
    log_level: log::LevelFilter,
    /// A snapshot of the registry index is also included when mirroring or syncing
    #[structopt(short, long = "include-index")]
    include_index: bool,
    #[structopt(subcommand)]
    cmd: Command,
}

fn init_backend(
    loc: cf::CloudLocation<'_>,
    _credentials: Option<PathBuf>,
) -> Result<Box<dyn cf::Backend + Sync>, Error> {
    match loc {
        #[cfg(feature = "gcs")]
        cf::CloudLocation::Gcs(gcs) => {
            let cred_path = _credentials.context("GCS credentials not specified")?;

            let gcs = cf::backends::gcs::GcsBackend::new(gcs, &cred_path)?;
            Ok(Box::new(gcs))
        }
        #[cfg(not(feature = "gcs"))]
        cf::CloudLocation::Gcs(_) => anyhow::bail!("GCS backend not enabled"),
        #[cfg(feature = "s3")]
        cf::CloudLocation::S3(s3) => {
            let s3 = cf::backends::s3::S3Backend::new(s3)?;
            Ok(Box::new(s3))
        }
        #[cfg(not(feature = "s3"))]
        cf::CloudLocation::S3(_) => anyhow::bail!("S3 backend not enabled"),
    }
}

fn real_main() -> Result<(), Error> {
    let args = Opts::from_iter({
        std::env::args().enumerate().filter_map(|(i, a)| {
            if i == 1 && a == "fetcher" {
                None
            } else {
                Some(a)
            }
        })
    });

    env_logger::builder().filter_level(args.log_level).init();

    let location = cf::util::parse_cloud_location(&args.url)?;
    let backend = init_backend(location, args.credentials)?;

    let krates = cf::gather(args.lock_file).context("failed to get crates from lock file")?;

    let ctx = cf::Ctx {
        client: Client::builder().gzip(false).build()?,
        backend,
        krates: &krates[..],
    };

    match args.cmd {
        Command::Mirror(margs) => mirror::cmd(ctx, args.include_index, margs),
        Command::Sync(sargs) => sync::cmd(ctx, args.include_index, sargs),
    }
}

fn main() {
    match real_main() {
        Ok(_) => {}
        Err(e) => {
            error!("{}", e);
            std::process::exit(1);
        }
    }
}
