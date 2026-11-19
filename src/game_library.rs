//! Installed-game folders, learned from the launchers' own read-only manifests.
//!
//! A foreground program that lives under the install folder of a library game
//! is that game, named as the launcher names it. This module is the pure part:
//! parsers for the manifest formats and the index that matches an image path.
//! Reading the files and the registry is `game_library_scan.rs`.
//!
//! Only data the launchers keep on disk for themselves is read. No launcher
//! API, no account data, no network, no process list.

use serde::Deserialize;

#[path = "game_library/steam.rs"]
mod steam;

pub use steam::{steam_app, steam_game, steam_libraries};

/// One installed game: the name its launcher shows and the folder it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledGame {
    pub name: String,
    /// Normalized with [`normalize_dir`]; compared case-insensitively.
    dir: String,
}

impl InstalledGame {
    /// `None` when the folder is unusable as a game folder: relative, with a
    /// `..`, or so broad that every program on the machine would be "in" it.
    pub fn new(name: &str, dir: &str) -> Option<Self> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
            return None;
        }
        Some(Self {
            name: name.to_owned(),
            dir: normalize_dir(dir)?,
        })
    }
}

/// Folders that hold many unrelated programs. A launcher manifest that points
/// at one of these (or above) is ignored instead of marking everything a game.
const BROAD: &[&str] = &[
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "users",
    "applications",
    "library",
    "system",
    "usr",
    "opt",
    "home",
    "bin",
    "etc",
    "var",
    "tmp",
    "temp",
];

/// How an absolute path starts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Root {
    /// `X:/`, a drive letter followed by a separator.
    Drive,
    /// A leading `/`.
    Slash,
}

/// Splits a `/`-separated path into its root and the part after it. The one
/// place that decides what "absolute and local" means: the root is `X:/` or a
/// leading `/` (not `//`, which is UNC or a device path, and not the
/// drive-relative `X:name`), and nothing after it may contain `:` (an
/// alternate data stream, a second drive or a `\??\c:` device prefix).
fn split_root(flat: &str) -> Option<(Root, &str)> {
    if flat.starts_with("//") {
        return None;
    }
    let bytes = flat.as_bytes();
    let (root, rest) = if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2] == b'/'
    {
        (Root::Drive, &flat[2..])
    } else if flat.starts_with('/') {
        (Root::Slash, flat)
    } else {
        return None;
    };
    (!rest.contains(':')).then_some((root, rest))
}

/// Lower-case root prefix and path components of an absolute path: no empty or
/// `.` parts, and `None` for control characters, `..` or anything
/// [`split_root`] refuses.
fn rooted_parts(path: &str) -> Option<(String, Vec<String>)> {
    let flat = path.trim().replace('\\', "/").to_lowercase();
    if flat.is_empty() || flat.chars().any(char::is_control) {
        return None;
    }
    let (root, rest) = split_root(&flat)?;
    let parts: Vec<String> = rest
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .map(str::to_owned)
        .collect();
    if parts.iter().any(|p| p == "..") {
        return None;
    }
    let prefix = match root {
        Root::Drive => flat[..2].to_owned(),
        Root::Slash => String::new(),
    };
    Some((prefix, parts))
}

/// Lower-case, `/`-separated, without trailing slash. `None` for anything that
/// is not an absolute path deeper than a drive root or a known broad folder.
pub fn normalize_dir(dir: &str) -> Option<String> {
    let (prefix, parts) = rooted_parts(dir)?;
    // A game folder is at least one level below a broad folder.
    let first = parts.first()?;
    if parts.len() < 2
        || (parts.len() == 2 && BROAD.contains(&first.as_str()))
        || BROAD.contains(&parts[parts.len() - 1].as_str())
    {
        return None;
    }
    Some(format!("{prefix}/{}", parts.join("/")))
}

/// Normalizes an image path like [`normalize_dir`] but without the depth rules.
fn normalize_image(path: &str) -> Option<String> {
    let (prefix, parts) = rooted_parts(path)?;
    Some(format!("{prefix}/{}", parts.join("/")))
}

/// A path read from a launcher manifest or the registry that is safe to open:
/// absolute and local, with no `..`, no control characters and no alternate
/// data stream. UNC (`\\host\share`) and device (`\\?\`, `\\.\`, `\??\`) paths
/// are refused, because opening one makes Windows contact another machine and
/// can leak the user's credentials. `windows` selects the Windows rule (a drive
/// letter is required); otherwise a rooted `/` path is accepted.
pub fn safe_disk_path_for(path: &str, windows: bool) -> bool {
    let flat = path.trim().replace('\\', "/");
    if flat.is_empty()
        || flat.chars().any(char::is_control)
        || flat.split('/').any(|part| part == "..")
    {
        return false;
    }
    match split_root(&flat) {
        Some((Root::Drive, _)) => true,
        Some((Root::Slash, _)) => !windows,
        None => false,
    }
}

/// [`safe_disk_path_for`] for the system this runs on.
pub fn safe_disk_path(path: &str) -> bool {
    safe_disk_path_for(path, cfg!(windows))
}

/// All installed games found by the launchers, searchable by image path.
#[derive(Debug, Default, Clone)]
pub struct InstallIndex {
    games: Vec<InstalledGame>,
}

impl InstallIndex {
    pub fn new(mut games: Vec<InstalledGame>) -> Self {
        // Longest folder first, so a game nested in another folder wins.
        games.sort_by(|a, b| {
            b.dir
                .len()
                .cmp(&a.dir.len())
                .then_with(|| a.name.cmp(&b.name))
        });
        games.dedup();
        Self { games }
    }

    pub fn len(&self) -> usize {
        self.games.len()
    }

    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    /// The game whose folder contains `image`, if any. The match is on whole
    /// path components: `/games/foo` does not contain `/games/foobar/x.exe`.
    pub fn find(&self, image: &str) -> Option<&str> {
        let image = normalize_image(image)?;
        self.games
            .iter()
            .find(|game| {
                image.len() > game.dir.len()
                    && image.starts_with(&game.dir)
                    && image.as_bytes()[game.dir.len()] == b'/'
            })
            .map(|game| game.name.as_str())
    }
}

// ---------------------------------------------------------------------------
// Epic Games Store (JSON `.item` manifests)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EpicItem {
    display_name: Option<String>,
    install_location: Option<String>,
    app_name: Option<String>,
    main_game_app_name: Option<String>,
    #[serde(default, rename = "bIsIncompleteInstall")]
    incomplete: bool,
}

/// A game from an Epic `.item` manifest. Add-ons (their main game is another
/// app) and unfinished installs are skipped.
pub fn epic_game(json: &str) -> Option<InstalledGame> {
    let item: EpicItem = serde_json::from_str(json).ok()?;
    if item.incomplete
        || item
            .main_game_app_name
            .as_deref()
            .is_some_and(|main| !main.is_empty() && Some(main) != item.app_name.as_deref())
    {
        return None;
    }
    InstalledGame::new(&item.display_name?, &item.install_location?)
}

#[cfg(test)]
#[path = "game_library/tests.rs"]
mod tests;
