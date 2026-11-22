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
    root().join("tests/conformance/a2ui/upstream")
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
    let path = root().join("tests/conformance/a2ui/MANIFEST.sha256");
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
    assert!(
        manifest.lines >= 400,
        "the manifest has {} lines; the oracle is 400 or more",
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
                let found = sha256_of(&dir.join(path));
                if found != *wanted {
                    problems.push(format!(
                        "{path}: the manifest says {wanted}, the file is {found}"
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
    let readme = read(&root().join("tests/conformance/a2ui/README.md"));
    assert!(
        readme.contains(lucina_a2ui::UPSTREAM_COMMIT),
        "tests/conformance/a2ui/README.md does not name the upstream commit {}",
        lucina_a2ui::UPSTREAM_COMMIT
    );
    let pin = format!("pin = {:?}", lucina_a2ui::UPSTREAM_COMMIT);
    assert!(
        has_line(&registry_block(), &pin),
        "the a2ui block of docs/upstreams.toml has no line {pin}"
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
