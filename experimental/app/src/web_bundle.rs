//! Signed web app bundles (mnema-talk `doc/desktop-trusted-client.md`).
//!
//! A bundle is one zip: `manifest.json`, `manifest.sig` (base64 Ed25519 over
//! the exact manifest bytes) and `app/<path>` for every file of the web app.
//! The host runs only bundles whose signature matches a key pinned in this
//! build. Nothing here trusts where bytes came from: the installer, the cache
//! and a download all go through [`verify`], and a cached bundle is verified
//! again on every start.
mod serve;
mod store;
mod verify;

pub use serve::{Held, Refusal, ServerInfo, Source};
pub use store::{CHECK_INTERVAL_SECS, Store, install_current};
pub use verify::Bundle;
#[cfg(test)]
pub use verify::{valid_path, verify};

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    path::{Path, PathBuf},
};

/// A public key the client trusts. `id` is the first 8 bytes of SHA-256 over
/// the raw key, hex; it only selects the key, the signature decides.
#[derive(Clone)]
pub struct TrustedKey {
    pub id: String,
    pub key: VerifyingKey,
}

/// Keys pinned in this build. To rotate, ship a release that pins both the old
/// and the new key, then drop the old one in a later release.
const PINNED: &[&str] = &["7815c02fddb14038cf3641036ba2e4e20ffdd0e42d00be3477671ca4628de06d"];

pub fn trusted_keys() -> Vec<TrustedKey> {
    PINNED
        .iter()
        .filter_map(|raw| parse_key(raw))
        .collect::<Vec<_>>()
}

pub fn parse_key(raw_hex: &str) -> Option<TrustedKey> {
    let raw: [u8; 32] = hex::decode(raw_hex).ok()?.try_into().ok()?;
    Some(TrustedKey {
        id: key_id(&raw),
        key: VerifyingKey::from_bytes(&raw).ok()?,
    })
}

pub fn key_id(raw: &[u8; 32]) -> String {
    hex::encode(&Sha256::digest(raw)[..8])
}

/// Whether `id` has the shape of a key or instance id: the first 8 bytes of a
/// SHA-256 as 16 lowercase hex characters (see [`key_id`], `instances::id_for`).
pub(crate) fn is_hex_id16(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `MAJOR.MINOR.PATCH`, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = text.split('.');
        let mut next = || {
            let part = parts.next()?;
            if part.is_empty()
                || part.len() > 9
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            part.parse::<u32>().ok()
        };
        let version = Version(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// Writes through a synced `.partial` file and a rename, so a crash leaves
/// either the old or the new content and never a torn file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let mut file = std::fs::File::create(&partial)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&partial, path)
}

#[cfg(test)]
pub(crate) mod fixture;

#[cfg(test)]
mod tests;

/// The vectors in `test-vectors/` are shared with the Linux client, which
/// ports this module to TypeScript; both read the same files and must give the
/// same answers (`linux/test/vectors.test.ts`).
#[cfg(test)]
mod shared_vectors;
