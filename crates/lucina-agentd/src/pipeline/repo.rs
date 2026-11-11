//! A repo's pipeline rules: `.lucina/pipeline.toml`, read from the trunk (so a branch can't
//! loosen the gates it is checked with), and which gates a change needs.
//!
//! ```toml
//! trunk = "main"                 # default: origin's HEAD, else main
//!
//! [merge]
//! diffgate = true                # diffgate over the merged range (default true)
//! cargo = "check"                # compile only: the session ran the tests in its workspace
//!                                # "touched" (the default) also tests each touched crate;
//!                                # "none" runs no cargo gate (`--full` still tests touched crates)
//! timeout_min = 45               # per gate
//! skip = ["firmware/lucinavoice"] # crate dirs the cargo gates leave alone (other toolchains)
//! test_skip = ["crates/big"]     # crate dirs left out of the test gate only (still checked, unless cargo = "none")
//! gate_files = ["tools/gates/"]  # extra files the gates call (plain path, or `dir/` / `dir/**` prefix); changing one,
//!                                # or anything in `.lucina/` or a script a `[[gate]]` run command names, needs
//!                                # `lagent merge --allow gate-change="why"`
//!
//! [[gate]]                       # extra gates, run when a changed path starts with one of `paths`
//! name = "web build"
//! paths = ["web/", "packages/lucina-stage/"]
//! run = "cd web && npm ci --prefer-offline && npm run build"
//!
//! [deploy.lucina-agentd]         # what `lagent deploy lucina-agentd` runs from main's tip
//! build = "cargo build --release -p lucina-agentd"
//! install = "deploy/lucina-agentd/install.sh"
//! ```

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

pub const FILE: &str = ".lucina/pipeline.toml";

fn yes() -> bool {
    true
}

fn touched() -> String {
    "touched".into()
}

fn forty_five() -> u64 {
    45
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct MergeRules {
    #[serde(default = "yes")]
    pub diffgate: bool,
    #[serde(default = "touched")]
    pub cargo: String,
    #[serde(default = "forty_five")]
    pub timeout_min: u64,
    #[serde(default)]
    pub skip: Vec<String>,
    #[serde(default)]
    pub test_skip: Vec<String>,
    /// Extra files the gates call (helper scripts, tools): a plain path, or a prefix ending in
    /// `/` or `/**`. Together with `.lucina/` and the scripts named by `[[gate]]` `run` commands
    /// they are the "gate files" of [`gate_files_changed`].
    #[serde(default)]
    pub gate_files: Vec<String>,
}

impl Default for MergeRules {
    fn default() -> Self {
        MergeRules {
            diffgate: true,
            cargo: touched(),
            timeout_min: 45,
            skip: Vec::new(),
            test_skip: Vec::new(),
            gate_files: Vec::new(),
        }
    }
}

/// What `[merge] cargo` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CargoMode {
    /// Compile only.
    Check,
    /// Compile, and test each touched crate.
    Touched,
    /// No cargo gate (`--full` still tests the touched crates).
    None,
}

/// The accepted `cargo` values, for error messages.
pub const CARGO_VALUES: &str = "check, touched, none";

/// `"off"` is the older spelling of `"none"` and is still read as such.
pub fn cargo_mode(value: &str) -> Option<CargoMode> {
    match value {
        "check" => Some(CargoMode::Check),
        "touched" => Some(CargoMode::Touched),
        "none" | "off" => Some(CargoMode::None),
        _ => None,
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct GateRule {
    pub name: String,
    #[serde(default)]
    pub paths: Vec<String>,
    pub run: String,
    #[serde(default)]
    pub timeout_min: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct DeployRule {
    #[serde(default)]
    pub build: Option<String>,
    pub install: String,
    #[serde(default)]
    pub health: Option<String>,
    #[serde(default)]
    pub timeout_min: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Rules {
    #[serde(default)]
    pub trunk: Option<String>,
    #[serde(default)]
    pub merge: MergeRules,
    #[serde(default, rename = "gate")]
    pub gates: Vec<GateRule>,
    #[serde(default)]
    pub deploy: BTreeMap<String, DeployRule>,
}

impl Rules {
    pub fn parse(text: &str) -> Result<Rules, String> {
        let rules: Rules = toml::from_str(text).map_err(|e| format!("{FILE}: {e}"))?;
        if cargo_mode(&rules.merge.cargo).is_none() {
            return Err(format!("{FILE}: [merge] cargo = {:?} is not a known value; accepted values: {CARGO_VALUES}", rules.merge.cargo));
        }
        Ok(rules)
    }
}

/// One crate of the repo: its directory (relative, no trailing slash; `""` = root), its
/// package name, and whether it is outside the root workspace (not one of its members:
/// tested with `--manifest-path`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crate {
    pub dir: String,
    pub name: String,
    pub standalone: bool,
}

/// A crate from its `Cargo.toml`, if it is a package.
pub fn crate_of(dir: &str, manifest: &str) -> Option<Crate> {
    let v: toml::Value = toml::from_str(manifest).ok()?;
    let name = v.get("package")?.get("name")?.as_str()?.to_string();
    let standalone = !dir.is_empty() && v.get("workspace").is_some();
    Some(Crate {
        dir: dir.to_string(),
        name,
        standalone,
    })
}

/// The crate a path belongs to: the deepest crate directory that contains it.
pub fn owner<'a>(path: &str, crates: &'a [Crate]) -> Option<&'a Crate> {
    crates
        .iter()
        .filter(|c| c.dir.is_empty() || path.starts_with(&format!("{}/", c.dir)))
        .max_by_key(|c| c.dir.len())
}

/// A gate to run: its name, the shell command and its time limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    pub name: String,
    pub run: String,
    pub timeout_min: u64,
}

/// The gates a change needs, cheapest first, so a refusal costs seconds, not a test run:
/// diffgate over `range`, the workspace check, then (`cargo = "touched"`) the tests of the
/// touched workspace crates, each touched standalone crate (checked, or tested), and the
/// declared gates whose paths it touches.
pub fn plan(
    rules: &Rules,
    changed: &[String],
    crates: &[Crate],
    has_workspace: bool,
    range: &str,
    allow: &[String],
) -> Vec<Gate> {
    plan_with(rules, changed, crates, has_workspace, range, allow, false).0
}

/// [`plan`] with `--full` (`full`: run the tests of touched crates whatever `cargo` says), and
/// the log lines saying which cargo gate runs or is skipped and which setting decided it.
pub fn plan_with(
    rules: &Rules,
    changed: &[String],
    crates: &[Crate],
    has_workspace: bool,
    range: &str,
    allow: &[String],
    full: bool,
) -> (Vec<Gate>, Vec<String>) {
    let t = rules.merge.timeout_min;
    let mode = cargo_mode(&rules.merge.cargo).unwrap_or(CargoMode::Touched);
    let mut gates = Vec::new();
    let mut notes = Vec::new();
    if rules.merge.diffgate {
        let mut run = format!("diffgate --repo . --range {range}");
        for a in allow.iter().filter(|a| !is_gate_change(a)) {
            run.push_str(&format!(" --allow {}", shell_quote(a)));
        }
        gates.push(Gate {
            name: "diffgate".into(),
            run,
            timeout_min: 10,
        });
    }
    let run_check = mode != CargoMode::None;
    let tests = mode == CargoMode::Touched || full;
    let under = |list: &[String], dir: &str| list.iter().any(|s| dir == s.trim_end_matches('/'));
    let mut members = BTreeSet::new();
    let mut standalone = BTreeSet::new();
    let mut test_members = BTreeSet::new();
    let mut test_standalone = BTreeSet::new();
    let mut root_manifest = false;
    for p in changed {
        if p == "Cargo.toml" || p == "Cargo.lock" {
            root_manifest = true;
        }
        if let Some(c) = owner(p, crates) {
            if under(&rules.merge.skip, &c.dir) {
                continue;
            }
            let tested = !under(&rules.merge.test_skip, &c.dir);
            if c.standalone {
                standalone.insert(c.dir.clone());
                if tested {
                    test_standalone.insert(c.dir.clone());
                }
            } else if !c.dir.is_empty() || !has_workspace {
                members.insert(c.name.clone());
                if tested {
                    test_members.insert(c.name.clone());
                }
            }
        }
    }
    let cargo_setting = format!("cargo = {:?}", rules.merge.cargo);
    // Checks.
    if run_check {
        if has_workspace && (root_manifest || !members.is_empty()) {
            gates.push(Gate {
                name: "cargo check (workspace)".into(),
                run: "cargo check --quiet --workspace".into(),
                timeout_min: t,
            });
        }
        // Crates the tests cover need no separate check; a crate left out of the tests (test_skip) is still checked.
        let check_members: Vec<&String> = members
            .iter()
            .filter(|m| !tests || !test_members.contains(*m))
            .collect();
        if !has_workspace && !check_members.is_empty() {
            let ps: Vec<String> = check_members.iter().map(|m| format!("-p {m}")).collect();
            gates.push(Gate {
                name: "cargo check (touched crates)".into(),
                run: format!("cargo check --quiet {}", ps.join(" ")),
                timeout_min: t,
            });
        }
        for dir in standalone
            .iter()
            .filter(|d| !tests || !test_standalone.contains(*d))
        {
            gates.push(Gate {
                name: format!("cargo check ({dir})"),
                run: format!("cargo check --quiet --manifest-path {dir}/Cargo.toml"),
                timeout_min: t,
            });
        }
    }
    // Tests.
    if tests {
        if !test_members.is_empty() {
            let ps: Vec<String> = test_members.iter().map(|m| format!("-p {m}")).collect();
            gates.push(Gate {
                name: "cargo test (touched crates)".into(),
                run: format!("cargo test --quiet {}", ps.join(" ")),
                timeout_min: t,
            });
        }
        for dir in &test_standalone {
            gates.push(Gate {
                name: format!("cargo test ({dir})"),
                run: format!("cargo test --quiet --manifest-path {dir}/Cargo.toml"),
                timeout_min: t,
            });
        }
    }
    let ran = |prefix: &str| gates.iter().any(|g| g.name.starts_with(prefix));
    notes.push(if !run_check {
        format!("cargo check: skipped ({cargo_setting})")
    } else if ran("cargo check") {
        format!("cargo check: runs ({cargo_setting})")
    } else {
        format!("cargo check: nothing to check ({cargo_setting}; no touched crate)")
    });
    notes.push(if ran("cargo test") {
        if mode == CargoMode::Touched {
            format!("cargo test: runs ({cargo_setting})")
        } else {
            format!("cargo test: runs (--full overrides {cargo_setting})")
        }
    } else if !tests {
        format!("cargo test: skipped ({cargo_setting}; --full runs the tests of touched crates)")
    } else if !members.is_empty() || !standalone.is_empty() {
        "cargo test: skipped (every touched crate is in test_skip)".to_string()
    } else {
        format!("cargo test: nothing to test ({cargo_setting}; no touched crate)")
    });
    for g in &rules.gates {
        if g.paths.is_empty()
            || changed
                .iter()
                .any(|p| g.paths.iter().any(|pre| p.starts_with(pre.as_str())))
        {
            gates.push(Gate {
                name: g.name.clone(),
                run: g.run.clone(),
                timeout_min: g.timeout_min.unwrap_or(t),
            });
        }
    }
    (gates, notes)
}

/// The `--allow` rule that waives the gate-change refusal (it is ours, not a diffgate rule).
pub const GATE_CHANGE: &str = "gate-change";

/// Is this `--allow` entry the gate-change waiver (with or without a reason)?
fn is_gate_change(entry: &str) -> bool {
    entry == GATE_CHANGE || entry.starts_with("gate-change=")
}

/// The reason of a `gate-change=<reason>` entry; `None` when there is none or it is empty.
pub fn gate_change_reason(allow: &[String]) -> Option<String> {
    allow
        .iter()
        .filter_map(|a| a.strip_prefix("gate-change="))
        .map(str::trim)
        .find(|r| !r.is_empty())
        .map(str::to_string)
}

/// Does `path` match a `[merge] gate_files` entry: the same path, or under a `dir/` or `dir/**` prefix?
fn gate_file_matches(entry: &str, path: &str) -> bool {
    let prefix = entry.strip_suffix("**").unwrap_or(entry);
    if prefix.ends_with('/') {
        path.starts_with(prefix)
    } else {
        path == entry
    }
}

/// A script-like name: no extension (`scripts/check-fast`) or a script extension. Data files a gate
/// only reads (`test -f b.txt`, `Cargo.lock`) are not gate files; name those in `gate_files` if they matter.
fn is_script_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            matches!(
                ext,
                "sh" | "bash" | "zsh" | "py" | "pl" | "rb" | "js" | "mjs" | "cjs" | "ts"
            )
        }
        _ => true,
    }
}

/// The files a gate `run` command names: whitespace-separated tokens (quotes, parentheses and `;`
/// trimmed, a leading `./` dropped) for which `exists` is true. Interpreters (`bash`), flags and
/// arguments are not files of the repo and fall away.
pub fn run_command_files(run: &str, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    for tok in run.split_whitespace() {
        let tok = tok.trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')' | ';'));
        let tok = tok.strip_prefix("./").unwrap_or(tok);
        if tok.is_empty() || tok.starts_with('-') || tok.starts_with('/') || tok.contains("..") {
            continue;
        }
        if is_script_name(tok) && exists(tok) && !out.iter().any(|o| o == tok) {
            out.push(tok.to_string());
        }
    }
    out
}

/// The changed files that are gate files: anything under `.lucina/`, a script a `[[gate]]` `run`
/// command names (`exists` says whether a token is a file of the candidate or the base), and the
/// paths of `[merge] gate_files`. Pure: the caller supplies `exists`.
pub fn gate_files_changed(
    rules: &Rules,
    changed: &[String],
    exists: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut scripts: BTreeSet<String> = BTreeSet::new();
    for g in &rules.gates {
        scripts.extend(run_command_files(&g.run, exists));
    }
    changed
        .iter()
        .filter(|f| {
            f.starts_with(".lucina/")
                || scripts.contains(f.as_str())
                || rules
                    .merge
                    .gate_files
                    .iter()
                    .any(|e| gate_file_matches(e, f))
        })
        .cloned()
        .collect()
}

/// `'…'` for a POSIX shell.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crates() -> Vec<Crate> {
        vec![
            Crate {
                dir: "".into(),
                name: "root".into(),
                standalone: false,
            },
            Crate {
                dir: "crates/lucina-agentd".into(),
                name: "lucina-agentd".into(),
                standalone: false,
            },
            Crate {
                dir: "crates/lucina-agents".into(),
                name: "lucina-agents".into(),
                standalone: false,
            },
            Crate {
                dir: "crates/secretary-pixiv".into(),
                name: "secretary-pixiv".into(),
                standalone: true,
            },
        ]
    }

    fn gate_rules() -> Rules {
        let mut r = Rules::default();
        r.gates.push(GateRule {
            name: "proofs".into(),
            paths: vec![],
            run: "bash scripts/gate-github-checks.sh".into(),
            timeout_min: None,
        });
        r.gates.push(GateRule {
            name: "fast".into(),
            paths: vec![],
            run: "scripts/check-fast --base HEAD^1 --merge-gate".into(),
            timeout_min: None,
        });
        r.gates.push(GateRule {
            name: "apply".into(),
            paths: vec![],
            run: "(cd tools && bash scripts/x.sh apply)".into(),
            timeout_min: None,
        });
        r
    }

    fn files(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    fn exists(p: &str) -> bool {
        matches!(
            p,
            "scripts/gate-github-checks.sh" | "scripts/check-fast" | "scripts/x.sh" | "tools/h.sh"
        )
    }

    #[test]
    fn no_gate_file_changed() {
        let r = gate_rules();
        let c = files(&["src/main.rs", "docs/a.md", "scripts/other.sh"]);
        assert!(gate_files_changed(&r, &c, &exists).is_empty());
    }

    #[test]
    fn pipeline_toml_and_lucina_dir_are_gate_files() {
        let r = gate_rules();
        let c = files(&["a.rs", ".lucina/pipeline.toml", ".lucina/other"]);
        assert_eq!(
            gate_files_changed(&r, &c, &exists),
            files(&[".lucina/pipeline.toml", ".lucina/other"])
        );
    }

    #[test]
    fn scripts_named_by_run_commands_are_gate_files() {
        let r = gate_rules();
        let c = files(&[
            "scripts/gate-github-checks.sh",
            "scripts/check-fast",
            "scripts/x.sh",
            "scripts/y.sh",
        ]);
        assert_eq!(
            gate_files_changed(&r, &c, &exists),
            files(&[
                "scripts/gate-github-checks.sh",
                "scripts/check-fast",
                "scripts/x.sh"
            ])
        );
        // Data files a gate only reads are not gate files.
        assert!(run_command_files("test -f b.txt", &|p| p == "b.txt").is_empty());
        // Interpreters, flags and arguments are not files.
        assert_eq!(
            run_command_files("bash scripts/x.sh apply", &exists),
            files(&["scripts/x.sh"])
        );
        assert_eq!(
            run_command_files("scripts/check-fast --base HEAD^1 --merge-gate", &exists),
            files(&["scripts/check-fast"])
        );
        assert_eq!(
            run_command_files("bash ./scripts/x.sh", &exists),
            files(&["scripts/x.sh"])
        );
    }

    #[test]
    fn gate_files_key_matches_paths_and_prefixes() {
        let mut r = gate_rules();
        r.merge.gate_files = files(&["tools/h.sh", "helpers/", "lib/**"]);
        let c = files(&[
            "tools/h.sh",
            "tools/h.sh.bak",
            "helpers/a/b.sh",
            "helpers",
            "lib/x/y",
            "libx/y",
        ]);
        assert_eq!(
            gate_files_changed(&r, &c, &exists),
            files(&["tools/h.sh", "helpers/a/b.sh", "lib/x/y"])
        );
        let t = Rules::parse("[merge]\ngate_files = [\"a/\"]\n").unwrap();
        assert_eq!(t.merge.gate_files, files(&["a/"]));
    }

    #[test]
    fn gate_change_allow_needs_a_reason_and_is_not_given_to_diffgate() {
        assert_eq!(
            gate_change_reason(&files(&["gate-change=second look"])),
            Some("second look".into())
        );
        assert_eq!(gate_change_reason(&files(&["gate-change="])), None);
        assert_eq!(gate_change_reason(&files(&["gate-change=  "])), None);
        assert_eq!(gate_change_reason(&files(&["gate-change"])), None);
        assert_eq!(gate_change_reason(&files(&["todo=docs"])), None);
        let g = plan(
            &Rules::default(),
            &files(&["a.md"]),
            &[],
            false,
            "A..B",
            &files(&["gate-change=why", "todo=docs"]),
        );
        assert_eq!(
            g[0].run,
            "diffgate --repo . --range A..B --allow 'todo=docs'"
        );
        let g = plan(
            &Rules::default(),
            &files(&["a.md"]),
            &[],
            false,
            "A..B",
            &files(&["gate-change=why"]),
        );
        assert_eq!(g[0].run, "diffgate --repo . --range A..B");
    }

    #[test]
    fn rules_parse_with_defaults() {
        let r = Rules::parse("").unwrap();
        assert_eq!(r.merge, MergeRules::default());
        let r = Rules::parse(
            "trunk = \"main\"\n[merge]\ncargo = \"off\"\n[[gate]]\nname = \"web\"\npaths = [\"web/\"]\nrun = \"make web\"\n[deploy.lucina-agentd]\ninstall = \"deploy/x.sh\"\n",
        )
        .unwrap();
        assert_eq!(r.trunk.as_deref(), Some("main"));
        assert!(r.merge.diffgate);
        assert_eq!(r.gates[0].paths, vec!["web/".to_string()]);
        assert_eq!(r.deploy["lucina-agentd"].install, "deploy/x.sh");
        assert!(Rules::parse("[merge]\ncargo = 3").is_err());
    }

    #[test]
    fn crate_manifests() {
        assert_eq!(
            crate_of("crates/a", "[package]\nname = \"a\"\n").unwrap(),
            Crate {
                dir: "crates/a".into(),
                name: "a".into(),
                standalone: false
            }
        );
        assert!(
            crate_of("crates/b", "[package]\nname = \"b\"\n[workspace]\n")
                .unwrap()
                .standalone
        );
        assert!(crate_of("", "[workspace]\nmembers = []\n").is_none());
    }

    #[test]
    fn the_deepest_crate_owns_a_path() {
        let c = crates();
        assert_eq!(
            owner("crates/lucina-agentd/src/lib.rs", &c).unwrap().name,
            "lucina-agentd"
        );
        assert_eq!(
            owner("crates/lucina-agents-x/src/lib.rs", &c).unwrap().name,
            "root"
        );
        assert_eq!(owner("README.md", &c).unwrap().name, "root");
    }

    #[test]
    fn gates_follow_the_change() {
        let mut r = Rules::default();
        r.gates.push(GateRule {
            name: "web".into(),
            paths: vec!["web/".into()],
            run: "make web".into(),
            timeout_min: None,
        });
        let changed = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Rust crates: tests of the touched ones, then the workspace check, then diffgate.
        let g = plan(
            &r,
            &changed(&[
                "crates/lucina-agentd/src/a.rs",
                "crates/lucina-agents/src/b.rs",
            ]),
            &crates(),
            true,
            "A..B",
            &[],
        );
        let names: Vec<&str> = g.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "diffgate",
                "cargo check (workspace)",
                "cargo test (touched crates)"
            ]
        );
        assert_eq!(
            g[2].run,
            "cargo test --quiet -p lucina-agentd -p lucina-agents"
        );
        assert_eq!(g[0].run, "diffgate --repo . --range A..B");
        // Docs only: the root crate of a workspace isn't a package to test; diffgate still runs.
        let g = plan(
            &r,
            &changed(&["docs/x.md"]),
            &crates(),
            true,
            "A..B",
            &["todo=docs".into()],
        );
        assert_eq!(g.len(), 1);
        assert_eq!(
            g[0].run,
            "diffgate --repo . --range A..B --allow 'todo=docs'"
        );
        // A standalone crate and a declared gate.
        let g = plan(
            &r,
            &changed(&["crates/secretary-pixiv/src/x.rs", "web/src/a.ts"]),
            &crates(),
            true,
            "A..B",
            &[],
        );
        let names: Vec<&str> = g.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(
            names,
            ["diffgate", "cargo test (crates/secretary-pixiv)", "web"]
        );
        // The lock file alone: the workspace check.
        let g = plan(&r, &changed(&["Cargo.lock"]), &crates(), true, "A..B", &[]);
        assert_eq!(g[1].name, "cargo check (workspace)");
        // cargo = "check": compile only, whatever it touches.
        let mut fast = r.clone();
        fast.merge.cargo = "check".into();
        let g = plan(
            &fast,
            &changed(&[
                "crates/lucina-agentd/src/a.rs",
                "crates/secretary-pixiv/src/x.rs",
            ]),
            &crates(),
            true,
            "A..B",
            &[],
        );
        assert_eq!(
            g.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            [
                "diffgate",
                "cargo check (workspace)",
                "cargo check (crates/secretary-pixiv)"
            ]
        );
        assert!(g.iter().all(|g| !g.run.contains("cargo test")));
        // A skipped crate (another toolchain) gets no cargo gate.
        r.merge.skip = vec!["crates/secretary-pixiv/".into()];
        let g = plan(
            &r,
            &changed(&["crates/secretary-pixiv/src/x.rs"]),
            &crates(),
            true,
            "A..B",
            &[],
        );
        assert_eq!(
            g.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            ["diffgate"]
        );
        // cargo off, diffgate off: nothing.
        r.merge.cargo = "off".into();
        r.merge.diffgate = false;
        assert!(plan(
            &r,
            &changed(&["crates/lucina-agentd/src/a.rs"]),
            &crates(),
            true,
            "A..B",
            &[]
        )
        .is_empty());
    }

    #[test]
    fn cargo_values_are_validated() {
        assert_eq!(cargo_mode("check"), Some(CargoMode::Check));
        assert_eq!(cargo_mode("touched"), Some(CargoMode::Touched));
        assert_eq!(cargo_mode("none"), Some(CargoMode::None));
        assert_eq!(cargo_mode("off"), Some(CargoMode::None));
        assert_eq!(cargo_mode("nonee"), None);
        // Missing key: today's default, "touched".
        assert_eq!(
            Rules::parse("[merge]\ndiffgate = true\n")
                .unwrap()
                .merge
                .cargo,
            "touched"
        );
        for v in ["check", "touched", "none"] {
            assert_eq!(
                Rules::parse(&format!("[merge]\ncargo = \"{v}\"\n"))
                    .unwrap()
                    .merge
                    .cargo,
                v
            );
        }
        let e = Rules::parse("[merge]\ncargo = \"nope\"\n").unwrap_err();
        assert!(
            e.contains(".lucina/pipeline.toml")
                && e.contains("\"nope\"")
                && e.contains("check, touched, none"),
            "{e}"
        );
        let r = Rules::parse("[merge]\ntest_skip = [\"crates/lucina-agents\"]\n").unwrap();
        assert_eq!(r.merge.test_skip, vec!["crates/lucina-agents".to_string()]);
    }

    fn names(g: &[Gate]) -> Vec<&str> {
        g.iter().map(|g| g.name.as_str()).collect()
    }

    fn with(cargo: &str, test_skip: &[&str]) -> Rules {
        let mut r = Rules::default();
        r.merge.cargo = cargo.into();
        r.merge.test_skip = test_skip.iter().map(|s| s.to_string()).collect();
        r
    }

    fn two() -> Vec<String> {
        vec![
            "crates/lucina-agentd/src/a.rs".into(),
            "crates/lucina-agents/src/b.rs".into(),
        ]
    }

    #[test]
    fn cargo_none_runs_no_cargo_gate_but_diffgate_and_declared_gates() {
        let mut r = with("none", &[]);
        r.gates.push(GateRule {
            name: "web".into(),
            paths: vec!["web/".into()],
            run: "make web".into(),
            timeout_min: None,
        });
        let mut ch = two();
        ch.push("crates/secretary-pixiv/src/x.rs".into());
        ch.push("web/a.ts".into());
        let (g, notes) = plan_with(&r, &ch, &crates(), true, "A..B", &[], false);
        assert_eq!(names(&g), ["diffgate", "web"]);
        assert_eq!(notes[0], "cargo check: skipped (cargo = \"none\")");
        assert!(
            notes[1].starts_with("cargo test: skipped (cargo = \"none\""),
            "{notes:?}"
        );
    }

    #[test]
    fn cargo_none_with_full_still_tests_touched_crates() {
        let mut ch = two();
        ch.push("crates/secretary-pixiv/src/x.rs".into());
        let (g, notes) = plan_with(
            &with("none", &["crates/lucina-agents"]),
            &ch,
            &crates(),
            true,
            "A..B",
            &[],
            true,
        );
        assert_eq!(
            names(&g),
            [
                "diffgate",
                "cargo test (touched crates)",
                "cargo test (crates/secretary-pixiv)"
            ]
        );
        assert_eq!(g[1].run, "cargo test --quiet -p lucina-agentd");
        assert_eq!(notes[0], "cargo check: skipped (cargo = \"none\")");
        assert_eq!(
            notes[1],
            "cargo test: runs (--full overrides cargo = \"none\")"
        );
    }

    #[test]
    fn check_and_touched_are_unchanged() {
        let (g, n) = plan_with(
            &with("check", &[]),
            &two(),
            &crates(),
            true,
            "A..B",
            &[],
            false,
        );
        assert_eq!(names(&g), ["diffgate", "cargo check (workspace)"]);
        assert_eq!(n[0], "cargo check: runs (cargo = \"check\")");
        assert!(n[1].starts_with("cargo test: skipped (cargo = \"check\""));
        let (g, n) = plan_with(
            &with("check", &[]),
            &two(),
            &crates(),
            true,
            "A..B",
            &[],
            true,
        );
        assert_eq!(
            names(&g),
            [
                "diffgate",
                "cargo check (workspace)",
                "cargo test (touched crates)"
            ]
        );
        assert_eq!(
            n[1],
            "cargo test: runs (--full overrides cargo = \"check\")"
        );
        let (g, n) = plan_with(
            &with("touched", &[]),
            &two(),
            &crates(),
            true,
            "A..B",
            &[],
            false,
        );
        assert_eq!(
            names(&g),
            [
                "diffgate",
                "cargo check (workspace)",
                "cargo test (touched crates)"
            ]
        );
        assert_eq!(
            g[2].run,
            "cargo test --quiet -p lucina-agentd -p lucina-agents"
        );
        assert_eq!(n[1], "cargo test: runs (cargo = \"touched\")");
    }

    #[test]
    fn test_skip_leaves_a_crate_out_of_the_test_gate_only() {
        let (g, _) = plan_with(
            &with("touched", &["crates/lucina-agents/"]),
            &two(),
            &crates(),
            true,
            "A..B",
            &[],
            false,
        );
        assert_eq!(
            names(&g),
            [
                "diffgate",
                "cargo check (workspace)",
                "cargo test (touched crates)"
            ]
        );
        assert_eq!(g[2].run, "cargo test --quiet -p lucina-agentd");
        // `skip` keeps its meaning: out of both.
        let mut r = with("touched", &[]);
        r.merge.skip = vec!["crates/lucina-agents".into()];
        let ch = vec!["crates/lucina-agents/src/b.rs".to_string()];
        assert_eq!(
            names(&plan_with(&r, &ch, &crates(), true, "A..B", &[], false).0),
            ["diffgate"]
        );
    }

    #[test]
    fn all_touched_crates_in_test_skip_skips_the_test_gate_with_a_reason() {
        let r = with(
            "touched",
            &["crates/lucina-agentd", "crates/secretary-pixiv"],
        );
        let ch = vec![
            "crates/lucina-agentd/src/a.rs".to_string(),
            "crates/secretary-pixiv/src/x.rs".to_string(),
        ];
        let (g, n) = plan_with(&r, &ch, &crates(), true, "A..B", &[], false);
        assert_eq!(
            names(&g),
            [
                "diffgate",
                "cargo check (workspace)",
                "cargo check (crates/secretary-pixiv)"
            ]
        );
        assert_eq!(
            n[1],
            "cargo test: skipped (every touched crate is in test_skip)"
        );
        // The whole workspace is never run instead.
        assert!(g
            .iter()
            .all(|g| !g.run.contains("--workspace") || g.run.starts_with("cargo check")));
    }

    #[test]
    fn a_test_skip_crate_is_still_checked_where_the_mode_checks() {
        let px = vec!["crates/secretary-pixiv/src/x.rs".to_string()];
        let skip = ["crates/secretary-pixiv"];
        // touched: standalone crate in test_skip gets a check gate and no test gate.
        let (g, n) = plan_with(
            &with("touched", &skip),
            &px,
            &crates(),
            true,
            "A..B",
            &[],
            false,
        );
        assert_eq!(
            names(&g),
            ["diffgate", "cargo check (crates/secretary-pixiv)"]
        );
        assert_eq!(n[0], "cargo check: runs (cargo = \"touched\")");
        assert_eq!(
            n[1],
            "cargo test: skipped (every touched crate is in test_skip)"
        );
        // check + --full: the same.
        let (g, n) = plan_with(
            &with("check", &skip),
            &px,
            &crates(),
            true,
            "A..B",
            &[],
            true,
        );
        assert_eq!(
            names(&g),
            ["diffgate", "cargo check (crates/secretary-pixiv)"]
        );
        assert_eq!(
            n[1],
            "cargo test: skipped (every touched crate is in test_skip)"
        );
        // none + --full: no check, no test.
        let (g, _) = plan_with(
            &with("none", &skip),
            &px,
            &crates(),
            true,
            "A..B",
            &[],
            true,
        );
        assert_eq!(names(&g), ["diffgate"]);
        // A workspace member in test_skip: the workspace check covers it, nothing else.
        let m = vec!["crates/lucina-agents/src/b.rs".to_string()];
        let (g, _) = plan_with(
            &with("touched", &["crates/lucina-agents"]),
            &m,
            &crates(),
            true,
            "A..B",
            &[],
            false,
        );
        assert_eq!(names(&g), ["diffgate", "cargo check (workspace)"]);
        // Without a workspace: a tested member needs no check, a test_skip member gets `cargo check -p`.
        let both = two();
        let (g, _) = plan_with(
            &with("touched", &["crates/lucina-agents"]),
            &both,
            &crates(),
            false,
            "A..B",
            &[],
            false,
        );
        assert_eq!(
            names(&g),
            [
                "diffgate",
                "cargo check (touched crates)",
                "cargo test (touched crates)"
            ]
        );
        assert_eq!(g[1].run, "cargo check --quiet -p lucina-agents");
        assert_eq!(g[2].run, "cargo test --quiet -p lucina-agentd");
    }
}
