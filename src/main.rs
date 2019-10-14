extern crate cargo_fetcher as cf;
use anyhow::{anyhow, Context, Error};
use log::{debug, error};
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
    #[structopt(short, long, parse(from_os_str))]
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

#[cfg(feature = "gcs")]
fn parse_gs_url(url: &Url) -> Result<cf::CloudLocation, Error> {
    use std::convert::TryFrom;

    let bucket = url.domain().context("url doesn't contain a bucket")?;
    // Remove the leading slash that url gives us
    let path = if !url.path().is_empty() {
        &url.path()[1..]
    } else {
        url.path()
    };

    let loc = cf::GcsLocation {
        bucket: tame_gcs::BucketName::try_from(bucket)?,
        prefix: path,
    };

    Ok(cf::CloudLocation::Gcs(loc))
}

#[cfg(not(feature = "gcs"))]
fn parse_gs_url(url: &Url) -> Result<cf::CloudLocation, Error> {
    bail!("GCS support was not enabled, you must compile with the 'gcs' feature")
}

fn parse_cloud_location(url: &Url) -> Result<cf::CloudLocation, Error> {
    match url.scheme() {
        "gs" => parse_gs_url(url),
        scheme => anyhow::bail!("the scheme '{}' is not supported", scheme),
    }
}

#[cfg(feature = "gcs")]
fn acquire_gcs_token(cred_path: PathBuf) -> Result<tame_oauth::Token, Error> {
    // If we're not completing whatever task in under an hour then
    // have more problems than the token expiring
    use tame_oauth::gcp;

    let svc_account_info =
        gcp::ServiceAccountInfo::deserialize(std::fs::read_to_string(&cred_path)?)
            .context("failed to deserilize service account")?;
    let svc_account_access = gcp::ServiceAccountAccess::new(svc_account_info)?;

    let token = match svc_account_access.get_token(&[tame_gcs::Scopes::ReadWrite])? {
        gcp::TokenOrRequest::Request {
            request,
            scope_hash,
            ..
        } => {
            let (parts, body) = request.into_parts();

            let client = reqwest::Client::new();

            let uri = parts.uri.to_string();

            let builder = match parts.method {
                http::Method::GET => client.get(&uri),
                http::Method::POST => client.post(&uri),
                http::Method::DELETE => client.delete(&uri),
                http::Method::PUT => client.put(&uri),
                method => unimplemented!("{} not implemented", method),
            };

            let req = builder
                .headers(parts.headers)
                .body(reqwest::Body::new(std::io::Cursor::new(body)))
                .build()?;

            let mut res = client.execute(req)?;

            let response = cf::util::convert_response(&mut res)?;
            svc_account_access.parse_token_response(scope_hash, response)?
        }
        _ => unreachable!(),
    };

    Ok(token)
}

fn init_client(loc: &cf::CloudLocation<'_>, credentials: Option<PathBuf>) -> Result<Client, Error> {
    use reqwest::header;
    use std::convert::TryInto;

    let cred_path = credentials
        .or_else(|| {
            let var = match loc {
                #[cfg(feature = "gcs")]
                cf::CloudLocation::Gcs(_) => "GOOGLE_APPLICATION_CREDENTIALS",
            };

            std::env::var_os(var).map(PathBuf::from)
        })
        .context("credentials not specified")?;

    debug!("using credentials in {}", cred_path.display());

    let client = match loc {
        #[cfg(feature = "gcs")]
        cf::CloudLocation::Gcs(_) => {
            let token = acquire_gcs_token(cred_path)?;

            let mut hm = header::HeaderMap::new();
            hm.insert(header::AUTHORIZATION, token.try_into()?);

            Client::builder().default_headers(hm).gzip(false).build()?
        }
    };

    Ok(client)
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

    let location = parse_cloud_location(&args.url)?;
    let client = init_client(&location, args.credentials)?;

    let krates = cargo_fetcher::gather(args.lock_file)?;

    let ctx = cf::Ctx {
        client,
        location,
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
