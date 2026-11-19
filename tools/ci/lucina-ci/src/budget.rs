//! The outer limit of the merge gate "fast local check" (finding GATE-19).
//!
//! `.lucina/pipeline.toml` gives the gate `scripts/check-fast --base HEAD^1 --merge-gate` a `timeout_min`, and inside it every
//! step has a limit of its own. The outer one must hold the sum of the inner ones, or a correct change is killed while its build
//! is still inside its own limit (job_70791dbbc01a: killed at 12 minutes, 20 minutes of build allowed). The worst case is
//! computed here from the same constants and tables the gate runs with, and a test fails when the pipeline file is below it.

use super::{
    ACTIONLINT_STEP_TIMEOUT_S, DIFFGATE_STEP_TIMEOUT_S, INVENTORY_STEP_TIMEOUT_S,
    MERGE_GATE_BUDGET_STEP_TIMEOUT_S, MERGE_STEP_LIMIT_DEFAULT, MergeCaps, SECRETS_STEP_TIMEOUT_S,
    SHELLCHECK_STEP_TIMEOUT_S, fast,
};
use std::collections::BTreeSet;

/// How many standalone crates (a manifest under `tools/` with its own workspace) a change is assumed to touch. A change that
/// touches more than two such crates is rare; if one does, it may need the gate's limit raised, or to be split.
pub const ASSUMED_STANDALONE_CRATES: usize = 2;
/// Seconds the gate may wait for its slot in the build queue before check-fast starts. The gate takes bq's reserved gate slot,
/// so the wait is short; the gate's timer starts when lagent starts the command, so the wait counts against the outer limit.
pub const BQ_WAIT_ALLOWANCE_S: u64 = 300;
/// Seconds of margin on top of every step running to its limit. It covers what happens inside the queue slot before the first step:
/// the launcher's `cargo build --release` of the CI tool, its install copy, and the run's own self-copy of the binary; then `git`
/// and the final report.
pub const MARGIN_S: u64 = 300;
/// The name of the gate in `.lucina/pipeline.toml`.
pub const GATE_NAME: &str = "fast local check";

/// The seconds a step of own timeout `own` runs at the most at the merge gate: its table limit or the default, never above `own`.
fn at_gate(name: &str, own: u64) -> u64 {
    own.min(MergeCaps::default().limit_of(name, MERGE_STEP_LIMIT_DEFAULT))
}

/// The parts of the worst case, as (what, seconds).
///
/// Counted: the workspace steps (`fmt`, `clippy`, `unit-tests-build`, `unit-tests`) once, the four per-crate steps for
/// `ASSUMED_STANDALONE_CRATES` standalone crates, `diffgate`, `shellcheck`, `actionlint`, `secrets`, `inventory` and `merge-gate-budget` once each,
/// then the queue wait and the margin. Not counted: the proofs `--proofs` runs (the merge gate does not pass it).
pub fn merge_gate_parts() -> Vec<(String, u64)> {
    let mut owners: BTreeSet<fast::Owner> = BTreeSet::new();
    owners.insert(fast::Owner::Member("budget-member".into()));
    for i in 0..ASSUMED_STANDALONE_CRATES {
        owners.insert(fast::Owner::Standalone(format!(
            "tools/budget-{i}/Cargo.toml"
        )));
    }
    let mut parts: Vec<(String, u64)> = fast::cargo_steps(&owners, &["budget-member.rs".into()])
        .into_iter()
        .map(|(name, _, own)| {
            let s = at_gate(&name, own);
            (name, s)
        })
        .collect();
    for (name, own) in [
        ("diffgate", DIFFGATE_STEP_TIMEOUT_S),
        ("shellcheck", SHELLCHECK_STEP_TIMEOUT_S),
        ("actionlint", ACTIONLINT_STEP_TIMEOUT_S),
        ("secrets", SECRETS_STEP_TIMEOUT_S),
        ("inventory", INVENTORY_STEP_TIMEOUT_S),
        ("merge-gate-budget", MERGE_GATE_BUDGET_STEP_TIMEOUT_S),
    ] {
        parts.push((name.to_string(), at_gate(name, own)));
    }
    parts.push(("bq wait allowance".into(), BQ_WAIT_ALLOWANCE_S));
    parts.push(("margin".into(), MARGIN_S));
    parts
}

/// Whether a change edits the pipeline file, which makes check-fast run the `merge-gate-budget` step at the merge gate.
pub fn pipeline_changed(changed: &std::collections::BTreeSet<String>) -> bool {
    changed.contains(".lucina/pipeline.toml")
}

/// The longest a `--merge-gate` check-fast run can legitimately take, in seconds.
pub fn merge_gate_worst_case_secs() -> u64 {
    merge_gate_parts().iter().map(|(_, s)| s).sum()
}

/// The `timeout_min` of the gate named `name` in a pipeline file: the first `timeout_min` after its `name = "..."` line and before
/// the next table header.
pub fn gate_timeout_min(toml: &str, name: &str) -> Option<u64> {
    let mut in_gate = false;
    for l in toml.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_gate = false;
        } else if t.starts_with("name") && t.contains('=') {
            in_gate = t.split_once('=')?.1.trim().trim_matches('"') == name;
        } else if in_gate && t.starts_with("timeout_min") {
            return t.split_once('=')?.1.split('#').next()?.trim().parse().ok();
        }
    }
    None
}

/// `Ok` when the gate's outer limit in `toml` holds the worst case; `Err` says both numbers and which file to change.
pub fn check_pipeline(toml: &str) -> Result<(), String> {
    let worst = merge_gate_worst_case_secs();
    let min = gate_timeout_min(toml, GATE_NAME).ok_or_else(|| {
        format!("no gate named {GATE_NAME:?} with a timeout_min in the pipeline file")
    })?;
    if min * 60 < worst {
        return Err(format!(
            "the gate {GATE_NAME:?} has timeout_min = {min} ({} s) in .lucina/pipeline.toml, below the {worst} s its own steps may take \
             (`budget::merge_gate_worst_case_secs`, {} min rounded up): raise timeout_min in .lucina/pipeline.toml, or lower the step limits in tools/ci/lucina-ci",
            min * 60,
            worst.div_ceil(60)
        ));
    }
    Ok(())
}

/// `lucina-ci merge-gate-budget`: print the worst case in minutes, with its parts.
pub fn cmd_merge_gate_budget() -> Result<i32, String> {
    for (what, s) in merge_gate_parts() {
        println!("{s:>6} s  {what}");
    }
    let worst = merge_gate_worst_case_secs();
    println!(
        "{worst:>6} s  total = {} min; timeout_min to write: {}",
        worst.div_ceil(60),
        worst.div_ceil(60).div_ceil(5) * 5
    );
    let root = super::repo_root()?;
    let toml =
        std::fs::read_to_string(root.join(".lucina/pipeline.toml")).map_err(|e| e.to_string())?;
    match check_pipeline(&toml) {
        Ok(()) => {
            println!("pipeline.toml: ok");
            Ok(0)
        }
        Err(m) => {
            println!("pipeline.toml: {m}");
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn real_pipeline() -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.lucina/pipeline.toml");
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    #[test]
    fn the_real_gate_holds_the_worst_case() {
        if let Err(m) = check_pipeline(&real_pipeline()) {
            panic!("{m}");
        }
    }

    #[test]
    fn a_too_low_limit_fails_and_names_both_numbers() {
        let low = "[[gate]]\nname = \"fast local check\"\nrun = \"x\"\ntimeout_min = 12\n";
        let e = check_pipeline(low).expect_err("12 minutes is below the worst case");
        assert!(
            e.contains("720 s") && e.contains(&format!("{} s", merge_gate_worst_case_secs())),
            "{e}"
        );
        assert!(e.contains(".lucina/pipeline.toml"), "{e}");
    }

    #[test]
    fn a_too_low_limit_in_a_pipeline_file_on_disk_fails() {
        let dir = std::env::temp_dir().join(format!("gate19-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("pipeline.toml");
        let real = real_pipeline();
        let min = gate_timeout_min(&real, GATE_NAME).unwrap();
        std::fs::write(
            &f,
            real.replacen(&format!("timeout_min = {min}"), "timeout_min = 12", 99),
        )
        .unwrap();
        let r = check_pipeline(&std::fs::read_to_string(&f).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let e = r.expect_err("12 minutes is below the worst case");
        assert!(
            e.contains("timeout_min = 12")
                && e.contains(&format!("{} s", merge_gate_worst_case_secs())),
            "{e}"
        );
    }

    #[test]
    fn the_worst_case_counts_what_the_gate_runs() {
        let parts = merge_gate_parts();
        let get = |n: &str| {
            parts
                .iter()
                .filter(|(k, _)| k == n)
                .map(|(_, s)| *s)
                .sum::<u64>()
        };
        assert_eq!(get("clippy"), 480);
        assert_eq!(get("unit-tests-build"), 1200);
        assert_eq!(get("unit-tests"), 240);
        let standalone = parts.iter().filter(|(k, _)| k.contains("budget-")).count();
        assert_eq!(standalone, 4 * ASSUMED_STANDALONE_CRATES);
        assert_eq!(get("merge-gate-budget"), 30);
        assert_eq!(merge_gate_worst_case_secs(), 4020);
    }

    /// The wait the launcher gives the gate and the allowance the budget counts are one number: the shell literal is read here.
    #[test]
    fn the_launcher_waits_for_the_gate_slot_no_longer_than_the_budget_allows() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../scripts/check-fast");
        let sh = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert!(
            sh.contains(r#"--max-wait "$maxwait""#),
            "the launcher's bq call no longer passes --max-wait \"$maxwait\""
        );
        let mut lits: Vec<u64> = Vec::new();
        for v in sh.lines().filter_map(|l| l.trim().strip_prefix("maxwait=")) {
            match v.parse() {
                Ok(n) => lits.push(n),
                Err(_) => {
                    panic!("scripts/check-fast: `maxwait={v}` is not a plain number of seconds")
                }
            }
        }
        assert_eq!(
            lits.len(),
            2,
            "the launcher sets maxwait twice (plain run, merge gate): {lits:?}"
        );
        assert_eq!(lits[0], 3600, "a plain run keeps its hour");
        assert_eq!(
            lits[1], BQ_WAIT_ALLOWANCE_S,
            "scripts/check-fast gives the merge gate --max-wait {} but budget.rs counts BQ_WAIT_ALLOWANCE_S = {BQ_WAIT_ALLOWANCE_S}: change both",
            lits[1]
        );
    }

    #[test]
    fn only_a_change_to_the_pipeline_file_selects_the_budget_step() {
        let only: BTreeSet<String> = [".lucina/pipeline.toml".to_string()].into();
        let other: BTreeSet<String> = [
            "scripts/check-fast".to_string(),
            ".lucina/other.toml".to_string(),
        ]
        .into();
        assert!(pipeline_changed(&only));
        assert!(!pipeline_changed(&other));
    }

    #[test]
    fn a_later_table_does_not_lend_its_timeout_to_the_gate() {
        let t = "[[gate]]\nname = \"fast local check\"\n[other]\ntimeout_min = 99\n";
        assert_eq!(gate_timeout_min(t, GATE_NAME), None);
    }

    #[test]
    fn the_gate_is_found_among_others() {
        let t = "[[gate]]\nname = \"a\"\ntimeout_min = 5\n[[gate]]\nname = \"fast local check\"\npaths = []\ntimeout_min = 70 # why\n[[gate]]\nname = \"b\"\ntimeout_min = 9\n";
        assert_eq!(gate_timeout_min(t, GATE_NAME), Some(70));
        assert_eq!(gate_timeout_min(t, "b"), Some(9));
        assert_eq!(gate_timeout_min(t, "none"), None);
    }
}
