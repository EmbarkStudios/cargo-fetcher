use cargo_fetcher as cf;

pub fn s3_ctx(bucket: &str, prefix: &str) -> cf::Ctx {
    let _ = env_logger::builder().is_test(true).try_init();

    let backend = Box::new(
        cf::backends::s3::S3Backend::new(cf::S3Location {
            bucket,
            region: "",
            host: &std::env::var("S3_ENDPOINT").unwrap(),
            prefix,
        })
        .expect("failed to create backend"),
    );

    backend.make_bucket().expect("failed to make bucket");

    cf::Ctx::new(None, backend, Vec::new()).expect("failed to create context")
}
