//! Event-driven keyboard and mouse-button input for push-to-talk and the gaming
//! shortcuts. The operating system tells us when a key goes down or up; nothing
//! here polls key state, hooks another process or injects input.
//!
//! The platform source (raw input on Windows; the other systems are paused) sits behind [`InputSource`] and only ever report *which* key changed. They
//! never see text, and [`ChordTracker`] keeps no history: it holds the set of
//! keys currently down and compares it with the four configured chords, so a
//! session's keystrokes are neither stored nor logged.
//!
//! Key identity is the Windows virtual-key code on every system (the shortcut
//! settings already store VK codes).

/// Windows virtual-key codes used by chords and modifiers.
pub const VK_XBUTTON1: u16 = 0x05;
pub const VK_XBUTTON2: u16 = 0x06;
pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12;
pub const VK_LWIN: u16 = 0x5b;
pub const VK_RWIN: u16 = 0x5c;
pub const VK_LSHIFT: u16 = 0xa0;
pub const VK_RSHIFT: u16 = 0xa1;
pub const VK_LCONTROL: u16 = 0xa2;
pub const VK_RCONTROL: u16 = 0xa3;
pub const VK_LMENU: u16 = 0xa4;
pub const VK_RMENU: u16 = 0xa5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: u16,
    pub down: bool,
}

/// What a source reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    Key(KeyEvent),
    /// Key-up events may have been lost (screen lock, a disabled event tap, a
    /// device change): forget every key.
    Reset,
}

pub type Sink = Box<dyn Fn(InputEvent) + Send + Sync + 'static>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    /// This system has no way to see keys outside the app. Say why.
    Unsupported(&'static str),
    /// Another part of the app already owns the system registration.
    Conflict(&'static str),
    Failed(&'static str),
}

impl InputError {
    /// A short, plain reason the user can be shown.
    pub fn note(&self) -> &'static str {
        match self {
            Self::Unsupported(note) | Self::Conflict(note) | Self::Failed(note) => note,
        }
    }
}

/// A source of key events.
pub trait InputSource {
    fn start(&mut self, sink: Sink) -> Result<(), InputError>;
    fn stop(&mut self);
}

/// Where no system source exists. The Linux and macOS desktop clients are paused,
/// so only Windows has a source.
pub struct UnavailableSource;

impl InputSource for UnavailableSource {
    fn start(&mut self, _sink: Sink) -> Result<(), InputError> {
        Err(InputError::Unsupported(
            "Shortcuts and push-to-talk outside Mnema are available on Windows only.",
        ))
    }
    fn stop(&mut self) {}
}

/// The chords the gaming shortcuts and push-to-talk are configured with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Chord {
    pub key: u16,
    pub alt: bool,
    pub control: bool,
    pub shift: bool,
}

pub const CHORDS: usize = 4;

/// Which keys are down right now, matched against the configured chords.
///
/// A chord is "down" when its key is down, exactly its modifiers (alt, control,
/// shift) are down and no Windows/Command key is down. A press that begins and
/// ends between two looks at the tracker is latched once, so a short tap is
/// never lost.
#[derive(Debug, Clone)]
pub struct ChordTracker {
    down: [u64; 4],
    chords: [Chord; CHORDS],
    matched: [bool; CHORDS],
    latched: [bool; CHORDS],
}

impl Default for ChordTracker {
    fn default() -> Self {
        Self {
            down: [0; 4],
            chords: [Chord::default(); CHORDS],
            matched: [false; CHORDS],
            latched: [false; CHORDS],
        }
    }
}

impl ChordTracker {
    fn is_down(&self, key: u16) -> bool {
        key < 256 && self.down[usize::from(key) / 64] & (1 << (key % 64)) != 0
    }

    fn set(&mut self, key: u16, down: bool) {
        if key >= 256 {
            return;
        }
        let bit = 1u64 << (key % 64);
        let word = &mut self.down[usize::from(key) / 64];
        if down {
            *word |= bit;
        } else {
            *word &= !bit;
        }
    }

    fn any(&self, keys: &[u16]) -> bool {
        keys.iter().any(|k| self.is_down(*k))
    }

    fn evaluate(&self, chord: &Chord) -> bool {
        chord.key != 0
            && !self.any(&[VK_LWIN, VK_RWIN])
            && chord.alt == self.any(&[VK_MENU, VK_LMENU, VK_RMENU])
            && chord.control == self.any(&[VK_CONTROL, VK_LCONTROL, VK_RCONTROL])
            && chord.shift == self.any(&[VK_SHIFT, VK_LSHIFT, VK_RSHIFT])
            && self.is_down(chord.key)
    }

    fn refresh(&mut self) -> bool {
        let mut changed = false;
        for i in 0..CHORDS {
            let now = self.evaluate(&self.chords[i]);
            if now && !self.matched[i] {
                self.latched[i] = true;
            }
            changed |= now != self.matched[i];
            self.matched[i] = now;
        }
        changed
    }

    /// Replaces the chords (the settings changed). Held keys stay held.
    pub fn set_chords(&mut self, chords: [Chord; CHORDS]) {
        self.chords = chords;
        self.latched = [false; CHORDS];
        self.matched = [false; CHORDS];
        // A chord that is already held when it is configured is not a press.
        for i in 0..CHORDS {
            self.matched[i] = self.evaluate(&self.chords[i]);
        }
    }

    /// Forgets every key. True when a chord had been down.
    pub fn clear(&mut self) -> bool {
        self.down = [0; 4];
        self.latched = [false; CHORDS];
        self.refresh()
    }

    /// Applies one event. True when a chord went down or up, which is the only
    /// time a consumer needs to look again.
    pub fn apply(&mut self, event: InputEvent) -> bool {
        match event {
            InputEvent::Reset => self.clear(),
            InputEvent::Key(KeyEvent { key, down }) => {
                self.set(key, down);
                self.refresh()
            }
        }
    }

    /// The chords down now, plus any that went down and up since the last
    /// sample. The second value says a latched press still has to be followed by
    /// a sample that sees it released.
    pub fn sample(&mut self) -> ([bool; CHORDS], bool) {
        let mut keys = self.matched;
        let mut again = false;
        for ((key, latched), matched) in keys.iter_mut().zip(&mut self.latched).zip(self.matched) {
            if *latched {
                again |= !matched;
                *key = true;
                *latched = false;
            }
        }
        (keys, again)
    }
}

/// Windows raw input reports Shift, Control and Alt with generic codes; the
/// scan code and the extended-key flag say which side. Mapping to the
/// left/right codes keeps one released side from releasing the other.
pub fn side_specific(vkey: u16, make_code: u16, extended: bool) -> u16 {
    match vkey {
        VK_SHIFT => {
            if make_code == 0x36 {
                VK_RSHIFT
            } else {
                VK_LSHIFT
            }
        }
        VK_CONTROL => {
            if extended {
                VK_RCONTROL
            } else {
                VK_LCONTROL
            }
        }
        VK_MENU => {
            if extended {
                VK_RMENU
            } else {
                VK_LMENU
            }
        }
        other => other,
    }
}

// Raw Input constants that are macros in winuser.h / wtsapi32.h, not typed API
// metadata. Kept here, next to the pure decoding below, so both Windows sources
// (the shipped one and the fixture owner) read a report the same way.
pub const RI_KEY_BREAK: u16 = 0x1;
pub const RI_KEY_E0: u16 = 0x2;
pub const RI_KEY_E1: u16 = 0x4;
/// `usButtonFlags` bits of the two extra mouse buttons (4 and 5), down and up.
pub const RI_MOUSE_EXTRA_BUTTONS: u16 = 0x40 | 0x80 | 0x100 | 0x200;

/// What one Raw Input keyboard report means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyReport {
    Key(KeyEvent),
    /// Not a key change (the "fake" key Windows sends around some sequences).
    Ignore,
    /// A report that cannot be trusted (unknown flags, a keyboard overrun, a
    /// Shift of unknown side): forget every key rather than risk a stuck one.
    Reset,
}

/// Decodes the three fields of a `RAWKEYBOARD` report.
pub fn decode_keyboard(vkey: u16, make_code: u16, flags: u16) -> KeyReport {
    if vkey == 0 || vkey == 0xff {
        return KeyReport::Ignore;
    }
    if make_code == 0xff || flags & !(RI_KEY_BREAK | RI_KEY_E0 | RI_KEY_E1) != 0 {
        return KeyReport::Reset;
    }
    if vkey == VK_SHIFT && !matches!(make_code, 0x2a | 0x36) {
        return KeyReport::Reset;
    }
    KeyReport::Key(KeyEvent {
        key: side_specific(vkey, make_code, flags & RI_KEY_E0 != 0),
        down: flags & RI_KEY_BREAK == 0,
    })
}

/// The changes of buttons 4 and 5 in a mouse report's `usButtonFlags`. Movement,
/// position and wheel are never read.
pub fn decode_mouse_buttons(button_flags: u16) -> impl Iterator<Item = KeyEvent> {
    [
        (0x40, VK_XBUTTON1, true),
        (0x80, VK_XBUTTON1, false),
        (0x100, VK_XBUTTON2, true),
        (0x200, VK_XBUTTON2, false),
    ]
    .into_iter()
    .filter(move |(flag, _, _)| button_flags & flag != 0)
    .map(|(_, key, down)| KeyEvent { key, down })
}

/// `WM_WTSSESSION_CHANGE` codes after which key-up events may have gone to
/// another desktop or session: lock and unlock, console or remote disconnect
/// and logoff.
pub fn session_change_loses_keys(code: u32) -> bool {
    matches!(code, 0x2 | 0x4 | 0x6 | 0x7 | 0x8)
}

/// `WM_POWERBROADCAST` codes (`PBT_APMSUSPEND`, `PBT_APMRESUMESUSPEND`,
/// `PBT_APMRESUMEAUTOMATIC`): nothing is delivered while the machine sleeps.
pub fn power_change_loses_keys(code: u32) -> bool {
    matches!(code, 0x4 | 0x7 | 0x12)
}

/// Test double: pushes whatever the test emits.
#[cfg(test)]
#[derive(Default)]
pub struct FakeInput {
    sink: Option<Sink>,
    pub running: bool,
    pub deny: Option<InputError>,
}

#[cfg(test)]
impl FakeInput {
    pub fn emit(&self, event: InputEvent) {
        if let Some(sink) = &self.sink {
            sink(event);
        }
    }
}

#[cfg(test)]
impl InputSource for FakeInput {
    fn start(&mut self, sink: Sink) -> Result<(), InputError> {
        if let Some(error) = self.deny.clone() {
            return Err(error);
        }
        self.running = true;
        self.sink = Some(sink);
        Ok(())
    }
    fn stop(&mut self) {
        self.running = false;
        self.sink = None;
    }
}

#[cfg(test)]
#[path = "input/tests.rs"]
mod tests;
