use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
};

fn load(text: &str) -> Value {
    serde_json::from_str(text).expect("the vector file is JSON")
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect()
}

fn keys(hexes: &[&str]) -> Vec<TrustedKey> {
    hexes.iter().map(|h| parse_key(h).unwrap()).collect()
}

fn version(value: &Value) -> Option<Version> {
    value.as_array().map(|a| {
        Version(
            a[0].as_u64().unwrap() as u32,
            a[1].as_u64().unwrap() as u32,
            a[2].as_u64().unwrap() as u32,
        )
    })
}

#[test]
fn versions_and_paths() {
    let v = load(include_str!("../../../../test-vectors/bundle.json"));
    for c in v["versions"].as_array().unwrap() {
        let input = c["input"].as_str().unwrap();
        assert_eq!(Version::parse(input), version(&c["parsed"]), "{input:?}");
    }
    for c in v["paths"].as_array().unwrap() {
        let path = c["path"].as_str().unwrap();
        assert_eq!(valid_path(path), c["valid"].as_bool().unwrap(), "{path:?}");
    }
}

#[test]
fn bundles() {
    check_bundles(&load(include_str!("../../../../test-vectors/bundle.json")));
}

/// The same manifest in forms a strict reader refuses: byte order mark, repeated
/// keys, unknown fields, wrong types, data after the document.
#[test]
fn manifest_json() {
    check_bundles(&load(include_str!("../../../../test-vectors/json.json")));
}

/// Hostile zip structures: both clients refuse each of them with the same message.
#[test]
fn zip_structures() {
    let v = load(include_str!("../../../../test-vectors/zip.json"));
    check_bundles(&v);
    let open: Vec<&str> = v["bundles"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["result"]["ok"].is_object())
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        open,
        [
            "a plain stored bundle",
            "an entry that announces a data descriptor",
            "an entry with a regular file mode",
            "a link mode written by a system that does not use modes"
        ]
    );
}

fn check_bundles(v: &Value) {
    for c in v["bundles"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let zip = STANDARD.decode(c["zip"].as_str().unwrap()).unwrap();
        let result = verify(&zip, &keys(&strings(&c["trusted"])));
        match (&result, &c["result"]["ok"], &c["result"]["error"]) {
            (Ok(bundle), ok, Value::Null) => {
                assert_eq!(
                    bundle.version.to_string(),
                    ok["version"].as_str().unwrap(),
                    "{name}"
                );
                assert_eq!(
                    bundle.web_client_api as u64,
                    ok["web_client_api"].as_u64().unwrap(),
                    "{name}"
                );
                assert_eq!(
                    bundle.min_server_version.to_string(),
                    ok["min_server_version"].as_str().unwrap(),
                    "{name}"
                );
                assert_eq!(
                    bundle.security_floor.to_string(),
                    ok["security_floor"].as_str().unwrap(),
                    "{name}"
                );
                assert_eq!(bundle.key_id, ok["key_id"].as_str().unwrap(), "{name}");
                assert_eq!(bundle.commit, ok["commit"].as_str().unwrap(), "{name}");
                for (path, hash) in ok["files"].as_object().unwrap() {
                    let body = bundle
                        .file(path)
                        .unwrap_or_else(|| panic!("{name}: {path}"));
                    assert_eq!(
                        &hex::encode(Sha256::digest(body)),
                        hash.as_str().unwrap(),
                        "{name}: {path}"
                    );
                }
            }
            (Err(message), Value::Null, error) => {
                assert_eq!(message, error.as_str().unwrap(), "{name}");
            }
            _ => panic!(
                "{name}: the answers differ: {:?} against {}",
                result.as_ref().map(|b| b.version),
                c["result"]
            ),
        }
    }
}

fn named_keys(v: &Value, names: &Value) -> Vec<TrustedKey> {
    strings(names)
        .into_iter()
        .map(|n| parse_key(v["keys"][n].as_str().unwrap()).unwrap())
        .collect()
}

fn refusal_kind(refusal: &Refusal) -> &'static str {
    match refusal {
        Refusal::NoBundle => "no_bundle",
        Refusal::ServerNewer => "server_newer",
        Refusal::ServerTooOld { .. } => "server_too_old",
        Refusal::ServerContractRetired => "server_contract_retired",
        Refusal::RolledBack => "rolled_back",
        Refusal::SignerWithdrawn => "signer_withdrawn",
    }
}

/// One scenario of `store.json` while its steps run: the vector file, the
/// store under test and the folder and built-in bundle it was loaded from.
struct Scenario<'a> {
    v: &'a Value,
    store: Store,
    dir: Option<PathBuf>,
    builtin: Option<Vec<u8>>,
}

impl Scenario<'_> {
    fn bytes(&self, name: &Value) -> Vec<u8> {
        STANDARD
            .decode(self.v["bundles"][name.as_str().unwrap()].as_str().unwrap())
            .unwrap()
    }

    fn load(&self, trusted: &Value) -> Store {
        Store::load(
            self.dir.as_deref(),
            self.builtin.as_deref(),
            &named_keys(self.v, trusted),
        )
    }

    fn install(&mut self, step: &Value, at: &str) {
        let bundle = self.bytes(&step["bundle"]);
        let got = self
            .store
            .install(&bundle, &named_keys(self.v, &step["trusted"]));
        match (&got, &step["expect"]["ok"], &step["expect"]["error"]) {
            (Ok(version), ok, Value::Null) => {
                assert_eq!(version.to_string(), ok.as_str().unwrap(), "{at}")
            }
            (Err(message), Value::Null, error) => {
                assert_eq!(message, error.as_str().unwrap(), "{at}")
            }
            _ => panic!("{at}: {got:?} against {}", step["expect"]),
        }
    }

    fn select(&self, step: &Value, at: &str) {
        let server = ServerInfo {
            version: step["server"]["version"]
                .as_str()
                .map(|s| Version::parse(s).unwrap()),
            web_client_api: step["server"]["api"].as_u64().unwrap() as u32,
        };
        match self.store.select(&server) {
            Ok(held) => assert_eq!(
                held.bundle.version.to_string(),
                step["expect"]["ok"].as_str().unwrap(),
                "{at}"
            ),
            Err(refusal) => {
                assert_eq!(
                    refusal_kind(&refusal),
                    step["expect"]["refusal"].as_str().unwrap(),
                    "{at}"
                );
                if let Refusal::ServerTooOld { needs } = refusal {
                    assert_eq!(
                        needs.to_string(),
                        step["expect"]["needs"].as_str().unwrap(),
                        "{at}"
                    );
                }
            }
        }
    }

    fn caps(&mut self, step: &Value) {
        let caps: BTreeMap<u32, Version> = step["caps"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, c)| {
                (
                    k.parse().unwrap(),
                    Version::parse(c.as_str().unwrap()).unwrap(),
                )
            })
            .collect();
        self.store.apply_caps(&caps);
    }

    fn state(&self, step: &Value, at: &str) {
        let expect = &step["expect"];
        assert_eq!(
            self.store.floor().to_string(),
            expect["floor"].as_str().unwrap(),
            "{at}"
        );
        let mut high_water = serde_json::Map::new();
        for api in 1..=3u32 {
            if let Some(mark) = self.store.high_water(api) {
                high_water.insert(api.to_string(), Value::String(mark.to_string()));
            }
        }
        assert_eq!(Value::Object(high_water), expect["high_water"], "{at}");
        let held: Vec<String> = self
            .store
            .held()
            .iter()
            .map(|h| format!("{}:{}", h.bundle.version, h.source.as_str()))
            .collect();
        assert_eq!(held, strings(&expect["held"]), "{at}");
        assert_eq!(
            self.store.rejected.len() as u64,
            expect["rejected"].as_u64().unwrap(),
            "{at}"
        );
    }

    fn step(&mut self, step: &Value, at: &str) {
        match step["op"].as_str().unwrap() {
            "install" => self.install(step, at),
            "select" => self.select(step, at),
            "reconcile" => assert_eq!(
                self.store.reconcile(&named_keys(self.v, &step["trusted"])),
                step["expect"].as_bool().unwrap(),
                "{at}"
            ),
            "caps" => self.caps(step),
            "state" => self.state(step, at),
            "restart" => self.store = self.load(&step["trusted"]),
            "remove_cached" => {
                let file = format!("mnema-web-{}.zip", step["version"].as_str().unwrap());
                std::fs::remove_file(self.dir.as_ref().unwrap().join(file)).unwrap();
            }
            "forget_state" => {
                let _ = std::fs::remove_file(self.dir.as_ref().unwrap().join("state.json"));
            }
            other => panic!("{at}: unknown step {other}"),
        }
    }
}

#[test]
fn stores() {
    let v = load(include_str!("../../../../test-vectors/store.json"));
    for (index, s) in v["scenarios"].as_array().unwrap().iter().enumerate() {
        let name = s["name"].as_str().unwrap();
        let dir = s["persist"].as_bool().unwrap().then(|| {
            let d = std::env::temp_dir().join(format!("mnema-vec-{}-{index}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            d
        });
        let mut run = Scenario {
            v: &v,
            store: Store::load(None, None, &[]),
            dir,
            builtin: None,
        };
        run.builtin = s["builtin"]
            .as_str()
            .map(|b| run.bytes(&Value::String(b.to_owned())));
        run.store = run.load(&s["trusted"]);
        for (n, step) in s["steps"].as_array().unwrap().iter().enumerate() {
            run.step(
                step,
                &format!("{name} #{n} {}", step["op"].as_str().unwrap()),
            );
        }
        if let Some(d) = run.dir {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

/// The signed release bundle CI stages (`MNEMA_TEST_BUNDLE_ZIP`, from the
/// mnema-talk GitHub release) against the key pinned in this build, so a
/// change to the signature or hash crates is checked on real data, not only
/// on bundles the tests sign themselves; every tampered form is refused.
#[test]
fn a_real_signed_release_bundle_verifies_and_every_tampered_form_is_refused() {
    let Some(path) = std::env::var_os("MNEMA_TEST_BUNDLE_ZIP").filter(|p| Path::new(p).exists())
    else {
        return;
    };
    let data = std::fs::read(path).unwrap();
    let keys = trusted_keys();
    let bundle = verify(&data, &keys).expect("the release bundle verifies");
    assert!(bundle.file("index.html").is_some());

    type Entries = Vec<(String, Vec<u8>)>;
    let mut entries: Entries = {
        let mut zip = zip::ZipArchive::new(Cursor::new(&data[..])).unwrap();
        (0..zip.len())
            .filter_map(|i| {
                let mut f = zip.by_index(i).unwrap();
                if f.is_dir() {
                    return None;
                }
                let mut body = Vec::new();
                f.read_to_end(&mut body).unwrap();
                Some((f.name().unwrap().into_owned(), body))
            })
            .collect()
    };
    // Repacked unchanged: still valid, so the refusals below are the edits.
    verify(&fixture::zip_of(&entries), &keys).expect("the repacked bundle verifies");
    let refused = |edit: &dyn Fn(&mut Entries)| {
        let mut copy = entries.clone();
        edit(&mut copy);
        verify(&fixture::zip_of(&copy), &keys).is_err()
    };
    let flip = |e: &mut Entries, name: &str| {
        let body = &mut e.iter_mut().find(|(n, _)| n == name).unwrap().1;
        body[0] ^= 1;
    };
    assert!(refused(&|e| flip(e, "app/index.html")));
    assert!(refused(&|e| flip(e, "manifest.json")));
    assert!(refused(&|e| {
        let sig = &mut e.iter_mut().find(|(n, _)| n == "manifest.sig").unwrap().1;
        // A different, well-formed signature of the right length.
        let mut other = STANDARD.decode(sig.trim_ascii()).unwrap();
        other[0] ^= 1;
        *sig = STANDARD.encode(other).into_bytes();
    }));
    assert!(refused(&|e| e.push(("app/evil.js".into(), b"x".to_vec()))));
    assert!(refused(&|e| e.retain(|(n, _)| n != "manifest.sig")));
    // The right bundle under a key that did not sign it.
    assert!(verify(&data, &fixture::trusted(9)).is_err());
    entries.clear();
}
