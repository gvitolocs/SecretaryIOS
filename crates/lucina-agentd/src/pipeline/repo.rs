//! A repo's pipeline rules: `.lucina/pipeline.toml`, read from the trunk (so a branch can't
//! loosen the gates it is checked with), and which gates a change needs.
//!
//! ```toml
//! trunk = "main"                 # default: origin's HEAD, else main
//!
//! [merge]
//! diffgate = true                # diffgate over the merged range (default true)
//! cargo = "check"                # compile only: the session ran the tests in its workspace
//!                                # (the default "touched" also tests each touched crate; "off")
//! timeout_min = 45               # per gate
//! skip = ["firmware/lucinavoice"] # crate dirs the cargo gates leave alone (other toolchains)
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
}

impl Default for MergeRules {
    fn default() -> Self {
        MergeRules { diffgate: true, cargo: touched(), timeout_min: 45, skip: Vec::new() }
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
        toml::from_str(text).map_err(|e| format!("{FILE}: {e}"))
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
    Some(Crate { dir: dir.to_string(), name, standalone })
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
pub fn plan(rules: &Rules, changed: &[String], crates: &[Crate], has_workspace: bool, range: &str, allow: &[String]) -> Vec<Gate> {
    let t = rules.merge.timeout_min;
    let mut gates = Vec::new();
    if rules.merge.diffgate {
        let mut run = format!("diffgate --repo . --range {range}");
        for a in allow {
            run.push_str(&format!(" --allow {}", shell_quote(a)));
        }
        gates.push(Gate { name: "diffgate".into(), run, timeout_min: 10 });
    }
    if rules.merge.cargo != "off" {
        let tests = rules.merge.cargo != "check";
        let mut members = BTreeSet::new();
        let mut standalone = BTreeSet::new();
        let mut root_manifest = false;
        for p in changed {
            if p == "Cargo.toml" || p == "Cargo.lock" {
                root_manifest = true;
            }
            if let Some(c) = owner(p, crates) {
                if rules.merge.skip.iter().any(|s| c.dir == s.trim_end_matches('/')) {
                    continue;
                }
                if c.standalone {
                    standalone.insert(c.dir.clone());
                } else if !c.dir.is_empty() || !has_workspace {
                    members.insert(c.name.clone());
                }
            }
        }
        if has_workspace && (root_manifest || !members.is_empty()) {
            gates.push(Gate { name: "cargo check (workspace)".into(), run: "cargo check --quiet --workspace".into(), timeout_min: t });
        }
        if !members.is_empty() && (tests || !has_workspace) {
            let ps: Vec<String> = members.iter().map(|m| format!("-p {m}")).collect();
            let (verb, what) = if tests { ("test", "cargo test (touched crates)") } else { ("check", "cargo check (touched crates)") };
            gates.push(Gate { name: what.into(), run: format!("cargo {verb} --quiet {}", ps.join(" ")), timeout_min: t });
        }
        let verb = if tests { "test" } else { "check" };
        for dir in &standalone {
            gates.push(Gate { name: format!("cargo {verb} ({dir})"), run: format!("cargo {verb} --quiet --manifest-path {dir}/Cargo.toml"), timeout_min: t });
        }
    }
    for g in &rules.gates {
        if g.paths.is_empty() || changed.iter().any(|p| g.paths.iter().any(|pre| p.starts_with(pre.as_str()))) {
            gates.push(Gate { name: g.name.clone(), run: g.run.clone(), timeout_min: g.timeout_min.unwrap_or(t) });
        }
    }
    gates
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
            Crate { dir: "".into(), name: "root".into(), standalone: false },
            Crate { dir: "crates/lucina-agentd".into(), name: "lucina-agentd".into(), standalone: false },
            Crate { dir: "crates/lucina-agents".into(), name: "lucina-agents".into(), standalone: false },
            Crate { dir: "crates/secretary-pixiv".into(), name: "secretary-pixiv".into(), standalone: true },
        ]
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
        assert_eq!(crate_of("crates/a", "[package]\nname = \"a\"\n").unwrap(), Crate { dir: "crates/a".into(), name: "a".into(), standalone: false });
        assert!(crate_of("crates/b", "[package]\nname = \"b\"\n[workspace]\n").unwrap().standalone);
        assert!(crate_of("", "[workspace]\nmembers = []\n").is_none());
    }

    #[test]
    fn the_deepest_crate_owns_a_path() {
        let c = crates();
        assert_eq!(owner("crates/lucina-agentd/src/lib.rs", &c).unwrap().name, "lucina-agentd");
        assert_eq!(owner("crates/lucina-agents-x/src/lib.rs", &c).unwrap().name, "root");
        assert_eq!(owner("README.md", &c).unwrap().name, "root");
    }

    #[test]
    fn gates_follow_the_change() {
        let mut r = Rules::default();
        r.gates.push(GateRule { name: "web".into(), paths: vec!["web/".into()], run: "make web".into(), timeout_min: None });
        let changed = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Rust crates: tests of the touched ones, then the workspace check, then diffgate.
        let g = plan(&r, &changed(&["crates/lucina-agentd/src/a.rs", "crates/lucina-agents/src/b.rs"]), &crates(), true, "A..B", &[]);
        let names: Vec<&str> = g.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["diffgate", "cargo check (workspace)", "cargo test (touched crates)"]);
        assert_eq!(g[2].run, "cargo test --quiet -p lucina-agentd -p lucina-agents");
        assert_eq!(g[0].run, "diffgate --repo . --range A..B");
        // Docs only: the root crate of a workspace isn't a package to test; diffgate still runs.
        let g = plan(&r, &changed(&["docs/x.md"]), &crates(), true, "A..B", &["todo=docs".into()]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].run, "diffgate --repo . --range A..B --allow 'todo=docs'");
        // A standalone crate and a declared gate.
        let g = plan(&r, &changed(&["crates/secretary-pixiv/src/x.rs", "web/src/a.ts"]), &crates(), true, "A..B", &[]);
        let names: Vec<&str> = g.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["diffgate", "cargo test (crates/secretary-pixiv)", "web"]);
        // The lock file alone: the workspace check.
        let g = plan(&r, &changed(&["Cargo.lock"]), &crates(), true, "A..B", &[]);
        assert_eq!(g[1].name, "cargo check (workspace)");
        // cargo = "check": compile only, whatever it touches.
        let mut fast = r.clone();
        fast.merge.cargo = "check".into();
        let g = plan(&fast, &changed(&["crates/lucina-agentd/src/a.rs", "crates/secretary-pixiv/src/x.rs"]), &crates(), true, "A..B", &[]);
        assert_eq!(g.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["diffgate", "cargo check (workspace)", "cargo check (crates/secretary-pixiv)"]);
        assert!(g.iter().all(|g| !g.run.contains("cargo test")));
        // A skipped crate (another toolchain) gets no cargo gate.
        r.merge.skip = vec!["crates/secretary-pixiv/".into()];
        let g = plan(&r, &changed(&["crates/secretary-pixiv/src/x.rs"]), &crates(), true, "A..B", &[]);
        assert_eq!(g.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["diffgate"]);
        // cargo off, diffgate off: nothing.
        r.merge.cargo = "off".into();
        r.merge.diffgate = false;
        assert!(plan(&r, &changed(&["crates/lucina-agentd/src/a.rs"]), &crates(), true, "A..B", &[]).is_empty());
    }
}
