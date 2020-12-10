use cargo_fetcher::{cargo::read_lock_file, Registry};

#[test]
fn parses_v1() {
    let (krates, _) = read_lock_file("tests/v1.lock", vec![Registry::default()]).unwrap();
    assert_eq!(krates.len(), 258);
}

#[test]
fn parses_v2() {
    let (krates, _) = read_lock_file("tests/v2.lock", vec![Registry::default()]).unwrap();
    assert_eq!(krates.len(), 258);
}

#[test]
fn matches() {
    let (krates1, _) = read_lock_file("tests/v1.lock", vec![Registry::default()]).unwrap();
    let (krates2, _) = read_lock_file("tests/v2.lock", vec![Registry::default()]).unwrap();

    assert_eq!(krates1, krates2);
}
