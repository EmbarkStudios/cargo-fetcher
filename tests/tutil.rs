#![cfg(feature = "s3_test")]

use cargo_fetcher as cf;

pub async fn s3_ctx(bucket: &str, prefix: &str) -> cf::Ctx {
    use std::sync::Once;

    static SUB: Once = Once::new();

    SUB.call_once(|| {
        let subscriber =
            tracing_subscriber::FmtSubscriber::builder().with_max_level(tracing::Level::DEBUG);
        tracing::subscriber::set_global_default(subscriber.finish()).unwrap();
    });

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
