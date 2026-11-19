use super::*;
use std::sync::mpsc;

/// A throwaway directory removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "mnema-library-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, relative: &str, text: &str) {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn acf(name: &str, installdir: &str) -> String {
    format!("\"AppState\"\n{{\n\t\"name\"\t\"{name}\"\n\t\"installdir\"\t\"{installdir}\"\n}}\n")
}

#[test]
fn steam_scan_follows_libraryfolders_and_reads_every_manifest() {
    let steam = Dir::new("steam");
    let other = Dir::new("steam-other");
    let other_path = other.0.to_string_lossy().replace('\\', "\\\\");
    steam.write(
        "steamapps/libraryfolders.vdf",
        &format!(
            "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\"{other_path}\"\n\t}}\n}}\n"
        ),
    );
    steam.write("steamapps/appmanifest_1.acf", &acf("Alpha", "Alpha"));
    other.write("steamapps/appmanifest_2.acf", &acf("Beta", "Beta Game"));
    other.write(
        "steamapps/appmanifest_3.acf",
        &acf("Proton 9", "Proton 9.0"),
    );
    other.write("steamapps/appmanifest_4.acf", "garbage");
    other.write("steamapps/notes.txt", "ignored");
    let index = scan_all(&[Box::new(Steam {
        roots: vec![steam.0.clone(), steam.0.join("missing")],
    })]);
    assert_eq!(index.len(), 2);
    let alpha = steam.0.join("steamapps/common/Alpha/alpha.exe");
    let beta = other.0.join("steamapps/common/Beta Game/bin/beta.exe");
    assert_eq!(index.find(&alpha.to_string_lossy()), Some("Alpha"));
    assert_eq!(index.find(&beta.to_string_lossy()), Some("Beta"));
}

#[test]
fn epic_scan_reads_item_files_only() {
    let manifests = Dir::new("epic");
    manifests.write(
        "A.item",
        include_str!("../../fixtures/gaming/epic-example.item"),
    );
    manifests.write("B.txt", "{}");
    manifests.write("C.item", "not json");
    let games = Epic {
        manifests: manifests.0.clone(),
    }
    .scan();
    assert_eq!(games.len(), 1);
    assert_eq!(games[0].name, "Example Racer");
}

#[test]
fn oversized_manifests_are_ignored() {
    let steam = Dir::new("big");
    let padding = " ".repeat(MAX_MANIFEST_BYTES as usize);
    steam.write(
        "steamapps/appmanifest_1.acf",
        &format!("{}{padding}", acf("Big", "Big")),
    );
    assert!(
        Steam {
            roots: vec![steam.0.clone()]
        }
        .scan()
        .is_empty()
    );
}

struct Fake(Mutex<Vec<InstalledGame>>);

impl Launcher for Fake {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn scan(&self) -> Vec<InstalledGame> {
        self.0.lock().unwrap().clone()
    }
}

#[test]
fn refresher_scans_at_start_and_again_on_request_but_not_on_a_timer_tick() {
    let game = |n: &str, d: &str| InstalledGame::new(n, d).unwrap();
    let library = Arc::new(Library::default());
    assert!(library.current().is_empty());
    // A launcher whose answer changes between scans.
    struct Changing(mpsc::Sender<()>, Mutex<u32>);
    impl Launcher for Changing {
        fn name(&self) -> &'static str {
            "changing"
        }
        fn scan(&self) -> Vec<InstalledGame> {
            let mut count = self.1.lock().unwrap();
            *count += 1;
            let dir = format!("/games/g{count}");
            let _ = self.0.send(());
            vec![InstalledGame::new(&format!("G{count}"), &dir).unwrap()]
        }
    }
    let (tx, rx) = mpsc::channel();
    let handle = start_refresher(
        library.clone(),
        vec![
            Box::new(Changing(tx, Mutex::new(0))),
            Box::new(Fake(Mutex::new(vec![game("Fixed", "/games/fixed")]))),
        ],
        Duration::from_secs(3600),
    )
    .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // The first scan has finished once the index holds both launchers' games.
    for _ in 0..200 {
        if library.current().len() == 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(library.current().find("/games/g1/x.exe"), Some("G1"));
    assert_eq!(library.current().find("/games/fixed/x.exe"), Some("Fixed"));
    library.rescan_now();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    for _ in 0..200 {
        if library.current().find("/games/g2/x.exe").is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(library.current().find("/games/g2/x.exe"), Some("G2"));
    assert_eq!(library.current().find("/games/g1/x.exe"), None);
    library.shutdown();
    handle.join().unwrap();
}
