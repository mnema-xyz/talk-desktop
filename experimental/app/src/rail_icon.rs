//! The icon a server's owner sets for the community, shown on the rail.
//!
//! It comes from the server itself and from nowhere else: `GET /api/community/icon`
//! on the address the person added, over the host's own HTTPS stack, without
//! following a redirect. What arrives is only used when it is a well-formed
//! PNG or WebP of at most 1 MiB and 2048 pixels each way (never SVG), and is
//! kept in `icons/` next to the other per-server data, so the portable build
//! keeps it next to the executable. The rail gets it as a data URL from the
//! cache, never from the network.
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use url::Url;

/// Largest icon taken over, in bytes.
pub const LIMIT: usize = 1 << 20;
/// Largest width and height, in pixels.
pub const MAX_SIDE: u32 = 2048;
pub const PATH: &str = "api/community/icon";
const MAX_ETAG: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    Webp,
}

impl Kind {
    fn extension(self) -> &'static str {
        match self {
            Kind::Png => "png",
            Kind::Webp => "webp",
        }
    }

    fn media_type(self) -> &'static str {
        match self {
            Kind::Png => "image/png",
            Kind::Webp => "image/webp",
        }
    }
}

// ---------------------------------------------------------------- validation

fn crc32(parts: &[&[u8]]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut n = 0;
        while n < 256 {
            let mut c = n as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 == 1 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[n] = c;
            n += 1;
        }
        table
    };
    let mut crc = u32::MAX;
    for byte in parts.iter().flat_map(|p| p.iter()) {
        crc = TABLE[((crc ^ u32::from(*byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn in_range(width: u32, height: u32) -> Option<(u32, u32)> {
    ((1..=MAX_SIDE).contains(&width) && (1..=MAX_SIDE).contains(&height)).then_some((width, height))
}

/// The size of a PNG that is well formed from its signature to its end: every
/// chunk checksummed, the header first, image data, the end chunk last.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(SIGNATURE) {
        return None;
    }
    let mut at = SIGNATURE.len();
    let mut size = None;
    let mut data = false;
    loop {
        let head = bytes.get(at..at + 8)?;
        let length = usize::try_from(be32(head)).ok()?;
        let end = (at + 8).checked_add(length)?.checked_add(4)?;
        let body = bytes.get(at + 8..end - 4)?;
        let checksum = bytes.get(end - 4..end)?;
        if be32(checksum) != crc32(&[&head[4..8], body]) {
            return None;
        }
        match &head[4..8] {
            b"IHDR" if size.is_none() && at == SIGNATURE.len() => {
                if body.len() != 13 || !matches!(body[9], 0 | 2 | 3 | 4 | 6) {
                    return None;
                }
                size = Some(in_range(be32(body), be32(&body[4..]))?);
            }
            _ if size.is_none() => return None,
            b"IDAT" => data = true,
            b"IEND" => return (body.is_empty() && data && end == bytes.len()).then_some(size?),
            _ => {}
        }
        at = end;
    }
}

/// The size of a WebP (lossy, lossless or the extended format) whose RIFF
/// frame and first chunk fit the bytes.
fn webp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 30 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    let riff = usize::try_from(u32::from_le_bytes(bytes[4..8].try_into().ok()?)).ok()?;
    let chunk = usize::try_from(u32::from_le_bytes(bytes[16..20].try_into().ok()?)).ok()?;
    if riff.checked_add(8)? != bytes.len() || 20usize.checked_add(chunk)? > bytes.len() {
        return None;
    }
    let le16 = |at: usize| u32::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]));
    let le24 = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], 0]);
    match &bytes[12..16] {
        b"VP8 " if chunk >= 10 && bytes[23..26] == [0x9d, 0x01, 0x2a] => {
            in_range(le16(26) & 0x3fff, le16(28) & 0x3fff)
        }
        b"VP8L" if chunk >= 5 && bytes[20] == 0x2f => {
            let bits = u32::from_le_bytes(bytes[21..25].try_into().ok()?);
            in_range((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
        }
        b"VP8X" if chunk >= 10 => in_range(le24(24) + 1, le24(27) + 1),
        _ => None,
    }
}

/// What an icon is when it is acceptable: the format its bytes are, and its
/// size. The content type and the bytes have to say the same, and the bytes
/// have to be well formed; anything else (SVG included) is refused.
pub fn validate(content_type: Option<&str>, bytes: &[u8]) -> Option<(Kind, u32, u32)> {
    if bytes.is_empty() || bytes.len() > LIMIT {
        return None;
    }
    let declared = content_type?.split(';').next()?.trim().to_ascii_lowercase();
    let (kind, size) = match declared.as_str() {
        "image/png" => (Kind::Png, png_size(bytes)?),
        "image/webp" => (Kind::Webp, webp_size(bytes)?),
        _ => return None,
    };
    Some((kind, size.0, size.1))
}

/// Whether the health answer wants the icon fetched. `community_icon` is a
/// flag or a version; off says there is none, and a server that does not say
/// (an older one) is asked anyway: a 404 is the cheap answer.
pub fn wanted(health: &[u8]) -> bool {
    let Some(value) = crate::web_json::parse::<serde_json::Value>(health) else {
        return true;
    };
    match value.get("community_icon") {
        None => true,
        Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(on)) => *on,
        Some(serde_json::Value::String(version)) => !version.is_empty(),
        Some(serde_json::Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0),
        Some(_) => true,
    }
}

// --------------------------------------------------------------------- fetch

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A new icon, and the validator the server gave it.
    Fresh(Kind, Vec<u8>, Option<String>),
    Unchanged,
    /// The server has none (any more).
    Gone,
    /// Nothing usable came; what is cached stays.
    Failed,
}

/// Asks the server itself for its icon. No redirect is followed, the body is
/// read up to the limit and not a byte further.
pub async fn fetch(origin: &Url, etag: Option<&str>) -> Outcome {
    let Ok(url) = origin.join(PATH) else {
        return Outcome::Failed;
    };
    fetch_url(url, etag).await
}

async fn fetch_url(url: Url, etag: Option<&str>) -> Outcome {
    let Ok(client) = crate::web_proxy::client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
    else {
        return Outcome::Failed;
    };
    let mut request = client.get(url).header("accept", "image/png, image/webp");
    if let Some(etag) = etag {
        request = request.header("if-none-match", etag);
    }
    let Ok(mut response) = request.send().await else {
        return Outcome::Failed;
    };
    match response.status().as_u16() {
        304 => return Outcome::Unchanged,
        404 | 410 => return Outcome::Gone,
        200 => {}
        _ => return Outcome::Failed,
    }
    if response.content_length().is_some_and(|n| n > LIMIT as u64) {
        return Outcome::Failed;
    }
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let content_type = header("content-type");
    let etag = header("etag")
        .filter(|e| !e.is_empty() && e.len() <= MAX_ETAG && !e.chars().any(char::is_control));
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if chunk.len() <= LIMIT.saturating_sub(body.len()) => {
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            _ => return Outcome::Failed,
        }
    }
    match validate(content_type.as_deref(), &body) {
        Some((kind, ..)) => Outcome::Fresh(kind, body, etag),
        None => Outcome::Failed,
    }
}

// --------------------------------------------------------------------- cache

fn file(dir: &Path, id: &str, kind: Kind) -> PathBuf {
    dir.join(format!("{id}.{}", kind.extension()))
}

fn etag_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.etag"))
}

/// The cached icon of a server, checked again as it is read.
pub fn read(dir: &Path, id: &str) -> Option<(Kind, Vec<u8>)> {
    if !crate::web_bundle::is_hex_id16(id) {
        return None;
    }
    [Kind::Png, Kind::Webp].into_iter().find_map(|kind| {
        let bytes = std::fs::read(file(dir, id, kind)).ok()?;
        let (found, ..) = validate(Some(kind.media_type()), &bytes)?;
        (found == kind).then_some((kind, bytes))
    })
}

/// The validator to send along: only when an icon is there to go with it.
pub fn cached_etag(dir: &Path, id: &str) -> Option<String> {
    read(dir, id)?;
    let etag = std::fs::read_to_string(etag_file(dir, id)).ok()?;
    (!etag.is_empty() && etag.len() <= MAX_ETAG).then_some(etag)
}

pub fn store(
    dir: &Path,
    id: &str,
    kind: Kind,
    bytes: &[u8],
    etag: Option<&str>,
) -> std::io::Result<()> {
    if !crate::web_bundle::is_hex_id16(id) {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let other = match kind {
        Kind::Png => Kind::Webp,
        Kind::Webp => Kind::Png,
    };
    let _ = std::fs::remove_file(file(dir, id, other));
    crate::web_bundle::write_atomic(&file(dir, id, kind), bytes)?;
    match etag {
        Some(etag) => crate::web_bundle::write_atomic(&etag_file(dir, id), etag.as_bytes()),
        None => {
            let _ = std::fs::remove_file(etag_file(dir, id));
            Ok(())
        }
    }
}

pub fn clear(dir: &Path, id: &str) {
    if !crate::web_bundle::is_hex_id16(id) {
        return;
    }
    for kind in [Kind::Png, Kind::Webp] {
        let _ = std::fs::remove_file(file(dir, id, kind));
    }
    let _ = std::fs::remove_file(etag_file(dir, id));
}

/// Removes the icons of servers that are not in the list (a removal cut short,
/// a server that was taken out while the list could not be written).
pub fn sweep(dir: &Path, keep: &[String]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if !keep.iter().any(|id| id == stem) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Names a cached icon for the rail: the first eight bytes of its hash, so a
/// new icon is a new name and the rail asks for it once.
pub fn token(bytes: &[u8]) -> String {
    hex::encode(&Sha256::digest(bytes)[..8])
}

pub fn cached_token(dir: &Path, id: &str) -> Option<String> {
    read(dir, id).map(|(_, bytes)| token(&bytes))
}

pub fn data_url(kind: Kind, bytes: &[u8]) -> String {
    use base64::Engine;
    format!(
        "data:{};base64,{}",
        kind.media_type(),
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Brings the cache of one server up to date with what the server says now.
/// Returns whether the icon the rail should show changed.
pub async fn refresh(dir: &Path, id: &str, origin: &Url, wanted: bool) -> bool {
    let before = cached_token(dir, id);
    if !wanted {
        clear(dir, id);
    } else {
        match fetch(origin, cached_etag(dir, id).as_deref()).await {
            Outcome::Fresh(kind, bytes, etag) => {
                if let Err(why) = store(dir, id, kind, &bytes, etag.as_deref()) {
                    eprintln!("Mnema Desktop: the icon of a server could not be saved: {why}");
                }
            }
            Outcome::Gone => clear(dir, id),
            Outcome::Unchanged | Outcome::Failed => {}
        }
    }
    cached_token(dir, id) != before
}

#[cfg(test)]
mod tests;
