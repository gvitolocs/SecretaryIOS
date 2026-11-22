//! The vendored A2UI oracle is what upstream published, unchanged, and is recorded as such.
//!
//! These tests only read files; they never write one.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

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

/// What is under `upstream/`: regular files and symlinks, as `sha256sum` prints them (`./a/b`).
struct Tree {
    files: BTreeSet<String>,
    symlinks: Vec<String>,
}

fn walk(dir: &Path) -> Tree {
    let mut tree = Tree {
        files: BTreeSet::new(),
        symlinks: Vec::new(),
    };
    collect(dir, ".", &mut tree);
    tree
}

fn collect(dir: &Path, prefix: &str, tree: &mut Tree) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|error| panic!("cannot list {}: {error}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("cannot list {}: {error}", dir.display()));
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = format!("{prefix}/{name}");
        let kind = entry
            .file_type()
            .unwrap_or_else(|error| panic!("cannot stat {path}: {error}"));
        if kind.is_symlink() {
            tree.symlinks.push(path);
        } else if kind.is_dir() {
            collect(&Path::new(dir).join(name), &path, tree);
        } else if kind.is_file() {
            tree.files.insert(path);
        }
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

/// `tests/conformance/a2ui/MANIFEST.sha256`, path to hash.
fn manifest() -> BTreeMap<String, String> {
    let path = root().join("tests/conformance/a2ui/MANIFEST.sha256");
    let text = read(&path);
    let mut entries = BTreeMap::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let (hash, file) = line
            .split_once("  ")
            .unwrap_or_else(|| panic!("not a sha256sum line in {}: {line}", path.display()));
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            panic!("not a sha256 hash in {}: {line}", path.display());
        }
        entries.insert(file.to_string(), hash.to_string());
    }
    entries
}

/// The lines of the `[[upstream]]` block in `docs/upstreams.toml` whose `id` is `a2ui`: from that
/// `id` line to the next `[[upstream]]` line, or the end of the file.
fn a2ui_block() -> Vec<String> {
    let path = root().join("docs/upstreams.toml");
    let text = read(&path);
    let lines = text.lines().collect::<Vec<_>>();
    let id = format!("id = {:?}", "a2ui");
    let start = lines
        .iter()
        .position(|line| line.trim() == id)
        .unwrap_or_else(|| panic!("no [[upstream]] block with {id} in {}", path.display()));
    let rest = &lines[start + 1..];
    let end = rest
        .iter()
        .position(|line| line.trim() == "[[upstream]]")
        .map_or(lines.len(), |offset| start + 1 + offset);
    lines[start..end]
        .iter()
        .map(|line| (*line).to_string())
        .collect()
}

fn has_line(block: &[String], expected: &str) -> bool {
    block.iter().any(|line| line.trim() == expected)
}

#[test]
fn vendored_tree_matches_manifest() {
    let dir = upstream_dir();
    let entries = manifest();
    assert!(
        entries.len() >= 400,
        "the manifest lists {} files; the oracle is 400 or more",
        entries.len()
    );
    let tree = walk(&dir);
    let mut problems = Vec::new();
    for path in &tree.files {
        match entries.get(path) {
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
    for path in entries.keys() {
        if !tree.files.contains(path) {
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
fn vendored_tree_has_no_python_and_no_symlinks() {
    let tree = walk(&upstream_dir());
    let python = tree
        .files
        .iter()
        .chain(&tree.symlinks)
        .filter(|path| path.ends_with(".py"))
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        python.is_empty(),
        "the vendored tree holds Python: {python:?}"
    );
    assert!(
        tree.symlinks.is_empty(),
        "the vendored tree holds symlinks: {:?}",
        tree.symlinks
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
        has_line(&a2ui_block(), &pin),
        "the a2ui block of docs/upstreams.toml has no line {pin}"
    );
}

#[test]
fn licence_is_recorded() {
    let licence = read(&upstream_dir().join("LICENSE"));
    assert!(
        licence.contains("Apache License"),
        "upstream/LICENSE is not the Apache Licence: it has no line `Apache License`"
    );
    assert!(
        licence.contains("Version 2.0"),
        "upstream/LICENSE is not version 2.0: it has no text `Version 2.0`"
    );
    assert!(
        has_line(&a2ui_block(), "licence_upstream = \"Apache-2.0\""),
        "the a2ui block of docs/upstreams.toml does not record licence_upstream = \"Apache-2.0\""
    );
}
