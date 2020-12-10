#![cfg(feature = "fs")]

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

#[allow(dead_code)]
pub fn get_sync_dirs(ctx: &cf::Ctx) -> (PathBuf, PathBuf) {
    ctx.registries[0].sync_dirs(&ctx.root_dir)
}
