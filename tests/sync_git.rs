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

#[test]
fn multiple_from_same_repo() {
    util::hook_logger();

    let fs_root = tempfile::TempDir::new().expect("failed to create tempdir");
    let registry = std::sync::Arc::new(Registry::default());
    let registries = vec![registry];
    let mut fs_ctx = util::fs_ctx(fs_root.path().to_owned(), registries);

    let missing_root = tempfile::TempDir::new().expect("failed to create tempdir");
    fs_ctx.root_dir = missing_root.path().to_owned();

    fs_ctx.krates = vec![
        Krate {
            name: "asio-sys".to_owned(),
            version: "0.2.1".to_owned(),
            source: git_source!("git+https://github.com/RustAudio/cpal?rev=971c46346#971c463462e3560e66f7629e5afcd6b25c4411ab"),
        },
        Krate {
            name: "cpal".to_owned(),
            version: "0.13.5".to_owned(),
            source: git_source!("git+https://github.com/rustaudio/cpal?rev=971c46346#971c463462e3560e66f7629e5afcd6b25c4411ab"),
        },
    ];

    cf::mirror::crates(&fs_ctx).expect("failed to mirror crates");
    fs_ctx.prep_sync_dirs().expect("create base dirs");
    assert_eq!(
        cf::sync::crates(&fs_ctx).expect("synced 1 git source").good,
        1,
    );

    let ident = "c2179e82da06da7e";
    let rev = "971c463";

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
