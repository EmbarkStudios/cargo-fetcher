use anyhow::Context;
use cargo_fetcher as cf;
use cf::{Krate, Registry, Source};

mod tutil;
use tutil as util;

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
    let fs_root = tempfile::TempDir::new().expect("failed to create tempdir");
    let registry = Registry::new(
        "https://github.com/rust-lang/crates.io-index".to_owned(),
        None,
        Some("https://crates.io/api/v1/crates".to_owned()),
        None,
    );
    let registries = vec![registry.clone()];
    let mut fs_ctx = util::fs_ctx(fs_root.path().to_owned(), registries).await;

    let missing_root = tempfile::TempDir::new().expect("failed to create tempdir");
    fs_ctx.root_dir = missing_root.path().to_owned();

    fs_ctx.krates = vec![
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

    cf::mirror::crates(&fs_ctx)
        .await
        .expect("failed to mirror crates");
    fs_ctx.prep_sync_dirs().expect("create base dirs");
    assert_eq!(
        cf::sync::crates(&fs_ctx)
            .await
            .expect("synced 1 git source")
            .good,
        1,
    );

    let ident = "a7ffd7cabefac714";
    let rev = "e68e61f";

    // Ensure there is a db for cpal
    {
        let db_root = fs_ctx.root_dir.join(cf::sync::GIT_DB_DIR);

        let cpal_root = db_root.join(format!("cpal-{}", ident));
        assert!(cpal_root.exists(), "unable to find cpal db");

        // We expect a pack and idx file
        let mut has_idx = false;
        let mut has_pack = false;
        for entry in std::fs::read_dir(cpal_root.join("objects/pack")).unwrap() {
            let entry = entry.unwrap();

            let path = entry.path();
            let path = path.to_str().unwrap();

            if path.ends_with(".pack") {
                has_pack = true;
            }

            if path.ends_with(".idx") {
                has_idx = true;
            }
        }

        assert!(has_idx && has_pack);
    }

    // Ensure cpal is checked out
    {
        let co_root = fs_ctx.root_dir.join(cf::sync::GIT_CO_DIR);

        let cpal_root = co_root.join(format!("cpal-{}", ident));
        assert!(cpal_root.exists(), "unable to find cpal checkout");

        assert!(cpal_root.join(rev).exists(), "unable to find cpal checkout");

        let ok = cpal_root.join(format!("{}/.cargo-ok", rev));
        assert!(ok.exists(), "unable to find .cargo-ok");

        assert_eq!(std::fs::read_to_string(ok).unwrap(), "");
    }
}
