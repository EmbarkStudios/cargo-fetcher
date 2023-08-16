use std::{cmp::Ordering, fs::File, path::Path};
use walkdir::{DirEntry, WalkDir};

#[cfg(unix)]
fn perms(_p: &std::fs::Permissions) -> u32 {
    // use std::os::unix::fs::PermissionsExt;
    // p.mode()
    0
}

#[cfg(windows)]
fn perms(_p: &std::fs::Permissions) -> u32 {
    0
}

fn assert_diff<A: AsRef<Path>, B: AsRef<Path>>(a_base: A, b_base: B) {
    let a_walker = walk_dir(&a_base).expect("failed to open root dir");
    let b_walker = walk_dir(&b_base).expect("failed to open root dir");

    let write_tree = |p: &Path, walker: walkdir::IntoIter| -> String {
        use std::fmt::Write;

        let mut tree = String::with_capacity(4 * 1024);

        for item in walker.filter_entry(|entry| {
            let path = entry.path();
            if entry.metadata().unwrap().is_dir() {
                // Both .git and git/db contain things like pack files that are
                // non-deterministic, and are otherwise just uninteresting to check
                // as the checked out source matching is what actually matters
                !(path.ends_with(".git") || path.strip_prefix(p).unwrap().starts_with("git/db"))
            } else {
                !(
                    // We don't write this file, it's a nicety added by cargo but
                    // not really relevant for the primary use case of short-lived CI
                    // jobs
                    path.ends_with("CACHEDIR.TAG") ||
                    // We don't write this file, again, not really relevant for
                    // primary use case
                    path.ends_with(".package-cache")
                )
            }
        }) {
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

            writeln!(&mut tree, "{} {perms:o} {hash}", path.display()).unwrap();
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
        panic!(
            "{}\nfetcher: {} cargo: {}",
            similar_asserts::SimpleDiff::from_str(&a, &b, "fetcher", "cargo"),
            a_base.display(),
            b_base.display()
        );
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

#[inline]
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
                let read = f.read(&mut chunk).unwrap_or(0xdead_beef);

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

#[tokio::test]
async fn diff_cargo() {
    if std::env::var("CARGO_FETCHER_CRATES_IO_PROTOCOL")
        .ok()
        .as_deref()
        == Some("git")
    {
        // Git registry is too unstable for diffing as the index changes too often
        return;
    }

    util::hook_logger();

    let fs_root = util::tempdir();
    let (the_krates, registries) = cf::cargo::read_lock_files(
        vec!["tests/full/Cargo.lock".into()],
        vec![util::crates_io_registry()],
    )
    .unwrap();

    let mut fs_ctx = util::fs_ctx(fs_root.pb(), registries);
    fs_ctx.krates = the_krates;

    let fetcher_root = util::tempdir();
    let cargo_home = util::tempdir();
    let cargo_home_path = cargo_home.pb();

    // Fetch with cargo
    let cargo_fetch = std::thread::spawn(move || {
        std::process::Command::new("cargo")
            .env("CARGO_HOME", cargo_home_path)
            .args([
                "fetch",
                "--quiet",
                "--locked",
                "--manifest-path",
                "tests/full/Cargo.toml",
            ])
            .status()
            .unwrap();
    });

    // Synchronize with cargo-fetcher
    {
        fs_ctx.root_dir = fetcher_root.pb();

        let registry_sets = fs_ctx.registry_sets();

        assert_eq!(registry_sets.len(), 1);
        let the_registry = fs_ctx.registries[0].clone();

        cf::mirror::registry_indices(&fs_ctx, std::time::Duration::new(10, 0), registry_sets).await;
        cf::mirror::crates(&fs_ctx)
            .await
            .expect("failed to mirror crates");

        fs_ctx.prep_sync_dirs().expect("create base dirs");
        cf::sync::crates(&fs_ctx).await.expect("synced crates");
        cf::sync::registry_index(&fs_ctx.root_dir, fs_ctx.backend.clone(), the_registry)
            .await
            .expect("failed to sync index");
    }

    cargo_fetch.join().unwrap();

    if std::env::var_os("CARGO_FETCHER_DEBUG_DIFF_CARGO").is_none() {
        assert_diff(&fetcher_root, &cargo_home);
    } else {
        // Can be useful when iterating to keep the temp directories
        let fetcher_root = fetcher_root.into_path();
        let cargo_home = cargo_home.into_path();

        // Compare the outputs to ensure they match "exactly"
        assert_diff(fetcher_root, cargo_home);
    }
}

/// Validates that a cargo sync following a fetcher sync should do nothing
#[tokio::test]
async fn nothing_to_do() {
    if std::env::var("CARGO_FETCHER_CRATES_IO_PROTOCOL")
        .ok()
        .as_deref()
        == Some("git")
    {
        // Git registry is too unstable for diffing as the index changes too often
        return;
    }

    util::hook_logger();

    let sync_dir = util::tempdir();
    let fs_root = util::tempdir();

    {
        let (the_krates, registries) = cf::cargo::read_lock_files(
            vec!["tests/full/Cargo.lock".into()],
            vec![util::crates_io_registry()],
        )
        .unwrap();

        let mut fs_ctx = util::fs_ctx(fs_root.pb(), registries);
        fs_ctx.krates = the_krates;
        fs_ctx.root_dir = sync_dir.pb();

        let registry_sets = fs_ctx.registry_sets();
        let the_registry = fs_ctx.registries[0].clone();

        cf::mirror::registry_indices(&fs_ctx, std::time::Duration::new(10, 0), registry_sets).await;
        cf::mirror::crates(&fs_ctx)
            .await
            .expect("failed to mirror crates");

        fs_ctx.prep_sync_dirs().expect("create base dirs");
        cf::sync::crates(&fs_ctx).await.expect("synced crates");
        cf::sync::registry_index(&fs_ctx.root_dir, fs_ctx.backend.clone(), the_registry)
            .await
            .expect("failed to sync index");
    }

    let output = std::process::Command::new("cargo")
        .env("CARGO_HOME", sync_dir.path())
        .args([
            "fetch",
            "--locked",
            "--manifest-path",
            "tests/full/Cargo.toml",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    if !stdout.is_empty() || !stderr.is_empty() {
        panic!("expected no output from cargo, got:\nstdout:\n{stdout}\nstderr:{stderr}\n");
    }
}
