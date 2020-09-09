use cargo_fetcher::read_lock_file;

#[test]
fn parses_v1() {
    let krates = read_lock_file("tests/v1.lock", None).unwrap();
    assert_eq!(krates.len(), 258);
}

#[test]
fn parses_v2() {
    let krates = read_lock_file("tests/v2.lock", None).unwrap();
    assert_eq!(krates.len(), 258);
}

#[test]
fn matches() {
    let krates1 = read_lock_file("tests/v1.lock", None).unwrap();
    let krates2 = read_lock_file("tests/v2.lock", None).unwrap();

    assert_eq!(krates1, krates2);
}
