use super::*;
use serde_json::Value;
use std::collections::BTreeMap;

fn load() -> Value {
    serde_json::from_str(include_str!("../../../../test-vectors/trust.json"))
        .expect("the vector file is JSON")
}

fn state(root: &str) -> TrustState {
    TrustState::load(None, parse_key(root).unwrap().key)
}

fn accepted(result: Result<Accepted, String>) -> Value {
    match result {
        Ok(Accepted::Updated) => Value::String("updated".into()),
        Ok(Accepted::Unchanged) => Value::String("unchanged".into()),
        Err(message) => serde_json::json!({ "error": message }),
    }
}

#[test]
fn times() {
    let v = load();
    for c in v["times"].as_array().unwrap() {
        let input = c["input"].as_str().unwrap();
        assert_eq!(parse_utc(input), c["secs"].as_u64(), "{input:?}");
    }
}

#[test]
fn statements() {
    let v = load();
    let root = parse_key(v["root"].as_str().unwrap()).unwrap().key;
    for c in v["statements"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let doc = c["doc"].as_str().unwrap().as_bytes();
        let sig = c["sig"].as_str().unwrap().as_bytes();
        let verify = match verify_statement(doc, sig, &root) {
            Ok(s) => serde_json::json!({ "ok": {
                "serial": s.serial,
                "bundle_keys": s.bundle_keys().iter().map(|k| k.id.clone()).collect::<Vec<_>>(),
                "caps": s.caps().iter().map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect::<serde_json::Map<_, _>>(),
                "updater_keys": s.updater_keys.len(),
            }}),
            Err(message) => serde_json::json!({ "error": message }),
        };
        assert_eq!(verify, c["verify"], "verify: {name}");
        let got = state(v["root"].as_str().unwrap()).accept(doc, sig, c["now"].as_u64().unwrap());
        assert_eq!(accepted(got), c["accept"], "accept: {name}");
    }
}

/// The same statement in forms a strict reader refuses: byte order mark,
/// repeated keys, unknown fields, wrong types, data after the document.
#[test]
fn statement_json() {
    let v: Value = serde_json::from_str(include_str!("../../../../test-vectors/json.json"))
        .expect("the vector file is JSON");
    let root = parse_key(v["root"].as_str().unwrap()).unwrap().key;
    for c in v["statements"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let doc = STANDARD.decode(c["doc"].as_str().unwrap()).unwrap();
        let sig = c["sig"].as_str().unwrap().as_bytes();
        let verify = match verify_statement(&doc, sig, &root) {
            Ok(s) => serde_json::json!({ "ok": {
                "serial": s.serial,
                "bundle_keys": s.bundle_keys().iter().map(|k| k.id.clone()).collect::<Vec<_>>(),
                "caps": s.caps().iter().map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect::<serde_json::Map<_, _>>(),
            }}),
            Err(message) => serde_json::json!({ "error": message }),
        };
        assert_eq!(verify, c["verify"], "{name}");
    }
}

#[test]
fn sequences() {
    let v = load();
    for seq in v["sequences"].as_array().unwrap() {
        let mut trust = state(v["root"].as_str().unwrap());
        for (i, s) in seq["steps"].as_array().unwrap().iter().enumerate() {
            let got = trust.accept(
                s["doc"].as_str().unwrap().as_bytes(),
                s["sig"].as_str().unwrap().as_bytes(),
                s["now"].as_u64().unwrap(),
            );
            let at = format!("{} #{i}", seq["name"].as_str().unwrap());
            assert_eq!(accepted(got), s["expect"], "{at}");
            assert_eq!(
                trust.statement().map(|s| s.serial),
                s["serial"].as_u64(),
                "{at} serial"
            );
        }
    }
}

#[test]
fn minisign_ids_and_updater_choices() {
    let v = load();
    for c in v["minisign_ids"].as_array().unwrap() {
        let input = c["input"].as_str().unwrap();
        let got = match c["kind"].as_str().unwrap() {
            "key" => minisign_key_id(input),
            _ => signature_key_id(input),
        };
        assert_eq!(got.as_deref(), c["id"].as_str(), "{} {input:?}", c["kind"]);
    }
    for c in v["updater_choices"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let list = |key: &str| -> Vec<String> {
            c["statement"][key]
                .as_array()
                .unwrap()
                .iter()
                .map(|k| k.as_str().unwrap().to_owned())
                .collect()
        };
        let statement = c["statement"].as_object().map(|_| Statement {
            schema: 1,
            serial: 2,
            issued_at: "x".into(),
            expires_at: "y".into(),
            bundle_keys: vec![],
            revoked_bundle_keys: vec![],
            updater_keys: list("updater_keys"),
            revoked_updater_keys: list("revoked_updater_keys"),
            max_version: BTreeMap::new(),
        });
        let got = choose_updater_key(
            c["pinned"].as_str().unwrap(),
            statement.as_ref(),
            c["signature"].as_str().unwrap(),
        );
        let got = match got {
            Ok(key) => serde_json::json!({ "key": key }),
            Err(UpdaterRefusal::Revoked) => serde_json::json!({ "refusal": "revoked" }),
            Err(UpdaterRefusal::NotAllowed) => serde_json::json!({ "refusal": "not_allowed" }),
            Err(UpdaterRefusal::Malformed) => serde_json::json!({ "refusal": "malformed" }),
        };
        assert_eq!(got, c["expect"], "{name}");
    }
}
