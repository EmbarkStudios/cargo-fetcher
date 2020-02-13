use anyhow::Context;
use cargo_fetcher as cf;
use cf::{Krate, Source};

mod util;

macro_rules! git_source {
    ($url:expr) => {{
        let url = cf::Url::parse($url).expect("failed to parse url");
        Source::from_git_url(&url)
            .context("failed to create git source")
            .unwrap()
    }};
}

#[tokio::test(threaded_scheduler)]
async fn multiple_from_same_repo() {
    let mut s3_ctx = util::s3_ctx("sync-multi-git", "multi-git/").await;

    let missing_root = tempfile::TempDir::new().expect("failed to crate tempdir");
    s3_ctx.root_dir = missing_root.path().to_owned();

    s3_ctx.krates = vec![
        Krate {
            name: "alsa-sys".to_owned(),
            version: "0.1.1".to_owned(),
            source: git_source!("git+https://github.com/EmbarkStudios/cpal?rev=e68e61f7d#e68e61f7d4c9b4c946b927e868a27193fa11c3f0"),
        },
        Krate {
            name: "cpal".to_owned(),
            version: "0.10.0".to_owned(),
            source: git_source!("git+https://github.com/EmbarkStudios/cpal?rev=e68e61f7d#e68e61f7d4c9b4c946b927e868a27193fa11c3f0"),
        },
    ];

    cf::mirror::locked_crates(&s3_ctx)
        .await
        .expect("failed to mirror crates");
    s3_ctx.prep_sync_dirs().expect("create base dirs");
    assert_eq!(
        cf::sync::locked_crates(&s3_ctx)
            .await
            .expect("synced 1 git source"),
        1,
    );

    let ident = "a7ffd7cabefac714";
    let rev = "e68e61f";

    // Ensure there is a db for cpal
    {
        let db_root = s3_ctx.root_dir.join(cf::sync::GIT_DB_DIR);

        let cpal_root = db_root.join(format!("cpal-{}", ident));
        assert!(cpal_root.exists(), "unable to find cpal db");

        assert!(
            cpal_root
                .join("objects/pack/pack-8cd88d098a99144f96ebc73435ee36b37598453b.pack")
                .exists(),
            "unable to find pack file"
        );
    }

    // Ensure cpal is checked out
    {
        let co_root = s3_ctx.root_dir.join(cf::sync::GIT_CO_DIR);

        let cpal_root = co_root.join(format!("cpal-{}", ident));
        assert!(cpal_root.exists(), "unable to find cpal checkout");

        assert!(cpal_root.join(rev).exists(), "unable to find cpal checkout");
        assert!(
            cpal_root.join(format!("{}/.cargo-ok", rev)).exists(),
            "unable to find .cargo-ok"
        );
    }
}
