//! The shared state of gaming: which server owns the Talk, what the person's
//! settings are, and the state machine that turns key edges into commands.
use super::command::Command;
#[cfg(windows)]
use super::native::GameLease;
use super::settings::Settings;
use super::surface::close_surfaces;
use super::voice::Voice;
use super::{foreground, fullscreen, game_library_scan, input};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::Manager;

pub struct Gaming {
    /// The app origin the sessions are bound to (`local_origin` of their pages).
    pub(super) origin: Option<String>,
    /// The bridge token of each server's page, by server id.
    pub(super) tokens: HashMap<String, String>,
    /// The server that owns the Talk gaming is attached to.
    pub(super) owner: Option<String>,
    pub(super) voice: Voice,
    pub(super) updated: Instant,
    pub(super) settings: Settings,
    pub(super) overlay: bool,
    pub(super) active: bool,
    pub(super) previous: [bool; 4],
    pub(super) ptt_down: bool,
    pub(super) ptt_armed: bool,
    pub(super) last_tick: Instant,
    /// The application in front, as the operating system last announced it.
    /// Updated by the foreground source's events, never by polling.
    pub(super) foreground: Option<foreground::Foreground>,
    /// File name of the last other application that was in front, offered only
    /// when the person asks to add it as a game. Never a path.
    pub(super) candidate: Option<String>,
    /// Why game detection is off on this system, when it is.
    pub(super) detection_note: Option<&'static str>,
    /// Keys that are down, matched against the chords; fed by the input source.
    pub(super) keys: std::sync::Arc<Mutex<input::ChordTracker>>,
    /// Why shortcuts and push-to-talk do not work outside Mnema, when they do not.
    pub(super) input_note: Option<&'static str>,
    /// When to tell the person, once, that a game in exclusive fullscreen hides our windows.
    pub(super) hint: fullscreen::HintGate,
    /// The hint was given during this run.
    pub(super) exclusive_hint: bool,
    /// Install folders of the launcher libraries, scanned off the hot path.
    pub(super) library: std::sync::Arc<game_library_scan::Library>,
    #[cfg(windows)]
    pub(super) game: Option<GameLease>,
}
impl Default for Gaming {
    fn default() -> Self {
        Self {
            origin: None,
            tokens: HashMap::new(),
            owner: None,
            voice: Voice::default(),
            updated: Instant::now(),
            settings: Settings::default(),
            overlay: false,
            active: false,
            previous: [true; 4],
            ptt_down: false,
            ptt_armed: false,
            last_tick: Instant::now(),
            foreground: None,
            candidate: None,
            detection_note: None,
            keys: Default::default(),
            input_note: None,
            hint: Default::default(),
            exclusive_hint: false,
            library: Default::default(),
            #[cfg(windows)]
            game: None,
        }
    }
}

impl Gaming {
    /// A server's page reported a live Talk within `fresh`.
    pub fn talking(&self, fresh: Duration) -> bool {
        self.owner.is_some() && self.voice.connected && self.updated.elapsed() < fresh
    }

    /// Tests elsewhere: a page has just reported a Talk (or its end).
    #[cfg(test)]
    pub fn test_talk(&mut self, connected: bool) {
        self.owner = connected.then(|| "test".to_owned());
        self.voice.connected = connected;
        self.updated = Instant::now();
    }

    /// A page reports its Talk. The page must hold the token of its own
    /// server, and while another server's Talk is live that one keeps gaming:
    /// this page's state is ignored, not mixed in.
    pub(super) fn accept_sync(
        &mut self,
        server: &str,
        token: &str,
        origin: Option<&str>,
        voice: Voice,
    ) -> Result<(), String> {
        let token_ok = !token.is_empty() && self.tokens.get(server).is_some_and(|t| t == token);
        if !token_ok || origin.is_none() || origin != self.origin.as_deref() {
            return Err("Wrong gaming session.".into());
        }
        voice.validate()?;
        let held_elsewhere = self.owner.as_deref().is_some_and(|o| o != server)
            && self.voice.connected
            && self.updated.elapsed() < Duration::from_secs(2);
        if held_elsewhere || (!voice.connected && self.owner.as_deref() != Some(server)) {
            return Ok(());
        }
        if !voice.connected
            || voice.account != self.voice.account
            || voice.channel != self.voice.channel
            || voice.scope != self.voice.scope
        {
            self.overlay = false;
            self.active = false;
            self.previous = [true; 4];
            self.ptt_down = false;
            self.ptt_armed = false;
        }
        self.owner = voice.connected.then(|| server.to_owned());
        self.voice = voice;
        self.updated = Instant::now();
        Ok(())
    }
}
/// Hands a server's page the token that lets it feed gaming.
pub fn bind(app: &tauri::AppHandle, server: &str) -> String {
    let token = uuid::Uuid::new_v4().to_string();
    if let Ok(mut g) = app.state::<Mutex<Gaming>>().lock() {
        g.origin = crate::web_client::local_origin(&crate::web_client::app_url());
        g.tokens.insert(server.to_owned(), token.clone());
        if g.owner.as_deref() == Some(server) {
            g.release();
        }
    }
    token
}
impl Gaming {
    /// Tells the key tracker the chords the settings now hold.
    pub(super) fn sync_chords(&self) {
        if let Ok(mut keys) = self.keys.lock() {
            keys.set_chords(self.settings.chords());
        }
    }
    /// Nobody owns the Talk: no overlay, nothing held.
    pub(super) fn release(&mut self) {
        let keep = (self.origin.clone(), std::mem::take(&mut self.tokens));
        *self = Gaming {
            settings: self.settings.clone(),
            origin: keep.0,
            tokens: keep.1,
            foreground: self.foreground.take(),
            candidate: self.candidate.take(),
            detection_note: self.detection_note,
            library: self.library.clone(),
            keys: self.keys.clone(),
            hint: std::mem::take(&mut self.hint),
            ..Gaming::default()
        };
    }
}
/// A server's page is gone (its server left the list): it no longer has a
/// token, and gaming lets go of it if it owned the Talk.
pub fn forget(app: &tauri::AppHandle, server: &str) {
    let owned = app.state::<Mutex<Gaming>>().lock().is_ok_and(|mut g| {
        g.tokens.remove(server);
        let owned = g.owner.as_deref() == Some(server);
        if owned {
            g.release();
        }
        owned
    });
    if owned {
        close_surfaces(app);
    }
}
/// The window is closing: everything lets go.
pub fn clear(app: &tauri::AppHandle) {
    if let Ok(mut g) = app.state::<Mutex<Gaming>>().lock() {
        g.tokens.clear();
        g.release();
    }
    close_surfaces(app);
}
/// How long without an evaluation counts as a stall. The heartbeat runs every
/// half second while a Talk is live, so this is only ever exceeded after sleep,
/// a frozen host or an idle period.
const STALL: Duration = Duration::from_secs(5);
impl Gaming {
    // Pure state transition: native UI effects and command dispatch happen only
    // after the caller releases the shared state lock.
    pub(super) fn advance(&mut self, now: Instant, game: bool, keys: [bool; 4]) -> Vec<Command> {
        let mut commands = Vec::new();
        // Nothing looked at the keys for a long while (sleep, a stalled window
        // thread, an idle period): a held key says nothing about a new press.
        if now.duration_since(self.last_tick) > STALL {
            self.previous = keys;
            self.ptt_armed = false;
        }
        self.last_tick = now;
        let active = self.settings.enabled
            && game
            && self.origin.is_some()
            && self.voice.connected
            && self.updated.elapsed() < Duration::from_secs(2);
        if !active {
            self.overlay = false;
            self.previous = keys;
            if self.ptt_down {
                commands.push(Command::new(self, "ptt-release"));
            }
            self.ptt_down = false;
            self.ptt_armed = false;
        } else {
            if keys[0] && !self.previous[0] {
                self.overlay = !self.overlay;
            }
            for (i, action) in [(1, "mute"), (2, "deafen")] {
                if keys[i] && !self.previous[i] {
                    commands.push(Command::new(self, action));
                }
            }
            if !keys[3] {
                self.ptt_armed = true;
            }
            if self.voice.ptt_mode && !self.overlay && self.ptt_armed && keys[3] {
                commands.push(Command::new(self, "ptt-press"));
                self.ptt_down = true;
            } else if self.ptt_down {
                commands.push(Command::new(self, "ptt-release"));
                self.ptt_down = false;
            }
            self.previous = keys;
        }
        self.active = active;
        commands
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
