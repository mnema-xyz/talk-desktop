use super::steam::parse_vdf;
use super::*;

const LIBRARIES: &str = include_str!("../../fixtures/gaming/libraryfolders.vdf");
const APP: &str = include_str!("../../fixtures/gaming/appmanifest_220.acf");
const EPIC: &str = include_str!("../../fixtures/gaming/epic-example.item");

fn game(name: &str, dir: &str) -> InstalledGame {
    InstalledGame::new(name, dir).unwrap()
}

#[test]
fn libraryfolders_lists_every_library_in_both_formats() {
    assert_eq!(
        steam_libraries(LIBRARIES),
        vec![r"C:\Program Files (x86)\Steam", r"D:\SteamLibrary"]
    );
    let old = "\"LibraryFolders\"\n{\n\t\"TimeNextStatsReport\"\t\"1\"\n\t\"1\"\t\"E:\\\\Old\"\n}";
    assert_eq!(steam_libraries(old), vec![r"E:\Old"]);
    assert!(steam_libraries("not a manifest").is_empty());
    assert!(steam_libraries("\"libraryfolders\" {").is_empty());
}

#[test]
fn appmanifest_gives_name_and_installdir() {
    assert_eq!(
        steam_app(APP),
        Some(("Example Quest".to_owned(), "Example Quest".to_owned()))
    );
    let g = steam_game(r"D:\SteamLibrary\", "Example Quest", "Example Quest").unwrap();
    assert_eq!(
        g,
        game(
            "Example Quest",
            "d:/steamlibrary/steamapps/common/example quest"
        )
    );
}

#[test]
fn installdir_cannot_leave_the_common_folder_and_tools_are_not_games() {
    for bad in ["..", "../Windows", "a/b", "a\\\\b", "C:", "", "."] {
        let text = format!("\"AppState\" {{ \"name\" \"X\" \"installdir\" \"{bad}\" }}");
        assert!(steam_app(&text).is_none(), "{bad}");
    }
    for tool in [
        "Steamworks Shared",
        "SteamVR",
        "Proton 9.0",
        "Steam Linux Runtime 3.0 (sniper)",
        "Wallpaper Engine",
    ] {
        let text = format!("\"AppState\" {{ \"name\" \"X\" \"installdir\" \"{tool}\" }}");
        assert!(steam_app(&text).is_none(), "{tool}");
    }
}

#[test]
fn vdf_handles_escapes_comments_bom_and_rejects_garbage() {
    let text = "\u{feff}// header\n\"a\" { \"b\" \"c\\\\d\\\"e\" // note\n \"n\" { } }";
    let root = parse_vdf(text).unwrap();
    assert_eq!(
        root.get("a").unwrap().get("b").unwrap().as_str(),
        Some("c\\d\"e")
    );
    assert!(root.get("A").unwrap().get("n").is_some());
    for bad in ["\"a\" {", "}", "\"a\"", "\"a\" { \"b\" }", "\"unterminated"] {
        assert!(parse_vdf(bad).is_none(), "{bad}");
    }
    let deep = format!("{}{}", "\"k\" {".repeat(40), "}".repeat(40));
    assert!(parse_vdf(&deep).is_none());
}

#[test]
fn epic_manifest_gives_the_display_name_and_folder() {
    assert_eq!(
        epic_game(EPIC),
        Some(game("Example Racer", r"C:\Games\Epic\ExampleRacer"))
    );
}

#[test]
fn epic_addons_unfinished_installs_and_broken_files_are_skipped() {
    let addon = r#"{"DisplayName":"DLC","InstallLocation":"C:\\Games\\Epic\\Base","AppName":"dlc","MainGameAppName":"base"}"#;
    assert!(epic_game(addon).is_none());
    let same = r#"{"DisplayName":"Base","InstallLocation":"C:\\Games\\Epic\\Base","AppName":"base","MainGameAppName":"base"}"#;
    assert!(epic_game(same).is_some());
    let partial =
        r#"{"DisplayName":"P","InstallLocation":"C:\\Games\\Epic\\P","bIsIncompleteInstall":true}"#;
    assert!(epic_game(partial).is_none());
    assert!(epic_game("{").is_none());
    assert!(epic_game(r#"{"DisplayName":"No folder"}"#).is_none());
}

#[test]
fn broad_or_relative_folders_never_become_game_folders() {
    for dir in [
        "",
        "relative/game",
        "C:",
        r"C:\",
        r"C:\Games",
        r"C:\Program Files",
        r"C:\Program Files\Common",
        r"C:\Windows\System32",
        "/",
        "/Applications",
        "/Users/me",
        r"C:\Games\..\Windows\x",
        "D:\u{0}\\x\\y",
    ] {
        assert!(normalize_dir(dir).is_none(), "{dir:?}");
    }
    assert!(normalize_dir(r"C:\Games\Foo").is_some());
    assert!(
        normalize_dir("/Users/me/Library/Application Support/Steam/steamapps/common/Foo").is_some()
    );
    assert!(normalize_dir(r"D:\Program Files\Foo\Game").is_some());
}

#[test]
fn unc_device_relative_and_dotdot_paths_are_never_opened() {
    for bad in [
        r"\\attacker\share",
        r"\\attacker\share\steam",
        "//attacker/share",
        r"\\?\C:\Steam",
        r"\\.\pipe\x",
        r"\??\C:\Steam",
        r"\\?\UNC\attacker\share",
        "steam",
        r"Steam\lib",
        r"C:",
        r"C:Steam",
        r"C:\Steam\..\..\Windows",
        r"C:\Steam:stream",
        "C:\\Steam\u{0}x",
        "",
        "  ",
    ] {
        assert!(!safe_disk_path_for(bad, true), "{bad:?}");
        assert!(!safe_disk_path_for(bad, false), "{bad:?}");
    }
    assert!(safe_disk_path_for(r"D:\SteamLibrary", true));
    assert!(safe_disk_path_for("d:/SteamLibrary", true));
    assert!(!safe_disk_path_for("/home/me/steam", true), "no drive");
    assert!(safe_disk_path_for("/home/me/steam", false));
    assert!(!safe_disk_path_for("/home/../etc", false));
    // The folder normalizer refuses the same network and device roots.
    for bad in [
        r"\\host\share\Games\Foo",
        "//host/share/games/foo",
        r"\\?\C:\Games\Foo",
    ] {
        assert!(normalize_dir(bad).is_none(), "{bad}");
    }
    assert!(InstalledGame::new("X", r"\\host\share\Games\Foo").is_none());
    assert!(
        InstallIndex::new(vec![])
            .find(r"\\host\share\a\b.exe")
            .is_none()
    );
}

#[test]
fn every_path_reader_applies_the_same_root_rule() {
    // Drive-relative, device-prefixed and stream paths are refused by all three.
    for bad in [
        r"D:Games\Foo",
        r"c:\a:stream\b",
        "/??/c:/games/foo",
        r"C:Games\Foo\bin\x.exe",
    ] {
        assert!(normalize_dir(bad).is_none(), "{bad}");
        assert!(normalize_image(bad).is_none(), "{bad}");
        assert!(!safe_disk_path_for(bad, true), "{bad}");
        assert!(!safe_disk_path_for(bad, false), "{bad}");
    }
    assert!(InstalledGame::new("X", r"D:Games\Foo").is_none());
    assert_eq!(
        normalize_dir(r"D:\Games\Foo\").as_deref(),
        Some("d:/games/foo")
    );
    assert_eq!(
        normalize_image(r"D:\Games\.\Foo\x.exe").as_deref(),
        Some("d:/games/foo/x.exe")
    );
    assert_eq!(
        normalize_dir("/mnt/games/foo").as_deref(),
        Some("/mnt/games/foo")
    );
}

#[test]
fn index_matches_whole_components_case_insensitively_and_prefers_the_deepest() {
    let index = InstallIndex::new(vec![
        game("Outer", r"D:\Games\Suite"),
        game("Inner", r"D:\Games\Suite\Inner"),
        game("Foo", r"D:\Games\Foo"),
    ]);
    assert_eq!(index.len(), 3);
    assert_eq!(index.find(r"d:\games\FOO\bin\Foo.exe"), Some("Foo"));
    assert_eq!(index.find(r"D:\Games\Foobar\x.exe"), None);
    assert_eq!(index.find(r"D:\Games\Foo"), None);
    assert_eq!(index.find(r"D:\Games\Suite\Inner\a.exe"), Some("Inner"));
    assert_eq!(index.find(r"D:\Games\Suite\other.exe"), Some("Outer"));
    assert_eq!(index.find(r"D:\Games\Foo\..\Other\x.exe"), None);
    assert_eq!(index.find(""), None);
    assert_eq!(index.find("foo.exe"), None);
    assert!(InstallIndex::default().is_empty());
}
