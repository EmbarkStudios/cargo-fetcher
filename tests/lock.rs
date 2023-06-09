use cargo_fetcher::{cargo::read_lock_files, Registry, RegistryProtocol};

#[test]
fn parses_v2() {
    let (krates, _) = read_lock_files(
        vec!["tests/v2.lock".into()],
        vec![Registry::crates_io(RegistryProtocol::Git)],
    )
    .unwrap();
    assert_eq!(krates.len(), 258);
}

#[test]
fn parses_v3() {
    let (krates, _) = read_lock_files(
        vec!["tests/v3.lock".into()],
        vec![Registry::crates_io(RegistryProtocol::Sparse)],
    )
    .unwrap();
    assert_eq!(krates.len(), 223);
}
