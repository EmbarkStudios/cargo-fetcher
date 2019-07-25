use cargo_fetcher::Krate;
use failure::{format_err, Error, ResultExt};
use log::{debug, error};
use reqwest::Client;
use std::path::PathBuf;
use structopt::StructOpt;
use tame_gcs::BucketName;

mod mirror;
mod sync;

#[derive(StructOpt)]
enum Command {
    /// Uploads any crates in the lockfile that aren't already present
    #[structopt(name = "mirror")]
    Mirror(mirror::Args),
    /// Downloads missing crates to the local cargo cache
    #[structopt(name = "sync")]
    Sync(sync::Args),
}

fn parse_level(s: &str) -> Result<log::LevelFilter, Error> {
    s.parse::<log::LevelFilter>()
        .map_err(|_| format_err!("failed to parse level '{}'", s))
}

#[derive(StructOpt)]
struct Opts {
    /// Path to a service account credentials file used to obtain
    /// oauth2 tokens. By default uses GOOGLE_APPLICATION_CREDENTIALS
    /// environment variable.
    #[structopt(short, long, parse(from_os_str))]
    credentials: Option<PathBuf>,
    /// A gs:// url to the bucket and prefix path where crates
    /// will be/are stored
    #[structopt(short = "g", long = "gcs")]
    gcs_url: String,
    /// Path to the lockfile used for determining what crates to operate on
    #[structopt(short, long, parse(from_os_str))]
    lock_file: PathBuf,
    #[structopt(
        short = "L",
        long = "log-level",
        default_value = "info",
        parse(try_from_str = "parse_level"),
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
    #[structopt(short, long)]
    include_index: bool,
    #[structopt(subcommand)]
    cmd: Command,
}

fn gs_url_to_bucket_and_prefix(url: &str) -> Result<(BucketName<'_>, &str), Error> {
    use std::convert::TryFrom;

    let no_scheme = url.trim_start_matches("gs://");

    let mut split = no_scheme.splitn(2, '/');
    let bucket = split
        .next()
        .ok_or_else(|| failure::err_msg("unknown bucket"))?;
    let bucket = BucketName::try_from(bucket)?;

    let prefix = split.next().unwrap_or_default();

    Ok((bucket, prefix))
}

// If we're not completing whatever task in under an hour then
// have more problems than the token expiring
fn acquire_token(cred_path: PathBuf) -> Result<tame_oauth::Token, Error> {
    use tame_gcs::http;
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

            let response = cargo_fetcher::convert_response(&mut res)?;
            svc_account_access.parse_token_response(scope_hash, response)?
        }
        _ => unreachable!(),
    };

    Ok(token)
}

pub struct Context<'a> {
    client: Client,
    gcs_bucket: BucketName<'a>,
    prefix: &'a str,
    krates: &'a [Krate],
    include_index: bool,
}

fn real_main() -> Result<(), Error> {
    use reqwest::header;
    use std::convert::TryInto;

    let args = Opts::from_args();

    env_logger::builder().filter_level(args.log_level).init();

    let (bucket, prefix) =
        gs_url_to_bucket_and_prefix(&args.gcs_url).context("gs:// url is invalid")?;

    let cred_path = args
        .credentials
        .or_else(|| std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS").map(PathBuf::from))
        .ok_or_else(|| format_err!("credentials not specified"))?;

    debug!("using credentials in {}", cred_path.display());

    let token = acquire_token(cred_path)?;

    let krates = cargo_fetcher::gather(args.lock_file)?;

    let mut hm = header::HeaderMap::new();
    hm.insert(header::AUTHORIZATION, token.try_into()?);

    let client = Client::builder().default_headers(hm).gzip(false).build()?;

    let ctx = Context {
        client,
        gcs_bucket: bucket,
        prefix,
        krates: &krates[..],
        include_index: args.include_index,
    };

    match args.cmd {
        Command::Mirror(args) => mirror::cmd(ctx, args),
        Command::Sync(args) => sync::cmd(ctx, args),
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
