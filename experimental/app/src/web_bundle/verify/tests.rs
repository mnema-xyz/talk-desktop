use super::*;
use crate::web_bundle::fixture::*;

#[test]
fn path_rules_match_the_signing_tool() {
    for ok in ["index.html", "assets/a-b.js", "\u{fc}n\u{ef}/x.js"] {
        assert!(valid_path(ok), "{ok}");
    }
    let long = "a".repeat(MAX_PATH + 1);
    for bad in [
        "",
        "/abs",
        "a//b",
        "a/./b",
        "a/../b",
        "..",
        ".",
        "a/",
        "a\\b",
        "a\0b",
        "a\u{7f}",
        "a\nb",
        ".hidden",
        "a/.well-known/x",
        long.as_str(),
    ] {
        assert!(!valid_path(bad), "{bad:?}");
    }
}

#[test]
fn a_bundle_signed_by_a_pinned_key_verifies_and_serves_its_files() {
    let bundle = verify(&bundle(&Spec::new("0.8.0")), &trusted(7)).unwrap();
    assert_eq!(bundle.version, Version(0, 8, 0));
    assert_eq!(bundle.web_client_api, 1);
    assert_eq!(
        bundle.file("index.html"),
        Some(b"<html>app</html>".as_slice())
    );
    assert_eq!(bundle.file("missing"), None);
    assert_eq!(bundle.commit, "test");
    assert_eq!(bundle.key_id, trusted(7)[0].id);
}

#[test]
fn a_bundle_from_another_key_or_with_no_keys_is_refused() {
    let data = bundle(&Spec::new("0.8.0"));
    assert!(verify(&data, &trusted(8)).is_err());
    assert!(verify(&data, &[]).is_err());
}

fn damaged(edit: impl FnOnce(&mut Vec<(String, Vec<u8>)>)) -> Result<Bundle, String> {
    let mut e = entries(&Spec::new("0.8.0"));
    edit(&mut e);
    verify(&zip_of(&e), &trusted(7))
}

fn entry<'a>(e: &'a mut [(String, Vec<u8>)], name: &str) -> &'a mut Vec<u8> {
    &mut e.iter_mut().find(|(n, _)| n == name).unwrap().1
}

/// Re-signs an edited manifest so only the manifest rules are exercised.
fn resigned(edit: impl FnOnce(&mut serde_json::Value)) -> Result<Bundle, String> {
    use ed25519_dalek::Signer;
    damaged(|e| {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(entry(e, "manifest.json")).unwrap();
        edit(&mut manifest);
        let raw = manifest.to_string().into_bytes();
        *entry(e, "manifest.sig") = STANDARD
            .encode(signing_key(7).sign(&raw).to_bytes())
            .into_bytes();
        *entry(e, "manifest.json") = raw;
    })
}

#[test]
fn tampering_with_any_part_of_a_signed_bundle_is_refused() {
    assert!(damaged(|e| *entry(e, "app/index.html") = b"<html>evil</html>".to_vec()).is_err());
    assert!(damaged(|e| e.push(("app/evil.js".into(), b"x".to_vec()))).is_err());
    assert!(damaged(|e| e.retain(|(n, _)| n != "app/assets/app-1.js")).is_err());
    assert!(
        damaged(|e| {
            e.retain(|(n, _)| n != "app/assets/app-1.js");
            e.push(("app/assets/other.js".into(), b"console.log(1)".to_vec()));
        })
        .is_err()
    );
    assert!(damaged(|e| e.retain(|(n, _)| n != "manifest.json")).is_err());
    assert!(damaged(|e| e.retain(|(n, _)| n != "manifest.sig")).is_err());
    assert!(
        damaged(|e| {
            let manifest = entry(e, "manifest.json");
            *manifest = String::from_utf8(manifest.clone())
                .unwrap()
                .replace("0.8.0", "9.9.9")
                .into_bytes();
        })
        .is_err()
    );
    assert!(damaged(|e| *entry(e, "manifest.sig") = b"***".to_vec()).is_err());
    assert!(damaged(|e| *entry(e, "manifest.sig") = b"AAAA".to_vec()).is_err());
    assert!(
        damaged(|e| *entry(e, "manifest.json") = vec![b' '; (MAX_MANIFEST + 1) as usize]).is_err()
    );
    assert!(damaged(|e| *entry(e, "manifest.sig") = vec![b'A'; 2000]).is_err());
    assert!(verify(b"PK not a zip", &trusted(7)).is_err());
}

#[test]
fn a_correctly_signed_manifest_still_has_to_be_well_formed() {
    assert!(resigned(|_| {}).is_ok());
    for (field, value) in [
        ("schema", serde_json::json!(2)),
        ("name", serde_json::json!("other")),
        ("key_id", serde_json::json!("0000000000000000")),
        ("version", serde_json::json!("1")),
        ("web_client_api", serde_json::json!(0)),
        ("min_server_version", serde_json::json!("9.0.0")),
        ("security_floor", serde_json::json!("9.0.0")),
        ("files", serde_json::json!([])),
    ] {
        assert!(resigned(|m| m[field] = value.clone()).is_err(), "{field}");
    }
    assert!(resigned(|m| m["surprise"] = serde_json::json!(1)).is_err());
    assert!(resigned(|m| m["files"][0]["path"] = serde_json::json!("../x")).is_err());
    assert!(resigned(|m| m["files"][1]["path"] = m["files"][0]["path"].clone()).is_err());
    assert!(resigned(|m| m["files"][0]["sha256"] = serde_json::json!("zz")).is_err());
    assert!(resigned(|m| m["files"][0]["sha256"] = serde_json::json!("A".repeat(64))).is_err());
    assert!(resigned(|m| m["files"][0]["size"] = serde_json::json!(MAX_FILE + 1)).is_err());
    assert!(resigned(|m| m["files"][0]["size"] = serde_json::json!(1)).is_err());
    assert!(resigned(|m| m["files"][0]["sha256"] = serde_json::json!("0".repeat(64))).is_err());
    assert!(resigned(|m| m["files"][0]["path"] = serde_json::json!("home.html")).is_err());
    assert!(resigned(|m| {
        let files = m["files"].as_array_mut().unwrap();
        for i in 0..3 {
            files.push(serde_json::json!({"path": format!("big{i}"), "sha256": "0".repeat(64), "size": MAX_FILE}));
        }
        for file in files.iter_mut() {
            file["size"] = serde_json::json!(MAX_FILE);
        }
        files.push(serde_json::json!({"path": "big9", "sha256": "0".repeat(64), "size": MAX_FILE}));
        files.push(serde_json::json!({"path": "big10", "sha256": "0".repeat(64), "size": MAX_FILE}));
    })
    .is_err());
    assert!(resigned(|m| {
        let files = m["files"].as_array_mut().unwrap();
        for i in 0..=MAX_ENTRIES {
            files.push(serde_json::json!({"path": format!("f{i}"), "sha256": "0".repeat(64), "size": 0}));
        }
    })
    .is_err());
}

#[test]
fn too_many_zip_entries_are_refused_before_anything_is_read() {
    let mut e = entries(&Spec::new("0.8.0"));
    for i in 0..MAX_ENTRIES + 1 {
        e.push((format!("app/pad{i}"), Vec::new()));
    }
    assert_eq!(
        verify(&zip_of(&e), &trusted(7)).unwrap_err(),
        "too many entries"
    );
}
