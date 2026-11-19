//! How the servers sit on the rail: in order, in folders, under a local name.
//!
//! Pure bookkeeping, free of windows and of the registry, so that every rule
//! is tested against the vectors in `test-vectors/rail-layout.json`, which the
//! Linux client reads too. The servers themselves live in the registry; this
//! only says where each one is shown and what the person calls it. A folder
//! never holds a folder, and a folder without a server does not exist.
mod edit;
mod request;

pub use request::{Edit, apply, new_folder_id};

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Longest name of a folder or of a server, in characters.
pub const MAX_NAME: usize = 40;
const MAX_FOLDER_ID: usize = 24;

/// One thing on the rail: a server, or a folder with servers in it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Item {
    Server {
        id: String,
    },
    Folder {
        id: String,
        name: String,
        color: Option<String>,
        collapsed: bool,
        servers: Vec<String>,
    },
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub items: Vec<Item>,
    /// Local names, by server id.
    pub aliases: BTreeMap<String, String>,
}

enum Spot {
    Top(usize),
    In(usize, usize),
}

/// A name as it is kept: no control characters, trimmed, at most 40 characters.
pub fn clean_name(raw: &str) -> String {
    let kept: String = raw.chars().filter(|c| !c.is_control()).collect();
    kept.trim().chars().take(MAX_NAME).collect()
}

/// `#rrggbb`, in lower case.
pub fn clean_color(raw: &str) -> Option<String> {
    let digits = raw.strip_prefix('#')?;
    (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| raw.to_ascii_lowercase())
}

/// What the host calls a folder it makes: `f-` and eight hex digits.
pub fn is_folder_id(id: &str) -> bool {
    id.strip_prefix("f-").is_some_and(|rest| {
        rest.len() == 8 && rest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn loadable_folder_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_FOLDER_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Layout {
    /// The layout as it was stored (`items` is none for a list written before
    /// folders existed), reconciled with the servers there are: whatever does
    /// not hold up is dropped, and servers it does not place follow in `order`.
    pub fn load(
        items: Option<Vec<Item>>,
        aliases: BTreeMap<String, String>,
        known: &[String],
        order: &[String],
    ) -> Layout {
        // Sets: a list may hold any number of servers, a hostile one thousands of folders.
        let known: std::collections::HashSet<&String> = known.iter().collect();
        let mut placed: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut folders: BTreeSet<String> = BTreeSet::new();
        let mut kept: Vec<Item> = Vec::new();
        let take = |id: &String, placed: &mut std::collections::HashSet<String>| {
            known.contains(id) && placed.insert(id.clone())
        };
        for item in items.unwrap_or_default() {
            match item {
                Item::Server { id } => {
                    if take(&id, &mut placed) {
                        kept.push(Item::Server { id });
                    }
                }
                Item::Folder {
                    id,
                    name,
                    color,
                    collapsed,
                    servers,
                } => {
                    if !loadable_folder_id(&id) || !folders.insert(id.clone()) {
                        continue;
                    }
                    let servers: Vec<String> = servers
                        .into_iter()
                        .filter(|s| take(s, &mut placed))
                        .collect();
                    if !servers.is_empty() {
                        kept.push(Item::Folder {
                            id,
                            name: clean_name(&name),
                            color: color.as_deref().and_then(clean_color),
                            collapsed,
                            servers,
                        });
                    }
                }
            }
        }
        for id in order {
            if take(id, &mut placed) {
                kept.push(Item::Server { id: id.clone() });
            }
        }
        let aliases = aliases
            .into_iter()
            .filter(|(id, _)| known.contains(id))
            .map(|(id, name)| (id, clean_name(&name)))
            .filter(|(_, name)| !name.is_empty())
            .collect();
        Layout {
            items: kept,
            aliases,
        }
    }

    /// The servers in the order they are shown, folders opened up.
    pub fn flat(&self) -> Vec<String> {
        let mut out = Vec::new();
        for item in &self.items {
            match item {
                Item::Server { id } => out.push(id.clone()),
                Item::Folder { servers, .. } => out.extend(servers.iter().cloned()),
            }
        }
        out
    }

    fn spot(&self, id: &str) -> Option<Spot> {
        for (top, item) in self.items.iter().enumerate() {
            match item {
                Item::Server { id: s } if s == id => return Some(Spot::Top(top)),
                Item::Folder { servers, .. } => {
                    if let Some(at) = servers.iter().position(|s| s == id) {
                        return Some(Spot::In(top, at));
                    }
                }
                Item::Server { .. } => {}
            }
        }
        None
    }

    fn folder_at(&self, id: &str) -> Option<usize> {
        self.items
            .iter()
            .position(|i| matches!(i, Item::Folder { id: f, .. } if f == id))
    }

    fn has_server(&self, id: &str) -> bool {
        self.spot(id).is_some()
    }

    fn id_free(&self, folder: &str) -> bool {
        loadable_folder_id(folder) && self.folder_at(folder).is_none() && !self.has_server(folder)
    }

    /// Takes a server out of where it is, leaving an emptied folder for the
    /// caller to deal with (`prune`).
    fn take_out(&mut self, id: &str) -> Option<Spot> {
        let spot = self.spot(id)?;
        match spot {
            Spot::Top(top) => {
                self.items.remove(top);
            }
            Spot::In(top, at) => {
                if let Item::Folder { servers, .. } = &mut self.items[top] {
                    servers.remove(at);
                }
            }
        }
        Some(spot)
    }

    fn prune(&mut self) {
        self.items
            .retain(|i| !matches!(i, Item::Folder { servers, .. } if servers.is_empty()));
    }

    pub fn add(&mut self, id: &str) -> bool {
        if self.has_server(id) {
            return false;
        }
        self.items.push(Item::Server { id: id.to_owned() });
        true
    }

    pub fn remove(&mut self, id: &str) -> bool {
        if self.take_out(id).is_none() {
            return false;
        }
        self.prune();
        self.aliases.remove(id);
        true
    }
}

#[cfg(test)]
mod tests;
