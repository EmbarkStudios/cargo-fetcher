#![cfg(feature = "fs")]
#![allow(dead_code)]

use cargo_fetcher as cf;
use std::path::PathBuf;

pub async fn fs_ctx(root: PathBuf, registries: Vec<std::sync::Arc<cf::Registry>>) -> cf::Ctx {
    let backend = std::sync::Arc::new(
        cf::backends::fs::FSBackend::new(cf::FilesystemLocation { path: &root })
            .await
            .expect("failed to create fs backend"),
    );

    cf::Ctx::new(None, backend, Vec::new(), registries).expect("failed to create context")
}

pub fn get_sync_dirs(ctx: &cf::Ctx) -> (PathBuf, PathBuf) {
    ctx.registries[0].sync_dirs(&ctx.root_dir)
}

pub fn hook_logger() {
    const HOOK: std::sync::Once = std::sync::Once::new();

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
