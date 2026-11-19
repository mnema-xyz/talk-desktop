//! Signed trust statement: remote revocation of the CI signing keys
//! (mnema-talk `doc/desktop-trusted-client.md`, "Key revocation").
//!
//! The client pins one offline root key. A statement signed by it names the
//! web bundle keys and Tauri updater keys that are allowed and the ones that
//! are revoked. Where the bytes come from decides nothing: the signature is
//! checked before anything is parsed, and a statement with a lower serial than
//! the highest one accepted is refused.
mod statement;
mod updater;

#[cfg(test)]
pub use statement::parse_utc;
pub use statement::{Statement, verify_statement};
#[cfg(test)]
pub use updater::{UpdaterRefusal, choose_updater_key, signature_key_id};
pub use updater::{minisign_key_id, publish, updater_key};

use crate::web_bundle::{TrustedKey, is_hex_id16, parse_key, write_atomic};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// Raw Ed25519 root public key (`desktop-trust/root.pub` in mnema-talk).
const ROOT: &str = "0c6b31c234adbeb1449c63acd03fcd044d7b15b640607f9943a1e3f6a819ed77";

pub const TRUST_URL: &str =
    "https://raw.githubusercontent.com/mnema-xyz/talk/main/desktop-trust/trust.json";
pub const TRUST_SIG_URL: &str =
    "https://raw.githubusercontent.com/mnema-xyz/talk/main/desktop-trust/trust.json.sig";
pub const MAX_STATEMENT: usize = 64 << 10;

const STATEMENT_FILE: &str = "trust.json";
const SIGNATURE_FILE: &str = "trust.json.sig";

pub fn root_key() -> VerifyingKey {
    let raw: [u8; 32] = hex::decode(ROOT)
        .ok()
        .and_then(|b| b.try_into().ok())
        .expect("the pinned root key is well formed");
    VerifyingKey::from_bytes(&raw).expect("the pinned root key is a valid point")
}

#[derive(Debug, PartialEq, Eq)]
pub enum Accepted {
    Updated,
    Unchanged,
}

/// What a release knows at build time, so a client with no stored statement
/// (a fresh install, a removed file) cannot be talked back into an older state.
#[derive(Debug, Clone, Copy)]
pub struct Floor {
    /// Statements with a lower serial are refused.
    pub min_serial: u64,
    /// Key ids no statement may allow again.
    pub revoked_bundle_keys: &'static [&'static str],
    pub revoked_updater_keys: &'static [&'static str],
}

impl Floor {
    #[cfg(test)]
    pub const NONE: Floor = Floor {
        min_serial: 0,
        revoked_bundle_keys: &[],
        revoked_updater_keys: &[],
    };
}

/// The floor of this release: the serial of the statement it shipped with
/// (`fixtures/trust.json`). Raise it, and list the key ids revoked since, when
/// a later statement revokes a key.
pub const RELEASE_FLOOR: Floor = Floor {
    min_serial: 3,
    revoked_bundle_keys: &[],
    revoked_updater_keys: &[],
};

/// Statement, signature and the revocations carried forward, in one file so a
/// crash can never leave a new signature next to an old document.
const STATE_FILE: &str = "trust-state.json";

#[derive(serde::Serialize, Deserialize)]
struct Stored {
    statement: String,
    signature: String,
    #[serde(default)]
    revoked_bundle_keys: Vec<String>,
    #[serde(default)]
    revoked_updater_keys: Vec<String>,
}

/// What the state file (or an older client's statement pair) holds: the
/// statement and its signature, and the revocations kept beside them.
struct StoredBytes {
    doc: Vec<u8>,
    sig: Vec<u8>,
    extra_bundle: Vec<String>,
    extra_updater: Vec<String>,
}

struct Held {
    statement: Statement,
    doc: Vec<u8>,
    sig: Vec<u8>,
}

/// The last good statement, persisted next to the bundle cache.
pub struct TrustState {
    dir: Option<PathBuf>,
    root: VerifyingKey,
    floor: Floor,
    current: Option<Held>,
    /// Every key id any accepted statement (or the floor) revoked. A later
    /// statement can neither allow one again nor drop it from the lists in force.
    revoked_bundle: BTreeSet<String>,
    revoked_updater: BTreeSet<String>,
    /// The state in force is on disk.
    stored: bool,
}

impl TrustState {
    /// Loads and re-verifies the stored statement; a damaged or foreign file is
    /// ignored, which falls back to the compiled-in keys.
    #[cfg(test)]
    pub fn load(dir: Option<&Path>, root: VerifyingKey) -> TrustState {
        Self::load_with(dir, root, Floor::NONE)
    }

    pub fn load_with(dir: Option<&Path>, root: VerifyingKey, floor: Floor) -> TrustState {
        let mut state = TrustState {
            dir: dir.map(Path::to_path_buf),
            root,
            floor,
            current: None,
            revoked_bundle: floor
                .revoked_bundle_keys
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            revoked_updater: floor
                .revoked_updater_keys
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            stored: true,
        };
        if let Some(StoredBytes {
            doc,
            sig,
            extra_bundle,
            extra_updater,
        }) = dir.and_then(|d| state.read_stored(d))
            && let Ok(statement) = verify_statement(&doc, &sig, &state.root)
            // A statement below the floor is an older release's.
            && statement.serial >= floor.min_serial
        {
            state.revoked_bundle.extend(extra_bundle);
            state.revoked_updater.extend(extra_updater);
            state.hold(statement, doc, sig);
        }
        state
    }

    /// The stored bytes: the state file, else the pair an older client wrote.
    fn read_stored(&self, dir: &Path) -> Option<StoredBytes> {
        if let Ok(raw) = std::fs::read(dir.join(STATE_FILE)) {
            let stored: Stored = serde_json::from_slice(&raw).ok()?;
            let ids = |list: Vec<String>| list.into_iter().filter(|id| is_hex_id16(id)).collect();
            return Some(StoredBytes {
                doc: STANDARD.decode(stored.statement).ok()?,
                sig: stored.signature.into_bytes(),
                extra_bundle: ids(stored.revoked_bundle_keys),
                extra_updater: ids(stored.revoked_updater_keys),
            });
        }
        let doc = std::fs::read(dir.join(STATEMENT_FILE)).ok()?;
        let sig = std::fs::read(dir.join(SIGNATURE_FILE)).ok()?;
        Some(StoredBytes {
            doc,
            sig,
            extra_bundle: vec![],
            extra_updater: vec![],
        })
    }

    /// Puts a verified statement in force, with every revocation seen so far.
    fn hold(&mut self, mut statement: Statement, doc: Vec<u8>, sig: Vec<u8>) {
        self.revoked_bundle
            .extend(statement.revoked_bundle_keys.iter().cloned());
        self.revoked_updater
            .extend(statement.revoked_updater_keys.iter().cloned());
        statement.revoked_bundle_keys = self.revoked_bundle.iter().cloned().collect();
        statement.revoked_updater_keys = self.revoked_updater.iter().cloned().collect();
        self.current = Some(Held {
            statement,
            doc,
            sig,
        });
    }

    pub fn statement(&self) -> Option<&Statement> {
        self.current.as_ref().map(|h| &h.statement)
    }

    /// A statement is in force and has not expired. An expired one still
    /// decides which keys count (its revocations stay), but nothing new is
    /// installed while it is not fresh.
    pub fn fresh(&self, now: u64) -> bool {
        self.statement().is_some_and(|s| !s.expired(now))
    }

    /// Bundle keys in force: the statement's, else the compiled-in ones.
    pub fn effective_keys(&self, pinned: Vec<TrustedKey>) -> Vec<TrustedKey> {
        match self.statement() {
            Some(s) => s.bundle_keys(),
            None => pinned,
        }
    }

    /// Writes the state in force in one atomic step.
    fn persist(&self) -> std::io::Result<()> {
        let (Some(dir), Some(held)) = (&self.dir, &self.current) else {
            return Ok(());
        };
        std::fs::create_dir_all(dir)?;
        let stored = Stored {
            statement: STANDARD.encode(&held.doc),
            signature: String::from_utf8_lossy(&held.sig).into_owned(),
            revoked_bundle_keys: self.revoked_bundle.iter().cloned().collect(),
            revoked_updater_keys: self.revoked_updater.iter().cloned().collect(),
        };
        write_atomic(&dir.join(STATE_FILE), &serde_json::to_vec(&stored)?)?;
        // The pair an older client wrote must not outlive the state: a stale
        // one would be read back should the state file ever be lost.
        for name in [STATEMENT_FILE, SIGNATURE_FILE] {
            let _ = std::fs::remove_file(dir.join(name));
        }
        Ok(())
    }

    fn store(&mut self) {
        self.stored = self.persist().is_ok();
        if !self.stored {
            eprintln!("Mnema Desktop: cannot store the trust statement; it applies until restart");
        }
    }

    /// Takes a fetched statement. An expired one is never accepted, lower
    /// serials are refused, and the same serial only with identical bytes. A
    /// key an earlier statement revoked cannot be allowed again. The statement
    /// applies as soon as it verified; failing to store it is logged and
    /// retried with the next identical fetch.
    pub fn accept(&mut self, doc: &[u8], sig: &[u8], now: u64) -> Result<Accepted, String> {
        let statement = verify_statement(doc, sig, &self.root)?;
        if statement.expired(now) {
            return Err("trust statement has expired".into());
        }
        if statement.serial < self.floor.min_serial {
            return Err(format!(
                "trust statement {} is older than this release accepts",
                statement.serial
            ));
        }
        if let Some(held) = &self.current {
            if statement.serial < held.statement.serial {
                return Err(format!(
                    "trust statement {} is older than the {} already accepted",
                    statement.serial, held.statement.serial
                ));
            }
            if statement.serial == held.statement.serial {
                return if held.doc == doc {
                    if !self.stored {
                        self.store();
                    }
                    Ok(Accepted::Unchanged)
                } else {
                    Err("a different trust statement with the same serial".into())
                };
            }
        }
        if statement
            .bundle_keys
            .iter()
            .filter_map(|raw| parse_key(raw))
            .any(|k| self.revoked_bundle.contains(&k.id))
        {
            return Err("a revoked bundle key is allowed again".into());
        }
        if statement
            .updater_keys
            .iter()
            .filter_map(|k| minisign_key_id(k))
            .any(|id| self.revoked_updater.contains(&id))
        {
            return Err("a revoked updater key is allowed again".into());
        }
        self.hold(statement, doc.to_vec(), sig.to_vec());
        self.store();
        Ok(Accepted::Updated)
    }
}

#[cfg(test)]
mod tests;

/// The vectors in `test-vectors/` are shared with the Linux client, which
/// ports this module to TypeScript (`linux/test/vectors.test.ts`).
#[cfg(test)]
mod shared_vectors;
