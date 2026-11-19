use super::throttle::MIN_GAP;
use super::*;
use std::time::Duration;

#[test]
fn text_is_flattened_and_bounded() {
    assert_eq!(clean("a\nb\t c\u{0}d", 50), "a b c d");
    assert_eq!(clean("  spaced   out ", 50), "spaced out");
    let long = "x".repeat(500);
    let cut = clean(&long, 20);
    assert_eq!(cut.chars().count(), 20);
    assert!(cut.ends_with('\u{2026}'));
    assert_eq!(clean(&"\u{e9}".repeat(10), 10), "\u{e9}".repeat(10));
}

#[test]
fn only_plain_ids_are_accepted() {
    assert!(valid_id("0b1e2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d"));
    for bad in ["", "a b", "../x", "a;b", &"a".repeat(65), "\u{e9}"] {
        assert!(!valid_id(bad), "{bad:?}");
    }
}

fn toast(n: u8) -> Toast {
    Toast {
        title: format!("t{n}"),
        body: String::new(),
        click: Click {
            channel_id: "c".into(),
            message_id: n.to_string(),
        },
    }
}

#[test]
fn toasts_are_rate_limited_per_server() {
    let n = Notifier::default();
    let t = Instant::now();
    let ms = Duration::from_millis;
    assert!(matches!(n.offer("a", t, toast(1)), Offer::Show(_)));
    assert!(matches!(
        n.offer("a", t + ms(100), toast(2)),
        Offer::Held { .. }
    ));
    assert!(
        matches!(n.offer("b", t + ms(100), toast(3)), Offer::Show(_)),
        "another server is not held up"
    );
    assert!(matches!(
        n.offer("a", t + MIN_GAP + ms(1), toast(4)),
        Offer::Show(_)
    ));
    n.forget("a");
    assert!(matches!(
        n.offer("a", t + MIN_GAP + ms(2), toast(5)),
        Offer::Show(_)
    ));
}

#[test]
fn a_toast_shown_after_the_gap_supersedes_the_held_one() {
    let n = Notifier::default();
    let t = Instant::now();
    let ms = Duration::from_millis;
    assert!(matches!(n.offer("a", t, toast(1)), Offer::Show(_)));
    assert!(matches!(
        n.offer("a", t + ms(100), toast(2)),
        Offer::Held { .. }
    ));
    assert!(matches!(
        n.offer("a", t + MIN_GAP + ms(1), toast(3)),
        Offer::Show(_)
    ));
    assert!(
        n.take_held("a", t + MIN_GAP + ms(2)).is_none(),
        "the older held toast is not shown after the newer one"
    );
}

#[test]
fn a_burst_is_coalesced_into_its_newest_toast() {
    let n = Notifier::default();
    let t = Instant::now();
    let ms = Duration::from_millis;
    assert!(matches!(n.offer("a", t, toast(1)), Offer::Show(_)));
    let Offer::Held { wait, flush } = n.offer("a", t + ms(100), toast(2)) else {
        panic!("held");
    };
    assert!(flush, "the first held toast schedules the showing");
    assert_eq!(wait, MIN_GAP - ms(100));
    let Offer::Held { flush, .. } = n.offer("a", t + ms(150), toast(3)) else {
        panic!("held");
    };
    assert!(!flush, "a showing is already scheduled");
    let shown = n.take_held("a", t + MIN_GAP).expect("the newest is kept");
    assert_eq!(shown.click.message_id, "3");
    assert!(n.take_held("a", t + MIN_GAP).is_none());
    assert!(
        matches!(
            n.offer("a", t + MIN_GAP + ms(10), toast(4)),
            Offer::Held { .. }
        ),
        "the held toast that was shown restarts the gap"
    );
}

#[test]
fn a_removed_server_drops_its_held_toast_and_a_failed_one_frees_the_gap() {
    let n = Notifier::default();
    let t = Instant::now();
    n.offer("a", t, toast(1));
    n.offer("a", t + Duration::from_millis(10), toast(2));
    n.forget("a");
    assert!(n.take_held("a", t + MIN_GAP).is_none());
    n.offer("a", t, toast(3));
    n.release("a");
    assert!(matches!(
        n.offer("a", t + Duration::from_millis(10), toast(4)),
        Offer::Show(_)
    ));
}

#[test]
fn invisible_format_characters_are_dropped() {
    assert_eq!(
        clean("a\u{202e}b\u{200b}c\u{feff}d\u{2066}e\u{2069}", 50),
        "abcde"
    );
    assert_eq!(
        clean(
            "pay\u{e0069}\u{e0067}\u{e006e}\u{e006f}\u{e0072}\u{e0065} now",
            50
        ),
        "pay now",
        "tag characters hide text and are dropped"
    );
    assert_eq!(
        clean(
            "\u{645}\u{6cc}\u{200c}\u{62e}\u{648}\u{627}\u{647}\u{645}",
            50
        ),
        "\u{645}\u{6cc}\u{200c}\u{62e}\u{648}\u{627}\u{647}\u{645}",
        "the zero-width non-joiner belongs to Persian words"
    );
    assert_eq!(
        clean("\u{1f468}\u{200d}\u{1f469}", 50),
        "\u{1f468}\u{200d}\u{1f469}",
        "emoji joiners stay"
    );
}

#[test]
fn a_toast_names_its_server_once_there_are_several() {
    assert_eq!(
        tagged("Ann in #general", "chat.example", false),
        "Ann in #general"
    );
    assert_eq!(
        tagged("Ann in #general", "chat.example", true),
        "Ann in #general \u{b7} chat.example"
    );
    let long = tagged(&"t".repeat(500), &"n\n".repeat(200), true);
    assert!(long.chars().count() <= MAX_TITLE + MAX_SERVER_NAME + 3);
    assert!(!long.contains('\n'));
}

#[test]
fn the_permission_is_named_like_the_web_api() {
    let names: Vec<String> = [
        Permission::Granted,
        Permission::Denied,
        Permission::Default,
        Permission::Unsupported,
    ]
    .iter()
    .map(|p| serde_json::to_string(p).unwrap())
    .collect();
    assert_eq!(
        names,
        [
            "\"granted\"",
            "\"denied\"",
            "\"default\"",
            "\"unsupported\""
        ]
    );
}

/// Only the instance window may call the notification commands; no other
/// capability (selector, gaming surfaces, updater) may name them.
#[test]
fn only_the_instance_window_is_allowed_to_notify() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
    let mut instance = false;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let names_notify = json["permissions"].as_array().is_some_and(|all| {
            all.iter().filter_map(|p| p.as_str()).any(|p| {
                p.starts_with("allow-desktop-notify")
                    || p == "allow-desktop-set-badge"
                    || p == "allow-desktop-report-unread"
            })
        });
        if json["identifier"] == "trusted-instance" {
            assert!(names_notify);
            assert_eq!(json["webviews"], serde_json::json!(["i-*"]));
            assert!(json.get("windows").is_none());
            instance = true;
        } else {
            assert!(!names_notify, "{path:?}");
        }
    }
    assert!(instance);
}
