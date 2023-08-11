#![allow(dead_code)]

use cargo_fetcher as cf;
use cf::{Path, PathBuf};

pub fn fs_ctx(root: PathBuf, registries: Vec<std::sync::Arc<cf::Registry>>) -> cf::Ctx {
    let backend = std::sync::Arc::new(
        cf::backends::fs::FsBackend::new(cf::FilesystemLocation { path: &root })
            .expect("failed to create fs backend"),
    );

    cf::Ctx::new(None, backend, Vec::new(), registries).expect("failed to create context")
}

pub struct TempDir {
    pub td: tempfile::TempDir,
}

impl TempDir {
    #[inline]
    pub fn path(&self) -> &Path {
        Path::from_path(self.td.path()).unwrap()
    }

    #[inline]
    pub fn pb(&self) -> PathBuf {
        self.path().to_owned()
    }

    #[inline]
    pub fn into_path(self) -> PathBuf {
        PathBuf::from_path_buf(self.td.into_path()).unwrap()
    }
}

impl Default for TempDir {
    #[inline]
    fn default() -> Self {
        Self {
            td: tempfile::TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).unwrap(),
        }
    }
}

impl AsRef<std::path::Path> for TempDir {
    #[inline]
    fn as_ref(&self) -> &std::path::Path {
        self.td.path()
    }
}

#[inline]
pub fn tempdir() -> TempDir {
    TempDir::default()
}

pub fn get_sync_dirs(ctx: &cf::Ctx) -> (PathBuf, PathBuf) {
    ctx.registries[0].sync_dirs(&ctx.root_dir)
}

#[inline]
pub fn crates_io_registry() -> cf::Registry {
    use anyhow::Context as _;
    let protocol = std::env::var("CARGO_FETCHER_CRATES_IO_PROTOCOL")
        .context("invalid env")
        .and_then(|prot| prot.parse())
        .unwrap_or(cf::RegistryProtocol::Sparse);

    cf::Registry::crates_io(protocol)
}

pub fn hook_logger() {
    static HOOK: std::sync::Once = std::sync::Once::new();

    HOOK.call_once(|| {
        let mut env_filter = tracing_subscriber::EnvFilter::from_default_env();

        // If a user specifies a log level, we assume it only pertains to cargo_fetcher,
        // if they want to trace other crates they can use the RUST_LOG env approach
        env_filter = env_filter.add_directive(
            format!("cargo_fetcher={}", tracing::Level::DEBUG)
                .parse()
                .unwrap(),
        );

        let subscriber = tracing_subscriber::FmtSubscriber::builder().with_env_filter(env_filter);

        tracing::subscriber::set_global_default(subscriber.finish()).unwrap();
    });
}
