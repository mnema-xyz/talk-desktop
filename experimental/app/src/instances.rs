//! The servers this client is signed in to, side by side.
//!
//! Every server is one slot with its own session (cookie jar, upload limits,
//! socket budget), its own browser storage and its own counters; nothing is
//! shared between slots. A window is tied to its slot by its label alone
//! (`i-<id>`), so a page can only ever reach the session of the server it was
//! opened for. This module is the bookkeeping, free of any window handling so
//! that the isolation rules can be tested: the shell (`web_shell`) turns it
//! into webviews.
use crate::{
    web_bundle::{Held, is_hex_id16},
    web_proxy::Instance,
    web_ws::Sockets,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use url::Url;

mod mutes;
mod rail;
mod registry;
mod stored;
#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
pub use rail::RailInstance;
pub use rail::RailState;
pub use registry::Registry;
pub use stored::Stored;

/// Where Ctrl+`number` goes among `count` servers: 1 to 8 the server at that
/// place, 9 the last one (as browsers do with tabs).
pub fn shortcut_place(number: usize, count: usize) -> Option<usize> {
    match number {
        1..=8 if number <= count => Some(number - 1),
        9 => count.checked_sub(1),
        _ => None,
    }
}

/// What the badge and the rail show at most.
pub const MAX_COUNT: u32 = 999;
const INSTANCE_PREFIX: &str = "i-";
const ATTACHMENT_PREFIX: &str = "a-";
const STORE_VERSION: u32 = 1;
/// The most an imported list may weigh. There is no limit on the number of
/// servers: this only keeps a damaged or hostile file from being read whole
/// into memory. At the longest names, folders and mutes a server takes well
/// under 100 bytes more than its address, so this is room for 100 000 and
/// more (the rail's import dialog asks for the same bound).
pub const MAX_IMPORT_BYTES: usize = 8 << 20;
/// A mute with no end.
pub const FOREVER: i64 = -1;
/// The longest a mute may last: a year and a day.
pub const MAX_MUTE_SECS: i64 = 366 * 24 * 3600;
/// The same for the list file of this client.
const MAX_STORE_BYTES: u64 = 8 << 20;

/// A server the client talks to and the verified bundle its pages run.
pub struct Session {
    pub id: String,
    pub instance: Arc<Instance>,
    pub held: Held,
    /// What the server calls itself (`/api/health`), when it says.
    pub community: Option<String>,
    /// Sockets this server's pages hold open; the budget is per server.
    pub sockets: Arc<Sockets>,
}

/// Names the slot of a server. Derived from its origin, so the same server is
/// always the same slot, and the key of its stored session (the account in the
/// keychain, the fallback file) follows from it.
pub fn id_for(origin: &Url) -> String {
    hex::encode(&Sha256::digest(origin.as_str().as_bytes())[..8])
}

/// The label of the page of one server.
pub fn instance_label(id: &str) -> String {
    format!("{INSTANCE_PREFIX}{id}")
}

/// The label of the attachment window a server's page opened.
pub fn attachment_label(id: &str) -> String {
    format!("{ATTACHMENT_PREFIX}{id}")
}

/// The slot a page label belongs to. Anything that is not exactly a label
/// made by `instance_label` belongs to no server.
pub fn id_of_instance_label(label: &str) -> Option<&str> {
    label
        .strip_prefix(INSTANCE_PREFIX)
        .filter(|id| is_hex_id16(id))
}

pub fn id_of_attachment_label(label: &str) -> Option<&str> {
    label
        .strip_prefix(ATTACHMENT_PREFIX)
        .filter(|id| is_hex_id16(id))
}

/// What the server's page is doing: still being checked, running, or refused
/// (too old or too new for any verified bundle, unreachable). A refusal is
/// shown in the server's own tab only.
pub enum Phase {
    Starting,
    Ready(Arc<Session>),
    Refused(String),
}

pub struct Slot {
    pub id: String,
    pub origin: Url,
    pub phase: Phase,
    /// A start (discover, choose, build the session) is under way for this
    /// slot; a second one must not begin.
    pub connecting: bool,
    /// Channels with unread messages that may notify, and unread mentions,
    /// as the page last reported them.
    pub unread: u32,
    pub mentions: u32,
    /// The page reports both counts (`desktop_report_unread`); until it does,
    /// the plain badge of an older bundle stands in for the mentions.
    pub reported: bool,
    /// Names the run of this server's page. A toast keeps the generation it
    /// was shown for, so one from an earlier run (or an earlier server in
    /// this slot) can never deliver its ids to the page that replaced it.
    pub generation: String,
    /// The page of this slot asked for notifications (a channel is open).
    pub clicks: Option<tauri::ipc::Channel<crate::web_notify::Click>>,
    /// Names the community icon the server set, as cached on this device.
    pub icon: Option<String>,
    /// What the page last said about itself: busy (an unsent draft, an
    /// upload, a Talk) or idle; `None` until it says (an older bundle never
    /// does, and its page is never closed for being hidden).
    pub busy: Option<bool>,
}

impl Slot {
    fn new(origin: Url) -> Slot {
        Slot {
            id: id_for(&origin),
            origin,
            phase: Phase::Starting,
            connecting: false,
            unread: 0,
            mentions: 0,
            reported: false,
            generation: uuid::Uuid::new_v4().simple().to_string(),
            clicks: None,
            icon: None,
            busy: None,
        }
    }

    /// The name on the rail: what the server calls itself once it answered
    /// (the Owner's community name), else the host of the address. A server
    /// that only repeats its host keeps the address with its port.
    pub fn name(&self) -> String {
        let host = self.origin.host_str().unwrap_or_default();
        if let Phase::Ready(session) = &self.phase
            && let Some(name) = session.community.as_deref()
            && !name.eq_ignore_ascii_case(host)
        {
            return name.to_owned();
        }
        match self.origin.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        }
    }

    /// A new run of the page: counters and notification state start over.
    pub fn restart(&mut self) {
        self.unread = 0;
        self.mentions = 0;
        self.reported = false;
        self.clicks = None;
        self.busy = None;
        self.generation = uuid::Uuid::new_v4().simple().to_string();
    }

    /// Puts a new phase in place. A session it replaces is closed first, so
    /// its sockets cannot outlive the slot's hold on it.
    pub fn settle(&mut self, phase: Phase) {
        if let Phase::Ready(old) = &self.phase
            && !matches!(&phase, Phase::Ready(new) if Arc::ptr_eq(old, new))
        {
            old.sockets.close_all();
        }
        self.connecting = false;
        self.phase = phase;
    }

    fn session(&self) -> Option<Arc<Session>> {
        match &self.phase {
            Phase::Ready(session) => Some(Arc::clone(session)),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AddError {
    /// Already on the list, under this id.
    Known(String),
}

impl std::fmt::Display for AddError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AddError::Known(_) => f.write_str("This server is already in the list."),
        }
    }
}

#[cfg(test)]
mod tests;
