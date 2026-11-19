use super::*;

#[test]
fn the_pinned_key_is_well_formed_and_matches_its_published_id() {
    let keys = trusted_keys();
    assert_eq!(keys.len(), PINNED.len());
    assert_eq!(keys[0].id, "1069c76ff2e11272");
    assert!(parse_key("00").is_none());
    assert!(parse_key("zz").is_none());
}

#[test]
fn versions_parse_strictly_and_order_numerically() {
    assert_eq!(Version::parse("0.7.16"), Some(Version(0, 7, 16)));
    for bad in [
        "",
        "1",
        "1.2",
        "1.2.3.4",
        "v1.2.3",
        "01.2.3",
        "1.2.x",
        "1..3",
        "1.2.3-rc1",
        "-1.0.0",
        "99999999999.0.0",
        "+1.0.0",
    ] {
        assert_eq!(Version::parse(bad), None, "{bad}");
    }
    assert!(Version(0, 7, 9) < Version(0, 7, 10));
    assert!(Version(0, 9, 0) < Version(1, 0, 0));
    assert_eq!(Version(1, 2, 3).to_string(), "1.2.3");
}
