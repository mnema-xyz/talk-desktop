use super::*;
use serde_json::{Value, json};

/// Who may call what is decided by the capability files: the rail drives
/// the list and nothing else; a server's page reaches its own socket,
/// toasts, counters and gaming bridge and can neither drive the list nor
/// add a server; only the page for adding may do that.
fn capabilities() -> Vec<Value> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            serde_json::from_str(&text).unwrap()
        })
        .collect()
}

fn permissions(capability: &Value) -> Vec<&str> {
    capability["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect()
}

fn named<'a>(capabilities: &'a [Value], id: &str) -> &'a Value {
    capabilities
        .iter()
        .find(|c| c["identifier"] == id)
        .unwrap_or_else(|| panic!("no capability {id}"))
}

#[test]
fn the_rail_drives_the_list_and_nothing_else() {
    let all = capabilities();
    let rail = named(&all, "server-rail");
    assert_eq!(rail["webviews"], json!(["rail"]));
    assert!(rail.get("windows").is_none());
    let permissions = permissions(rail);
    assert!(permissions.iter().all(|p| p.starts_with("allow-rail-")));
    assert_eq!(permissions.len(), 14);
    // The update pill: its state, and "Update now" or the update window.
    for command in [
        "edit",
        "menu",
        "dialog",
        "icon",
        "update-state",
        "update-action",
    ] {
        assert!(permissions.contains(&format!("allow-rail-{command}").as_str()));
    }
    assert!(!permissions.contains(&"allow-rail-move"));
}

#[test]
fn the_dialog_can_hand_back_its_answer_and_nothing_else() {
    // It is reached by its webview alone.
    let all = capabilities();
    let dialog = named(&all, "rail-dialog");
    assert_eq!(dialog["webviews"], json!(["rail-dialog"]));
    assert!(dialog.get("windows").is_none());
    assert_eq!(permissions(dialog), ["allow-dialog-done"]);
}

#[test]
fn no_other_page_drives_the_list_answers_the_dialog_or_adds_a_server() {
    for capability in capabilities() {
        let id = capability["identifier"].as_str().unwrap();
        if id == "server-rail" || id == "rail-dialog" {
            continue;
        }
        let permissions = permissions(&capability);
        let names = |prefix: &str| permissions.iter().any(|p| p.starts_with(prefix));
        assert!(!names("allow-rail-"), "{id} may drive the list");
        assert!(!names("allow-dialog-"), "{id} may answer the dialog");
        if capability["webviews"] == json!(["i-*"]) {
            assert!(
                !names("allow-desktop-open-instance"),
                "{id}: a page of a server may not add servers"
            );
            assert!(
                capability.get("windows").is_none(),
                "{id}: by webview, not window"
            );
        }
    }
}

#[test]
fn every_capability_file_is_in_force() {
    // tauri.conf.json names each one.
    let conf: Value = serde_json::from_str(include_str!("../../../tauri.conf.json")).unwrap();
    let in_force: Vec<&str> = conf["app"]["security"]["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for capability in capabilities() {
        assert!(in_force.contains(&capability["identifier"].as_str().unwrap()));
    }
}

#[test]
fn marking_a_server_read_dispatches_the_event_the_web_app_listens_for() {
    assert!(MARK_READ.contains("mnema-desktop:mark-all-read"));
}
