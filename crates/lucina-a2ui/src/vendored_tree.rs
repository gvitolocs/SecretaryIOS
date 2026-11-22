//! The vendored A2UI oracle is what upstream published, unchanged, and is recorded as such.
//!
//! These tests only read files; they never write one. The rules are plain functions over text and
//! over the entries of the tree, so the checks themselves have tests.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// The line naming the registry block this port reads its pin and licence from.
const A2UI_ID: &str = r#"id = "a2ui""#;

fn root() -> &'static Path {
    Path::new(ROOT)
}

fn upstream_dir() -> PathBuf {
    root().join("third_party/a2ui/upstream")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Regular,
    Directory,
    Symlink,
    Other,
}

fn kind_of(file_type: &fs::FileType) -> Kind {
    if file_type.is_symlink() {
        Kind::Symlink
    } else if file_type.is_dir() {
        Kind::Directory
    } else if file_type.is_file() {
        Kind::Regular
    } else {
        Kind::Other
    }
}

/// One entry of the vendored tree: its `./`-relative path, what it is and, for a directory, how
/// many entries it holds.
#[derive(Clone, PartialEq, Debug)]
struct Entry {
    path: String,
    kind: Kind,
    children: usize,
}

impl Entry {
    /// Why this entry does not belong in a vendored tree, or `None` when it does.
    fn offence(&self) -> Option<&'static str> {
        if self.kind == Kind::Symlink {
            return Some("a symlink");
        }
        if self.kind == Kind::Other {
            return Some("neither a regular file nor a directory");
        }
        if self.kind == Kind::Directory && self.children == 0 {
            return Some("an empty directory");
        }
        if self.path.ends_with(".py") {
            return Some("Python");
        }
        None
    }
}

/// Every entry under `dir`, directories included, sorted by path.
fn walk(dir: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    collect(dir, ".", &mut entries);
    entries.sort_by(|one, other| one.path.cmp(&other.path));
    entries
}

fn collect(dir: &Path, path: &str, out: &mut Vec<Entry>) {
    let read =
        fs::read_dir(dir).unwrap_or_else(|error| panic!("cannot list {}: {error}", dir.display()));
    let mut children = Vec::new();
    for entry in read {
        let entry = entry.unwrap_or_else(|error| panic!("cannot list {path}: {error}"));
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|error| panic!("cannot stat {path}/{name}: {error}"));
        children.push((format!("{path}/{name}"), kind_of(&file_type), entry.path()));
    }
    if children.is_empty() {
        if path != "." {
            out.push(Entry {
                path: path.to_string(),
                kind: Kind::Directory,
                children: 0,
            });
        }
        return;
    }
    for (child, kind, child_path) in &children {
        out.push(Entry {
            path: child.clone(),
            kind: *kind,
            children: children.len(),
        });
        if *kind == Kind::Directory {
            collect(child_path, child, out);
        }
    }
}

/// The entries that are not plain regular files: `path: why`, in the order of the tree.
fn offences(tree: &[Entry]) -> Vec<String> {
    tree.iter()
        .filter_map(|entry| entry.offence().map(|why| format!("{}: {why}", entry.path)))
        .collect()
}

fn entry(path: &str, kind: Kind, children: usize) -> Entry {
    Entry {
        path: path.to_string(),
        kind,
        children,
    }
}

/// The five outcomes of one directory entry, as the walker reads it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Shape {
    /// A regular file: what the copy is made of.
    Regular,
    /// A directory holding entries: walked into.
    Branch,
    /// A directory holding nothing: it carries no case.
    Empty,
    /// A link: it points outside the copy.
    Link,
    /// A fifo, a socket, a device: not a file at all.
    Strange,
}

/// What the file system reports for one name: whether it is a symlink, a directory, a regular
/// file and, for a directory, how many entries it holds.
#[derive(Clone, Debug)]
struct Found {
    name: String,
    symlink: bool,
    directory: bool,
    file: bool,
    children: usize,
}

fn found(name: &str, symlink: bool, directory: bool, file: bool, children: usize) -> Found {
    Found {
        name: name.to_string(),
        symlink,
        directory,
        file,
        children,
    }
}

/// What one entry is, from what the file system reports and from the number it carries.
fn shape_of(symlink: bool, directory: bool, file: bool, children: usize) -> Shape {
    if symlink {
        Shape::Link
    } else if directory {
        if children == 0 {
            Shape::Empty
        } else {
            Shape::Branch
        }
    } else if file {
        Shape::Regular
    } else {
        Shape::Strange
    }
}

/// Why a shape does not belong in a vendored tree, or `None` when it does.
fn why(shape: Shape) -> Option<&'static str> {
    match shape {
        Shape::Regular | Shape::Branch => None,
        Shape::Link => Some("a symlink"),
        Shape::Empty => Some("an empty directory"),
        Shape::Strange => Some("neither a regular file nor a directory"),
    }
}

/// One listing of `parent`, as the walker records it today: every name gets the shape the
/// listing's own length gives it, and a directory that holds nothing is recorded again as itself.
fn listed(parent: &str, listing: &[Found]) -> Vec<(String, Shape)> {
    let mut out = Vec::new();
    if listing.is_empty() {
        if parent != "." {
            out.push((parent.to_string(), Shape::Empty));
        }
        return out;
    }
    for item in listing {
        let shape = shape_of(item.symlink, item.directory, item.file, listing.len());
        out.push((format!("{parent}/{}", item.name), shape));
    }
    out
}

/// Whether this line is the block's `pin_date = "YYYY-MM-DD"`. The checks do not read it today.
fn is_pin_date(line: &str) -> bool {
    let _ = line;
    false
}

/// How many files the vendored copy holds. A pin move changes this together with
/// `UPSTREAM_COMMIT`.
const VENDORED_FILES: usize = 415;

fn sha256_of(path: &Path) -> String {
    let mut file =
        File::open(path).unwrap_or_else(|error| panic!("cannot open {}: {error}", path.display()));
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

/// `MANIFEST.sha256`: the lines it holds and the hash of every path.
#[derive(PartialEq, Debug)]
struct Manifest {
    lines: usize,
    hashes: BTreeMap<String, String>,
}

/// Reads `sha256sum` output: `<64 hex><two spaces>./<path>`, one line per file.
fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let mut manifest = Manifest {
        lines: 0,
        hashes: BTreeMap::new(),
    };
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        manifest.lines += 1;
        let Some((hash, path)) = line.split_once("  ") else {
            return Err(format!("not a sha256sum line: {line}"));
        };
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("not a sha256 hash: {line}"));
        }
        if let Some(before) = manifest.hashes.insert(path.to_string(), hash.to_string()) {
            return Err(format!(
                "{path}: the manifest names it twice, first {before} then {hash}"
            ));
        }
    }
    Ok(manifest)
}

fn manifest() -> Manifest {
    let path = root().join("third_party/a2ui/MANIFEST.sha256");
    parse_manifest(&read(&path)).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Whether this line opens a table, and so ends the block before it and starts a new one.
fn opens_table(line: &&str) -> bool {
    line.trim().starts_with("[[")
}

/// The lines of the `[[upstream]]` block whose `id` is `a2ui`: from the table line that opens it,
/// which is the last one before the `id` line, to the next line opening a table or the end of the
/// file. Keys are pairs, so one written above the `id` line is as much in the block as one below.
fn a2ui_block(text: &str) -> Result<Vec<String>, String> {
    let lines = text.lines().collect::<Vec<_>>();
    let ids = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim() == A2UI_ID)
        .map(|(number, _)| number)
        .collect::<Vec<_>>();
    let id = match ids.as_slice() {
        [] => return Err(format!("no block with {A2UI_ID}")),
        [one] => *one,
        many => {
            let numbers = many
                .iter()
                .map(|number| (number + 1).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "more than one block claims {A2UI_ID}, on lines {numbers}; the pin is ours only when one does"
            ));
        }
    };
    let start = (0..id)
        .rfind(|number| opens_table(&lines[*number]))
        .ok_or_else(|| format!("{A2UI_ID} on line {} has no table line before it", id + 1))?;
    let end = (id + 1..lines.len())
        .find(|number| opens_table(&lines[*number]))
        .unwrap_or(lines.len());
    Ok(lines[start..end]
        .iter()
        .map(|line| (*line).to_string())
        .collect())
}

fn registry_block() -> Vec<String> {
    let path = root().join("docs/upstreams.toml");
    a2ui_block(&read(&path)).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn has_line(block: &[String], expected: &str) -> bool {
    block.iter().any(|line| line.trim() == expected)
}

#[test]
fn vendored_tree_matches_manifest() {
    let dir = upstream_dir();
    let manifest = manifest();
    assert_eq!(
        manifest.lines, VENDORED_FILES,
        "the manifest has {} lines; the copy holds {VENDORED_FILES} files",
        manifest.lines
    );
    let files = walk(&dir)
        .into_iter()
        .filter(|item| item.kind == Kind::Regular)
        .map(|item| item.path)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        manifest.lines,
        files.len(),
        "the manifest has {} lines for {} files under upstream/",
        manifest.lines,
        files.len()
    );
    let mut problems = Vec::new();
    for path in &files {
        match manifest.hashes.get(path) {
            None => problems.push(format!("{path}: on disk, but not in the manifest")),
            Some(wanted) => {
                let actual = sha256_of(&dir.join(path));
                if actual != *wanted {
                    problems.push(format!(
                        "{path}: the manifest says {wanted}, the file is {actual}"
                    ));
                }
            }
        }
    }
    for path in manifest.hashes.keys() {
        if !files.contains(path) {
            problems.push(format!("{path}: in the manifest, but not on disk"));
        }
    }
    assert!(
        problems.is_empty(),
        "{} problem(s) in the vendored tree:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

#[test]
fn vendored_tree_holds_only_regular_files() {
    let tree = walk(&upstream_dir());
    let python = tree
        .iter()
        .filter(|item| item.path.ends_with(".py"))
        .map(|item| item.path.clone())
        .collect::<Vec<_>>();
    assert!(
        python.is_empty(),
        "the vendored tree holds Python: {python:?}"
    );
    let links = tree
        .iter()
        .filter(|item| item.kind == Kind::Symlink)
        .map(|item| item.path.clone())
        .collect::<Vec<_>>();
    assert!(
        links.is_empty(),
        "the vendored tree holds symlinks: {links:?}"
    );
    let strange = offences(&tree);
    assert!(
        strange.is_empty(),
        "{} entry/entries in the vendored tree are not a plain regular file:\n{}",
        strange.len(),
        strange.join("\n")
    );
}

#[test]
fn pin_is_recorded_everywhere() {
    let readme = read(&root().join("third_party/a2ui/README.md"));
    assert!(
        readme.contains(crate::UPSTREAM_COMMIT),
        "third_party/a2ui/README.md does not name the upstream commit {}",
        crate::UPSTREAM_COMMIT
    );
    let pin = format!("pin = {:?}", crate::UPSTREAM_COMMIT);
    let block = registry_block();
    assert!(
        has_line(&block, &pin),
        "the a2ui block of docs/upstreams.toml has no line {pin}"
    );
    let record = read(&root().join("docs/design/a2ui-port.md"));
    assert!(
        record.contains(crate::UPSTREAM_COMMIT),
        "docs/design/a2ui-port.md does not name the upstream commit {}",
        crate::UPSTREAM_COMMIT
    );
    assert!(
        block.iter().any(|line| is_pin_date(line)),
        "the a2ui block of docs/upstreams.toml has no pin_date = \"YYYY-MM-DD\" line"
    );
}

#[test]
fn licence_is_recorded() {
    let licence = read(&upstream_dir().join("LICENSE"));
    assert!(
        licence.contains("Apache License"),
        "upstream/LICENSE is not the Apache Licence: it has no text `Apache License`"
    );
    assert!(
        licence.contains("Version 2.0"),
        "upstream/LICENSE is not version 2.0: it has no text `Version 2.0`"
    );
    assert!(
        has_line(&registry_block(), r#"licence_upstream = "Apache-2.0""#),
        "the a2ui block of docs/upstreams.toml does not record licence_upstream = \"Apache-2.0\""
    );
}

/// The merge gate does not read `third_party/` file by file, so the scan of the vendored copy is
/// done by hand once per pin move and its result is recorded next to the copy.
#[test]
fn the_readme_records_a_secret_scan_of_the_pin() {
    let readme = read(&root().join("third_party/a2ui/README.md"));
    let scanned = readme
        .lines()
        .find(|line| line.contains(crate::UPSTREAM_COMMIT) && line.contains("no leaks found"));
    assert!(
        scanned.is_some(),
        "third_party/a2ui/README.md records no secret scan of pin {}",
        crate::UPSTREAM_COMMIT
    );
}

const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const ONE: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// A manifest that names one path twice is not a manifest: which line wins is an accident.
#[test]
fn a_manifest_naming_a_path_twice_is_refused() {
    let text = format!("{ZERO}  ./LICENSE\n{ONE}  ./LICENSE\n");
    let error = parse_manifest(&text).expect_err("a path named twice must be refused");
    assert!(
        error.contains("./LICENSE"),
        "the error does not name the path: {error}"
    );
}

/// The control: two different paths are a well formed manifest.
#[test]
fn a_manifest_of_two_paths_is_read() {
    let text = format!("{ZERO}  ./LICENSE\n{ONE}  ./specification/versions.json\n");
    let manifest = parse_manifest(&text).expect("two different paths are a valid manifest");
    assert_eq!(
        manifest,
        Manifest {
            lines: 2,
            hashes: BTreeMap::from([
                ("./LICENSE".to_string(), ZERO.to_string()),
                ("./specification/versions.json".to_string(), ONE.to_string()),
            ]),
        }
    );
}

/// A fifo, a socket, a device or a directory with nothing in it carries no case: such a tree is
/// not the one that was copied.
#[test]
fn anything_that_is_not_a_regular_file_is_an_offence() {
    let tree = [
        entry("./conformance/core/validator_v1_0.yaml", Kind::Regular, 0),
        entry("./helper.py", Kind::Regular, 0),
        entry("./link", Kind::Symlink, 0),
        entry("./pipe", Kind::Other, 0),
        entry("./empty", Kind::Directory, 0),
        entry("./specification", Kind::Directory, 2),
    ];
    assert_eq!(
        offences(&tree),
        vec![
            "./helper.py: Python",
            "./link: a symlink",
            "./pipe: neither a regular file nor a directory",
            "./empty: an empty directory",
        ]
    );
}

const REGISTRY: &str = r#"[[upstream]]
id = "before"
pin = "the-pin-of-the-block-before"

[[upstream]]
pin = "the-a2ui-pin"
id = "a2ui"
rely_on = ["protocol v1.0 messages", "conformance suite"]

[[upstream]]
id = "after"
pin = "the-pin-of-the-block-after"
"#;

/// A key written above the `id` line belongs to the block as much as one written below it.
#[test]
fn the_block_holds_the_lines_written_above_its_id() {
    let block = a2ui_block(REGISTRY).expect("the fixture has an a2ui block");
    assert!(
        has_line(&block, r#"pin = "the-a2ui-pin""#),
        "the block left out the pin written above the id: {block:?}"
    );
}

/// The control: a block ends where the next table opens.
#[test]
fn the_block_stops_at_the_next_table() {
    let block = a2ui_block(REGISTRY).expect("the fixture has an a2ui block");
    assert!(
        !has_line(&block, r#"pin = "the-pin-of-the-block-before""#),
        "the block reached into the block before it: {block:?}"
    );
    assert!(
        !has_line(&block, r#"pin = "the-pin-of-the-block-after""#),
        "the block reached into the block after it: {block:?}"
    );
    assert!(
        has_line(&block, A2UI_ID),
        "the block lost its own id: {block:?}"
    );
}

/// Two blocks with the same id say nothing about which one is ours.
#[test]
fn two_blocks_with_the_same_id_are_refused() {
    let text = format!("{REGISTRY}{REGISTRY}");
    let error = a2ui_block(&text).expect_err("a duplicated id must be refused");
    assert!(
        error.contains("more than one"),
        "the error is unclear: {error}"
    );
}

#[test]
fn a_registry_without_the_a2ui_block_is_refused() {
    let text = String::from("id = \"other\"\npin = \"one\"\n");
    assert!(a2ui_block(&text).is_err());
}

/// The rule the walker follows, on the five outcomes of one entry and nothing else.
#[test]
fn the_five_outcomes_of_one_entry_are_told_apart() {
    assert_eq!(shape_of(false, false, true, 0), Shape::Regular);
    assert_eq!(shape_of(false, true, false, 2), Shape::Branch);
    assert_eq!(shape_of(false, true, false, 0), Shape::Empty);
    assert_eq!(shape_of(true, false, false, 0), Shape::Link);
    assert_eq!(shape_of(false, false, false, 0), Shape::Strange);
}

/// One listing gives one entry per name, and a directory is judged by the entries it holds
/// itself, not by how many names its parent listing has.
#[test]
fn a_listing_judges_every_name_by_what_it_holds() {
    let listing = [
        found("empty", false, true, false, 0),
        found("held", false, true, false, 2),
        found("plain.txt", false, false, true, 0),
    ];
    assert_eq!(
        listed("./catalogs", &listing),
        vec![
            ("./catalogs/empty".to_string(), Shape::Empty),
            ("./catalogs/held".to_string(), Shape::Branch),
            ("./catalogs/plain.txt".to_string(), Shape::Regular),
        ]
    );
}

/// A block that says one pin, one date and one licence, as a real one does.
const WELL_FORMED: &str = r#"[[upstream]]
pin = "the-a2ui-pin"
pin_date = "2026-11-20"
id = "a2ui"
licence_upstream = "Apache-2.0"
"#;

/// A line with one pair of brackets opens a table as much as `[[upstream]]` does, and so ends the
/// block; this registry holds a `[settings]` section.
#[test]
fn a_table_line_with_one_bracket_ends_the_block() {
    let text = format!("{WELL_FORMED}\n[settings]\ndata_dir = \"/var/lib/lucina/watch\"\n");
    let block = a2ui_block(&text).expect("the fixture has an a2ui block");
    assert!(
        !block.iter().any(|line| line.contains("data_dir")),
        "the block ran past the [settings] line: {block:?}"
    );
}

/// A line that begins with a bracket because it is a value inside a multi-line array is not a
/// table header: the block keeps going to the next real one.
#[test]
fn a_value_line_of_an_array_does_not_end_the_block() {
    let text = "[[upstream]]\npin = \"the-a2ui-pin\"\npin_date = \"2026-11-20\"\nid = \"a2ui\"\nrely_on = [\n[\"protocol v1.0 messages\", \"actions\"]\n]\nlicence_upstream = \"Apache-2.0\"\n\n[[upstream]]\nid = \"after\"\n";
    let block = a2ui_block(text).expect("the fixture has an a2ui block");
    assert!(
        has_line(&block, r#"["protocol v1.0 messages", "actions"]"#),
        "the block lost a line of the array: {block:?}"
    );
    assert!(
        has_line(&block, "]"),
        "the block lost the array's closing bracket: {block:?}"
    );
    assert!(
        has_line(&block, r#"licence_upstream = "Apache-2.0""#),
        "the block ended inside the array: {block:?}"
    );
    assert!(
        !has_line(&block, r#"id = "after""#),
        "the block reached into the block after it: {block:?}"
    );
}

/// Two pins in one block are two claims about where the port stands; the same for two licences.
#[test]
fn a_block_with_two_pins_is_refused() {
    let text = String::from(
        "[[upstream]]\npin = \"one\"\npin = \"two\"\npin_date = \"2026-11-20\"\nid = \"a2ui\"\nlicence_upstream = \"Apache-2.0\"\n",
    );
    let error = a2ui_block(&text).expect_err("two pins must be refused");
    assert!(
        error.contains("pin"),
        "the error does not name the key: {error}"
    );
}

#[test]
fn a_block_with_two_licences_is_refused() {
    let text = String::from
        ("[[upstream]]\npin = \"one\"\npin_date = \"2026-11-20\"\nid = \"a2ui\"\nlicence_upstream = \"Apache-2.0\"\nlicence_upstream = \"MIT\"\n");
    let error = a2ui_block(&text).expect_err("two licences must be refused");
    assert!(
        error.contains("licence_upstream"),
        "the error does not name the key: {error}"
    );
}

/// The date of the pin, read by its shape: four digits, two, two, in `pin_date = "..."`.
#[test]
fn a_pin_date_line_is_read_by_its_shape() {
    assert!(is_pin_date(r#"pin_date = "2026-11-20""#));
    assert!(!is_pin_date(r#"pin_date = "2026-11-2""#));
    assert!(!is_pin_date(r#"pin = "2026-11-20""#));
    assert!(!is_pin_date(r#"pin_date = "next tuesday""#));
    assert!(!is_pin_date("pin_date = 2026-11-20"));
}

/// `LC_ALL=C sort` is byte order: capital letters before small ones, and `./a` before `./b`. A
/// manifest that is not in that order is not the one the command makes.
#[test]
fn a_manifest_not_in_ascending_order_is_refused() {
    let text = format!("{ZERO}  ./b.yaml\n{ONE}  ./a.yaml\n");
    let error = parse_manifest(&text).expect_err("./a.yaml must come first");
    assert!(
        error.contains("./b.yaml"),
        "the error does not name the path out of order: {error}"
    );
    let good = format!("{ZERO}  ./a.yaml\n{ONE}  ./b.yaml\n");
    assert!(
        parse_manifest(&good).is_ok(),
        "an ascending manifest refused"
    );
    let capitals = format!("{ZERO}  ./B.yaml\n{ONE}  ./a.yaml\n");
    assert!(
        parse_manifest(&capitals).is_ok(),
        "byte order puts ./B.yaml first"
    );
}
