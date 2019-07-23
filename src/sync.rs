use failure::{format_err, Error};
use log::{error, info};
use rayon::prelude::*;
use std::path::PathBuf;

#[derive(structopt::StructOpt)]
pub struct Args {
    /// The root path for cargo. This defaults to either
    /// CARGO_HOME or HOME/.cargo.
    #[structopt(short, long, parse(from_os_str))]
    cache: Option<PathBuf>,
}

const CACHE_DIR: &str = "registry/cache/github.com-1ecc6299db9ec823";

pub fn cmd(ctx: crate::Context<'_>, args: Args) -> Result<(), Error> {
    info!("synchronizing {} crates...", ctx.krates.len());

    let root_dir = args
        .cache
        .or_else(|| std::env::var_os("CARGO_HOME").map(PathBuf::from))
        .or_else(dirs::home_dir);

    let root_dir = root_dir.ok_or_else(|| format_err!("unable to determine cargo root"))?;

    // There should always be a bin/cargo(.exe) relative to the root directory, at a minimum
    let cargo_path = {
        let mut cpath = root_dir.join("bin/cargo");

        if cfg!(target_os = "windows") {
            cpath.set_extension("exe");
        }

        cpath
    };

    if !cargo_path.exists() {
        return Err(format_err!(
            "cargo root {} does not seem to contain the cargo binary",
            root_dir.display()
        ));
    }

    let cache_dir = root_dir.join(CACHE_DIR);
    std::fs::create_dir_all(&cache_dir)?;

    let dir_iter = std::fs::read_dir(&cache_dir)?;

    // TODO: Also check the untarred crates
    info!("checking local cache for missing crates...");

    let mut cached_crates: Vec<String> = dir_iter
        .filter_map(|entry| {
            entry
                .ok()
                .and_then(|entry| entry.file_name().to_str().map(|s| s.to_owned()))
        })
        .collect();

    cached_crates.sort();

    let mut to_sync = Vec::with_capacity(ctx.krates.len());
    let mut krate_name = String::with_capacity(128);

    for krate in ctx.krates {
        use std::fmt::Write;
        write!(&mut krate_name, "{}-{}.crate", krate.name, krate.version).unwrap();

        if cached_crates.binary_search(&krate_name).is_err() {
            to_sync.push(krate);
        }

        krate_name.clear();
    }

    if to_sync.is_empty() {
        info!("all crates already available on local disk");
        return Ok(());
    }

    info!("synchronizing {} missing crates...", to_sync.len());

    ctx.krates.par_iter().for_each(|krate| {
        match cargo_cacher::fetch::from_gcs(&ctx.client, krate, &ctx.gcs_bucket, ctx.prefix) {
            Err(e) => error!("failed to download {}-{}: {}", krate.name, krate.version, e),
            Ok(krate_data) => {
                use std::io::Write;

                let packed_krate_path =
                    cache_dir.join(format!("{}-{}.crate", krate.name, krate.version));

                match std::fs::File::create(&packed_krate_path) {
                    Ok(mut f) => {
                        let _ = f.set_len(krate_data.len() as u64);

                        if let Err(e) = f.write_all(&krate_data) {
                            error!(
                                "failed to write {}-{} to disk: {}",
                                krate.name, krate.version, e
                            );
                        }
                    }
                    Err(e) => {
                        error!("failed to create {}-{}: {}", krate.name, krate.version, e);
                    }
                }
            }
        }
    });

    Ok(())
}
