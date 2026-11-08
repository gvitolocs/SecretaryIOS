//! Sync between two folders, with the server played by a second [`Folder`] behind the [`Remote`] trait, as the lucina-rs
//! routes do it.
use crate::sync::*;
use crate::sync_rules::Act;
use crate::Error;
use std::fs;
use std::path::Path;

/// The server: a folder, with an optional hook that runs before each put (to change the file under the run's feet).
struct Server {
    folder: Folder,
    before_put: Option<Box<dyn FnMut(&Path)>>,
    gets: usize,
}

impl Server {
    fn new(root: &Path) -> Server {
        Server { folder: Folder::open(root).unwrap(), before_put: None, gets: 0 }
    }
}

fn rerr(e: Error) -> RemoteError {
    match e {
        Error::Conflict { .. } => RemoteError::Conflict,
        e => RemoteError::Other(e.to_string()),
    }
}

impl Remote for Server {
    fn manifest(&mut self) -> Result<Manifest, RemoteError> {
        self.folder.manifest().map_err(rerr)
    }
    fn get(&mut self, rel: &str) -> Result<(Vec<u8>, String), RemoteError> {
        self.gets += 1;
        self.folder.read(rel).map_err(rerr)
    }
    fn put(&mut self, rel: &str, bytes: &[u8], expect: Option<&str>) -> Result<String, RemoteError> {
        if let Some(h) = self.before_put.as_mut() {
            h(self.folder.root());
        }
        self.folder.put(rel, bytes, expect, false).map_err(rerr)
    }
    fn trash(&mut self, rel: &str, expect: &str) -> Result<(), RemoteError> {
        self.folder.trash(rel, expect).map(|_| ()).map_err(rerr)
    }
}

fn opts() -> Options {
    Options { device: "juno".into(), date: "2026-11-08".into(), max_size: DEFAULT_MAX_SIZE }
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn read(root: &Path, rel: &str) -> Option<String> {
    fs::read_to_string(root.join(rel)).ok()
}

struct Pair {
    _d: tempfile::TempDir,
    here: std::path::PathBuf,
    there: std::path::PathBuf,
    state: State,
}

impl Pair {
    fn new() -> Pair {
        let d = tempfile::tempdir().unwrap();
        let (here, there) = (d.path().join("here"), d.path().join("there"));
        fs::create_dir_all(&here).unwrap();
        fs::create_dir_all(&there).unwrap();
        Pair { _d: d, here, there, state: State::default() }
    }
    fn run_with(&mut self, server: &mut Server) -> Report {
        let folder = Folder::open(&self.here).unwrap();
        run(&folder, server, &mut self.state, &opts(), &mut |_| {}).unwrap()
    }
    fn run(&mut self) -> Report {
        let mut s = Server::new(&self.there);
        self.run_with(&mut s)
    }
}

#[test]
fn first_sync_pulls_everything_then_nothing_to_do() {
    let mut p = Pair::new();
    write(&p.there, "00 Home.md", "home");
    write(&p.there, "A/Permanent/Claim.md", "claim");
    write(&p.there, "A/pic.png", "\u{89}PNG");
    write(&p.there, ".obsidian/app.json", "{}");
    write(&p.there, ".obsidian/plugins/x/data.json", "{}");
    write(&p.there, ".trash/old.md", "old");
    write(&p.there, ".stversions/a.md", "v");
    write(&p.there, "A/.lucina-vault-1-2.tmp", "tmp");
    let r = p.run();
    assert_eq!(r.pulled, 4, "{r:?}");
    assert_eq!(read(&p.here, "A/Permanent/Claim.md").as_deref(), Some("claim"));
    assert_eq!(read(&p.here, ".obsidian/app.json").as_deref(), Some("{}"));
    for skipped in [".obsidian/plugins/x/data.json", ".trash/old.md", ".stversions/a.md", "A/.lucina-vault-1-2.tmp"] {
        assert_eq!(read(&p.here, skipped), None, "{skipped}");
    }
    assert_eq!(p.state.base.len(), 4);
    let again = p.run();
    assert!(!again.changed(), "{again:?}");
}

#[test]
fn edits_go_each_way() {
    let mut p = Pair::new();
    write(&p.there, "a.md", "1");
    write(&p.there, "b.md", "1");
    p.run();
    write(&p.here, "a.md", "here");
    write(&p.there, "b.md", "there");
    write(&p.here, "new here.md", "n");
    let r = p.run();
    assert_eq!((r.pushed, r.pulled), (2, 1), "{r:?}");
    assert_eq!(read(&p.there, "a.md").as_deref(), Some("here"));
    assert_eq!(read(&p.here, "b.md").as_deref(), Some("there"));
    assert_eq!(read(&p.there, "new here.md").as_deref(), Some("n"));
    assert!(!p.run().changed());
}

#[test]
fn both_edited_keeps_both_and_sends_the_copy() {
    let mut p = Pair::new();
    write(&p.there, "Note.md", "base");
    p.run();
    write(&p.here, "Note.md", "mine");
    write(&p.there, "Note.md", "theirs");
    let r = p.run();
    assert_eq!(r.conflicts, vec!["Note (conflict 2026-11-08 juno).md".to_owned()]);
    for root in [&p.here, &p.there] {
        assert_eq!(read(root, "Note.md").as_deref(), Some("theirs"));
        assert_eq!(read(root, "Note (conflict 2026-11-08 juno).md").as_deref(), Some("mine"));
    }
    assert!(!p.run().changed());
}

#[test]
fn a_second_conflict_the_same_day_gets_its_own_name() {
    let mut p = Pair::new();
    write(&p.there, "Note.md", "base");
    p.run();
    for (mine, theirs) in [("m1", "t1"), ("m2", "t2")] {
        write(&p.here, "Note.md", mine);
        write(&p.there, "Note.md", theirs);
        p.run();
    }
    assert_eq!(read(&p.there, "Note (conflict 2026-11-08 juno).md").as_deref(), Some("m1"));
    assert_eq!(read(&p.there, "Note (conflict 2026-11-08 juno 2).md").as_deref(), Some("m2"));
}

#[test]
fn deletions_go_to_the_trash_and_yield_to_edits() {
    let mut p = Pair::new();
    for f in ["gone here.md", "gone there.md", "deleted here edited there.md", "deleted there edited here.md"] {
        write(&p.there, f, "1");
    }
    p.run();
    fs::remove_file(p.here.join("gone here.md")).unwrap();
    fs::remove_file(p.there.join("gone there.md")).unwrap();
    fs::remove_file(p.here.join("deleted here edited there.md")).unwrap();
    write(&p.there, "deleted here edited there.md", "edit");
    fs::remove_file(p.there.join("deleted there edited here.md")).unwrap();
    write(&p.here, "deleted there edited here.md", "edit");
    let r = p.run();
    assert_eq!((r.trashed_here, r.trashed_there), (1, 1), "{r:?}");
    assert_eq!(read(&p.there, "gone here.md"), None);
    assert_eq!(read(&p.there, ".trash/gone here.md").as_deref(), Some("1"));
    assert_eq!(read(&p.here, "gone there.md"), None);
    assert_eq!(read(&p.here, ".trash/gone there.md").as_deref(), Some("1"));
    assert_eq!(read(&p.here, "deleted here edited there.md").as_deref(), Some("edit"));
    assert_eq!(read(&p.there, "deleted there edited here.md").as_deref(), Some("edit"));
    assert!(!p.run().changed());
}

#[test]
fn a_change_on_the_server_during_the_run_is_never_overwritten() {
    let mut p = Pair::new();
    write(&p.there, "a.md", "1");
    p.run();
    write(&p.here, "a.md", "mine");
    let mut s = Server::new(&p.there);
    let mut once = true;
    s.before_put = Some(Box::new(move |root: &Path| {
        if once {
            fs::write(root.join("a.md"), "someone else").unwrap();
            once = false;
        }
    }));
    let r = p.run_with(&mut s);
    // Round one is refused by the server; round two sees both edits and keeps both.
    assert!(r.deferred >= 1, "{r:?}");
    assert_eq!(read(&p.there, "a.md").as_deref(), Some("someone else"));
    assert_eq!(read(&p.there, "a (conflict 2026-11-08 juno).md").as_deref(), Some("mine"));
    assert_eq!(read(&p.here, "a.md").as_deref(), Some("someone else"));
}

#[test]
fn settings_only_come_from_the_server() {
    let mut p = Pair::new();
    write(&p.there, ".obsidian/app.json", "{\"a\":1}");
    p.run();
    write(&p.here, ".obsidian/app.json", "{\"mine\":1}");
    write(&p.here, ".obsidian/hotkeys.json", "{}");
    let r = p.run();
    assert_eq!(r.pushed, 0, "{r:?}");
    assert_eq!(read(&p.there, ".obsidian/app.json").as_deref(), Some("{\"a\":1}"));
    assert_eq!(read(&p.there, ".obsidian/hotkeys.json"), None);
    assert_eq!(read(&p.here, ".obsidian/app.json").as_deref(), Some("{\"a\":1}"));
}

#[test]
fn large_files_are_left_out() {
    let mut p = Pair::new();
    write(&p.there, "big.pdf", &"x".repeat(100));
    let folder = Folder::open(&p.here).unwrap();
    let mut s = Server::new(&p.there);
    let o = Options { max_size: 10, ..opts() };
    let r = run(&folder, &mut s, &mut p.state, &o, &mut |_| {}).unwrap();
    assert_eq!((r.pulled, r.large), (0, 1));
}

#[test]
fn compare_and_swap_on_the_folder() {
    let d = tempfile::tempdir().unwrap();
    let f = Folder::open(d.path()).unwrap();
    let r1 = f.put("x/a.md", b"1", None, false).unwrap();
    assert!(matches!(f.put("x/a.md", b"2", None, false), Err(Error::Conflict { .. })));
    assert!(matches!(f.put("x/a.md", b"2", Some("nope"), false), Err(Error::Conflict { .. })));
    let r2 = f.put("x/a.md", b"2", Some(&r1), false).unwrap();
    assert!(matches!(f.trash("x/a.md", &r1), Err(Error::Conflict { .. })));
    assert_eq!(f.trash("x/a.md", &r2).unwrap(), ".trash/x/a.md");
    for bad in ["../a.md", ".obsidian/app.json", ".trash/a.md", "a//b.md", "/a.md"] {
        assert!(matches!(f.put(bad, b"1", None, false), Err(Error::Invalid(_))), "{bad}");
    }
    assert!(f.put(".obsidian/app.json", b"{}", None, true).is_ok());
    // No temporary file is left behind.
    let left: Vec<_> = fs::read_dir(d.path().join("x")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert!(left.is_empty(), "{left:?}");
}

#[cfg(unix)]
#[test]
fn symlinks_are_never_followed() {
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.md"), "s").unwrap();
    std::os::unix::fs::symlink(outside.path(), d.path().join("link")).unwrap();
    let f = Folder::open(d.path()).unwrap();
    assert!(f.manifest().unwrap().files.is_empty());
    assert!(f.read("link/secret.md").is_err());
    assert!(f.put("link/new.md", b"1", None, false).is_err());
    assert!(!outside.path().join("new.md").exists());
}

#[test]
fn a_missing_root_is_an_error_not_an_empty_vault() {
    let d = tempfile::tempdir().unwrap();
    assert!(Folder::open(d.path().join("not mounted")).is_err());
}

#[test]
fn state_round_trips_and_a_damaged_one_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("lucina/vault/sync.json");
    assert_eq!(State::load(&path).unwrap(), State::default());
    let mut s = State::default();
    s.base.insert("a.md".into(), "r".into());
    s.save(&path).unwrap();
    assert_eq!(State::load(&path).unwrap(), s);
    fs::write(&path, "{").unwrap();
    assert!(State::load(&path).is_err());
}

#[test]
fn the_cache_spares_rehashing_but_sees_changes() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "a.md", "1");
    let f = Folder::open(d.path()).unwrap();
    let m1 = f.manifest().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(d.path(), "a.md", "2");
    let m2 = f.manifest().unwrap();
    assert_ne!(m1.files[0].rev, m2.files[0].rev);
}

#[test]
fn plan_matches_the_rules_table() {
    let e = |p: &str, r: &str| Entry { path: p.into(), size: 1, rev: r.into() };
    let local = Manifest { files: vec![e("a.md", "1"), e("b.md", "2"), e(".obsidian/app.json", "x")] };
    let remote = Manifest { files: vec![e("a.md", "1"), e("b.md", "3"), e(".obsidian/app.json", "y")] };
    let base = [("b.md".to_owned(), "2".to_owned()), ("c.md".to_owned(), "9".to_owned())].into_iter().collect();
    let (items, _) = plan(&local, &remote, &base, DEFAULT_MAX_SIZE);
    let acts: Vec<(&str, Act)> = items.iter().map(|i| (i.path.as_str(), i.act)).collect();
    assert_eq!(acts, vec![(".obsidian/app.json", Act::Pull), ("a.md", Act::Record), ("b.md", Act::Pull), ("c.md", Act::Nothing)]);
}
