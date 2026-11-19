use super::super::voice::Member;
use super::*;

fn ptt_talk() -> Gaming {
    let mut g = Gaming {
        origin: Some("https://example.invalid".into()),
        ..Gaming::default()
    };
    g.voice.connected = true;
    g.voice.ptt_mode = true;
    g.sync_chords();
    g
}
fn look(g: &mut Gaming, now: Instant, game: bool) -> (Vec<&'static str>, bool) {
    let (keys, again) = g.keys.lock().unwrap().sample();
    let actions = g.advance(now, game, keys);
    (
        actions
            .iter()
            .map(|c| c.action.as_str())
            .map(leak)
            .collect(),
        again,
    )
}
fn leak(text: &str) -> &'static str {
    match text {
        "ptt-press" => "ptt-press",
        "ptt-release" => "ptt-release",
        "mute" => "mute",
        "deafen" => "deafen",
        other => panic!("unexpected action {other}"),
    }
}
fn press(g: &Gaming, key: u16, down: bool) {
    g.keys
        .lock()
        .unwrap()
        .apply(input::InputEvent::Key(input::KeyEvent { key, down }));
}

#[test]
fn held_push_to_talk_key_presses_then_releases_from_key_events_alone() {
    let mut g = ptt_talk();
    let now = Instant::now();
    assert_eq!(look(&mut g, now, true), (vec![], false));
    press(&g, 0x20, true);
    assert_eq!(look(&mut g, now, true).0, vec!["ptt-press"]);
    // The page lets a press lapse after 150 ms, so a held key is re-sent.
    assert_eq!(look(&mut g, now, true).0, vec!["ptt-press"], "keepalive");
    press(&g, 0x20, false);
    assert_eq!(look(&mut g, now, true).0, vec!["ptt-release"]);
}
#[test]
fn a_tap_between_two_looks_still_presses_and_then_releases() {
    let mut g = ptt_talk();
    let now = Instant::now();
    look(&mut g, now, true);
    press(&g, 0x20, true);
    press(&g, 0x20, false);
    let (first, again) = look(&mut g, now, true);
    assert_eq!(first, vec!["ptt-press"]);
    assert!(again, "the release must still be looked at");
    assert_eq!(look(&mut g, now, true), (vec!["ptt-release"], false));
}
#[test]
fn keys_outside_a_game_do_nothing_and_a_key_held_into_a_game_waits_for_a_release() {
    let mut g = ptt_talk();
    let now = Instant::now();
    look(&mut g, now, false);
    press(&g, 0x20, true);
    assert_eq!(look(&mut g, now, false).0, Vec::<&str>::new());
    // The game comes to the front while the key is still down.
    assert_eq!(look(&mut g, now, true).0, Vec::<&str>::new());
    press(&g, 0x20, false);
    look(&mut g, now, true);
    press(&g, 0x20, true);
    assert_eq!(look(&mut g, now, true).0, vec!["ptt-press"]);
}
#[test]
fn leaving_the_game_or_a_lost_key_up_releases_push_to_talk() {
    let mut g = ptt_talk();
    let now = Instant::now();
    look(&mut g, now, true);
    press(&g, 0x20, true);
    look(&mut g, now, true);
    // Focus leaves the game while the key is down.
    assert_eq!(look(&mut g, now, false).0, vec!["ptt-release"]);
    // The session locked: Raw Input could not report the key-up, the source resets.
    let mut g = ptt_talk();
    look(&mut g, now, true);
    press(&g, 0x20, true);
    look(&mut g, now, true);
    g.keys.lock().unwrap().apply(input::InputEvent::Reset);
    assert_eq!(look(&mut g, now, true).0, vec!["ptt-release"]);
}
#[test]
fn a_long_gap_between_looks_rearms_instead_of_pressing() {
    let mut g = ptt_talk();
    let start = Instant::now();
    look(&mut g, start, true);
    press(&g, 0x20, true);
    let later = start + STALL + Duration::from_millis(1);
    assert_eq!(look(&mut g, later, true).0, Vec::<&str>::new());
}
#[test]
fn mute_and_deafen_chords_fire_once_per_press() {
    let mut g = ptt_talk();
    let now = Instant::now();
    look(&mut g, now, true);
    press(&g, 0x12, true);
    press(&g, 0x4e, true);
    assert_eq!(look(&mut g, now, true).0, vec!["mute"]);
    assert_eq!(look(&mut g, now, true).0, Vec::<&str>::new());
    press(&g, 0x4e, false);
    press(&g, 0x44, true);
    assert_eq!(look(&mut g, now, true).0, vec!["deafen"]);
}
#[test]
fn global_disable_hides_surfaces_and_releases_ptt_without_reopening_held_key() {
    let mut g = Gaming {
        origin: Some("https://example.invalid".into()),
        ..Gaming::default()
    };
    g.voice.connected = true;
    g.voice.ptt_mode = true;
    let now = Instant::now();
    g.advance(now, true, [false; 4]);
    g.advance(now, true, [false, false, false, true]);
    assert!(g.ptt_down);
    g.overlay = true;
    g.settings.enabled = false;
    let commands = g.advance(now, true, [true; 4]);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].action, "ptt-release");
    assert!(!g.active);
    assert!(!g.overlay);
    g.settings.enabled = true;
    assert!(g.advance(now, true, [true; 4]).is_empty());
    assert!(!g.ptt_down);
    assert!(!g.overlay);
}

#[test]
fn chords_act_only_on_new_key_edges_while_gaming_is_active() {
    let mut g = ptt_talk();
    let now = Instant::now();
    assert!(g.advance(now, false, [false; 4]).is_empty());
    assert!(!g.active);
    assert!(g.advance(now, true, [false; 4]).is_empty());
    assert!(g.active);
    let actions = g.advance(now, true, [true, true, true, false]);
    assert!(g.overlay);
    assert_eq!(
        actions
            .iter()
            .map(|c| c.action.as_str())
            .collect::<Vec<_>>(),
        ["mute", "deafen"]
    );
    assert!(g.advance(now, true, [true, true, true, false]).is_empty());
    g.advance(now, true, [false; 4]);
    g.advance(now, true, [true, false, false, false]);
    assert!(!g.overlay);
}

#[test]
fn leaving_the_game_releases_a_pressed_push_to_talk() {
    let mut g = ptt_talk();
    let now = Instant::now();
    g.advance(now, true, [false; 4]);
    let actions = g.advance(now, true, [false, false, false, true]);
    assert_eq!(actions[0].action, "ptt-press");
    let actions = g.advance(now, false, [false; 4]);
    assert_eq!(actions[0].action, "ptt-release");
    assert!(!g.active);
    assert!(!g.overlay);
}

#[test]
fn a_stale_report_or_a_gap_between_looks_ends_gaming() {
    let mut g = ptt_talk();
    let now = Instant::now();
    g.advance(now, true, [false; 4]);
    g.updated = now - Duration::from_secs(3);
    assert!(g.advance(now, true, [true; 4]).is_empty());
    assert!(!g.active);
    g.updated = Instant::now();
    g.last_tick = now - Duration::from_secs(1);
    assert!(g.advance(now, true, [true; 4]).is_empty());
    assert!(!g.ptt_armed);
}

fn talk(account: &str, channel: &str, scope: &str) -> Voice {
    Voice {
        connected: true,
        scope: Some(scope.into()),
        account: Some(account.into()),
        channel: Some(channel.into()),
        members: vec![Member {
            id: account.into(),
            name: "Player".into(),
            speaking: false,
            muted: false,
            sharing: false,
        }],
        ..Voice::default()
    }
}

fn two_servers() -> Gaming {
    let mut g = Gaming {
        origin: Some("mnema://localhost".into()),
        ..Gaming::default()
    };
    g.tokens.insert("a".into(), "token-a".into());
    g.tokens.insert("b".into(), "token-b".into());
    g
}

const U1: &str = "11111111-1111-4111-8111-111111111111";
const U2: &str = "22222222-2222-4222-8222-222222222222";
const U3: &str = "33333333-3333-4333-8333-333333333333";

#[test]
fn a_page_needs_the_token_of_its_own_server_and_the_apps_origin() {
    let mut g = two_servers();
    let origin = Some("mnema://localhost");
    assert!(
        g.accept_sync("a", "token-b", origin, talk(U1, U2, U3))
            .is_err()
    );
    assert!(g.accept_sync("a", "", origin, talk(U1, U2, U3)).is_err());
    assert!(
        g.accept_sync("c", "token-a", origin, talk(U1, U2, U3))
            .is_err()
    );
    assert!(
        g.accept_sync(
            "a",
            "token-a",
            Some("https://evil.example"),
            talk(U1, U2, U3)
        )
        .is_err()
    );
    assert!(
        g.accept_sync("a", "token-a", None, talk(U1, U2, U3))
            .is_err()
    );
    assert!(g.owner.is_none(), "nothing was accepted");
    assert!(
        g.accept_sync("a", "token-a", origin, talk(U1, U2, U3))
            .is_ok()
    );
    assert_eq!(g.owner.as_deref(), Some("a"));
}

#[test]
fn gaming_stays_with_the_server_that_owns_the_talk() {
    let mut g = two_servers();
    let origin = Some("mnema://localhost");
    g.accept_sync("a", "token-a", origin, talk(U1, U2, U3))
        .unwrap();
    // B reports its own Talk while A's is live: ignored, not mixed in.
    g.accept_sync("b", "token-b", origin, talk(U3, U2, U1))
        .unwrap();
    assert_eq!(g.owner.as_deref(), Some("a"));
    assert_eq!(g.voice.account.as_deref(), Some(U1));
    // B saying "not connected" does not end A's Talk either.
    g.accept_sync("b", "token-b", origin, Voice::default())
        .unwrap();
    assert_eq!(g.owner.as_deref(), Some("a"));
    assert!(g.voice.connected);
    // Commands go back to A's page with A's token.
    let command = Command::new(&g, "mute");
    assert_eq!(
        (command.owner.as_deref(), command.token.as_str()),
        (Some("a"), "token-a")
    );
    // A leaves its Talk: gaming lets go, and B may take it.
    g.accept_sync("a", "token-a", origin, Voice::default())
        .unwrap();
    assert_eq!(g.owner, None);
    g.accept_sync("b", "token-b", origin, talk(U3, U2, U1))
        .unwrap();
    assert_eq!(g.owner.as_deref(), Some("b"));
    let command = Command::new(&g, "mute");
    assert_eq!(
        (command.owner.as_deref(), command.token.as_str()),
        (Some("b"), "token-b")
    );
}

#[test]
fn a_stale_owner_does_not_hold_gaming_forever() {
    let mut g = two_servers();
    let origin = Some("mnema://localhost");
    g.accept_sync("a", "token-a", origin, talk(U1, U2, U3))
        .unwrap();
    g.updated = Instant::now() - Duration::from_secs(3);
    g.accept_sync("b", "token-b", origin, talk(U3, U2, U1))
        .unwrap();
    assert_eq!(
        g.owner.as_deref(),
        Some("b"),
        "A stopped reporting, B takes over"
    );
    assert!(!g.overlay && !g.active, "a different Talk starts clean");
}

#[test]
fn letting_go_of_a_server_forgets_its_token_and_keeps_the_settings() {
    let mut g = two_servers();
    g.settings.sidepeek_opacity = 20;
    g.accept_sync("a", "token-a", Some("mnema://localhost"), talk(U1, U2, U3))
        .unwrap();
    g.release();
    assert_eq!(g.owner, None);
    assert!(!g.voice.connected);
    assert_eq!(g.settings.sidepeek_opacity, 20);
    assert_eq!(g.tokens.len(), 2, "release keeps the tokens of live pages");
    assert_eq!(g.origin.as_deref(), Some("mnema://localhost"));
}
