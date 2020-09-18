use std::{cmp::Ordering, fs::File, path::Path};
use walkdir::{DirEntry, WalkDir};

#[cfg(unix)]
fn perms(p: &std::fs::Permissions) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    p.mode()
}

#[cfg(windows)]
fn perms(p: &std::fs::Permissions) -> u32 {
    0
}

fn assert_diff<A: AsRef<Path>, B: AsRef<Path>>(a_base: A, b_base: B) {
    let a_walker = walk_dir(&a_base).expect("failed to open root dir");
    let b_walker = walk_dir(&b_base).expect("failed to open root dir");

    let write_tree = |p: &Path, walker: walkdir::IntoIter| -> String {
        use std::fmt::Write;

        let mut tree = String::with_capacity(4 * 1024);

        for item in walker {
            let item = item.unwrap();

            let hash = if item.file_type().is_file() {
                hash(item.path())
            } else {
                0
            };

            let md = item.metadata().unwrap();
            let perms = perms(&md.permissions());

            // Strip off the root prefix so only the stems are matched against
            let path = item.path().strip_prefix(p).unwrap();

            writeln!(&mut tree, "{} {:o} {}", path.display(), perms, hash).unwrap();
        }

        tree
    };

    let a_base = a_base.as_ref();
    let b_base = b_base.as_ref();

    let (a, b) = rayon::join(
        || write_tree(a_base, a_walker),
        || write_tree(b_base, b_walker),
    );

    if a != b {
        let changeset = difference::Changeset::new(&a, &b, "\n");

        let err = std::io::stderr();
        let mut w = err.lock();

        use std::io::Write;

        // Only print the diffs
        for d in &changeset.diffs {
            match d {
                difference::Difference::Add(dif) => {
                    writeln!(&mut w, "\x1b[92m{}\x1b[0m", dif).unwrap()
                }
                difference::Difference::Rem(dif) => {
                    writeln!(&mut w, "\x1b[91m{}\x1b[0m", dif).unwrap()
                }
                _ => {}
            }
        }

        panic!("directories didn't match");
    }
}

fn walk_dir<P: AsRef<Path>>(path: P) -> Result<walkdir::IntoIter, std::io::Error> {
    let mut walkdir = WalkDir::new(path).sort_by(compare_by_file_name).into_iter();
    if let Some(Err(e)) = walkdir.next() {
        Err(e.into())
    } else {
        Ok(walkdir)
    }
}

fn compare_by_file_name(a: &DirEntry, b: &DirEntry) -> Ordering {
    a.file_name().cmp(b.file_name())
}

fn hash<P: AsRef<Path>>(file: P) -> u64 {
    use std::{hash::Hasher, io::Read};
    use twox_hash::XxHash64 as xx;

    match File::open(file.as_ref()) {
        Ok(mut f) => {
            let mut xh = xx::with_seed(0);

            let mut chunk = [0; 8 * 1024];

            loop {
                let read = match f.read(&mut chunk) {
                    Ok(r) => r,
                    Err(_) => 0xdead_beef,
                };

                if read > 0 {
                    xh.write(&chunk[..read]);
                } else {
                    break;
                }
            }

            xh.finish()
        }
        Err(_) => 0xdead_dead,
    }
}

use cargo_fetcher as cf;

mod tutil;
use tutil as util;

#[tokio::test(threaded_scheduler)]
#[ignore]
async fn diff_cargo() {
    let fs_root = tempfile::TempDir::new().expect("failed to create tempdir");
    let registries = vec![cf::Registry::new(
        "https://github.com/rust-lang/crates.io-index".to_owned(),
        None,
        None,
        None,
    )];
    let mut fs_ctx = util::fs_ctx(fs_root.path().to_owned(), registries).await;

    let fetcher_root = tempfile::TempDir::new().expect("failed to create tempdir");

    // Synchronize with cargo-fetcher
    {
        fs_ctx.root_dir = fetcher_root.path().to_owned();

        let (the_krates, _) = cf::read_lock_file("tests/full/Cargo.lock", Vec::new()).unwrap();
        fs_ctx.krates = the_krates;
        let the_registry = cf::Registry::new(
            "https://github.com/rust-lang/crates.io-index".to_owned(),
            None,
            None,
            None,
        );
        cf::mirror::registry_index(
            fs_ctx.backend.clone(),
            std::time::Duration::new(10, 0),
            &the_registry,
        )
        .await
        .expect("failed to mirror index");
        cf::mirror::crates(&fs_ctx)
            .await
            .expect("failed to mirror crates");

        fs_ctx.prep_sync_dirs().expect("create base dirs");
        cf::sync::crates(&fs_ctx).await.expect("synced crates");
        cf::sync::registry_index(fs_ctx.root_dir, fs_ctx.backend.clone(), &the_registry)
            .await
            .expect("failed to sync index");
    }

    let cargo_home = tempfile::TempDir::new().expect("failed to create tempdir");

    // Fetch with cargo
    {
        let path = cargo_home.path().to_str().unwrap();

        std::process::Command::new("cargo")
            .env("CARGO_HOME", &path)
            .args(&["fetch", "--manifest-path", "tests/full/Cargo.toml"])
            .status()
            .unwrap();
    }

    // Compare the outputs to ensure they match "exactly"
    assert_diff(fetcher_root.path(), cargo_home.path());
}
