#![cfg(feature = "s3_test")]

use cargo_fetcher as cf;

pub async fn s3_ctx(bucket: &str, prefix: &str) -> cf::Ctx {
    let _ = env_logger::builder().is_test(true).try_init();

    let backend = std::sync::Arc::new(
        cf::backends::s3::S3Backend::new(cf::S3Location {
            bucket,
            region: "",
            host: &std::env::var("S3_ENDPOINT").unwrap(),
            prefix,
        })
        .expect("failed to create backend"),
    );

    backend.make_bucket().await.expect("failed to make bucket");

    cf::Ctx::new(None, backend, Vec::new()).expect("failed to create context")
}
