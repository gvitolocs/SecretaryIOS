//! The conflict and write policy, on throwaway copies. `renames_match_real_obsidian`
//! holds link updates against `tests/fixture/rename-golden.json`, made by real Obsidian.
use lucina_vault::{protocol::Session, Error, Options, Vault};
use serde_json::{json, Value};
use std::{fs, path::Path};

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}
fn fixture_copy() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture/vault"),
        t.path(),
    );
    t
}
fn open(t: &tempfile::TempDir) -> Vault {
    Vault::open(
        t.path(),
        Options {
            writable: true,
            ..Default::default()
        },
    )
    .unwrap()
}
/// Every file in the tree with its bytes, ignoring nothing, for "nothing else moved".
fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                walk(root, &e.path(), out);
            } else {
                let rel = e.path().strip_prefix(root).unwrap().display().to_string();
                out.push((rel, fs::read(e.path()).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn read_only_is_the_default_and_refuses_everything() {
    let t = fixture_copy();
    let before = snapshot(t.path());
    let mut v = Vault::open(t.path(), Options::default()).unwrap();
    let rev = v.read("Alpha.md").unwrap().revision;
    assert!(matches!(
        v.save("Alpha.md", "x", &rev),
        Err(Error::ReadOnly)
    ));
    assert!(matches!(v.create("New.md", b"x"), Err(Error::ReadOnly)));
    assert!(matches!(v.trash("Alpha.md"), Err(Error::ReadOnly)));
    assert!(matches!(
        v.save_conflict_copy("Alpha.md", "x", "2026-11-02", "phone"),
        Err(Error::ReadOnly)
    ));
    let plan = v.plan_rename("Alpha.md", "B.md").unwrap();
    assert!(matches!(v.rename(&plan), Err(Error::ReadOnly)));
    assert_eq!(snapshot(t.path()), before, "a read-only vault changed");
}

#[test]
fn save_is_compare_and_swap_and_a_conflict_writes_nothing() {
    let t = fixture_copy();
    let mut v = open(&t);
    let first = v.read("Alpha.md").unwrap();
    let r2 = v.save("Alpha.md", "mine\n", &first.revision).unwrap();
    assert_eq!(v.read("Alpha.md").unwrap().text, "mine\n");
    // Someone else (real Obsidian, Syncthing) writes the file now.
    fs::write(t.path().join("Alpha.md"), "theirs\n").unwrap();
    let before = snapshot(t.path());
    match v.save("Alpha.md", "mine again\n", &r2) {
        Err(Error::Conflict { path, current }) => {
            assert_eq!(path, "Alpha.md");
            assert_eq!(current, v.read("Alpha.md").unwrap().revision);
        }
        o => panic!("{o:?}"),
    }
    assert_eq!(snapshot(t.path()), before, "a conflict changed something");
    // Neither side loses: theirs stays, mine becomes a new note Obsidian can open.
    let copy = v
        .save_conflict_copy("Alpha.md", "mine again\n", "2026-11-02", "phone 1")
        .unwrap();
    assert_eq!(copy, "Alpha (conflict 2026-11-02 phone1).md");
    assert_eq!(
        fs::read_to_string(t.path().join(&copy)).unwrap(),
        "mine again\n"
    );
    assert_eq!(
        fs::read_to_string(t.path().join("Alpha.md")).unwrap(),
        "theirs\n"
    );
    let again = v
        .save_conflict_copy("Alpha.md", "third\n", "2026-11-02", "phone 1")
        .unwrap();
    assert_eq!(again, "Alpha (conflict 2026-11-02 phone1 2).md");
    assert!(v.note(&copy).is_some());
}

#[test]
fn saves_keep_every_byte_and_leave_no_temporary_files() {
    let t = fixture_copy();
    let mut v = open(&t);
    for name in ["CRLF.md", "BOM.md", "Empty.md"] {
        let n = v.read(name).unwrap();
        let bytes = fs::read(t.path().join(name)).unwrap();
        v.save(name, &n.text, &n.revision).unwrap();
        assert_eq!(fs::read(t.path().join(name)).unwrap(), bytes, "{name}");
    }
    assert!(!snapshot(t.path())
        .iter()
        .any(|(p, _)| p.contains(".lucina-vault-")));
}

#[test]
fn a_failed_write_leaves_the_old_file() {
    let t = fixture_copy();
    let mut v = open(&t);
    let r = v.read("Alpha.md").unwrap();
    let text = r.text.clone();
    let path = t.path().join("Alpha.md");
    // The folder refuses the temporary file.
    use std::os::unix::fs::PermissionsExt;
    let root_mode = fs::metadata(t.path()).unwrap().permissions();
    fs::set_permissions(t.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let out = v.save("Alpha.md", "new", &r.revision);
    fs::set_permissions(t.path(), root_mode).unwrap();
    assert!(out.is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), text);
}

#[test]
fn nothing_leaves_the_vault_and_settings_stay_alone() {
    let t = fixture_copy();
    let mut v = open(&t);
    for bad in [
        "../x.md",
        ".obsidian/app.json",
        "a/../../x.md",
        "/etc/x.md",
        ".trash/x.md",
    ] {
        assert!(
            matches!(v.create(bad, b"x"), Err(Error::Invalid(_))),
            "{bad}"
        );
    }
    // A symlink on the way is refused even for a name that looks fine.
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), t.path().join("Link")).unwrap();
    assert!(matches!(
        v.create("Link/x.md", b"x"),
        Err(Error::Invalid(_))
    ));
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    v.refresh().unwrap();
    assert!(v.warnings().iter().any(|w| w.contains("Link")));
}

#[test]
fn create_refuses_a_taken_name_and_trash_only_moves() {
    let t = fixture_copy();
    let mut v = open(&t);
    assert!(matches!(v.create("Alpha.md", b"x"), Err(Error::Exists(_))));
    assert_eq!(v.new_note_path("Alpha", ""), "Project/Events/Alpha.md");
    let bytes = fs::read(t.path().join("Dup.md")).unwrap();
    let dest = v.trash("Dup.md").unwrap();
    assert_eq!(dest, ".trash/Dup.md");
    assert_eq!(fs::read(t.path().join(&dest)).unwrap(), bytes);
    assert!(v.note("Dup.md").is_none());
    assert_eq!(
        v.new_attachment_path("pic.png", "Project/x.md"),
        "Evidence/pic 1.png"
    );
}

#[test]
fn a_rename_stops_at_a_note_changed_after_the_plan() {
    let t = fixture_copy();
    let mut v = open(&t);
    let plan = v.plan_rename("Dup.md", "Dup moved.md").unwrap();
    assert!(!plan.edits.is_empty());
    let victim = plan.edits[0].0.clone();
    fs::write(t.path().join(&victim), "changed meanwhile\n").unwrap();
    assert!(matches!(v.rename(&plan), Err(Error::Conflict { .. })));
    assert_eq!(
        fs::read_to_string(t.path().join(&victim)).unwrap(),
        "changed meanwhile\n"
    );
}

#[test]
fn renames_match_real_obsidian() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture");
    let g: Value =
        serde_json::from_slice(&fs::read(dir.join("rename-golden.json")).unwrap()).unwrap();
    assert_eq!(g["obsidian"], "1.15.0");
    let t = fixture_copy();
    let mut v = open(&t);
    for step in g["steps"].as_array().unwrap() {
        let (from, to) = (step[0].as_str().unwrap(), step[1].as_str().unwrap());
        let plan = v.plan_rename(from, to).unwrap();
        v.rename(&plan).unwrap();
    }
    // Obsidian also rewrites a note's YAML when it touches a link there, and drops a BOM.
    // We deliberately change only the link: compare the bodies, and the links in the YAML.
    fn parts(text: &str) -> (Vec<String>, String) {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let Some(rest) = text.strip_prefix("---\n") else {
            return (Vec::new(), text.to_owned());
        };
        let Some(end) = rest.find("\n---\n") else {
            return (Vec::new(), text.to_owned());
        };
        let links = rest[..end]
            .split("\"[[")
            .skip(1)
            .filter_map(|p| p.split_once("]]\"").map(|(l, _)| l.to_owned()))
            .collect();
        (links, rest[end + 5..].to_owned())
    }
    let mut wrong = Vec::new();
    let theirs = g["notes"].as_object().unwrap();
    for (path, text) in theirs {
        let ours = fs::read_to_string(t.path().join(path)).unwrap_or_else(|_| "<missing>".into());
        let (ours_links, ours_body) = parts(&ours);
        let (their_links, their_body) = parts(text.as_str().unwrap());
        if ours_links != their_links {
            wrong.push(format!(
                "{path}: YAML links\n  ours   {ours_links:?}\n  theirs {their_links:?}"
            ));
        }
        if ours_body != their_body {
            let a: Vec<&str> = ours_body.lines().collect();
            let b: Vec<&str> = their_body.lines().collect();
            for (i, (x, y)) in a.iter().zip(&b).enumerate() {
                if x != y {
                    wrong.push(format!("{path}:{i}\n  ours   {x}\n  theirs {y}"));
                }
            }
            if a.len() != b.len() {
                wrong.push(format!("{path}: {} lines vs {}", a.len(), b.len()));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} lines differ
:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    assert_eq!(v.notes().count(), theirs.len());
}

#[test]
fn the_protocol_answers_and_reports_failures() {
    let t = fixture_copy();
    let mut s = Session::open(t.path().to_str().unwrap()).unwrap();
    let ask = |s: &mut Session, q: Value| -> Value {
        serde_json::from_str(&s.request(&q.to_string())).unwrap()
    };
    let st = ask(&mut s, json!({"op": "status"}));
    assert_eq!(st["ok"], true);
    assert_eq!(st["writable"], false);
    assert!(st["counts"]["notes"].as_u64().unwrap() > 30);
    let l = ask(&mut s, json!({"op": "list", "folder": "Folder A"}));
    assert_eq!(l["folders"], json!(["Folder A/Inner"]));
    assert_eq!(l["notes"][0]["name"], "Dup");
    let n = ask(&mut s, json!({"op": "note", "path": "Alpha.md"}));
    assert_eq!(n["name"], "Alpha");
    assert!(n["backlinks"].as_array().unwrap().len() > 5);
    assert_eq!(n["blocks"][1]["type"], "heading");
    let rev = n["revision"].as_str().unwrap().to_owned();
    let w = ask(
        &mut s,
        json!({"op": "save", "path": "Alpha.md", "text": "x", "revision": rev}),
    );
    assert_eq!(w["code"], "read_only");
    ask(&mut s, json!({"op": "set_writable", "writable": true}));
    let w = ask(
        &mut s,
        json!({"op": "save", "path": "Alpha.md", "text": "x", "revision": "stale"}),
    );
    assert_eq!(w["code"], "conflict");
    assert_eq!(w["current"], rev);
    assert_eq!(
        ask(
            &mut s,
            json!({"op": "save", "path": "Alpha.md", "text": "x", "revision": rev})
        )["ok"],
        true
    );
    let c = ask(&mut s, json!({"op": "create", "name": "Fresh"}));
    assert_eq!(c["path"], "Project/Events/Fresh.md");
    let h = ask(&mut s, json!({"op": "search", "query": "tag:hub"}));
    assert_eq!(h["hits"].as_array().unwrap().len(), 2);
    let q = ask(&mut s, json!({"op": "quick_switch", "query": "Start"}));
    assert_eq!(q["hits"][0]["path"], "00 Home.md");
    let r = ask(
        &mut s,
        json!({"op": "resolve", "from": "00 Home.md", "link": "pic.png#x"}),
    );
    assert_eq!(r["dest"], "Evidence/pic.png");
    assert_eq!(r["isNote"], false);
    let f = ask(
        &mut s,
        json!({"op": "file_path", "path": "Evidence/pic.png"}),
    );
    assert!(f["absolute"]
        .as_str()
        .unwrap()
        .ends_with("Evidence/pic.png"));
    assert_eq!(
        ask(
            &mut s,
            json!({"op": "file_path", "path": ".obsidian/app.json"})
        )["code"],
        "not_found"
    );
    assert_eq!(ask(&mut s, json!({"op": "nope"}))["code"], "invalid");
    assert_eq!(s.request("{bad json")[..12], *"{\"code\":\"inv");
    let g = ask(
        &mut s,
        json!({"op": "graph", "around": "Beta.md", "depth": 1}),
    );
    assert!(g["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["path"] == "Beta.md"));
}

#[test]
fn search_finds_words_phrases_and_filters_on_the_fixture() {
    let t = fixture_copy();
    let v = open(&t);
    let names = |q: &str| -> Vec<String> { v.search(q, 50).into_iter().map(|h| h.path).collect() };
    assert!(names("paragraph block").contains(&"Alpha.md".to_owned()));
    assert_eq!(names("\"Paragraph with a block\""), vec!["Alpha.md"]);
    assert!(names("cafe").contains(&"Café.md".to_owned()));
    assert!(!names("section -block").contains(&"Alpha.md".to_owned()));
    assert_eq!(names("path:\"Folder B/Sub\""), vec!["Folder B/Sub/Deep.md"]);
    assert_eq!(names("file:Café"), vec!["Café.md"]);
    assert!(names("zzzznothing").is_empty());
    let hits = v.search("block-1", 5);
    let hit = hits.iter().find(|h| h.path == "Alpha.md").unwrap();
    assert_eq!(hit.lines[0].0, 4);
    let mut n = 0;
    assert!(
        v.search_with("the", 5, |_| {
            n += 1;
            n < 2
        })
        .len()
            <= 1
    );
}

#[test]
fn backlinks_tags_and_graph_on_the_fixture() {
    let t = fixture_copy();
    let v = open(&t);
    let b = v.backlinks("Beta.md");
    assert!(b
        .iter()
        .any(|l| l.from == "00 Home.md" && l.line == 0 && l.original == "[[Beta]]"));
    assert!(!v.backlinks("Alpha.md").iter().any(|l| l.from == "Alpha.md"));
    assert!(v.unresolved().iter().any(|(p, _)| p == "Missing note"));
    assert_eq!(v.notes_with_tag("HUB").len(), 2);
    assert!(v
        .notes_with_tag("method")
        .iter()
        .any(|n| n.path == "00 Home.md"));
    assert!(v.folders().contains(&"Folder A/Inner".to_owned()));
    let g = v.graph(&Default::default());
    assert!(
        !g.nodes.iter().any(|n| n.path == "Empty.md"),
        "orphans are off by default"
    );
    let all = v.graph(&lucina_vault::graph::GraphOptions {
        with_orphans: true,
        ..Default::default()
    });
    assert!(all.nodes.iter().any(|n| n.path == "Empty.md"));
    assert_eq!(all.nodes.len(), v.notes().count());
    let local = v.graph(&lucina_vault::graph::GraphOptions {
        around: Some("Folder A/Twin.md".into()),
        depth: 1,
        ..Default::default()
    });
    assert!(local.nodes.len() < 12);
    assert!(local.nodes.iter().all(|n| n.x.is_finite()));
}
