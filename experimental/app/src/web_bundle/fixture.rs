use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};
use std::io::Cursor;
use std::io::Write;

pub fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub fn trusted(seed: u8) -> Vec<TrustedKey> {
    let raw = signing_key(seed).verifying_key().to_bytes();
    vec![TrustedKey {
        id: key_id(&raw),
        key: VerifyingKey::from_bytes(&raw).unwrap(),
    }]
}

#[derive(Clone)]
pub struct Spec {
    pub version: &'static str,
    pub api: u32,
    pub min_server: &'static str,
    pub floor: &'static str,
    pub seed: u8,
    pub files: Vec<(&'static str, &'static str)>,
}

impl Spec {
    pub fn new(version: &'static str) -> Spec {
        Spec {
            version,
            api: 1,
            min_server: "0.1.0",
            floor: "0.0.0",
            seed: 7,
            files: vec![
                ("index.html", "<html>app</html>"),
                ("assets/app-1.js", "console.log(1)"),
            ],
        }
    }
}

pub fn manifest_json(spec: &Spec) -> String {
    let key = signing_key(spec.seed).verifying_key().to_bytes();
    let files: Vec<serde_json::Value> = spec
        .files
        .iter()
        .map(|(path, body)| {
            serde_json::json!({"path": path, "sha256": hex::encode(Sha256::digest(body.as_bytes())), "size": body.len()})
        })
        .collect();
    serde_json::json!({
        "schema": 1, "name": "mnema-web", "version": spec.version, "web_client_api": spec.api,
        "min_server_version": spec.min_server, "security_floor": spec.floor, "commit": "test",
        "key_id": key_id(&key), "files": files
    })
    .to_string()
}

/// Entries as (name, bytes), signed by `seed`'s key.
pub fn entries(spec: &Spec) -> Vec<(String, Vec<u8>)> {
    let manifest = manifest_json(spec);
    let signature = STANDARD.encode(signing_key(spec.seed).sign(manifest.as_bytes()).to_bytes());
    let mut entries = vec![
        ("manifest.json".to_owned(), manifest.into_bytes()),
        ("manifest.sig".to_owned(), signature.into_bytes()),
    ];
    for (path, body) in &spec.files {
        entries.push((format!("app/{path}"), body.as_bytes().to_vec()));
    }
    entries
}

pub fn zip_of(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(&mut out);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, body) in entries {
        writer.start_file(name.as_str(), options).unwrap();
        writer.write_all(body).unwrap();
    }
    writer.finish().unwrap();
    out.into_inner()
}

pub fn bundle(spec: &Spec) -> Vec<u8> {
    zip_of(&entries(spec))
}
