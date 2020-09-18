use cargo_fetcher::read_lock_file;

#[test]
fn parses_v1() {
    let (krates, _) = read_lock_file("tests/v1.lock", Vec::new()).unwrap();
    assert_eq!(krates.len(), 258);
}

#[test]
fn parses_v2() {
    let (krates, _) = read_lock_file("tests/v2.lock", Vec::new()).unwrap();
    assert_eq!(krates.len(), 258);
}

#[test]
fn matches() {
    let (krates1, _) = read_lock_file("tests/v1.lock", Vec::new()).unwrap();
    let (krates2, _) = read_lock_file("tests/v2.lock", Vec::new()).unwrap();

    assert_eq!(krates1, krates2);
}
