use super::*;

const RENAME: &str = r##"{"title":"Rename","input":{"label":"Name","value":"Lantern","max":40},
        "buttons":[{"id":"cancel","text":"Cancel"},{"id":"save","text":"Save","primary":true}],"cancel":"cancel"}"##;

const FOLDER: &str = r##"{"title":"Folder","message":"Pick","input":{"label":"Name","value":"","max":40,"placeholder":"Folder"},
        "colors":{"label":"Colour","value":"#2da771","choices":[{"value":"","label":"None"},{"value":"#2da771","label":"Green"}]},
        "buttons":[{"id":"cancel","text":"Cancel"},{"id":"save","text":"Save","primary":true}],"cancel":"cancel"}"##;

#[test]
fn a_dialog_is_taken_as_the_rail_describes_it() {
    let rename = parse(RENAME).unwrap();
    assert_eq!(rename.input.as_ref().unwrap().value, "Lantern");
    assert!(parse(FOLDER).unwrap().colors.is_some());
    assert!(parse(r#"{"title":"Remove","message":"Sure?","buttons":[{"id":"no","text":"No"},{"id":"yes","text":"Yes","danger":true}],"cancel":"no"}"#).is_ok());
}

#[test]
fn a_dialog_that_is_not_what_the_rail_may_ask_for_is_refused() {
    for bad in [
        "{}",
        "[]",
        // Cancel must be one of the buttons; one to three buttons.
        r#"{"title":"x","buttons":[{"id":"a","text":"A"}],"cancel":"b"}"#,
        r#"{"title":"x","buttons":[],"cancel":"a"}"#,
        r#"{"title":"x","buttons":[{"id":"a","text":"A"},{"id":"b","text":"B"},{"id":"c","text":"C"},{"id":"d","text":"D"}],"cancel":"a"}"#,
        // Names of buttons are lower case words; unknown keys are refused.
        r#"{"title":"x","buttons":[{"id":"A b","text":"A"}],"cancel":"A b"}"#,
        r#"{"title":"x","buttons":[{"id":"a","text":"A"}],"cancel":"a","html":"<b>"}"#,
        // Limits of the fields.
        r#"{"title":"x","input":{"label":"l","value":"","max":81},"buttons":[{"id":"a","text":"A"}],"cancel":"a"}"#,
        r#"{"title":"x","input":{"label":"l","value":"too long","max":3},"buttons":[{"id":"a","text":"A"}],"cancel":"a"}"#,
        r#"{"title":"x","colors":{"label":"c","value":"red","choices":[]},"buttons":[{"id":"a","text":"A"}],"cancel":"a"}"#,
    ] {
        assert!(parse(bad).is_err(), "{bad}");
    }
    let long = format!(
        r#"{{"title":"{}","buttons":[{{"id":"a","text":"A"}}],"cancel":"a"}}"#,
        "t".repeat(81)
    );
    assert!(parse(&long).is_err());
}

#[test]
fn an_answer_that_the_dialog_could_not_have_given_is_a_cancel() {
    let spec = parse(FOLDER).unwrap();
    let said = |button: &str, value: Option<&str>, color: Option<&str>| {
        accept(
            &spec,
            button,
            value.map(str::to_owned),
            color.map(str::to_owned),
        )
    };
    let good = said("save", Some("Work"), Some("#2da771"));
    assert_eq!(
        (
            good.button.as_str(),
            good.value.as_deref(),
            good.color.as_deref()
        ),
        ("save", Some("Work"), Some("#2da771"))
    );
    assert_eq!(
        said("save", Some("Work"), Some("")).color.as_deref(),
        Some("")
    );
    for bad in [
        said("delete", Some("x"), None),
        said("save", Some(&"x".repeat(41)), None),
        said("save", Some("x"), Some("#ff0000")),
    ] {
        assert_eq!(
            (bad.button.as_str(), bad.value, bad.color),
            ("cancel", None, None)
        );
    }
    let rename = parse(RENAME).unwrap();
    // A colour where none was asked for.
    assert_eq!(
        accept(&rename, "save", Some("x".into()), Some("#2da771".into())).button,
        "cancel"
    );
    assert_eq!(
        accept(&rename, "save", Some("x".into()), None).button,
        "save"
    );
}

const SWITCH: &str = r##"{"title":"Switch","list":{"label":"Server","choices":[{"id":"a1","text":"One","hint":"one.example"},{"id":"b-2","text":"Two"}]},
        "buttons":[{"id":"cancel","text":"Cancel"}],"cancel":"cancel"}"##;
const IMPORT: &str = r##"{"title":"Import","file":{"label":"File","accept":".json","max":8388608},
        "buttons":[{"id":"cancel","text":"Cancel"},{"id":"import","text":"Import","primary":true}],"cancel":"cancel"}"##;

#[test]
fn a_list_to_pick_from_and_a_file_to_read_are_taken_as_described() {
    assert!(parse(SWITCH).is_ok());
    assert!(parse(IMPORT).is_ok());
    // The rail's import asks for exactly the host's bound.
    assert!(
        include_str!("../../rail-ui/rail-commands.js").contains(&format!("max: {MAX_FILE} }}")),
        "rail-commands.js asks for another bound than {MAX_FILE}"
    );
    let too_big = format!(
        r#"{{"title":"x","file":{{"label":"f","accept":".json","max":{}}},"buttons":[{{"id":"a","text":"A"}}],"cancel":"a"}}"#,
        MAX_FILE + 1
    );
    for bad in [
        // Only one of input, list and file.
        r#"{"title":"x","input":{"label":"l","value":"","max":5},"file":{"label":"f","accept":".json","max":5},"buttons":[{"id":"a","text":"A"}],"cancel":"a"}"#,
        r#"{"title":"x","list":{"label":"l","choices":[{"id":"a","text":"A"}]},"file":{"label":"f","accept":".json","max":5},"buttons":[{"id":"a","text":"A"}],"cancel":"a"}"#,
        // A list needs choices, with plain distinct ids.
        r#"{"title":"x","list":{"label":"l","choices":[]},"buttons":[{"id":"a","text":"A"}],"cancel":"a"}"#,
        r#"{"title":"x","list":{"label":"l","choices":[{"id":"A","text":"A"}]},"buttons":[{"id":"a","text":"A"}],"cancel":"a"}"#,
        r#"{"title":"x","list":{"label":"l","choices":[{"id":"a","text":"A"},{"id":"a","text":"B"}]},"buttons":[{"id":"a","text":"A"}],"cancel":"a"}"#,
        r#"{"title":"x","list":{"label":"l","choices":[{"id":"a","text":""}]},"buttons":[{"id":"a","text":"A"}],"cancel":"a"}"#,
        // A file is a .json of at most MAX_IMPORT_BYTES.
        r#"{"title":"x","file":{"label":"f","accept":".exe","max":5},"buttons":[{"id":"a","text":"A"}],"cancel":"a"}"#,
        too_big.as_str(),
    ] {
        assert!(parse(bad).is_err(), "{bad}");
    }
    let many: Vec<String> = (0..13)
        .map(|n| format!(r#"{{"id":"i{n}","text":"I"}}"#))
        .collect();
    assert!(
        parse(&format!(
            r#"{{"title":"x","list":{{"label":"l","choices":[{}]}},"buttons":[{{"id":"a","text":"A"}}],"cancel":"a"}}"#,
            many.join(",")
        ))
        .is_err()
    );
}

#[test]
fn a_pick_or_a_file_the_dialog_could_not_have_given_is_a_cancel() {
    let switch = parse(SWITCH).unwrap();
    let pick = |v: &str| accept(&switch, "cancel", Some(v.into()), None).value;
    assert_eq!(pick("b-2").as_deref(), Some("b-2"));
    assert_eq!(pick("zz"), None);
    let import = parse(IMPORT).unwrap();
    let file = |v: String| accept(&import, "import", Some(v), None);
    assert_eq!(file("{}".into()).button, "import");
    // A list well over the old 1 MiB is taken.
    assert_eq!(file(" ".repeat(3 << 20)).button, "import");
    assert_eq!(file("x".repeat(MAX_FILE + 1)).button, "cancel");
    // Text where the spec asked for none.
    let confirm = parse(r#"{"title":"x","buttons":[{"id":"a","text":"A"}],"cancel":"a"}"#).unwrap();
    assert_eq!(accept(&confirm, "a", Some("x".into()), None).value, None);
}

#[test]
fn the_window_is_as_tall_as_what_it_shows() {
    assert_eq!(height(&parse(SWITCH).unwrap()), 390.0);
    assert_eq!(height(&parse(IMPORT).unwrap()), 280.0);
    assert_eq!(height(&parse(RENAME).unwrap()), 260.0);
    assert_eq!(height(&parse(FOLDER).unwrap()), 400.0);
}

#[test]
fn only_the_dialog_page_may_be_shown_in_its_window() {
    assert!(dialog_url(&page_url()));
    let other = app_url().join("index.html").unwrap();
    assert!(!dialog_url(&other));
    assert!(!dialog_url(
        &Url::parse("https://example.com/dialog.html").unwrap()
    ));
    assert_ne!(LABEL, super::super::web_shell::RAIL);
}
