use cargo_fetcher as cf;

use std::{path::PathBuf, sync::Arc};

pub async fn fs_ctx(root: PathBuf, registries: Vec<cf::Registry>) -> cf::Ctx {
    let backend = Arc::new(
        cf::backends::fs::FSBackend::new(cf::FilesystemLocation { path: &root })
            .await
            .expect("failed to create fs backend"),
    );
    cf::Ctx::new(None, backend, Vec::new(), registries).expect("failed to create context")
}
