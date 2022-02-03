// Workaround for issue that was exacerbated by rust 1.46.0
#![type_length_limit = r#"18961884"#]
// BEGIN - Embark standard lints v5 for Rust 1.55+
// do not change or add/remove here, but one can add exceptions after this section
// for more info see: <https://github.com/EmbarkStudios/rust-ecosystem/issues/59>
#![deny(unsafe_code)]
#![warn(
    clippy::all,
    clippy::await_holding_lock,
    clippy::char_lit_as_u8,
    clippy::checked_conversions,
    clippy::dbg_macro,
    clippy::debug_assert_with_mut_call,
    clippy::disallowed_method,
    clippy::disallowed_type,
    clippy::doc_markdown,
    clippy::empty_enum,
    clippy::enum_glob_use,
    clippy::exit,
    clippy::expl_impl_clone_on_copy,
    clippy::explicit_deref_methods,
    clippy::explicit_into_iter_loop,
    clippy::fallible_impl_from,
    clippy::filter_map_next,
    clippy::flat_map_option,
    clippy::float_cmp_const,
    clippy::fn_params_excessive_bools,
    clippy::from_iter_instead_of_collect,
    clippy::if_let_mutex,
    clippy::implicit_clone,
    clippy::imprecise_flops,
    clippy::inefficient_to_string,
    clippy::invalid_upcast_comparisons,
    clippy::large_digit_groups,
    clippy::large_stack_arrays,
    clippy::large_types_passed_by_value,
    clippy::let_unit_value,
    clippy::linkedlist,
    clippy::lossy_float_literal,
    clippy::macro_use_imports,
    clippy::manual_ok_or,
    clippy::map_err_ignore,
    clippy::map_flatten,
    clippy::map_unwrap_or,
    clippy::match_on_vec_items,
    clippy::match_same_arms,
    clippy::match_wild_err_arm,
    clippy::match_wildcard_for_single_variants,
    clippy::mem_forget,
    clippy::mismatched_target_os,
    clippy::missing_enforced_import_renames,
    clippy::mut_mut,
    clippy::mutex_integer,
    clippy::needless_borrow,
    clippy::needless_continue,
    clippy::needless_for_each,
    clippy::option_option,
    clippy::path_buf_push_overwrite,
    clippy::ptr_as_ptr,
    clippy::rc_mutex,
    clippy::ref_option_ref,
    clippy::rest_pat_in_fully_bound_structs,
    clippy::same_functions_in_if_condition,
    clippy::semicolon_if_nothing_returned,
    clippy::single_match_else,
    clippy::string_add_assign,
    clippy::string_add,
    clippy::string_lit_as_bytes,
    clippy::string_to_string,
    clippy::todo,
    clippy::trait_duplication_in_bounds,
    clippy::unimplemented,
    clippy::unnested_or_patterns,
    clippy::unused_self,
    clippy::useless_transmute,
    clippy::verbose_file_reads,
    clippy::zero_sized_map_values,
    future_incompatible,
    nonstandard_style,
    rust_2018_idioms
)]
// END - Embark standard lints v0.5 for Rust 1.55+
// crate-specific exceptions:
#![allow(clippy::exit)]

extern crate cargo_fetcher as cf;

use anyhow::{anyhow, Context, Error};
use std::{path::PathBuf, sync::Arc};
use tracing_subscriber::filter::LevelFilter;
use url::Url;

mod mirror;
mod sync;

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
    #[structopt(
        short,
        long,
        env = "GOOGLE_APPLICATION_CREDENTIALS",
        parse(from_os_str)
    )]
    credentials: Option<PathBuf>,
    /// A url to a cloud storage bucket and prefix path at which to store
    /// or retrieve archives
    #[structopt(short, long)]
    url: Url,
    /// Path to the lockfile used for determining what crates to operate on
    #[structopt(short, long, default_value = "Cargo.lock", parse(from_os_str))]
    lock_file: PathBuf,
    #[structopt(
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
    #[structopt(long)]
    json: bool,
    /// A snapshot of the registry index is also included when mirroring or syncing
    #[structopt(short, long)]
    include_index: bool,
    #[structopt(subcommand)]
    cmd: Command,
}

fn init_backend(
    loc: cf::CloudLocation<'_>,
    _credentials: Option<PathBuf>,
) -> Result<Arc<dyn cf::Backend + Sync + Send>, Error> {
    match loc {
        #[cfg(feature = "gcs")]
        cf::CloudLocation::Gcs(gcs) => {
            let cred_path = _credentials.context("GCS credentials not specified")?;

            let gcs = cf::backends::gcs::GcsBackend::new(gcs, &cred_path)?;
            Ok(Arc::new(gcs))
        }
        #[cfg(not(feature = "gcs"))]
        cf::CloudLocation::Gcs(_) => anyhow::bail!("GCS backend not enabled"),
        #[cfg(feature = "s3")]
        cf::CloudLocation::S3(loc) => {
            // Special case local testing
            let make_bucket = loc.bucket == "testing" && loc.host.contains("localhost");

            let key = std::env::var("AWS_ACCESS_KEY_ID")
                .context("Set env variable AWS_ACCESS_KEY_ID first!")?;
            let secret = std::env::var("AWS_SECRET_ACCESS_KEY")
                .context("Set env variable AWS_SECRET_ACCESS_KEY first!")?;
            let s3 = cf::backends::s3::S3Backend::new(loc, key, secret)?;

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
        cf::CloudLocation::Blob(loc) => {
            let account = std::env::var("STORAGE_ACCOUNT")
                .context("Set env variable STORAGE_ACCOUNT first!")?;
            let master_key = std::env::var("STORAGE_MASTER_KEY")
                .context("Set env variable STORAGE_MASTER_KEY first!")?;
            Ok(Arc::new(cf::backends::blob::BlobBackend::new(
                loc, account, master_key,
            )?))
        }
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
    let backend = init_backend(location, args.credentials)?;

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

#[tokio::main]
async fn main() {
    match real_main() {
        Ok(_) => {}
        Err(e) => {
            tracing::error!("{:#}", e);
            std::process::exit(1);
        }
    }
}
