//! LucinaOS proof CI: see docs/ci/VERUS.md.
//!
//!   lucina-ci inventory --check | --write      the list of proof units and the proof matrix page
//!   lucina-ci plan ...                         which groups a commit needs (GitHub matrix)
//!   lucina-ci run --group G --phase P ...      run a group's units, each under its own memory cap
//!   lucina-ci selftest ...                     prove the cap, timeouts and verdicts work on this machine
//!   lucina-ci gate --plan F --reports D        every planned unit verified on this commit?
//!   lucina-ci merge-check ...                  the lagent merge gate: did GitHub verify this commit?

mod fast;
mod inventory;
mod runner;
mod snapshot;
mod verdict;

use inventory::Unit;
use runner::Spec;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use verdict::{CheckRun, MergeVerdict};

const PROOF_MATRIX: &str = "docs/ci/PROOF-MATRIX.md";
const REQUIRED_CHECKS: &[&str] = &["verus-gate", "rust-gate"];

struct Args(Vec<String>);

impl Args {
    fn val(&self, k: &str) -> Option<String> {
        self.0
            .iter()
            .position(|a| a == k)
            .and_then(|i| self.0.get(i + 1))
            .cloned()
    }
    fn vals(&self, k: &str) -> Vec<String> {
        self.0
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == k)
            .filter_map(|(i, _)| self.0.get(i + 1))
            .cloned()
            .collect()
    }
    fn has(&self, k: &str) -> bool {
        self.0.iter().any(|a| a == k)
    }
    fn need(&self, k: &str) -> Result<String, String> {
        self.val(k).ok_or_else(|| format!("{k} is required"))
    }
    fn num(&self, k: &str, default: u64) -> Result<u64, String> {
        self.val(k).map_or(Ok(default), |v| {
            v.parse().map_err(|_| format!("{k}: not a number"))
        })
    }
}

fn repo_root() -> Result<PathBuf, String> {
    let o = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err("not inside a git repository".into());
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
}

fn cmd_inventory(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    let units = inventory::discover(&root)?;
    let tsv = inventory::to_tsv(&units);
    let md = inventory::to_markdown(&units);
    if a.has("--write") {
        fs::write(root.join(inventory::INVENTORY), &tsv).map_err(|e| e.to_string())?;
        fs::create_dir_all(root.join("docs/ci")).map_err(|e| e.to_string())?;
        fs::write(root.join(PROOF_MATRIX), &md).map_err(|e| e.to_string())?;
        println!("{} units written", units.len());
        return Ok(0);
    }
    let committed = fs::read_to_string(root.join(inventory::INVENTORY)).unwrap_or_default();
    let page = fs::read_to_string(root.join(PROOF_MATRIX)).unwrap_or_default();
    if committed != tsv || page != md {
        let old: BTreeSet<&str> = committed
            .lines()
            .map(|l| l.split('\t').next().unwrap_or(""))
            .collect();
        let new: BTreeSet<&str> = tsv
            .lines()
            .map(|l| l.split('\t').next().unwrap_or(""))
            .collect();
        for u in new.difference(&old) {
            eprintln!("not in the inventory: {u}");
        }
        for u in old.difference(&new) {
            eprintln!("in the inventory but gone: {u}");
        }
        eprintln!(
            "the proof inventory is not current: run lucina-ci inventory --write and commit the result"
        );
        return Ok(1);
    }
    println!("{} units: inventory current", units.len());
    Ok(0)
}

/// `{group, phase}` rows and the units, for the units a change needs.
fn select(units: &[Unit], changed: Option<&[String]>, only_group: Option<&str>) -> Vec<Unit> {
    units
        .iter()
        .filter(|u| match changed {
            None => true,
            Some(c) => c.iter().any(|p| u.affected_by(p)) || only_group == Some(u.group.as_str()),
        })
        .cloned()
        .collect()
}

/// The one unit a `proof/<unit>` branch is for, if it names a unit.
fn proof_unit(units: &[Unit], branch: &str) -> Option<Vec<Unit>> {
    let wanted = branch.strip_prefix("proof/")?;
    units
        .iter()
        .find(|u| u.name == wanted)
        .map(|u| vec![u.clone()])
}

fn changed_between(root: &Path, a: &str, b: &str) -> Result<Vec<String>, String> {
    let o = Command::new("git")
        .args(["diff", "--name-only", a, b])
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!("git diff {a} {b} failed"));
    }
    Ok(String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

fn merge_base(root: &Path, a: &str, b: &str) -> Option<String> {
    let o = Command::new("git")
        .args(["merge-base", a, b])
        .current_dir(root)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// `--event --ref --before --sha --output --plan`. Pull requests and main get everything; another branch gets what its changes
/// reach, and any doubt about the base means everything.
fn cmd_plan(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    let units = inventory::load(&root)?;
    let event = a.val("--event").unwrap_or_default();
    let git_ref = a.val("--ref").unwrap_or_default();
    let sha = a.need("--sha")?;
    let before = a.val("--before").unwrap_or_default();
    let branch = git_ref.strip_prefix("refs/heads/").unwrap_or("");
    let only_group = branch.strip_prefix("proof/");
    // SecretaryIOS: every lagent merge pushes main, and a private repository pays for its minutes, so a push to main (like any
    // push) runs the units the change reaches; the weekly schedule, workflow_dispatch and pull requests run all of them.
    let full = event != "push";
    let changed: Option<Vec<String>> = if full {
        None
    } else {
        let zero = before.is_empty() || before.chars().all(|c| c == '0');
        let base = if zero {
            merge_base(&root, "origin/main", &sha)
        } else {
            Some(before.clone())
        };
        base.and_then(|b| changed_between(&root, &b, &sha).ok())
    };
    // `proof/<unit>`: exactly that unit, on its own runner, whatever the diff says. `main` keeps the full gate.
    let one = if event == "push" {
        proof_unit(&units, branch)
    } else {
        None
    };
    let single = one.is_some();
    let chosen = one.unwrap_or_else(|| select(&units, changed.as_deref(), only_group));
    // SecretaryIOS: one job, and so one runner, per proof unit (docs/ci/VERUS.md). Unit names are [a-z0-9_/-] only.
    let rows: BTreeSet<(String, String, String)> = chosen
        .iter()
        .map(|u| (u.name.clone(), u.group.clone(), u.phase().to_string()))
        .collect();
    let items: Vec<String> = rows
        .iter()
        .map(|(n, g, p)| format!("{{\"unit\":\"{n}\",\"group\":\"{g}\",\"phase\":\"{p}\"}}"))
        .collect();
    let matrix = format!("{{\"include\":[{}]}}", items.join(","));
    let plan: String = chosen.iter().map(|u| format!("{}\n", u.name)).collect();
    let scope = if single {
        "unit"
    } else if changed.is_none() {
        "all"
    } else {
        "affected"
    };
    fs::write(a.need("--plan")?, &plan).map_err(|e| e.to_string())?;
    let line = format!(
        "matrix={matrix}\nscope={scope}\ncount={}\nany={}\n",
        chosen.len(),
        !chosen.is_empty()
    );
    match a.val("--output") {
        Some(out) => fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(out)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()))
            .map_err(|e| e.to_string())?,
        None => print!("{line}"),
    }
    println!(
        "plan: {scope}, {} of {} units, {} jobs",
        chosen.len(),
        units.len(),
        rows.len()
    );
    Ok(0)
}

fn spec_for(
    unit: &Unit,
    b: &runner::Budget,
    tc: &runner::Toolchain,
    out: &str,
    timeout_s: u64,
) -> Spec {
    Spec {
        unit: unit.name.clone(),
        group: unit.group.clone(),
        kind: unit.kind.clone(),
        target: unit.target.clone(),
        command: unit.command(),
        out: out.to_string(),
        cap_bytes: b.cap_bytes,
        class: b.class.to_string(),
        mem_total: b.mem_total,
        timeout_s,
        verus: tc.verus.clone(),
        version: tc.version.clone(),
        verus_sha: tc.verus_sha.clone(),
        verifier_sha: tc.verifier_sha.clone(),
        rust: tc.rust.clone(),
        z3: tc.z3.clone(),
    }
}

fn budget_from(a: &Args) -> Result<runner::Budget, String> {
    let small = std::env::var("VERUS_SMALL_RUNNER_CAP_GIB")
        .ok()
        .filter(|s| !s.is_empty());
    let small = small
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| "VERUS_SMALL_RUNNER_CAP_GIB: not a number".to_string())
        })
        .transpose()?;
    runner::budget(a.num("--mem-cap-gib", 12)?, small, runner::mem_total()?)
}

/// One line per unit: what ran, how long, how much.
fn print_line(out: &str, u: &str) -> Option<String> {
    let json =
        fs::read_to_string(Path::new(out).join(format!("{}.json", u.replace('/', "__")))).ok()?;
    let num = |k: &str| -> f64 {
        json.find(&format!("\"{k}\":"))
            .and_then(|i| {
                json[i + k.len() + 3..]
                    .split([',', '}'])
                    .next()
                    .and_then(|s| s.parse().ok())
            })
            .unwrap_or(0.0)
    };
    let outcome = runner::report_field(&json, "outcome")?;
    Some(format!(
        "{outcome:<16} {u}  {:.1}s  peak {:.2} GiB  verified {}",
        num("duration_s"),
        num("peak_bytes") / (1u64 << 30) as f64,
        num("verified")
    ))
}

fn cmd_run(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    std::env::set_current_dir(&root).map_err(|e| e.to_string())?;
    let units = inventory::load(&root)?;
    let wanted: Option<BTreeSet<String>> = a
        .val("--plan")
        .map(|p| fs::read_to_string(p).map(|t| t.lines().map(str::to_string).collect()))
        .transpose()
        .map_err(|e| e.to_string())?;
    let group = a.val("--group");
    let phase = a.val("--phase");
    let name = a.val("--unit");
    let chosen: Vec<&Unit> = units
        .iter()
        .filter(|u| group.as_ref().is_none_or(|g| &u.group == g))
        .filter(|u| phase.as_ref().is_none_or(|p| u.phase() == p))
        .filter(|u| name.as_ref().is_none_or(|n| &u.name == n))
        .filter(|u| wanted.as_ref().is_none_or(|w| w.contains(&u.name)))
        .collect();
    if chosen.is_empty() {
        return Err("no proof unit matches".into());
    }
    let b = budget_from(a)?;
    let verus = a.need("--verus")?;
    let tc = runner::check_toolchain(&root, &verus)?;
    let out = a.need("--out")?;
    let timeout_s = a.num("--unit-timeout-min", 30)? * 60;
    let mut worst = 0;
    println!(
        "cap {} GiB ({}), MemTotal {:.1} GiB, verus {}",
        b.cap_bytes >> 30,
        b.class,
        b.mem_total as f64 / (1u64 << 30) as f64,
        tc.version
    );
    for u in chosen {
        let code = runner::run_unit(&spec_for(u, &b, &tc, &out, timeout_s))?;
        println!(
            "{}",
            print_line(&out, &u.name).unwrap_or_else(|| format!("no report {}", u.name))
        );
        if code != 0 {
            let log = Path::new(&out).join(format!("{}.log", u.name.replace('/', "__")));
            let t = fs::read_to_string(&log).unwrap_or_default();
            let tail: Vec<&str> = t.lines().rev().take(25).collect();
            for l in tail.iter().rev() {
                println!("    {l}");
            }
        }
        worst = worst.max(code);
    }
    Ok(worst)
}

/// Touches `mib` of memory and waits: run as a grandchild to prove that the whole process tree counts against the cap.
fn cmd_alloc(a: &Args) -> Result<i32, String> {
    let mib = a.num("--mib", 0)? as usize;
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    for _ in 0..mib / 16 {
        let mut v = vec![0u8; 16 << 20];
        for p in v.iter_mut().step_by(4096) {
            *p = 1;
        }
        blocks.push(v);
    }
    println!("allocated {} MiB", blocks.len() * 16);
    Ok(0)
}

/// What must be true of the machinery itself, on this machine. Each case is a real run through the same path as a proof.
fn cmd_selftest(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    std::env::set_current_dir(&root).map_err(|e| e.to_string())?;
    let b = budget_from(a)?;
    let out = a.need("--out")?;
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .display()
        .to_string();
    let verus = a.val("--verus");
    let tc = match &verus {
        Some(v) => runner::check_toolchain(&root, v)?,
        None => runner::Toolchain {
            verus: String::new(),
            version: String::new(),
            verus_sha: String::new(),
            verifier_sha: String::new(),
            rust: String::new(),
            z3: String::new(),
        },
    };
    let full_mib = b.cap_bytes >> 20;
    let mut cases: Vec<(String, String, String, &str, u64, u64)> = vec![
        // name, kind, command, expected outcome, cap MiB, timeout s
        (
            "selftest/oom-grandchild".into(),
            "script".into(),
            format!("bash -c '{exe} __alloc --mib 700; true'; true"),
            "oom",
            256,
            120,
        ),
        (
            "selftest/under-cap".into(),
            "script".into(),
            format!("{exe} __alloc --mib 64"),
            "verified",
            256,
            120,
        ),
        (
            "selftest/timeout".into(),
            "script".into(),
            "sleep 120".into(),
            "timeout",
            256,
            3,
        ),
        (
            "selftest/oom-at-real-cap".into(),
            "script".into(),
            format!("{exe} __alloc --mib {}", full_mib + 512),
            "oom",
            full_mib,
            300,
        ),
    ];
    if verus.is_some() {
        for (f, want) in [
            ("good", "verified"),
            ("bad", "proof_failed"),
            ("vacuous", "vacuous"),
        ] {
            cases.push((
                format!("selftest/verus-{f}"),
                "fixture".into(),
                format!("\"$VERUS\" tools/ci/fixtures/{f}.rs"),
                want,
                full_mib,
                300,
            ));
        }
    }
    let mut failed = 0;
    for (name, kind, command, want, cap, timeout_s) in cases {
        let unit = Unit {
            name: name.clone(),
            group: "selftest".into(),
            kind,
            target: command.clone(),
            inputs: vec![],
        };
        let mut s = spec_for(&unit, &b, &tc, &out, timeout_s);
        s.command = command;
        s.cap_bytes = cap << 20;
        runner::run_unit(&s)?;
        let got =
            fs::read_to_string(Path::new(&out).join(format!("{}.json", name.replace('/', "__"))))
                .ok()
                .and_then(|j| runner::report_field(&j, "outcome"))
                .unwrap_or_else(|| "no report".into());
        let ok = got == want;
        println!(
            "{} {name}: wanted {want}, got {got}",
            if ok { "ok  " } else { "FAIL" }
        );
        failed += !ok as i32;
    }
    Ok((failed > 0) as i32)
}

/// Every planned unit has a report, from this commit, saying verified. Anything else fails, a missing report above all.
fn cmd_gate(a: &Args) -> Result<i32, String> {
    let plan = fs::read_to_string(a.need("--plan")?).map_err(|e| format!("plan: {e}"))?;
    let dir = a.need("--reports")?;
    let sha = a.need("--sha")?;
    let mut reports = BTreeMap::new();
    for e in fs::read_dir(&dir)
        .map_err(|e| format!("reports: {e}"))?
        .flatten()
    {
        if e.path().extension().is_some_and(|x| x == "json") {
            let t = fs::read_to_string(e.path()).unwrap_or_default();
            if let Some(u) = runner::report_field(&t, "unit") {
                reports.insert(u, t);
            }
        }
    }
    let planned: Vec<&str> = plan.lines().filter(|l| !l.is_empty()).collect();
    let mut bad = 0;
    let mut small = 0;
    for u in &planned {
        let verdict = match reports.get(*u) {
            None => "missing report".to_string(),
            Some(t) if runner::report_field(t, "commit").as_deref() != Some(sha.as_str()) => {
                "report is from another commit".to_string()
            }
            Some(t) => {
                if runner::report_field(t, "runner_class").as_deref() == Some("small-runner") {
                    small += 1;
                }
                runner::report_field(t, "outcome").unwrap_or_else(|| "no outcome".into())
            }
        };
        if verdict != "verified" {
            println!("NOT VERIFIED {u}: {verdict}");
            bad += 1;
        }
    }
    println!(
        "{} planned, {} not verified, {} on a small runner",
        planned.len(),
        bad,
        small
    );
    Ok((bad > 0) as i32)
}

fn parse_runs(text: &str) -> Vec<CheckRun> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() == 4).then(|| CheckRun {
                name: f[0].into(),
                status: f[1].into(),
                conclusion: f[2].into(),
                app: f[3].into(),
            })
        })
        .collect()
}

/// Do both sides of a merge touch the input of one proof unit? Then the branch's result says nothing about the merged tree.
fn semantic_gap(root: &Path, base: &str, head: &str) -> Result<Vec<String>, String> {
    let units = inventory::load(root)?;
    let mb = merge_base(root, base, head).ok_or("no merge base")?;
    if mb == base {
        return Ok(Vec::new());
    }
    let ours = changed_between(root, &mb, head)?;
    let theirs = changed_between(root, &mb, base)?;
    Ok(units
        .iter()
        .filter(|u| {
            ours.iter().any(|p| u.affected_by(p)) && theirs.iter().any(|p| u.affected_by(p))
        })
        .map(|u| u.name.clone())
        .collect())
}

/// Exit 0 only when GitHub Actions verified `--sha` and no semantic gap exists.
fn cmd_merge_check(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    let repo = a.need("--repo")?;
    let sha = a.need("--sha")?;
    let required: Vec<String> = if a.has("--require") {
        a.vals("--require")
    } else {
        REQUIRED_CHECKS.iter().map(|s| s.to_string()).collect()
    };
    let jq = r#".check_runs | sort_by(.id) | reverse | .[] | [.name, .status, (.conclusion // ""), .app.slug] | @tsv"#;
    let path = format!("repos/{repo}/commits/{sha}/check-runs?per_page=100");
    let o = Command::new("gh")
        .args(["api", "--paginate", &path, "--jq", jq])
        .output()
        .map_err(|e| format!("cannot run gh: {e}"))?;
    if !o.status.success() {
        println!(
            "merge-check: GitHub did not answer for {sha}: refusing (a missing result is not a pass)"
        );
        return Ok(1);
    }
    let runs = parse_runs(&String::from_utf8_lossy(&o.stdout));
    match verdict::merge_verdict(&runs, &required) {
        MergeVerdict::Pass => {}
        MergeVerdict::Pending(w) => {
            println!("merge-check: still running ({w}): wait for CI, then run lagent merge again");
            return Ok(1);
        }
        MergeVerdict::Refuse(w) => {
            println!("merge-check: refused: {w}");
            return Ok(1);
        }
    }
    if let Some(base) = a.val("--base") {
        let gap = semantic_gap(&root, &base, &sha)?;
        if !gap.is_empty() {
            println!(
                "merge-check: main moved in the inputs of {} unit(s) this branch also changes (first: {}): merge main into the branch, push, let CI run",
                gap.len(),
                gap[0]
            );
            return Ok(1);
        }
    }
    println!("merge-check: {} verified {sha}", required.join(", "));
    Ok(0)
}

/// Members that can build from this checkout alone: those with no absolute path or `file://` git dependency, and none reaching
/// one through a sibling. `manifests`: member directory -> Cargo.toml text.
fn portable_members(
    manifests: &BTreeMap<String, String>,
    exists: &dyn Fn(&str) -> bool,
) -> (Vec<String>, Vec<String>) {
    let mut bad: BTreeSet<String> = BTreeSet::new();
    let mut edges: Vec<(String, String)> = Vec::new();
    for (dir, text) in manifests {
        if !exists(&format!("{dir}/Cargo.toml")) {
            bad.insert(dir.clone());
        }
        for line in text.lines() {
            let l = line.trim_start();
            if l.starts_with('#') {
                continue;
            }
            if l.contains("\"/home/") || l.contains("file:///") {
                bad.insert(dir.clone());
            }
            if let Some(i) = l.find("path = \"") {
                let p: String = l[i + 8..].chars().take_while(|c| *c != '"').collect();
                let mut parts: Vec<&str> = dir.split('/').collect();
                for c in p.split('/') {
                    match c {
                        ".." => {
                            parts.pop();
                        }
                        "." | "" => {}
                        c => parts.push(c),
                    }
                }
                if !exists(&parts.join("/")) {
                    bad.insert(dir.clone());
                }
                edges.push((dir.clone(), parts.join("/")));
            }
        }
    }
    loop {
        let more: Vec<String> = edges
            .iter()
            .filter(|(a, b)| !bad.contains(a) && bad.contains(b))
            .map(|(a, _)| a.clone())
            .collect();
        if more.is_empty() {
            break;
        }
        bad.extend(more);
    }
    let keep = manifests
        .keys()
        .filter(|d| !bad.contains(*d))
        .cloned()
        .collect();
    (keep, bad.into_iter().collect())
}

fn cmd_portable_workspace(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    let path = root.join("Cargo.toml");
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let line = text
        .lines()
        .find(|l| l.starts_with("members = ["))
        .ok_or("Cargo.toml: no members line")?;
    let members: Vec<String> = line
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    let mut manifests = BTreeMap::new();
    for m in &members {
        manifests.insert(
            m.clone(),
            fs::read_to_string(root.join(m).join("Cargo.toml")).unwrap_or_default(),
        );
    }
    let (keep, dropped) = portable_members(&manifests, &|p| root.join(p).exists());
    println!(
        "{} of {} workspace members can build on a runner; {} need sibling repositories and are not checked here:",
        keep.len(),
        members.len(),
        dropped.len()
    );
    for d in &dropped {
        println!("- {d}");
    }
    if a.has("--write") {
        let kept: Vec<String> = keep.iter().map(|m| format!("\"{m}\"")).collect();
        let new = format!("members = [{}]", kept.join(", "));
        fs::write(&path, text.replace(line, &new)).map_err(|e| e.to_string())?;
    }
    if a.has("--delete") {
        // A member some proof reads stays on disk, outside the workspace: removing it would remove the proof.
        let mut shipped =
            fs::read_to_string(root.join("scripts/verify-shipped.sh")).unwrap_or_default();
        for f in String::from_utf8_lossy(
            &Command::new("git")
                .args(["ls-files", "verification"])
                .current_dir(&root)
                .output()
                .map_err(|e| e.to_string())?
                .stdout,
        )
        .lines()
        {
            shipped.push_str(&fs::read_to_string(root.join(f)).unwrap_or_default());
        }
        for d in &dropped {
            if !shipped.contains(&format!("{d}/")) && root.join(d).exists() {
                fs::remove_dir_all(root.join(d)).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(0)
}

/// `--rules F --out D`: the public copy of the tracked files. Nothing is pushed.
fn cmd_snapshot(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    let rules =
        snapshot::parse_rules(&fs::read_to_string(a.need("--rules")?).map_err(|e| e.to_string())?)?;
    let out = PathBuf::from(a.need("--out")?);
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let (copied, left_out) = snapshot::build(&root, &rules, &out)?;
    println!("{copied} files copied, {left_out} left out");
    Ok(0)
}

fn main() -> ExitCode {
    let mut all: Vec<String> = std::env::args().skip(1).collect();
    if all.is_empty() {
        eprintln!("usage: lucina-ci inventory|plan|run|selftest|gate|merge-check ...");
        return ExitCode::from(2);
    }
    let cmd = all.remove(0);
    if cmd == "__inner" {
        return ExitCode::from(runner::inner(&all) as u8);
    }
    let a = Args(all);
    let r = match cmd.as_str() {
        "inventory" => cmd_inventory(&a),
        "plan" => cmd_plan(&a),
        "run" => cmd_run(&a),
        "selftest" => cmd_selftest(&a),
        "gate" => cmd_gate(&a),
        "merge-check" => cmd_merge_check(&a),
        "portable-workspace" => cmd_portable_workspace(&a),
        "snapshot" => cmd_snapshot(&a),
        "check-fast" => cmd_check_fast(&a),
        "perf" => cmd_perf(&a),
        "__alloc" => cmd_alloc(&a),
        _ => Err(format!("unknown command {cmd}")),
    };
    match r {
        Ok(c) => ExitCode::from(c as u8),
        Err(e) => {
            eprintln!("lucina-ci: {e}");
            ExitCode::from(2)
        }
    }
}

/// One step of `check-fast` under its own scope. Returns (passed, seconds). `waived` are the lead's
/// waivers taken for this step: counted on its line, each one printed under it.
/// `gate_limit` is the merge-gate limit actually enforced on this step: `Some` prints it beside the measured time
/// (`ok   clippy   97.3s / 240s`) and `near-limit` when the step used more than 80% of it. The columns before it are exactly
/// what they were, so nothing that reads the line has to change.
#[allow(clippy::too_many_arguments)]
fn fast_step(
    name: &str,
    command: &str,
    kind: &str,
    timeout_s: u64,
    gate_limit: Option<u64>,
    b: &runner::Budget,
    tc: &runner::Toolchain,
    out: &str,
    waived: &[String],
) -> Result<(bool, f64), String> {
    let unit = Unit {
        name: format!("fast/{name}"),
        group: "fast".into(),
        kind: kind.into(),
        target: name.into(),
        inputs: vec![],
    };
    let mut s = spec_for(&unit, b, tc, out, timeout_s);
    s.command = command.to_string();
    let t = std::time::Instant::now();
    let code = runner::run_unit(&s)?;
    let secs = t.elapsed().as_secs_f64();
    let ok = code == 0;
    let mut line = format!(
        "{} {name:<28} {secs:>6.1}s",
        if ok { "ok  " } else { "FAIL" }
    );
    if let Some(l) = gate_limit {
        line.push_str(&format!(" / {l}s"));
        if ok && secs > MERGE_NEAR_LIMIT_SHARE * l as f64 {
            line.push_str("  near-limit");
        }
    }
    if !waived.is_empty() {
        line.push_str(&format!("  ({} waived)", waived.len()));
    }
    println!("{line}");
    for w in waived {
        println!("    waived: {w}");
    }
    if !ok {
        let t = fs::read_to_string(
            Path::new(out).join(format!("fast__{}.log", name.replace('/', "__"))),
        )
        .unwrap_or_default();
        for l in t.lines().rev().take(30).collect::<Vec<_>>().iter().rev() {
            println!("    {l}");
        }
    }
    Ok((ok, secs))
}

fn tool_exists(t: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {t}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The merge-gate limit of one step, in seconds, beside the default of 180. Two steps do not fit 180 s on a loaded nezopt, and
/// a gate that fails on the clock while the tests pass teaches no one anything, so each is named here with the room it needs and
/// the measurement that set it. A limit is `ceil(p95 x 1.5)` over the completed merge-gate runs of that step, applied as a
/// floor-raiser only: it goes up where measurement asks for more, and a number measurement does not argue against stays.
/// The week of merges 2026-11-11..17 on nezopt, read-only (finding GATE-02, `/home/nez/scratch/proof-workflow/gate-02-step-times.md`
/// §3, §5, §6): `clippy` p95 151.6 s on 27 completed runs, slowest clean run 162.2 s, 3 kills at the fatal column and 3 at the
/// skipped one; `unit-tests` p95 154.5 s on 23 completed runs, slowest 179.9 s, 4 and 5 kills. Every kill is recorded at
/// 180.5-180.7 s, which is the wall and says nothing about what lies past it, while all 50 completed runs of the two steps are
/// under 240 s. So 240 is 1.58x / 1.55x the measured p95, 1.48x / 1.33x the slowest clean run, and 60 s below the step's own
/// timeout: the merge limit fires before the step's own timeout does.
/// A limit never lifts a step above its own timeout (the `t` of its `step(...)` call): it only ever cuts. `clippy` is built with
/// 300 s (`fast::cargo_steps`), so its 240 is a real cut and takes effect; a step of its own 60 s, `fmt` or `diffgate`, keeps
/// printing 60 whatever the table or the override says.
/// `unit-tests` at 480 s is TEMPORARY and steps outside both rules above: it is not `ceil(p95 x 1.5)` of a measured week, and the
/// step's own timeout was raised to 480 s with it (`fast::cargo_steps`), because a table limit of 480 on a step of its own 300
/// still cuts at 300 and would have changed nothing. 480 s until LB-C027's bq gate slot is installed and proven, then the limit
/// is re-measured in the slot and set so the slowest step uses at most 60 % of it. The reason is `lucina-server`: a root workspace
/// member, so the plain `unit-tests` step is what the gate runs for it, and its 556 tests take 128 s at load 8 while at load in
/// the high twenties they run over 240 s; three merges (JD-C018, LM-C008, CO-C003) failed or wait on this step, JD-C018 three
/// times. Coordinator allow 2026-11-18 (GATE-02).
const MERGE_STEP_LIMITS: &[(&str, u64)] = &[
    ("clippy", 240),     // measured p95 151.6 s, merges 2026-11-11..17, GATE-02
    ("unit-tests", 480), // TEMPORARY until LB-C027's bq gate slot; coordinator allow 2026-11-18, GATE-02
];
/// The limit of a step named in no table and no override.
const MERGE_STEP_LIMIT_DEFAULT: u64 = 180;
/// A step that used more than this share of its limit prints `near-limit`, so the next raise of a number comes from a line in
/// the log rather than from a merge that failed.
const MERGE_NEAR_LIMIT_SHARE: f64 = 0.8;

/// `LUCINA_CHECK_FAST_MERGE_CAP_S`: the gate's own override of every limit above.
/// A plain number is the limit of EVERY step (the semantics since 2026-11-12, and what a test injects so it never sleeps a real
/// 180 s); `step=SECONDS` entries separated by commas give one step another limit and win over a plain number beside them, which
/// is how a test drives a step the table names. Tokens that are neither are ignored.
#[derive(Default)]
struct MergeCaps {
    plain: Option<u64>,
    per_step: Vec<(String, u64)>,
}

impl MergeCaps {
    /// The override as read from the environment.
    fn from_env() -> Self {
        Self::parse(&std::env::var("LUCINA_CHECK_FAST_MERGE_CAP_S").unwrap_or_default())
    }

    fn parse(text: &str) -> Self {
        let mut c = MergeCaps::default();
        for tok in text.split(',') {
            let tok = tok.trim();
            if let Some((name, secs)) = tok.split_once('=') {
                if let Ok(s) = secs.trim().parse::<u64>() {
                    c.per_step.push((name.trim().to_string(), s));
                }
            } else if let Ok(s) = tok.parse::<u64>() {
                c.plain = Some(s);
            }
        }
        c
    }

    /// The most `name` may run at the merge gate: its own `step=SECONDS` override, then the override of every step, then the
    /// table, then `default`. Unlike LucinaOS this tree has no list of required steps, so nothing escapes the limit; the limit
    /// only ever cuts, so a step of its own shorter timeout is never lifted to these numbers.
    fn limit_of(&self, name: &str, default: u64) -> u64 {
        if let Some((_, s)) = self.per_step.iter().find(|(n, _)| n == name) {
            return *s;
        }
        if let Some(p) = self.plain {
            return p;
        }
        if let Some((_, s)) = MERGE_STEP_LIMITS.iter().find(|(n, _)| *n == name) {
            return *s;
        }
        default
    }
}

/// The merge-gate limit of one step: what `LUCINA_CHECK_FAST_MERGE_CAP_S` says, else the table, else 180.
/// The clock is the step's OWN run time: `scripts/check-fast` takes its queue slot (`bq run --class short --max-wait ...`) before
/// this binary starts, so a step's limit starts after the wait.
fn merge_step_limit(name: &str) -> u64 {
    MergeCaps::from_env().limit_of(name, merge_step_cap())
}

/// The default merge-gate limit: `LUCINA_CHECK_FAST_MERGE_CAP_S` when it is one plain number, else 180. It is what a step no
/// override and no table row names gets; `merge_step_limit` resolves a named one.
fn merge_step_cap() -> u64 {
    MergeCaps::from_env()
        .plain
        .unwrap_or(MERGE_STEP_LIMIT_DEFAULT)
}

/// One step of `check-fast`, with the merge-gate limit applied. `Some(entry)` is a failure. At the merge gate a step past its limit
/// is a failure that says so (`past its N s limit at merge`), never a skip: the limit is `merge_step_limit` for the step — 180 by
/// default, more for the two steps of `MERGE_STEP_LIMITS`, anything the override sets — and it never lifts the step above its own
/// timeout. This tree has no list of required steps, so the limit applies to every step, as it did before.
#[allow(clippy::too_many_arguments)]
fn gated_step(
    name: &str,
    cmd: &str,
    kind: &str,
    t: u64,
    merge_gate: bool,
    b: &runner::Budget,
    tc: &runner::Toolchain,
    out: &str,
    waived: &[String],
) -> Result<Option<String>, String> {
    let t = if merge_gate {
        t.min(merge_step_limit(name))
    } else {
        t
    };
    if fast_step(
        name,
        cmd,
        kind,
        t,
        merge_gate.then_some(t),
        b,
        tc,
        out,
        waived,
    )?
    .0
    {
        return Ok(None);
    }
    let json =
        fs::read_to_string(Path::new(out).join(format!("fast__{}.json", name.replace('/', "__"))))
            .unwrap_or_default();
    if merge_gate && runner::report_field(&json, "outcome").as_deref() == Some("timeout") {
        let m = format!(
            "{name}: past its {t} s limit at merge: the machine is loaded; retry when it is quieter, or run scripts/check-fast in the worktree"
        );
        println!("    {m}");
        return Ok(Some(m));
    }
    Ok(Some(name.to_string()))
}

/// lagent's own `--allow` rules: they waive something of lagent's (a pipeline file, a spec
/// rewording), never a diffgate rule. This mirrors `is_gate_change` in
/// crates/lucina-agentd/src/pipeline/repo.rs, which is exactly the set lagent keeps out of the
/// `--allow` it hands its own diffgate run, so the two diffgate runs of a merge honour the same set.
const LAGENT_OWN_RULES: &[&str] = &["gate-change", "spec-change"];

fn lagent_own_rule(entry: &str) -> bool {
    LAGENT_OWN_RULES
        .iter()
        .any(|r| entry == *r || entry.strip_prefix(*r).is_some_and(|t| t.starts_with('=')))
}

/// One diffgate waiver out of `LAGENT_ALLOW`, or out of the job's waivers file: what diffgate is handed as one `--allow`.
#[derive(Clone)]
struct Waiver {
    /// The entry as the lead typed it: what diffgate gets as `--allow`, quoted.
    entry: String,
    rule: String,
    reason: String,
}

/// The lead's waivers, as `lagent merge` puts them in the environment of every gate command but its
/// own diffgate: `LAGENT_ALLOW`, one `RULE=reason` entry per line, each as given
/// (crates/lucina-agentd/src/pipeline/repo.rs `allow_env`). An empty variable or an empty line is no
/// waiver, one of lagent's own rules is no diffgate waiver, and `Err` names every remaining entry
/// that is not `RULE=reason`: a typo in a waiver fails the step, it never waives nothing quietly.
fn lead_waivers(raw: &str) -> Result<Vec<Waiver>, Vec<String>> {
    let mut taken = Vec::new();
    let mut malformed = Vec::new();
    for line in raw.lines() {
        let entry = line.trim();
        if entry.is_empty() || lagent_own_rule(entry) {
            continue;
        }
        match entry.split_once('=') {
            Some((rule, reason))
                if !rule.is_empty()
                    && !reason.trim().is_empty()
                    && rule
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') =>
            {
                taken.push(Waiver {
                    entry: entry.to_string(),
                    rule: rule.to_string(),
                    reason: reason.trim().to_string(),
                })
            }
            _ => malformed.push(entry.to_string()),
        }
    }
    if malformed.is_empty() {
        Ok(taken)
    } else {
        Err(malformed)
    }
}

/// The merge waivers of the job that runs this gate (LA-C014): `lagent merge --allow rule=reason` writes
/// `<job dir>/waivers.jsonl` — one JSON object per line with exactly the four string fields `rule`, `reason`, `by`, `job`
/// (`crates/lucina-agentd/src/pipeline/repo.rs` `waiver_lines`) — and names it in `LAGENT_WAIVERS_FILE` for its gate
/// commands. The variable is set only when the job was given at least one `--allow`, so an ordinary run of this loop reads
/// nothing. Why: the merge gate waived its refusals and the `diffgate` step of `check-fast --merge-gate` refused the very
/// same lines again with "0 waived" (finding GATE-01, job_4ee819df2427: line 9 the allow it was given, line 160 the refusal).
const WAIVERS_FILE_ENV: &str = "LAGENT_WAIVERS_FILE";
/// Which waiver rules the `diffgate` step forwards: one name per line, `#` comments (tools/ci/merge-waiver-rules.txt).
/// Data and not a constant here because adding a rule is a gate change a human reads, like every other file of `tools/ci/`.
const WAIVER_RULES_FILE: &str = "tools/ci/merge-waiver-rules.txt";
/// The fields of a waivers line, all four of them strings and nothing else.
const WAIVER_FIELDS: [&str; 4] = ["rule", "reason", "by", "job"];

fn json_skip_ws(b: &[char], i: &mut usize) {
    while b.get(*i).is_some_and(|c| c.is_ascii_whitespace()) {
        *i += 1;
    }
}

/// The four hex digits of a `\u` escape (read past the `u`), a surrogate pair joined as JSON says it must.
fn json_codepoint(b: &[char], i: &mut usize) -> Result<char, String> {
    let hex4 = |i: &mut usize| -> Result<u32, String> {
        let d: String = b
            .get(*i..*i + 4)
            .ok_or("\\u needs four hex digits")?
            .iter()
            .collect();
        let v = u32::from_str_radix(&d, 16).map_err(|_| "\\u needs four hex digits")?;
        *i += 4;
        Ok(v)
    };
    let hi = hex4(i)?;
    if !(0xd800..0xdc00).contains(&hi) {
        return char::from_u32(hi).ok_or_else(|| "\\u names no character".to_string());
    }
    if b.get(*i) != Some(&'\\') || b.get(*i + 1) != Some(&'u') {
        return Err("a high surrogate needs its low pair".into());
    }
    *i += 2;
    let lo = hex4(i)?;
    if !(0xdc00..0xe000).contains(&lo) {
        return Err("a high surrogate needs its low pair".into());
    }
    char::from_u32(0x10000 + (hi - 0xd800) * 0x400 + (lo - 0xdc00))
        .ok_or_else(|| "\\u names no character".to_string())
}

/// One JSON string at `i`: the six short escapes, a backslash-u codepoint, and no raw control character.
fn json_string(b: &[char], i: &mut usize) -> Result<String, String> {
    if b.get(*i) != Some(&'"') {
        return Err("not a string".into());
    }
    *i += 1;
    let mut out = String::new();
    loop {
        let Some(&c) = b.get(*i) else {
            return Err("unterminated string".into());
        };
        *i += 1;
        match c {
            '"' => return Ok(out),
            '\\' => {
                let Some(&e) = b.get(*i) else {
                    return Err("unterminated escape".into());
                };
                *i += 1;
                match e {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => out.push(json_codepoint(b, i)?),
                    _ => return Err(format!("unknown escape \\{e}")),
                }
            }
            c if (c as u32) < 0x20 => return Err("an unescaped control character".into()),
            c => out.push(c),
        }
    }
}

/// One flat JSON object whose every value is a string, as pairs in the order written. A number, a nested object or array, a
/// missing colon, a trailing comma or text after the closing brace is an error: what this gate cannot read exactly it does
/// not act on. Strict and hand-written, because this crate has no dependencies.
fn json_object(b: &[char], i: &mut usize) -> Result<Vec<(String, String)>, String> {
    json_skip_ws(b, i);
    if b.get(*i) != Some(&'{') {
        return Err("not a JSON object".into());
    }
    *i += 1;
    let mut out = Vec::new();
    json_skip_ws(b, i);
    if b.get(*i) == Some(&'}') {
        *i += 1;
        return Ok(out);
    }
    loop {
        json_skip_ws(b, i);
        let k = json_string(b, i).map_err(|e| format!("field name: {e}"))?;
        json_skip_ws(b, i);
        if b.get(*i) != Some(&':') {
            return Err(format!("`{k}` has no value after its name"));
        }
        *i += 1;
        json_skip_ws(b, i);
        if b.get(*i) != Some(&'"') {
            return Err(format!("the value of `{k}` is not a string"));
        }
        let v = json_string(b, i)?;
        out.push((k, v));
        json_skip_ws(b, i);
        match b.get(*i) {
            Some(',') => *i += 1,
            Some('}') => {
                *i += 1;
                return Ok(out);
            }
            _ => {
                return Err(format!(
                    "`{}` after a field, expected `,` or `}}`",
                    b.get(*i).map_or("end".into(), |c| c.to_string())
                ));
            }
        }
    }
}

/// One waivers line: an object of exactly the four string fields, with a reason that says something. The reason is kept
/// exactly as the merge job wrote it, so what `lagent merge --allow` was given is what diffgate gets.
fn parse_waiver_line(line: &str) -> Result<Waiver, String> {
    let b: Vec<char> = line.chars().collect();
    let mut i = 0;
    let pairs = json_object(&b, &mut i)?;
    json_skip_ws(&b, &mut i);
    if i != b.len() {
        return Err("text after the closing `}`".into());
    }
    let names: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
    for f in WAIVER_FIELDS {
        if !names.contains(&f) {
            return Err(format!("no `{f}` field"));
        }
    }
    if pairs.len() != WAIVER_FIELDS.len() {
        return Err(format!(
            "{} fields where a waivers line holds {}",
            pairs.len(),
            WAIVER_FIELDS.len()
        ));
    }
    let field = |f: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == f)
            .map_or(String::new(), |(_, v)| v.clone())
    };
    let reason = field("reason");
    if reason.trim().is_empty() {
        return Err("`reason` is empty".into());
    }
    let rule = field("rule");
    Ok(Waiver {
        entry: format!("{rule}={reason}"),
        rule,
        reason,
    })
}

/// One line of the forwarding list: a rule, and whether its waivers travel only in diffgate's counted form.
#[derive(Debug)]
struct ForwardRule {
    rule: String,
    /// The `counted` marker: diffgate takes `--allow <rule>=N:<reason>`, and only that form is forwarded.
    counted_only: bool,
}

/// A reason in diffgate's counted form: a decimal integer of at least 1 and the colon that follows it.
fn counted_reason(reason: &str) -> bool {
    let digits = reason
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(reason.len());
    digits > 0
        && reason.as_bytes().get(digits) == Some(&b':')
        && reason[..digits].parse::<u64>().is_ok_and(|n| n >= 1)
}

/// The list text as the step reads it: a rule name and at most one `counted` marker per line, `#` comments and blank lines
/// ignored. `path` is only named in the message. Any other word is `Err`, because a marker this step cannot read is a
/// condition the lead put on a waiver, and reading it as no condition would forward more than was allowed.
fn parse_waiver_rules(text: &str, path: &Path) -> Result<Vec<ForwardRule>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let rule = words.next().unwrap_or_default().to_string();
        let counted_only = match words.next() {
            None => false,
            Some("counted") => match words.next() {
                Some(extra) => {
                    return Err(format!(
                        "{} line {}: {rule} carries a second marker {extra:?}: a line is a rule name and at most the word counted",
                        path.display(),
                        n + 1
                    ));
                }
                None => true,
            },
            Some(unknown) => {
                return Err(format!(
                    "{} line {}: unknown marker {unknown:?} after {rule}: a line is a rule name and at most the word counted",
                    path.display(),
                    n + 1
                ));
            }
        };
        out.push(ForwardRule { rule, counted_only });
    }
    Ok(out)
}

/// The rules whose waivers reach diffgate (`WAIVER_RULES_FILE`), read from the repository under test. A list that cannot be
/// read forwards nothing, which leaves every waiver unused and every refusal refused: the safe direction for a gate.
fn forwarded_waiver_rules(root: &Path) -> Result<Vec<ForwardRule>, String> {
    let path = root.join(WAIVER_RULES_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(Vec::new()),
    };
    parse_waiver_rules(&text, &path)
}

/// The waivers this merge job was given: `None` when `LAGENT_WAIVERS_FILE` is not set, which is every run that is not a
/// merge with an allow and leaves the step exactly as it was; `Err` when it is set but is not the file the merge job writes.
/// Every line is read or none is, so a file with one bad line waives nothing.
fn merge_waivers() -> Result<Option<Vec<Waiver>>, String> {
    let Some(p) = std::env::var_os(WAIVERS_FILE_ENV) else {
        return Ok(None);
    };
    let p = PathBuf::from(p);
    let text =
        fs::read_to_string(&p).map_err(|e| format!("{} cannot be read: {e}", p.display()))?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let w =
            parse_waiver_line(line).map_err(|e| format!("{} line {}: {e}", p.display(), n + 1))?;
        out.push(w);
    }
    if out.is_empty() {
        return Err(format!("{} holds no waiver line", p.display()));
    }
    Ok(Some(out))
}

/// The diffgate call of the step: the plain command, plus one `--allow` per waiver. The step runs its command through
/// `bash -c`, so the quoting is here and never the reason's: an apostrophe, `$(...)` or a backtick is content.
fn diffgate_command(base: &str, allows: &[Waiver]) -> String {
    let mut cmd = format!("diffgate --repo . --base {base}");
    for w in allows {
        cmd.push_str(&format!(" --allow {}", shell_quote(&w.entry)));
    }
    cmd
}

/// The first waiver of a rule the list marks `counted` whose reason is not in counted form, as the rule name. The condition
/// is on the rule, whichever source brought the waiver: `LAGENT_ALLOW` travels unfiltered, so an uncounted entry there for a
/// counted rule would hand diffgate exactly the form the coordinator ruled out and, one allow per rule, crowd out the counted
/// one the file holds.
fn uncounted_waiver<'a>(waivers: &'a [Waiver], rules: &[ForwardRule]) -> Option<&'a str> {
    waivers
        .iter()
        .find(|w| {
            rules
                .iter()
                .any(|r| r.rule == w.rule && r.counted_only && !counted_reason(&w.reason))
        })
        .map(|w| w.rule.as_str())
}

/// The step's refusal of an uncounted waiver of a rule the list forwards in counted form only.
fn counted_waiver_refusal(rule: &str) -> String {
    format!(
        "diffgate waivers: {rule} is forwarded in counted form only: write --allow {rule}=N:<reason> with N the exact number of refusals"
    )
}

/// The `--allow` entries the job's waivers file adds to the diffgate call, the reason the step must not run diffgate at all
/// (`Some`, when a waiver breaks a condition the list puts on its rule) and the one line the step prints about them.
/// Only a rule of the forwarding list travels, and its reason unchanged; the quotes are the shell's problem (`shell_quote`),
/// since the step runs its command through `bash -c`. A rule `LAGENT_ALLOW` already handed over is not handed over twice:
/// diffgate answers a waiver with the refusals the earlier entry left, so the second copy of the same allow would report
/// itself a failed waiver ("waiver expects exactly N refusals, found 0") on a diff this merge was allowed to pass.
/// A rule the list marks `counted` travels only as `N:reason`: an uncounted waiver of it is refused outright, because the
/// count is what ties the waiver to the exact refusals the lead looked at, and diffgate never runs with a partial list.
fn file_waiver_allows(
    waivers: &[Waiver],
    rules: &[ForwardRule],
    already: &[Waiver],
) -> (Vec<Waiver>, Option<String>, String) {
    let listed = |rule: &str| rules.iter().any(|r| r.rule == rule);
    let (fwd, kept): (Vec<&Waiver>, Vec<&Waiver>) = waivers.iter().partition(|w| listed(&w.rule));
    let halt = uncounted_waiver(waivers, rules).map(counted_waiver_refusal);
    let added: Vec<Waiver> = if halt.is_some() {
        // The step does not run diffgate at all, so nothing is added: a refusal cannot be read as a partial list.
        Vec::new()
    } else {
        fwd.iter()
            .filter(|w| !already.iter().any(|a| a.rule == w.rule))
            .map(|w| (*w).clone())
            .collect()
    };
    let fwd_list = if fwd.is_empty() {
        String::new()
    } else {
        format!(
            " ({})",
            fwd.iter()
                .map(|w| format!("{}: {}", w.rule, w.reason))
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    let kept_list = if kept.is_empty() {
        String::new()
    } else {
        format!(
            " ({})",
            kept.iter()
                .map(|w| w.rule.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let summary = format!(
        "diffgate waivers: {} forwarded{fwd_list}, {} not forwarded{kept_list}",
        fwd.len(),
        kept.len()
    );
    (added, halt, summary)
}

/// `check-fast [--base REF] [--proofs] [--all-proofs] [--mutants] [--no-build]`
fn cmd_check_fast(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    std::env::set_current_dir(&root).map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    let base_ref = a.val("--base").unwrap_or_else(|| "main".into());
    let base = merge_base(&root, &base_ref, "HEAD")
        .ok_or_else(|| format!("no merge base with {base_ref}"))?;
    let mut changed: BTreeSet<String> =
        changed_between(&root, &base, "HEAD")?.into_iter().collect();
    let git_lines = |args: &[&str]| -> Vec<String> {
        Command::new("git")
            .args(args)
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    changed.extend(git_lines(&["diff", "--name-only", "HEAD"]));
    changed.extend(git_lines(&["ls-files", "-o", "--exclude-standard"]));
    let existing: Vec<String> = changed
        .iter()
        .filter(|p| root.join(p).exists())
        .cloned()
        .collect();
    if changed.is_empty() {
        println!("check-fast: nothing changed against {base_ref}");
        return Ok(0);
    }
    println!(
        "check-fast: {} changed file(s) against {base_ref}",
        changed.len()
    );
    let b = runner::budget(a.num("--mem-cap-gib", 8)?, None, runner::mem_total()?)?;
    let out = std::env::temp_dir()
        .join(format!("check-fast-{}", std::process::id()))
        .display()
        .to_string();
    let none = runner::Toolchain {
        verus: String::new(),
        version: String::new(),
        verus_sha: String::new(),
        verifier_sha: String::new(),
        rust: String::new(),
        z3: String::new(),
    };
    let mut failed: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    // At `lagent merge` a step that runs past its limit FAILS the merge: a gate that reports "0 failed" for tests it did not run is
    // not a gate (2026-11-12: unit-tests skipped at the merge gate under load, job_b6094541ca4c). docs/ci/SHIFT-LEFT.md.
    let merge_gate = a.has("--merge-gate");
    let step = |name: &str,
                cmd: &str,
                kind: &str,
                t: u64,
                tc: &runner::Toolchain,
                waived: &[String],
                failed: &mut Vec<String>|
     -> Result<(), String> {
        if let Some(f) = gated_step(name, cmd, kind, t, merge_gate, &b, tc, &out, waived)? {
            failed.push(f);
        }
        Ok(())
    };

    if tool_exists("diffgate") {
        // `lagent merge --allow RULE=reason` hands the waiver to its own diffgate as `--allow` and to every
        // other gate command in `LAGENT_ALLOW`, and this gate command runs diffgate a second time over the
        // same diff. Without the same entries here a waiver could never get through a SecretaryIOS merge:
        // it passes lagent's diffgate and fails this one. The same job also names its waivers file
        // (`LAGENT_WAIVERS_FILE`, LA-C014), whose listed rules are forwarded beside these entries, once each
        // (finding GATE-01). Only the merge gate reads either — an exported `LAGENT_ALLOW` must not let a
        // worker or a lane verify step waive diffgate for itself.
        let raw = std::env::var("LAGENT_ALLOW").unwrap_or_default();
        let mut waived: Vec<String> = Vec::new();
        let mut refused: Vec<String> = Vec::new();
        // What diffgate gets, in the order it is put on its command line.
        let mut allows: Vec<Waiver> = Vec::new();
        let mut summary: Option<String> = None;
        // Why the step fails without running diffgate: a waivers file or a list line this step cannot read, or a waiver that
        // breaks a condition the list puts on its rule. diffgate never runs with a partial list.
        let mut halt: Option<String> = None;
        if merge_gate {
            match lead_waivers(&raw) {
                Ok(list) => allows = list,
                Err(bad) => refused = bad,
            }
            // With neither a waivers file nor an entry of `LAGENT_ALLOW` this is the plain command the step has always run:
            // the list is read only when there is a waiver to route, so a list line this step cannot read never fails a merge
            // that waives nothing.
            let file = match merge_waivers() {
                Ok(v) => v,
                // A waivers file this step cannot read exactly is not a reason to waive nothing quietly: the step fails,
                // diffgate never runs, and the message says which line of which file is what.
                Err(e) => {
                    halt = Some(format!(
                        "diffgate waivers: {e}: refusing, nothing is waived"
                    ));
                    None
                }
            };
            if halt.is_none() && (file.is_some() || !allows.is_empty()) {
                match forwarded_waiver_rules(&root) {
                    Err(e) => {
                        halt = Some(format!(
                            "diffgate waivers: {e}: refusing, nothing is waived"
                        ))
                    }
                    Ok(list) => {
                        // The condition a counted rule carries holds for the lead's typed entry as well: `LAGENT_ALLOW`
                        // is not filtered by the list, and an uncounted allow there would else reach diffgate anyway.
                        if let Some(rule) = uncounted_waiver(&allows, &list) {
                            halt = Some(counted_waiver_refusal(rule));
                        }
                        if let Some(ws) = file
                            && halt.is_none()
                        {
                            let (added, stop, s) = file_waiver_allows(&ws, &list, &allows);
                            summary = Some(s);
                            match stop {
                                Some(m) => halt = Some(m),
                                None => allows.extend(added),
                            }
                        }
                    }
                }
            }
        } else {
            if !raw.is_empty() {
                println!(
                    "diffgate: LAGENT_ALLOW is set but ignored: only the merge gate takes a lead's allows"
                );
            }
            if std::env::var_os(WAIVERS_FILE_ENV).is_some() {
                println!(
                    "diffgate: {WAIVERS_FILE_ENV} is set but ignored: only the merge gate reads the job's waivers file"
                );
            }
        }
        for entry in &refused {
            println!(
                "diffgate: LAGENT_ALLOW entry {entry:?} is not RULE=reason: nothing is waived and diffgate does not run"
            );
        }
        if let Some(m) = halt {
            step(
                "diffgate",
                &format!("echo {} >&2; exit 1", shell_quote(&m)),
                "script",
                60,
                &none,
                &[],
                &mut failed,
            )?;
        } else if refused.is_empty() {
            for w in &allows {
                waived.push(format!("{} = {}", w.rule, w.reason));
            }
            if let Some(s) = &summary {
                println!("{s}");
            }
            step(
                "diffgate",
                &diffgate_command(&base, &allows),
                "script",
                60,
                &none,
                &waived,
                &mut failed,
            )?;
        } else {
            failed.push("diffgate".into());
        }
    } else {
        skipped.push("diffgate: not installed here".into());
    }
    let members =
        fast::root_members(&fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default());
    let owners: BTreeSet<fast::Owner> = existing
        .iter()
        .filter_map(|p| fast::owner_of(&root, &members, p))
        .collect();
    for o in &owners {
        if let fast::Owner::Other(d) = o {
            skipped.push(format!(
                "{d}: not buildable by the fast loop; its gate runs on merge"
            ));
        }
    }
    if !a.has("--no-build") {
        for (name, cmd, t) in fast::cargo_steps(&owners, &existing) {
            step(&name, &cmd, "script", t, &none, &[], &mut failed)?;
        }
    } else {
        skipped.push("build, clippy and unit tests: --no-build".into());
    }
    let sh: Vec<&str> = existing
        .iter()
        .filter(|p| p.ends_with(".sh"))
        .map(String::as_str)
        .collect();
    if !sh.is_empty() {
        if tool_exists("shellcheck") {
            step(
                "shellcheck",
                &format!("shellcheck -S error {}", sh.join(" ")),
                "script",
                30,
                &none,
                &[],
                &mut failed,
            )?;
        } else {
            skipped.push("shellcheck: not installed here".into());
        }
    }
    if existing.iter().any(|p| p.starts_with(".github/workflows/")) && tool_exists("actionlint") {
        step(
            "actionlint",
            "actionlint -color=false .github/workflows/*.yml",
            "script",
            30,
            &none,
            &[],
            &mut failed,
        )?;
    }
    if tool_exists("gitleaks") {
        let config = if root.join(".gitleaks.toml").exists() {
            "--config .gitleaks.toml"
        } else {
            ""
        };
        let list = existing
            .iter()
            .filter(|p| !p.starts_with("third_party/"))
            .map(|p| format!("'{}'", p.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" ");
        if !list.is_empty() {
            let cmd = format!(
                "for f in {list}; do gitleaks dir --no-banner --redact {config} \"$f\" || exit 1; done"
            );
            step("secrets", &cmd, "script", 60, &none, &[], &mut failed)?;
        }
    } else {
        skipped.push("secret scan: gitleaks not installed here".into());
    }

    // The proofs the change reaches. Inventory first: a new proof must be listed.
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .display()
        .to_string();
    if root.join(inventory::INVENTORY).exists() {
        step(
            "inventory",
            &format!("{exe} inventory --check"),
            "script",
            30,
            &none,
            &[],
            &mut failed,
        )?;
    } else {
        skipped.push("proof inventory: this worktree predates it".into());
    }
    let units = inventory::load(&root).unwrap_or_default();
    let list: Vec<String> = changed.iter().cloned().collect();
    let hits: Vec<&Unit> = units
        .iter()
        .filter(|u| {
            matches!(u.kind.as_str(), "verus" | "shipped")
                || (a.has("--mutants") && u.kind == "mutants")
        })
        .filter(|u| list.iter().any(|p| u.affected_by(p)))
        .collect();
    let limit = if a.has("--all-proofs") {
        usize::MAX
    } else {
        12
    };
    if hits.len() > limit {
        skipped.push(format!("{} of {} affected proofs: over the limit of {limit}, GitHub runs them (or use --all-proofs)", hits.len() - limit, hits.len()));
    }
    if !hits.is_empty() && !a.has("--proofs") {
        skipped.push(format!(
            "{} proofs this change reaches: they run on GitHub (--proofs to run them here)",
            hits.len()
        ));
    } else if !hits.is_empty() {
        match a
            .val("--verus")
            .or_else(|| std::env::var("VERUS").ok())
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| format!("{h}/tools/verus/verus-x86-linux/verus"))
            })
            .filter(|v| Path::new(v).exists())
        {
            None => skipped.push(format!(
                "{} proofs: no Verus on this machine, GitHub runs them",
                hits.len()
            )),
            Some(v) => {
                let tc = runner::check_toolchain(&root, &v)?;
                for u in hits.iter().take(limit) {
                    // One verification at a time on nezopt (verify-gate): never wait for the lock here, the loop must stay fast.
                    let cmd = format!(
                        "flock -n -E 75 /tmp/lucina-verify.lock bash -c {}",
                        shell_quote(&u.command())
                    );
                    let unit_name = u.name.replace('/', "-");
                    let kind = if u.kind == "mutants" {
                        "mutants"
                    } else {
                        "script"
                    };
                    if !fast_step(&unit_name, &cmd, kind, 120, None, &b, &tc, &out, &[])?.0 {
                        let json = fs::read_to_string(
                            Path::new(&out).join(format!("fast__{unit_name}.json")),
                        )
                        .unwrap_or_default();
                        if json.contains("\"exit_code\":75,") {
                            skipped.push(format!(
                                "{}: the verify lock is busy, GitHub runs it",
                                u.name
                            ));
                        } else {
                            failed.push(unit_name);
                        }
                    }
                }
            }
        }
    }
    let _ = fs::remove_dir_all(&out);
    for s in &skipped {
        println!("skipped  {s}");
    }
    println!(
        "check-fast: {:.0}s, {} failed, {} skipped",
        started.elapsed().as_secs_f64(),
        failed.len(),
        skipped.len()
    );
    Ok(!failed.is_empty() as i32)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `perf --baselines F --out D`: each line of F is name, command, max seconds, max peak MiB. Over either limit fails.
fn cmd_perf(a: &Args) -> Result<i32, String> {
    let root = repo_root()?;
    std::env::set_current_dir(&root).map_err(|e| e.to_string())?;
    let out = a.need("--out")?;
    let b = budget_from(a)?;
    let none = runner::check_toolchain(&root, &a.need("--verus")?)?;
    let text = fs::read_to_string(a.need("--baselines")?).map_err(|e| e.to_string())?;
    let mut bad = 0;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 4 {
            return Err(format!("bad baseline line: {line}"));
        }
        let (name, command, max_s, max_mib): (&str, &str, f64, f64) = (
            f[0],
            f[1],
            f[2].parse().map_err(|_| "max seconds")?,
            f[3].parse().map_err(|_| "max MiB")?,
        );
        let unit = Unit {
            name: format!("perf/{name}"),
            group: "perf".into(),
            kind: "script".into(),
            target: name.into(),
            inputs: vec![],
        };
        let mut s = spec_for(&unit, &b, &none, &out, (max_s * 3.0) as u64 + 60);
        s.command = command.to_string();
        runner::run_unit(&s)?;
        let json = fs::read_to_string(Path::new(&out).join(format!("perf__{name}.json")))
            .unwrap_or_default();
        let num = |k: &str| -> f64 {
            json.find(&format!("\"{k}\":"))
                .and_then(|i| {
                    json[i + k.len() + 3..]
                        .split([',', '}'])
                        .next()
                        .and_then(|s| s.parse().ok())
                })
                .unwrap_or(f64::MAX)
        };
        let (secs, mib) = (num("duration_s"), num("peak_bytes") / (1u64 << 20) as f64);
        let ok = runner::report_field(&json, "outcome").as_deref() == Some("verified")
            && secs <= max_s
            && mib <= max_mib;
        println!(
            "{} {name:<24} {secs:>7.1}s / {max_s:.0}s   {mib:>8.0} MiB / {max_mib:.0} MiB",
            if ok { "ok  " } else { "FAIL" }
        );
        bad += !ok as i32;
    }
    Ok((bad > 0) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(name: &str, group: &str, inputs: &[&str]) -> Unit {
        Unit {
            name: name.into(),
            group: group.into(),
            kind: "verus".into(),
            target: "t".into(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn selection() {
        let us = [
            unit("a", "x", &["crates/a/"]),
            unit("b", "y", &["crates/b/"]),
        ];
        assert_eq!(select(&us, None, None).len(), 2, "no diff means everything");
        let changed = vec!["crates/a/src/lib.rs".to_string()];
        assert_eq!(select(&us, Some(&changed), None)[0].name, "a");
        assert_eq!(select(&us, Some(&changed), None).len(), 1);
        assert_eq!(
            select(&us, Some(&changed), Some("y")).len(),
            2,
            "a proof/<group> branch always runs its group"
        );
        assert_eq!(select(&us, Some(&[]), None).len(), 0);
    }

    #[test]
    fn a_member_reaching_a_sibling_repo_is_dropped_transitively() {
        let m: BTreeMap<String, String> = [
            (
                "crates/a",
                "[dependencies]\nx = { path = \"/home/nez/Projects/X/y\" }\n",
            ),
            ("crates/b", "[dependencies]\na = { path = \"../a\" }\n"),
            ("crates/c", "[dependencies]\nb = { path = \"../b\" }\n"),
            (
                "crates/d",
                "[dependencies]\n# x = { path = \"/home/nez/x\" }\ne = { path = \"../e\" }\n",
            ),
            ("crates/e", "[dependencies]\n"),
            (
                "crates/f",
                "[dependencies]\ng = { git = \"file:///home/nez/g\" }\n",
            ),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        let (keep, dropped) = portable_members(&m, &|_| true);
        assert_eq!(keep, vec!["crates/d".to_string(), "crates/e".to_string()]);
        assert_eq!(dropped.len(), 4);
    }

    #[test]
    fn a_member_with_a_removed_path_dep_is_dropped() {
        let m: BTreeMap<String, String> = [
            (
                "crates/a".to_string(),
                "x = { path = \"../gone\" }\n".to_string(),
            ),
            ("crates/b".to_string(), String::new()),
        ]
        .into_iter()
        .collect();
        let (keep, dropped) = portable_members(&m, &|p| p != "crates/gone");
        assert_eq!(keep, vec!["crates/b".to_string()]);
        assert_eq!(dropped, vec!["crates/a".to_string()]);
    }

    #[test]
    fn a_proof_branch_names_one_unit() {
        let us = [
            unit("verus/a/rules", "x", &[]),
            unit("verus/b/rules", "x", &[]),
        ];
        let one = proof_unit(&us, "proof/verus/a/rules").unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].name, "verus/a/rules");
        assert!(
            proof_unit(&us, "proof/x").is_none(),
            "a group is not a unit"
        );
        assert!(proof_unit(&us, "main").is_none());
    }

    #[test]
    fn check_runs_parse() {
        let r = parse_runs("verus-gate\tcompleted\tsuccess\tgithub-actions\nbad line\n");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].conclusion, "success");
    }

    // ---- the merge waivers of a gate job (finding GATE-01). The file is the one `lagent merge` writes, byte for byte:
    // {"rule":"...","reason":"...","by":"...","job":"job_..."} per line. LAGENT_WAIVERS_FILE is process-wide, so what a unit
    // test can judge is the reading of one line and the command it makes; tests/merge_gate_waivers.rs runs the real step.

    /// One waivers line as the merge job writes it, with `by` and `job` from job_4ee819df2427 of the failing merge. The
    /// reason is JSON-escaped the way its writer escapes it, quotes included.
    fn waiver_line(rule: &str, reason: &str) -> String {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        format!(
            "{{\"rule\":\"{}\",\"reason\":\"{}\",\"by\":\"nez@nez_opt\",\"job\":\"job_4ee819df2427\"}}",
            esc(rule),
            esc(reason)
        )
    }

    /// The real allow of job_4ee819df2427 (line 9 of its log), which its diffgate step then refused with "0 waived" (line 160).
    const GAL_REASON: &str = "real-server variants kept as #[ignore = needs LUCINA_MEDIA_BIN], reported ignored not \
                              passed; the same assertions run against the in-process Fake; the ignored variants run in a \
                              scheduled non-gating job; WM-C004 / GAL-01; coordinator allow 2026-11-17";

    fn rules(skipped: bool) -> Vec<ForwardRule> {
        if skipped {
            vec![ForwardRule {
                rule: "skipped-test".to_string(),
                counted_only: false,
            }]
        } else {
            vec![]
        }
    }

    /// The repository this test file sits in: the rules list is data, so the tests read the real one.
    fn this_repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .to_path_buf()
    }

    /// What the step makes of a waivers file, with the reason it refuses to run diffgate at all.
    fn dg_all(
        base: &str,
        allow: &[Waiver],
        file: &[Waiver],
        listed: &[ForwardRule],
    ) -> (String, Option<String>, String) {
        let (added, halt, summary) = file_waiver_allows(file, listed, allow);
        let mut all = allow.to_vec();
        all.extend(added);
        (diffgate_command(base, &all), halt, summary)
    }

    /// The command the step makes from the `LAGENT_ALLOW` entries plus what the file adds, for a case the list does not refuse.
    fn dg(
        base: &str,
        allow: &[Waiver],
        file: &[Waiver],
        listed: &[ForwardRule],
    ) -> (String, String) {
        let (cmd, halt, summary) = dg_all(base, allow, file, listed);
        assert!(halt.is_none(), "the step refuses to run diffgate: {cmd}");
        (cmd, summary)
    }

    /// A forwarding list of the given rules, every one of them plain.
    fn plain_list(rules: &[&str]) -> Vec<ForwardRule> {
        rules
            .iter()
            .map(|r| ForwardRule {
                rule: r.to_string(),
                counted_only: false,
            })
            .collect()
    }

    /// A forwarding list of the given rules, every one of them marked `counted`.
    fn counted_list(rules: &[&str]) -> Vec<ForwardRule> {
        rules
            .iter()
            .map(|r| ForwardRule {
                rule: r.to_string(),
                counted_only: true,
            })
            .collect()
    }

    /// The real list of this repository, read as the step reads it.
    fn real_list() -> Vec<ForwardRule> {
        forwarded_waiver_rules(&this_repo()).expect("the list of this repository is readable")
    }

    #[test]
    fn a_waivers_line_of_the_four_string_fields_is_read() {
        let w = parse_waiver_line(&waiver_line("skipped-test", GAL_REASON)).unwrap();
        assert_eq!(w.rule, "skipped-test");
        assert_eq!(w.reason, GAL_REASON, "the reason is kept as it was given");
        assert_eq!(
            w.entry,
            format!("skipped-test={GAL_REASON}"),
            "the entry diffgate gets"
        );
    }

    #[test]
    fn a_line_that_is_not_a_json_object_is_refused() {
        for (bad, what) in [
            ("", "an empty line"),
            ("not json", "plain text"),
            ("[\"rule\"]", "an array"),
            ("{\"rule\":\"a\" ", "an object with no closing brace"),
            ("{}{\"rule\":\"a\"}", "text before the object"),
        ] {
            assert!(
                parse_waiver_line(bad).is_err(),
                "{what}: {bad:?} was taken for a waiver"
            );
        }
    }

    #[test]
    fn a_line_missing_one_of_the_four_fields_is_refused() {
        for missing in WAIVER_FIELDS {
            let line = match missing {
                "rule" => "{\"reason\":\"why\",\"by\":\"nez@nez_opt\",\"job\":\"job_1\"}",
                "reason" => "{\"rule\":\"skipped-test\",\"by\":\"nez@nez_opt\",\"job\":\"job_1\"}",
                "by" => "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"job\":\"job_1\"}",
                _ => "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":\"nez@nez_opt\"}",
            };
            let e = parse_waiver_line(line)
                .err()
                .unwrap_or_else(|| panic!("{missing} was not asked for"));
            assert!(
                e.contains(missing),
                "{e} names the field that is gone: {missing}"
            );
        }
    }

    #[test]
    fn a_field_that_is_not_a_string_is_refused() {
        for bad in [
            "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":null,\"job\":\"job_1\"}",
            "{\"rule\":\"skipped-test\",\"reason\":7,\"by\":\"nez\",\"job\":\"job_1\"}",
            "{\"rule\":[\"skipped-test\"],\"reason\":\"why\",\"by\":\"nez\",\"job\":\"job_1\"}",
            "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":{\"n\":\"nez\"},\"job\":\"job_1\"}",
            "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":true,\"job\":\"job_1\"}",
        ] {
            assert!(
                parse_waiver_line(bad).is_err(),
                "a non-string value was taken: {bad}"
            );
        }
    }

    #[test]
    fn an_extra_or_a_repeated_field_is_refused() {
        for bad in [
            "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":\"nez\",\"job\":\"job_1\",\"gate\":\"g\"}",
            "{\"rule\":\"skipped-test\",\"rule\":\"other\",\"reason\":\"why\",\"by\":\"nez\",\"job\":\"job_1\"}",
        ] {
            assert!(
                parse_waiver_line(bad).is_err(),
                "a fifth field was taken: {bad}"
            );
        }
    }

    #[test]
    fn an_empty_or_blank_reason_is_refused() {
        for reason in ["", " ", "     "] {
            let e = parse_waiver_line(&waiver_line("skipped-test", reason))
                .err()
                .unwrap_or_else(|| panic!("a reason of {reason:?} says nothing and was taken"));
            assert!(e.contains("reason"), "{e}");
        }
    }

    /// The counted form of diffgate (`RULE=N:reason`) arrives byte for byte, quotes, brackets and semicolons included.
    #[test]
    fn a_counted_waiver_is_forwarded_with_its_reason_unchanged() {
        let reason = "7:seven tests are #[ignore = \"slow\"]; see a/b";
        let w = parse_waiver_line(&waiver_line("skipped-test", reason)).unwrap();
        assert_eq!(
            w.reason, reason,
            "the escapes of the file are the reason's own characters"
        );
        let (cmd, summary) = dg("abc123", &[], &[w], &rules(true));
        assert_eq!(
            cmd,
            "diffgate --repo . --base abc123 --allow 'skipped-test=7:seven tests are #[ignore = \"slow\"]; see a/b'"
        );
        assert_eq!(
            summary,
            format!("diffgate waivers: 1 forwarded (skipped-test: {reason}), 0 not forwarded")
        );
    }

    #[test]
    fn an_uncounted_waiver_is_forwarded_with_its_reason_unchanged() {
        let w = parse_waiver_line(&waiver_line("skipped-test", GAL_REASON)).unwrap();
        let (cmd, summary) = dg("2ef62d3b", &[], &[w], &rules(true));
        assert!(
            cmd.starts_with("diffgate --repo . --base 2ef62d3b --allow 'skipped-test=")
                && cmd.ends_with("'"),
            "{cmd}"
        );
        assert!(
            cmd.contains(GAL_REASON),
            "the reason is passed through unchanged: {cmd}"
        );
        assert_eq!(
            summary,
            format!("diffgate waivers: 1 forwarded (skipped-test: {GAL_REASON}), 0 not forwarded")
        );
    }

    /// `gate-change` and `spec-change` are the merge tool's decisions, not rules diffgate knows: forwarding one would fail
    /// the whole call with "--allow: unknown rule".
    #[test]
    fn a_rule_that_is_not_on_the_list_is_not_forwarded() {
        let ws = [
            parse_waiver_line(&waiver_line(
                "gate-change",
                "tools/ci/ of the merge; coordinator allow",
            ))
            .unwrap(),
            parse_waiver_line(&waiver_line(
                "spec-change",
                "a statement moved; coordinator allow",
            ))
            .unwrap(),
        ];
        let (cmd, summary) = dg("abc123", &[], &ws, &real_list());
        assert_eq!(
            cmd, "diffgate --repo . --base abc123",
            "the plain command, as today"
        );
        assert_eq!(
            summary,
            "diffgate waivers: 0 forwarded, 2 not forwarded (gate-change, spec-change)"
        );
    }

    /// The list is data: whatever it names is forwarded, so a rule added later is covered by this test and needs no code.
    /// The reason is counted, so a rule the list marks `counted` passes here too.
    #[test]
    fn every_rule_of_the_forwarding_list_is_forwarded() {
        let list = real_list();
        assert!(!list.is_empty(), "{WAIVER_RULES_FILE} reads as empty");
        for r in &list {
            let reason = "5:five of them; the coordinator allow of today";
            let w = parse_waiver_line(&waiver_line(&r.rule, reason)).unwrap();
            let (cmd, summary) = dg("abc123", &[], &[w], &list);
            assert_eq!(
                cmd,
                format!(
                    "diffgate --repo . --base abc123 --allow '{}={reason}'",
                    r.rule
                ),
                "{} is on the list but not forwarded as itself",
                r.rule
            );
            assert_eq!(
                summary,
                format!(
                    "diffgate waivers: 1 forwarded ({}: {reason}), 0 not forwarded",
                    r.rule
                )
            );
        }
    }

    #[test]
    fn the_forwarding_list_holds_skipped_test() {
        assert!(
            real_list().iter().any(|r| r.rule == "skipped-test"),
            "{WAIVER_RULES_FILE} does not name the rule the failing merge was allowed"
        );
    }

    #[test]
    fn a_list_that_is_not_there_forwards_nothing() {
        let w = parse_waiver_line(&waiver_line("skipped-test", "the reason")).unwrap();
        let (cmd, summary) = dg(
            "abc123",
            &[],
            &[w],
            &forwarded_waiver_rules(Path::new("/nonexistent-root")).unwrap(),
        );
        assert_eq!(cmd, "diffgate --repo . --base abc123");
        assert_eq!(
            summary,
            "diffgate waivers: 0 forwarded, 1 not forwarded (skipped-test)"
        );
    }

    /// A list line is a rule name and at most the word `counted`; comments and blank lines carry no rule.
    #[test]
    fn a_plain_list_line_names_a_rule_with_no_marker() {
        let list = parse_waiver_rules(
            "# a comment\n\n  skipped-test  \n",
            Path::new(WAIVER_RULES_FILE),
        )
        .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].rule, "skipped-test");
        assert!(
            !list[0].counted_only,
            "a plain line forwards both forms, as it always did"
        );
    }

    #[test]
    fn the_counted_marker_is_read_off_the_line() {
        let list = parse_waiver_rules("tmp-write counted\n", Path::new(WAIVER_RULES_FILE)).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].rule, "tmp-write");
        assert!(list[0].counted_only);
    }

    #[test]
    fn an_unknown_marker_on_a_list_line_is_an_error() {
        let e = parse_waiver_rules(
            "skipped-test\ntmp-write sometimes\n",
            Path::new(WAIVER_RULES_FILE),
        )
        .expect_err("a marker the step does not know is not read as no marker");
        assert!(e.contains("line 2"), "the line is named: {e}");
        assert!(e.contains("sometimes"), "the word is named: {e}");
        assert!(e.contains("tmp-write"), "the rule is named: {e}");
    }

    #[test]
    fn a_second_marker_on_a_list_line_is_an_error() {
        let e = parse_waiver_rules("tmp-write counted once\n", Path::new(WAIVER_RULES_FILE))
            .expect_err("a line holds at most one marker");
        assert!(e.contains("once"), "the word is named: {e}");
    }

    /// The counted form: digits and the colon after them, of at least one refusal.
    #[test]
    fn the_counted_form_is_digits_then_a_colon_of_at_least_one() {
        for r in ["7:seven lines", "1:one", "42:many"] {
            assert!(counted_reason(r), "{r} is counted");
        }
        for r in [
            "seven lines",
            "0:none of them",
            ":no count",
            "7 seven",
            " 7:leading space",
            "seven",
        ] {
            assert!(!counted_reason(r), "{r} is not counted");
        }
    }

    #[test]
    fn a_plain_rule_travels_in_both_forms() {
        let list = plain_list(&["skipped-test"]);
        for reason in ["the lead checked each one", "3:three of them"] {
            let w = parse_waiver_line(&waiver_line("skipped-test", reason)).unwrap();
            let (cmd, halt, _summary) = dg_all("abc123", &[], &[w], &list);
            assert!(halt.is_none(), "no condition on a plain rule: {cmd}");
            assert!(
                cmd.ends_with(&format!(" --allow 'skipped-test={reason}'")),
                "{reason} travels unchanged: {cmd}"
            );
        }
    }

    /// The coordinator's condition on `tmp-write`, as the list holds it: the counted form only.
    #[test]
    fn a_counted_rule_refuses_an_uncounted_waiver() {
        let list = counted_list(&["tmp-write"]);
        let w = parse_waiver_line(&waiver_line("tmp-write", "seven lines name /tmp")).unwrap();
        let (cmd, halt, _summary) = dg_all("abc123", &[], &[w], &list);
        assert_eq!(cmd, "diffgate --repo . --base abc123", "nothing is added");
        assert_eq!(
            halt.expect("the step says why it will not run diffgate"),
            "diffgate waivers: tmp-write is forwarded in counted form only: write --allow tmp-write=N:<reason> with N the exact number of refusals"
        );
    }

    #[test]
    fn a_counted_rule_forwards_its_counted_waiver_unchanged() {
        let list = counted_list(&["tmp-write"]);
        let reason = "7:seven sandbox test lines name /tmp on purpose (six assert it is refused or unused, one writes inside the fence private tmpfs); coordinator allow 2026-11-17";
        let w = parse_waiver_line(&waiver_line("tmp-write", reason)).unwrap();
        let (cmd, halt, _summary) = dg_all("abc123", &[], &[w], &list);
        assert!(halt.is_none(), "the form the rule asks for");
        assert_eq!(
            cmd,
            format!("diffgate --repo . --base abc123 --allow 'tmp-write={reason}'")
        );
    }

    #[test]
    fn a_count_of_zero_is_not_a_count() {
        let list = counted_list(&["tmp-write"]);
        let w = parse_waiver_line(&waiver_line("tmp-write", "0:nothing was refused")).unwrap();
        let (_cmd, halt, _summary) = dg_all("abc123", &[], &[w], &list);
        assert!(halt.is_some(), "N is at least 1: 0: is not a count");
    }

    /// The condition travels with the rule, not with the source: `LAGENT_ALLOW` is unfiltered, so its entry is checked here.
    #[test]
    fn the_counted_condition_holds_for_the_lead_s_typed_entry_too() {
        let list = counted_list(&["tmp-write"]);
        let bad = lead_waivers("tmp-write=seven sandbox lines").unwrap();
        assert_eq!(uncounted_waiver(&bad, &list), Some("tmp-write"));
        assert_eq!(
            counted_waiver_refusal("tmp-write"),
            "diffgate waivers: tmp-write is forwarded in counted form only: write --allow tmp-write=N:<reason> with N the exact number of refusals"
        );
        let good = lead_waivers("tmp-write=7:seven sandbox lines").unwrap();
        assert_eq!(
            uncounted_waiver(&good, &list),
            None,
            "the counted form is allowed"
        );
        let other = lead_waivers("guard-deleted=the lead read it").unwrap();
        assert_eq!(
            uncounted_waiver(&other, &list),
            None,
            "a rule the list does not mark is untouched"
        );
    }

    /// The refusal is of the whole step, not of one entry: a list that holds a waiver in the wrong form does not run
    /// diffgate with the others it did like.
    #[test]
    fn one_uncounted_waiver_stops_the_whole_list() {
        let list = counted_list(&["tmp-write"]);
        let bad = parse_waiver_line(&waiver_line("tmp-write", "seven lines")).unwrap();
        let good = parse_waiver_line(&waiver_line("tmp-write", "7:seven lines")).unwrap();
        let (_cmd, halt, _summary) = dg_all("abc123", &[], &[good, bad], &list);
        assert!(halt.is_some(), "the uncounted entry is refused: {halt:?}");
    }

    /// The data of this repository, as commit 3 leaves it: `tmp-write` travels only in counted form, `skipped-test` as before.
    #[test]
    fn the_real_list_marks_tmp_write_counted_and_skipped_test_plain() {
        let list = real_list();
        let tmp = list
            .iter()
            .find(|r| r.rule == "tmp-write")
            .unwrap_or_else(|| panic!("{WAIVER_RULES_FILE} does not name tmp-write: {list:?}"));
        assert!(
            tmp.counted_only,
            "tmp-write is on the list without its marker"
        );
        let skipped = list
            .iter()
            .find(|r| r.rule == "skipped-test")
            .unwrap_or_else(|| panic!("{WAIVER_RULES_FILE} does not name skipped-test"));
        assert!(
            !skipped.counted_only,
            "skipped-test keeps both forms: the waiver of job_4ee819df2427 is uncounted"
        );
    }

    /// The whole line of the log, forwarded and refused in the same call: one count for each side.
    #[test]
    fn the_summary_line_counts_both_sides_of_the_list() {
        let ws = [
            parse_waiver_line(&waiver_line("skipped-test", GAL_REASON)).unwrap(),
            parse_waiver_line(&waiver_line("gate-change", "tools/ci/; coordinator allow")).unwrap(),
        ];
        let (cmd, summary) = dg("38d67292", &[], &ws, &rules(true));
        assert_eq!(cmd.matches(" --allow ").count(), 1, "{cmd}");
        assert_eq!(
            summary,
            format!(
                "diffgate waivers: 1 forwarded (skipped-test: {GAL_REASON}), 1 not forwarded (gate-change)"
            )
        );
    }

    /// SecretaryIOS takes the same allows from `LAGENT_ALLOW` as well: a rule both sources name travels once, because
    /// diffgate answers a waiver with the refusals the earlier entry left and calls the copy a failed waiver.
    #[test]
    fn a_rule_the_lead_already_waived_is_handed_to_diffgate_once() {
        let reason = "7:seven #[ignore] variants; coordinator allow";
        let from_allow = lead_waivers(&format!("skipped-test={reason}")).unwrap();
        assert_eq!(from_allow.len(), 1);
        let w = parse_waiver_line(&waiver_line("skipped-test", reason)).unwrap();
        assert_eq!(w.entry, from_allow[0].entry, "the same allow, two sources");
        let (cmd, summary) = dg("abc123", &from_allow, &[w], &rules(true));
        assert_eq!(
            cmd,
            format!("diffgate --repo . --base abc123 --allow 'skipped-test={reason}'"),
            "one --allow for the rule both sources name"
        );
        assert_eq!(
            summary,
            format!("diffgate waivers: 1 forwarded (skipped-test: {reason}), 0 not forwarded")
        );
    }

    /// A rule only the file lists travels even when `LAGENT_ALLOW` holds other rules beside it.
    #[test]
    fn a_waiver_only_the_file_holds_is_added_after_the_lead_s_entries() {
        let entry = lead_waivers("guard-deleted=the lead checked it").unwrap();
        let ws = [
            parse_waiver_line(&waiver_line("skipped-test", "3:three variants")).unwrap(),
            parse_waiver_line(&waiver_line("tmp-write", "written outside the fence")).unwrap(),
        ];
        let (cmd, summary) = dg("abc123", &entry, &ws, &rules(true));
        assert_eq!(
            cmd,
            "diffgate --repo . --base abc123 --allow 'guard-deleted=the lead checked it' --allow 'skipped-test=3:three variants'",
            "the file adds only the rule of the list, after what is already there"
        );
        assert_eq!(
            summary,
            "diffgate waivers: 1 forwarded (skipped-test: 3:three variants), 1 not forwarded (tmp-write)"
        );
    }

    #[test]
    fn merge_step_cap_defaults_to_180_seconds() {
        if std::env::var_os("LUCINA_CHECK_FAST_MERGE_CAP_S").is_none() {
            assert_eq!(merge_step_cap(), 180);
        }
    }

    /// The two cargo steps have the room of their table row — `clippy` the week of merges measured, `unit-tests` the temporary
    /// 480 of GATE-02; every other step is at 180.
    #[test]
    fn merge_limits_are_per_step_with_an_180_default() {
        let no_override = MergeCaps::default();
        let limit = |n: &str| no_override.limit_of(n, MERGE_STEP_LIMIT_DEFAULT);
        assert_eq!(limit("clippy"), 240);
        assert_eq!(limit("unit-tests"), 480);
        assert_eq!(limit("diffgate"), 180);
        assert_eq!(limit("inventory"), 180);
        assert_eq!(limit("unit-tests-tools-ci-lucina-ci"), 180);
        assert_eq!(limit("secrets"), 180);
    }

    /// A limit only ever cuts, never lifts: what the gate enforces is the step's own timeout or its limit, whichever is smaller.
    /// The two cargo steps are built with 300 s and 480 s (`fast::cargo_steps`), so each table number takes effect; `fmt` and
    /// `diffgate` are built with 60 and a step no table names keeps the 180 rather than the 300 it could run.
    #[test]
    fn a_merge_limit_cuts_a_step_s_own_timeout_and_never_lifts_it() {
        let no_override = MergeCaps::default();
        for (name, own, enforced) in [
            ("clippy", 300_u64, 240_u64),
            ("unit-tests", 480, 480),
            ("unit-tests-tools-ci-lucina-ci", 300, 180),
            ("fmt", 60, 60),
            ("diffgate", 60, 60),
            ("inventory", 30, 30),
        ] {
            let at_gate = own.min(no_override.limit_of(name, MERGE_STEP_LIMIT_DEFAULT));
            assert_eq!(at_gate, enforced, "{name} at the merge gate");
        }
    }

    /// The override replaces every table limit with one number, and a `step=SECONDS` entry gives one step its own.
    #[test]
    fn the_override_replaces_all_the_step_limits() {
        let c = MergeCaps {
            plain: Some(7),
            per_step: vec![("clippy".to_string(), 40)],
        };
        let l = |n: &str| c.limit_of(n, MERGE_STEP_LIMIT_DEFAULT);
        assert_eq!(l("clippy"), 40);
        assert_eq!(l("unit-tests"), 7, "7 beats the table's 480");
        assert_eq!(l("secrets"), 7);
        assert_eq!(l("diffgate"), 7, "the override is every step");
        let nine = MergeCaps::parse("9");
        assert_eq!(nine.limit_of("clippy", MERGE_STEP_LIMIT_DEFAULT), 9);
        assert_eq!(nine.limit_of("inventory", MERGE_STEP_LIMIT_DEFAULT), 9);
    }

    /// The forms `LUCINA_CHECK_FAST_MERGE_CAP_S` is read in: a plain number, a per-step list, both together, junk ignored.
    #[test]
    fn the_override_is_read_as_a_number_a_list_or_both() {
        for (text, plain, steps) in [
            ("7", Some(7), vec![]),
            ("a=3", None, vec![("a", 3)]),
            ("5,a=3,b=4", Some(5), vec![("a", 3), ("b", 4)]),
            (" a = 3 ,, junk= , x", None, vec![("a", 3)]),
            ("nonsense", None, vec![]),
            ("", None, vec![]),
        ] {
            let c = MergeCaps::parse(text);
            assert_eq!(c.plain, plain, "{text}");
            let got: Vec<(&str, u64)> = c.per_step.iter().map(|(n, s)| (n.as_str(), *s)).collect();
            assert_eq!(got, steps, "{text}");
        }
    }
}
