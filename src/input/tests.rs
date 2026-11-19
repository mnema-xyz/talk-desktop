use super::*;
use std::sync::{Arc, Mutex};

const SPACE: u16 = 0x20;
const M: u16 = 0x4d;

fn key(key: u16, down: bool) -> InputEvent {
    InputEvent::Key(KeyEvent { key, down })
}

fn tracker() -> ChordTracker {
    let mut t = ChordTracker::default();
    t.set_chords([
        Chord {
            key: M,
            alt: true,
            ..Chord::default()
        },
        Chord {
            key: 0x4e,
            alt: true,
            ..Chord::default()
        },
        Chord {
            key: 0x44,
            alt: true,
            ..Chord::default()
        },
        Chord {
            key: SPACE,
            ..Chord::default()
        },
    ]);
    t
}

#[test]
fn a_chord_is_down_only_with_exactly_its_modifiers() {
    let mut t = tracker();
    assert!(!t.apply(key(M, true)), "M alone is not Alt+M");
    assert_eq!(t.sample().0, [false; 4]);
    t.apply(key(M, false));
    assert!(!t.apply(key(VK_LMENU, true)));
    assert!(t.apply(key(M, true)));
    assert_eq!(t.sample().0, [true, false, false, false]);
    // Adding Shift breaks the exact match; dropping it restores it.
    assert!(t.apply(key(VK_LSHIFT, true)));
    assert_eq!(t.sample().0, [false; 4]);
    assert!(t.apply(key(VK_LSHIFT, false)));
    assert_eq!(t.sample().0, [true, false, false, false]);
    assert!(t.apply(key(M, false)));
    assert_eq!(t.sample().0, [false; 4]);
}

#[test]
fn plain_push_to_talk_key_ignores_nothing_but_the_windows_key() {
    let mut t = tracker();
    assert!(t.apply(key(SPACE, true)));
    assert!(t.sample().0[3]);
    assert!(t.apply(key(VK_LWIN, true)), "Win+Space is not push-to-talk");
    assert!(!t.sample().0[3]);
    assert!(t.apply(key(VK_LWIN, false)));
    assert!(t.sample().0[3]);
    // Any modifier makes it a different chord.
    assert!(t.apply(key(VK_LCONTROL, true)));
    assert!(!t.sample().0[3]);
}

#[test]
fn key_repeat_and_unrelated_typing_do_not_wake_anyone() {
    let mut t = tracker();
    assert!(t.apply(key(SPACE, true)));
    for _ in 0..5 {
        assert!(!t.apply(key(SPACE, true)), "auto-repeat is not a change");
    }
    for typed in *b"HI!" {
        assert!(!t.apply(key(u16::from(typed), true)));
        assert!(!t.apply(key(u16::from(typed), false)));
    }
    assert!(t.apply(key(SPACE, false)));
}

#[test]
fn a_tap_between_two_samples_is_latched_once_and_then_released() {
    let mut t = tracker();
    assert!(t.apply(key(SPACE, true)));
    assert!(t.apply(key(SPACE, false)));
    let (keys, again) = t.sample();
    assert!(keys[3], "the tap is seen");
    assert!(again, "and the release still has to be sampled");
    let (keys, again) = t.sample();
    assert!(!keys[3]);
    assert!(!again);
}

#[test]
fn a_held_chord_is_not_latched_twice() {
    let mut t = tracker();
    t.apply(key(SPACE, true));
    assert_eq!(t.sample(), ([false, false, false, true], false));
    assert_eq!(t.sample(), ([false, false, false, true], false));
    t.apply(key(SPACE, false));
    assert_eq!(t.sample().0, [false; 4]);
}

#[test]
fn left_and_right_modifiers_release_independently() {
    let mut t = tracker();
    t.apply(key(VK_LMENU, true));
    t.apply(key(VK_RMENU, true));
    t.apply(key(M, true));
    assert!(t.sample().0[0]);
    t.apply(key(VK_LMENU, false));
    assert!(t.sample().0[0], "the right Alt is still down");
    t.apply(key(VK_RMENU, false));
    assert!(!t.sample().0[0]);
}

#[test]
fn reset_and_clear_drop_every_key_and_report_a_released_chord() {
    let mut t = tracker();
    t.apply(key(SPACE, true));
    let _ = t.sample();
    assert!(
        t.apply(InputEvent::Reset),
        "the chord that was down went up"
    );
    assert_eq!(t.sample(), ([false; 4], false));
    assert!(!t.clear());
    // A stuck key-up that never came does not survive a reset.
    t.apply(key(M, true));
    t.apply(InputEvent::Reset);
    t.apply(key(VK_LMENU, true));
    assert!(!t.sample().0[0]);
}

#[test]
fn reconfiguring_does_not_treat_a_held_chord_as_a_new_press() {
    let mut t = ChordTracker::default();
    t.apply(key(SPACE, true));
    t.set_chords([
        Chord::default(),
        Chord::default(),
        Chord::default(),
        Chord {
            key: SPACE,
            ..Chord::default()
        },
    ]);
    assert_eq!(t.sample().0, [false, false, false, true]);
    // An unset chord (key 0) never matches.
    assert!(!t.apply(key(0, true)));
}

#[test]
fn mouse_side_buttons_work_as_chord_keys() {
    let mut t = ChordTracker::default();
    t.set_chords([
        Chord {
            key: VK_XBUTTON1,
            ..Chord::default()
        },
        Chord::default(),
        Chord::default(),
        Chord {
            key: VK_XBUTTON2,
            shift: true,
            ..Chord::default()
        },
    ]);
    assert!(t.apply(key(VK_XBUTTON1, true)));
    assert!(t.sample().0[0]);
    // Shift turns the first chord off; the second needs Shift.
    assert!(t.apply(key(VK_LSHIFT, true)));
    assert!(!t.sample().0[0]);
    assert!(t.apply(key(VK_XBUTTON2, true)));
    assert!(t.sample().0[3]);
}

#[test]
fn out_of_range_keys_are_ignored() {
    let mut t = tracker();
    assert!(!t.apply(key(300, true)));
    assert!(!t.apply(key(u16::MAX, true)));
}

#[test]
fn keyboard_reports_decode_to_side_specific_keys() {
    let key = |key, down| KeyReport::Key(KeyEvent { key, down });
    assert_eq!(decode_keyboard(0x41, 0x1e, 0), key(0x41, true));
    assert_eq!(decode_keyboard(0x41, 0x1e, RI_KEY_BREAK), key(0x41, false));
    assert_eq!(decode_keyboard(VK_SHIFT, 0x2a, 0), key(VK_LSHIFT, true));
    assert_eq!(
        decode_keyboard(VK_SHIFT, 0x36, RI_KEY_BREAK),
        key(VK_RSHIFT, false)
    );
    assert_eq!(
        decode_keyboard(VK_CONTROL, 0x1d, RI_KEY_E0),
        key(VK_RCONTROL, true)
    );
    assert_eq!(
        decode_keyboard(0x13, 0x45, RI_KEY_E1),
        key(0x13, true),
        "Pause"
    );
}

#[test]
fn untrustworthy_keyboard_reports_reset_and_fake_keys_are_ignored() {
    assert_eq!(decode_keyboard(0xff, 0x2a, RI_KEY_E0), KeyReport::Ignore);
    assert_eq!(decode_keyboard(0, 0, 0), KeyReport::Ignore);
    assert_eq!(decode_keyboard(0x41, 0xff, 0), KeyReport::Reset, "overrun");
    assert_eq!(
        decode_keyboard(0x41, 0x1e, 0x8),
        KeyReport::Reset,
        "unknown flag"
    );
    assert_eq!(
        decode_keyboard(VK_SHIFT, 0x1e, 0),
        KeyReport::Reset,
        "unknown Shift side"
    );
}

#[test]
fn extra_mouse_buttons_decode_and_other_mouse_data_is_dropped() {
    let events: Vec<_> = decode_mouse_buttons(0x40 | 0x200).collect();
    assert_eq!(
        events,
        [
            KeyEvent {
                key: VK_XBUTTON1,
                down: true
            },
            KeyEvent {
                key: VK_XBUTTON2,
                down: false
            },
        ]
    );
    // Buttons 1-3 and the wheel are not ours.
    assert_eq!(decode_mouse_buttons(0x1 | 0x2 | 0x400).count(), 0);
    assert_eq!(RI_MOUSE_EXTRA_BUTTONS, 0x3c0);
}

#[test]
fn session_and_power_changes_that_lose_key_ups_are_recognised() {
    // lock 7, unlock 8, console disconnect 2, remote disconnect 4, logoff 6
    for code in [2, 4, 6, 7, 8] {
        assert!(session_change_loses_keys(code), "{code}");
    }
    // connects, logon, remote control: keys are still tracked
    for code in [1, 3, 5, 9, 0xa] {
        assert!(!session_change_loses_keys(code), "{code}");
    }
    for code in [0x4, 0x7, 0x12] {
        assert!(power_change_loses_keys(code), "{code}");
    }
    assert!(!power_change_loses_keys(0xa), "power status change");
}

#[test]
fn raw_input_modifiers_get_their_side() {
    assert_eq!(side_specific(VK_SHIFT, 0x2a, false), VK_LSHIFT);
    assert_eq!(side_specific(VK_SHIFT, 0x36, false), VK_RSHIFT);
    assert_eq!(side_specific(VK_CONTROL, 0x1d, false), VK_LCONTROL);
    assert_eq!(side_specific(VK_CONTROL, 0x1d, true), VK_RCONTROL);
    assert_eq!(side_specific(VK_MENU, 0x38, false), VK_LMENU);
    assert_eq!(side_specific(VK_MENU, 0x38, true), VK_RMENU);
    assert_eq!(side_specific(0x41, 0x1e, false), 0x41);
}

#[test]
fn fake_source_feeds_a_tracker_through_the_sink_and_stops_cleanly() {
    let shared = Arc::new(Mutex::new(tracker()));
    let woke = Arc::new(Mutex::new(0));
    let mut source = FakeInput::default();
    let (t, w) = (shared.clone(), woke.clone());
    source
        .start(Box::new(move |event| {
            if t.lock().unwrap().apply(event) {
                *w.lock().unwrap() += 1;
            }
        }))
        .unwrap();
    source.emit(key(SPACE, true));
    source.emit(key(b'X' as u16, true));
    source.emit(key(SPACE, false));
    assert_eq!(*woke.lock().unwrap(), 2);
    source.stop();
    source.emit(key(SPACE, true));
    assert_eq!(*woke.lock().unwrap(), 2);
    assert!(!source.running);
}

/// A game floods the input window with key events. What the input thread does
/// per event (decode, apply to the tracker under its lock) must stay trivial,
/// so a burst of them is never what delays anything else.
#[test]
fn an_input_flood_is_cheap_per_event() {
    let mut t = tracker();
    let flood = 200_000u32;
    let begin = std::time::Instant::now();
    let mut woke = 0u32;
    for i in 0..flood {
        let vkey = 0x41 + (i % 26) as u16;
        if let KeyReport::Key(event) = decode_keyboard(vkey, 0x1e, (i % 2) as u16) {
            woke += u32::from(t.apply(InputEvent::Key(event)));
        }
    }
    let took = begin.elapsed();
    // Not one of the 26 letters is a chord key except M: almost nothing wakes the UI.
    assert!(woke < flood / 10, "{woke} wake-ups for {flood} events");
    assert!(
        took < std::time::Duration::from_secs(2),
        "{flood} events took {took:?}"
    );
}
