use super::*;
use serde_json::Value;

const A: &str = "0123456789abcdef";
const B: &str = "fedcba9876543210";
const F: &str = "f-0123abcd";

fn layout() -> Layout {
    let known = [A.to_owned(), B.to_owned()];
    Layout::load(None, BTreeMap::new(), &known, &known)
}

fn ask(layout: &mut Layout, op: &str, id: &str, set: impl FnOnce(&mut Edit)) -> Result<(), String> {
    let mut edit = Edit {
        op: op.into(),
        id: Some(id.into()),
        ..Edit::default()
    };
    set(&mut edit);
    apply(layout, &edit, || F.to_owned())
}

#[test]
fn a_list_with_tens_of_thousands_of_folders_loads_in_linear_time_and_keeps_one_of_each() {
    // 60 000 different folder ids: a list compared against every folder
    // kept so far (quadratic) needs far longer than the bound below.
    let known: Vec<String> = (0..200).map(|n| format!("{n:016x}")).collect();
    let mut items: Vec<Item> = (0..60_000)
        .map(|n| Item::Folder {
            id: format!("f{n}"),
            name: String::new(),
            color: None,
            collapsed: false,
            servers: vec![known[n % 200].clone()],
        })
        .collect();
    items.push(Item::Folder {
        id: "f1".into(),
        name: String::new(),
        color: None,
        collapsed: false,
        servers: vec![known[1].clone()],
    });
    let started = std::time::Instant::now();
    let layout = Layout::load(Some(items), BTreeMap::new(), &known, &known);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(layout.flat().len(), 200);
    let mut seen = BTreeSet::new();
    for item in &layout.items {
        if let Item::Folder { id, .. } = item {
            assert!(seen.insert(id.clone()), "{id} twice");
        }
    }
}

#[test]
fn the_page_names_servers_and_folders_but_never_makes_up_a_folder() {
    let mut l = layout();
    ask(&mut l, "group", A, |e| e.target = Some(B.into())).unwrap();
    assert_eq!(l.items.len(), 1);
    assert!(
        matches!(&l.items[0], Item::Folder { id, servers, .. } if id == F && servers == &[B, A])
    );
    ask(&mut l, "toggle", F, |e| e.flag = Some(true)).unwrap();
    ask(&mut l, "folder_edit", F, |e| {
        e.name = Some(" Team ".into());
        e.color = Some("#AA00BB".into());
    })
    .unwrap();
    ask(&mut l, "rename", A, |e| e.name = Some("Fox".into())).unwrap();
    ask(&mut l, "step", A, |e| e.delta = Some(-1)).unwrap();
    ask(&mut l, "ungroup", A, |_| {}).unwrap();
    ask(&mut l, "place", A, |e| e.before = Some(F.into())).unwrap();
    ask(&mut l, "folder_delete", F, |_| {}).unwrap();
    assert_eq!(l.flat(), [A, B]);
    ask(&mut l, "new_folder", B, |_| {}).unwrap();
    assert_eq!(l.items.len(), 2);
}

#[test]
fn a_request_with_a_wrong_id_op_or_argument_changes_nothing() {
    let mut l = layout();
    let before = l.clone();
    let wrong = [
        ask(&mut l, "explode", A, |_| {}),
        ask(&mut l, "rename", "zz", |e| e.name = Some("x".into())),
        ask(&mut l, "rename", F, |e| e.name = Some("x".into())),
        ask(&mut l, "rename", A, |_| {}),
        ask(&mut l, "toggle", A, |_| {}),
        ask(&mut l, "folder_delete", A, |_| {}),
        ask(&mut l, "group", A, |e| e.target = Some("nope".into())),
        ask(&mut l, "group", F, |e| e.target = Some(B.into())),
        ask(&mut l, "step", A, |_| {}),
        ask(&mut l, "step", A, |e| e.delta = Some(2)),
        ask(&mut l, "place", A, |e| e.target = Some("somewhere".into())),
        ask(&mut l, "place", A, |e| e.before = Some("../x".into())),
        // Valid, but there is nothing to do.
        ask(&mut l, "ungroup", A, |_| {}),
        ask(&mut l, "step", A, |e| e.delta = Some(-1)),
    ];
    assert!(wrong.iter().all(Result::is_err), "{wrong:?}");
    assert_eq!(l, before);
    let none = Edit {
        op: "rename".into(),
        ..Edit::default()
    };
    assert!(apply(&mut l, &none, String::new).is_err());
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

fn text<'a>(op: &'a Value, key: &str) -> &'a str {
    op[key].as_str().unwrap_or_default()
}

fn run(layout: &mut Layout, op: &Value) -> bool {
    let id = text(op, "id");
    match text(op, "op") {
        "add" => layout.add(id),
        "remove" => layout.remove(id),
        "place" => layout.place(id, text(op, "target"), text(op, "before")),
        "group" => layout.group(id, text(op, "target"), text(op, "folder")),
        "new_folder" => layout.new_folder(id, text(op, "folder")),
        "ungroup" => layout.ungroup(id),
        "step" => layout.step(id, op["delta"].as_i64().unwrap() as i32),
        "toggle" => layout.toggle(id, op["flag"].as_bool()),
        "folder_edit" => layout.folder_edit(id, text(op, "name"), op["color"].as_str()),
        "folder_delete" => layout.folder_delete(id),
        "rename" => layout.rename(id, text(op, "name")),
        other => panic!("unknown op {other}"),
    }
}

#[test]
fn the_shared_vectors_give_the_answers_written_in_them() {
    let file: Value =
        serde_json::from_str(include_str!("../../../../test-vectors/rail-layout.json")).unwrap();
    for case in file["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let items: Option<Vec<Item>> = (!case["start"].is_null())
            .then(|| serde_json::from_value(case["start"].clone()).unwrap());
        let aliases: BTreeMap<String, String> =
            serde_json::from_value(case["start_aliases"].clone()).unwrap();
        let known = strings(&case["known"]);
        let mut layout = Layout::load(items, aliases, &known, &strings(&case["order"]));
        for op in case["ops"].as_array().unwrap() {
            let expected = op["ok"].as_bool().unwrap_or(true);
            assert_eq!(run(&mut layout, op), expected, "{name}: {op}");
        }
        let want: Vec<Item> = serde_json::from_value(case["items"].clone()).unwrap();
        assert_eq!(layout.items, want, "{name}");
        assert_eq!(layout.flat(), strings(&case["flat"]), "{name}");
        let aliases: BTreeMap<String, String> =
            serde_json::from_value(case["aliases"].clone()).unwrap();
        assert_eq!(layout.aliases, aliases, "{name}");
    }
}

#[test]
fn items_are_written_the_way_the_rail_and_the_list_file_read_them() {
    let items = vec![
        Item::Server { id: "a".into() },
        Item::Folder {
            id: "f-1".into(),
            name: "Work".into(),
            color: None,
            collapsed: true,
            servers: vec!["b".into()],
        },
    ];
    assert_eq!(
        serde_json::to_string(&items).unwrap(),
        r##"[{"kind":"server","id":"a"},{"kind":"folder","id":"f-1","name":"Work","color":null,"collapsed":true,"servers":["b"]}]"##
    );
}

#[test]
fn folder_ids_made_by_the_host_are_told_from_server_ids() {
    assert!(is_folder_id("f-0123abcd"));
    assert!(!is_folder_id("f-0123ABCD"));
    assert!(!is_folder_id("f-1"));
    assert!(!is_folder_id("0123456789abcdef"));
}
