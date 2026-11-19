use super::*;
use crate::web_bundle::Version;
use crate::web_bundle::fixture::{signing_key, trusted};
use ed25519_dalek::Signer;
use std::collections::BTreeMap;

const UPDATER: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDg3MUUyOTlDQUVCRjZCMkYKUldRdmE3K3VuQ2tlaDdOajlZaG9vbENvRFBXQmtQd1pKUjZmK1hwRW8rSCt4b04rZzhwWVY3SkEK";

/// 2026-11-01T00:00:00Z
const NOW: u64 = 1_793_491_200;

fn root(seed: u8) -> ed25519_dalek::SigningKey {
    signing_key(seed)
}

fn key_hex(seed: u8) -> String {
    hex::encode(signing_key(seed).verifying_key().to_bytes())
}

fn key_id_of(seed: u8) -> String {
    crate::web_bundle::key_id(&signing_key(seed).verifying_key().to_bytes())
}

/// A statement allowing the bundle keys of the given seeds and revoking the
/// keys of the revoked seeds.
fn doc(serial: u64, allowed: &[u8], revoked: &[u8]) -> String {
    serde_json::json!({
        "schema": 1,
        "serial": serial,
        "issued_at": "2026-10-17T10:00:00Z",
        "expires_at": "2027-01-15T10:00:00Z",
        "bundle_keys": allowed.iter().map(|s| key_hex(*s)).collect::<Vec<_>>(),
        "revoked_bundle_keys": revoked.iter().map(|s| key_id_of(*s)).collect::<Vec<_>>(),
        "updater_keys": [],
        "revoked_updater_keys": [],
    })
    .to_string()
}

fn signed(key: &ed25519_dalek::SigningKey, doc: &str) -> (Vec<u8>, Vec<u8>) {
    let sig = STANDARD.encode(key.sign(doc.as_bytes()).to_bytes());
    (doc.as_bytes().to_vec(), sig.into_bytes())
}

fn state(seed: u8) -> TrustState {
    TrustState::load(None, root(seed).verifying_key())
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mnema-trust-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn the_pinned_root_key_is_well_formed() {
    let _ = root_key();
}

#[test]
fn the_statement_from_the_signing_tool_verifies_with_the_pinned_root_key() {
    let doc = include_bytes!("../../fixtures/trust.json");
    let sig = include_bytes!("../../fixtures/trust.json.sig");
    let statement = verify_statement(doc, sig, &root_key()).expect("pinned root verifies");
    assert_eq!(statement.serial, 3);
    let keys = statement.bundle_keys();
    assert!(
        crate::web_bundle::trusted_keys()
            .iter()
            .all(|p| keys.iter().any(|k| k.id == p.id)),
        "every compiled-in key is allowed by the first statement"
    );
    let pinned_updater = minisign_key_id(UPDATER).unwrap();
    assert!(
        statement
            .updater_keys
            .iter()
            .any(|k| minisign_key_id(k).as_deref() == Some(pinned_updater.as_str()))
    );
}

#[test]
fn a_statement_signed_by_the_root_key_is_accepted() {
    let (d, s) = signed(&root(9), &doc(1, &[1], &[]));
    let mut trust = state(9);
    assert_eq!(trust.accept(&d, &s, NOW), Ok(Accepted::Updated));
    assert_eq!(trust.statement().unwrap().serial, 1);
}

#[test]
fn a_statement_signed_by_another_key_is_refused() {
    let (d, s) = signed(&root(8), &doc(1, &[1], &[]));
    let mut trust = state(9);
    assert!(trust.accept(&d, &s, NOW).is_err());
    assert!(trust.statement().is_none());
}

#[test]
fn a_tampered_statement_or_signature_is_refused() {
    let (d, s) = signed(&root(9), &doc(1, &[1], &[]));
    let mut trust = state(9);
    let mut bad = d.clone();
    bad[10] ^= 1;
    assert!(trust.accept(&bad, &s, NOW).is_err());
    assert!(trust.accept(&d, b"AAAA", NOW).is_err());
    assert!(trust.accept(&d, b"", NOW).is_err());
    let mut bad_sig = s.clone();
    bad_sig[0] = if bad_sig[0] == b'A' { b'B' } else { b'A' };
    assert!(trust.accept(&d, &bad_sig, NOW).is_err());
    assert!(trust.statement().is_none());
}

#[test]
fn a_correctly_signed_statement_must_still_be_well_formed() {
    let mut trust = state(9);
    for text in [
        doc(0, &[1], &[]),
        doc(1, &[], &[]),
        doc(1, &[1], &[1]),
        "{}".to_owned(),
        doc(1, &[1], &[]).replace("\"schema\":1", "\"schema\":2"),
        doc(1, &[1], &[]).replace("\"serial\"", "\"extra\":1,\"serial\""),
        doc(1, &[1], &[]).replace("2027-01-15", "2026-10-17"),
        doc(1, &[1], &[]).replace("2027-01-15", "2029-01-15"),
        doc(1, &[1], &[]).replace("2027-01-15T10:00:00Z", "soon"),
    ] {
        let (d, s) = signed(&root(9), &text);
        assert!(trust.accept(&d, &s, NOW).is_err(), "{text}");
    }
}

#[test]
fn an_older_serial_is_refused_and_an_equal_one_changes_nothing() {
    let mut trust = state(9);
    let (d2, s2) = signed(&root(9), &doc(2, &[2], &[1]));
    let (d1, s1) = signed(&root(9), &doc(1, &[1], &[]));
    assert_eq!(trust.accept(&d2, &s2, NOW), Ok(Accepted::Updated));
    assert!(trust.accept(&d1, &s1, NOW).unwrap_err().contains("older"));
    assert_eq!(trust.accept(&d2, &s2, NOW), Ok(Accepted::Unchanged));
    let (dx, sx) = signed(&root(9), &doc(2, &[3], &[]));
    assert!(trust.accept(&dx, &sx, NOW).is_err());
    assert_eq!(trust.statement().unwrap().serial, 2);
}

#[test]
fn a_revoked_key_is_dropped_and_a_new_one_is_allowed() {
    let mut trust = state(9);
    let pinned = trusted(1);
    assert_eq!(
        trust.effective_keys(trusted(1))[0].id,
        pinned[0].id,
        "default until a statement arrives"
    );
    let (d, s) = signed(&root(9), &doc(2, &[2], &[1]));
    trust.accept(&d, &s, NOW).unwrap();
    let keys = trust.effective_keys(trusted(1));
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].id, trusted(2)[0].id);
    assert!(keys.iter().all(|k| k.id != pinned[0].id));
}

#[test]
fn the_last_good_statement_survives_a_restart_and_a_damaged_file_is_ignored() {
    let d = dir("persist");
    let (doc_bytes, sig) = signed(&root(9), &doc(3, &[2], &[1]));
    let mut trust = TrustState::load(Some(&d), root(9).verifying_key());
    assert!(trust.statement().is_none(), "offline, no statement yet");
    trust.accept(&doc_bytes, &sig, NOW).unwrap();
    let again = TrustState::load(Some(&d), root(9).verifying_key());
    assert_eq!(again.statement().unwrap().serial, 3);
    // The same files under another root key are not a statement.
    assert!(
        TrustState::load(Some(&d), root(8).verifying_key())
            .statement()
            .is_none()
    );
    let mut edited = doc_bytes.clone();
    edited[5] ^= 1;
    let mut stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(d.join(STATE_FILE)).unwrap()).unwrap();
    stored["statement"] = STANDARD.encode(edited).into();
    std::fs::write(d.join(STATE_FILE), stored.to_string()).unwrap();
    assert!(
        TrustState::load(Some(&d), root(9).verifying_key())
            .statement()
            .is_none()
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_statement_and_its_signature_are_stored_as_one_file() {
    let d = dir("one-file");
    let (doc_bytes, sig) = signed(&root(9), &doc(3, &[2], &[1]));
    let mut trust = TrustState::load(Some(&d), root(9).verifying_key());
    trust.accept(&doc_bytes, &sig, NOW).unwrap();
    let mut names: Vec<_> = std::fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [STATE_FILE],
        "no second file that could be torn from it"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_pair_an_older_client_stored_is_read_and_replaced() {
    let d = dir("legacy");
    let (doc_bytes, sig) = signed(&root(9), &doc(2, &[2], &[1]));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(STATEMENT_FILE), &doc_bytes).unwrap();
    std::fs::write(d.join(SIGNATURE_FILE), &sig).unwrap();
    let mut trust = TrustState::load(Some(&d), root(9).verifying_key());
    assert_eq!(trust.statement().unwrap().serial, 2);
    let (next_doc, next_sig) = signed(&root(9), &doc(3, &[2], &[1]));
    trust.accept(&next_doc, &next_sig, NOW).unwrap();
    assert!(!d.join(STATEMENT_FILE).exists() && !d.join(SIGNATURE_FILE).exists());
    let again = TrustState::load(Some(&d), root(9).verifying_key());
    assert_eq!(again.statement().unwrap().serial, 3);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_verified_statement_applies_even_when_it_cannot_be_stored() {
    let d = dir("unwritable");
    std::fs::create_dir_all(d.parent().unwrap()).unwrap();
    // A file where the directory should be: nothing can be stored.
    std::fs::write(&d, b"in the way").unwrap();
    let (doc_bytes, sig) = signed(&root(9), &doc(2, &[2], &[1]));
    let mut trust = TrustState::load(Some(&d), root(9).verifying_key());
    assert_eq!(trust.accept(&doc_bytes, &sig, NOW), Ok(Accepted::Updated));
    assert_eq!(trust.statement().unwrap().serial, 2);
    assert!(
        trust
            .effective_keys(trusted(1))
            .iter()
            .all(|k| k.id != trusted(1)[0].id),
        "the revocation is enforced in this process"
    );
    // Once the directory works, the same fetch stores it.
    std::fs::remove_file(&d).unwrap();
    assert_eq!(trust.accept(&doc_bytes, &sig, NOW), Ok(Accepted::Unchanged));
    let again = TrustState::load(Some(&d), root(9).verifying_key());
    assert_eq!(again.statement().unwrap().serial, 2);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_later_statement_cannot_allow_a_revoked_key_again() {
    let mut trust = state(9);
    let (d2, s2) = signed(&root(9), &doc(2, &[2], &[1]));
    trust.accept(&d2, &s2, NOW).unwrap();
    // Serial 3 was signed without carrying the revocation forward.
    let (d3, s3) = signed(&root(9), &doc(3, &[1, 2], &[]));
    assert!(
        trust
            .accept(&d3, &s3, NOW)
            .unwrap_err()
            .contains("allowed again")
    );
    assert_eq!(trust.statement().unwrap().serial, 2);
    // One that merely forgets to list it keeps the key revoked.
    let (d4, s4) = signed(&root(9), &doc(4, &[2, 3], &[]));
    trust.accept(&d4, &s4, NOW).unwrap();
    assert!(
        trust
            .effective_keys(trusted(1))
            .iter()
            .all(|k| k.id != key_id_of(1))
    );
    assert!(
        trust
            .statement()
            .unwrap()
            .revoked_bundle_keys
            .contains(&key_id_of(1))
    );
}

#[test]
fn revocations_survive_a_restart_and_apply_to_updater_keys() {
    let d = dir("cumulative");
    let (old, old_pair) = pair();
    let (new, _) = pair();
    let old_id = minisign_key_id(&old).unwrap();
    let with_updaters = |serial: u64, allowed: &[&str], revoked: &[&str]| {
        let mut v: serde_json::Value = serde_json::from_str(&doc(serial, &[2], &[])).unwrap();
        v["updater_keys"] = allowed.to_vec().into();
        v["revoked_updater_keys"] = revoked.to_vec().into();
        v.to_string()
    };
    let mut trust = TrustState::load(Some(&d), root(9).verifying_key());
    let (d1, s1) = signed(&root(9), &with_updaters(1, &[&new], &[&old_id]));
    trust.accept(&d1, &s1, NOW).unwrap();
    let (d2, s2) = signed(&root(9), &with_updaters(2, &[&old, &new], &[]));
    let mut again = TrustState::load(Some(&d), root(9).verifying_key());
    assert!(again.accept(&d2, &s2, NOW).is_err());
    let (d3, s3) = signed(&root(9), &with_updaters(3, &[&new], &[]));
    again.accept(&d3, &s3, NOW).unwrap();
    assert_eq!(
        choose_updater_key(&old, again.statement(), &sig_by(&old_pair)),
        Err(UpdaterRefusal::Revoked),
        "serial 3 did not list the revocation, it still holds"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_floor_of_a_release_refuses_old_statements_and_revoked_keys() {
    let floor = Floor {
        min_serial: 3,
        revoked_bundle_keys: &["0123456789abcdef"],
        revoked_updater_keys: &[],
    };
    let fresh = || TrustState::load_with(None, root(9).verifying_key(), floor);
    // No stored statement: a replayed old one is not taken.
    let (d2, s2) = signed(&root(9), &doc(2, &[2], &[1]));
    assert!(
        fresh()
            .accept(&d2, &s2, NOW)
            .unwrap_err()
            .contains("this release")
    );
    let (d3, s3) = signed(&root(9), &doc(3, &[2], &[1]));
    assert_eq!(fresh().accept(&d3, &s3, NOW), Ok(Accepted::Updated));
    // A key the release dropped stays out of every statement and every list.
    let mut revoked_at_build = fresh();
    assert!(revoked_at_build.revoked_bundle.contains("0123456789abcdef"));
    revoked_at_build.revoked_bundle.insert(key_id_of(1));
    let (d4, s4) = signed(&root(9), &doc(4, &[1], &[]));
    assert!(revoked_at_build.accept(&d4, &s4, NOW).is_err());
    // Stored below the floor (an older release's file): ignored.
    let d = dir("floor");
    TrustState::load(Some(&d), root(9).verifying_key())
        .accept(&d2, &s2, NOW)
        .unwrap();
    let held = TrustState::load_with(Some(&d), root(9).verifying_key(), floor);
    assert!(held.statement().is_none());
    let _ = std::fs::remove_dir_all(&d);
}

fn pair() -> (String, minisign::KeyPair) {
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    (STANDARD.encode(pair.pk.to_box().unwrap().to_string()), pair)
}

fn sig_by(pair: &minisign::KeyPair) -> String {
    STANDARD.encode(
        minisign::sign(Some(&pair.pk), &pair.sk, &b"installer"[..], Some("c"), None)
            .unwrap()
            .to_string(),
    )
}

#[test]
fn minisign_key_ids_are_read_from_keys_and_signatures() {
    let (pubkey, pair) = pair();
    let id = minisign_key_id(&pubkey).unwrap();
    assert_eq!(
        signature_key_id(&sig_by(&pair)).as_deref(),
        Some(id.as_str())
    );
    assert_eq!(
        minisign_key_id(UPDATER).as_deref(),
        Some("2f6bbfae9c291e87")
    );
    assert!(minisign_key_id("%%%").is_none() && signature_key_id("").is_none());
}

fn updater_statement(allowed: &[&str], revoked: &[&str]) -> Statement {
    Statement {
        schema: 1,
        serial: 2,
        issued_at: "2026-10-17T10:00:00Z".into(),
        expires_at: "2027-01-15T10:00:00Z".into(),
        bundle_keys: vec![key_hex(1)],
        revoked_bundle_keys: vec![],
        updater_keys: allowed.iter().map(|k| (*k).to_owned()).collect(),
        revoked_updater_keys: revoked.iter().map(|k| (*k).to_owned()).collect(),
        max_version: BTreeMap::new(),
    }
}

#[test]
fn updates_are_checked_against_the_updater_keys_of_the_statement() {
    let (old, old_pair) = pair();
    let (new, new_pair) = pair();
    let (old_sig, new_sig) = (sig_by(&old_pair), sig_by(&new_pair));
    let old_id = minisign_key_id(&old).unwrap();

    // No statement: the pinned key decides, whatever the signature says.
    assert_eq!(choose_updater_key(&old, None, &new_sig), Ok(old.clone()));

    // Listed and not revoked: accepted. A statement can introduce a key.
    let both = updater_statement(&[&old, &new], &[]);
    assert_eq!(
        choose_updater_key(&old, Some(&both), &old_sig),
        Ok(old.clone())
    );
    assert_eq!(
        choose_updater_key(&old, Some(&both), &new_sig),
        Ok(new.clone())
    );

    // Revoked: refused, even if it is the pinned one.
    let rotated = updater_statement(&[&new], &[&old_id]);
    assert_eq!(
        choose_updater_key(&old, Some(&rotated), &old_sig),
        Err(UpdaterRefusal::Revoked)
    );
    assert_eq!(
        choose_updater_key(&old, Some(&rotated), &new_sig),
        Ok(new.clone())
    );

    // Not listed: refused. Malformed signature: refused.
    let only_new = updater_statement(&[&new], &[]);
    assert_eq!(
        choose_updater_key(&old, Some(&only_new), &old_sig),
        Err(UpdaterRefusal::NotAllowed)
    );
    assert_eq!(
        choose_updater_key(&old, Some(&only_new), "garbage"),
        Err(UpdaterRefusal::Malformed)
    );

    // No updater keys listed: the pinned key stays, minus revocations.
    let none = updater_statement(&[], &[]);
    assert_eq!(
        choose_updater_key(&old, Some(&none), &old_sig),
        Ok(old.clone())
    );
    let revoked_only = updater_statement(&[], &[&old_id]);
    assert_eq!(
        choose_updater_key(&old, Some(&revoked_only), &old_sig),
        Err(UpdaterRefusal::Revoked)
    );
}

#[test]
fn times_are_parsed_strictly_to_unix_seconds() {
    assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_utc("2026-11-01T00:00:00Z"), Some(NOW));
    assert_eq!(parse_utc("2024-02-29T12:30:45Z"), Some(1_709_209_845));
    assert_eq!(parse_utc("2000-02-29T00:00:00Z"), Some(951_782_400));
    assert!(parse_utc("2026-04-30T00:00:00Z").is_some());
    for bad in [
        "2026-11-01 00:00:00Z",
        "2026-11-01T00:00:00+00:00",
        "2026-13-01T00:00:00Z",
        "2026-11-32T00:00:00Z",
        "2026-02-29T00:00:00Z",
        "2026-02-31T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "2100-02-29T00:00:00Z",
        "2024-02-30T00:00:00Z",
        "2026-11-01T24:00:00Z",
        "2026-11-01T00:60:00Z",
        "2026-11-01T00:00:60Z",
        "2026-1x-01T00:00:00Z",
        "1969-12-31T23:59:59Z",
        "",
    ] {
        assert_eq!(parse_utc(bad), None, "{bad}");
    }
}

#[test]
fn an_expired_statement_is_never_accepted_and_a_stored_one_stays_in_force_but_stale() {
    let (d, s) = signed(&root(9), &doc(1, &[1], &[]));
    let after = parse_utc("2027-01-15T10:00:00Z").unwrap();
    let mut trust = state(9);
    assert!(!trust.fresh(NOW), "no statement is not fresh");
    assert!(trust.accept(&d, &s, after).unwrap_err().contains("expired"));
    assert!(trust.accept(&d, &s, after - 1).is_ok());
    assert!(trust.fresh(after - 1));
    // Past expiry: still the statement in force (its revocations count),
    // but not fresh, and a replay of it is refused.
    assert!(!trust.fresh(after));
    assert_eq!(trust.statement().unwrap().serial, 1);
    assert!(trust.accept(&d, &s, after).is_err());
    let again = TrustState::load(None, root(9).verifying_key());
    assert!(!again.fresh(NOW));
}

#[test]
fn caps_are_part_of_the_signed_statement_and_must_be_well_formed() {
    let with = |caps: serde_json::Value| {
        let mut v: serde_json::Value = serde_json::from_str(&doc(1, &[1], &[])).unwrap();
        v["max_version"] = caps;
        v.to_string()
    };
    let mut trust = state(9);
    for bad in [
        serde_json::json!({"x": "0.9.0"}),
        serde_json::json!({"0": "0.9.0"}),
        serde_json::json!({"1": "latest"}),
    ] {
        let (d, s) = signed(&root(9), &with(bad));
        assert!(trust.accept(&d, &s, NOW).is_err());
    }
    let (d, s) = signed(
        &root(9),
        &with(serde_json::json!({"1": "0.9.0", "2": "1.0.0"})),
    );
    trust.accept(&d, &s, NOW).unwrap();
    let caps = trust.statement().unwrap().caps();
    assert_eq!(caps[&1], Version(0, 9, 0));
    assert_eq!(caps[&2], Version(1, 0, 0));
    // Statements without the field have no caps.
    let (d, s) = signed(&root(9), &doc(2, &[1], &[]));
    trust.accept(&d, &s, NOW).unwrap();
    assert!(trust.statement().unwrap().caps().is_empty());
}
