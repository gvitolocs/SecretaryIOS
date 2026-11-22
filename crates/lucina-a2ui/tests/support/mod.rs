//! The plumbing every conformance harness of this crate shares, as the guide's section "Shared
//! harness plumbing" states it.
//!
//! The harness is a thin adapter: it reads a suite of the vendored oracle, calls this crate's
//! public entry points and compares the answers. Behaviour lives in `src/`, never here, and a case
//! that fails is fixed in `src/` or reported to the lead — the oracle under
//! `tests/conformance/a2ui/upstream/` is never edited, reformatted or skipped by editing it.

pub mod expect;
pub mod loader;

use serde_json::Value;

pub use expect::{compare, expect_failure, expectation};
pub use loader::{load_suite, Case};

/// The keys of a case that are always accepted and never checked: the guide's rule 2.1, "The
/// case-level metadata keys `name`, `description`, and `features` are always accepted and ignored".
pub const METADATA_KEYS: &[&str] = &["name", "description", "features"];

/// What one action does with a case: `Ok(())` when every expectation of the case holds, `Err` with
/// the first difference when it does not.
pub type Handler = fn(&Case) -> Result<(), String>;

/// Refuses any key of `object` that `allowed` does not name, with the guide's message.
///
/// This is the guide's rule 2.2, the reason a suite cannot quietly grow a key no handler reads: a
/// case that states something we never look at has to fail, not pass.
pub fn assert_known_keys(
    object: &Value,
    allowed: &[&str],
    kind: &str,
    id: &str,
) -> Result<(), String> {
    let Some(object) = object.as_object() else {
        return Err(format!(
            "{id}: {kind} was expected to be an object, it is {}",
            expect::kind(object)
        ));
    };
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("Unknown {kind} '{key}' in {id}"));
        }
    }
    Ok(())
}

/// `UNSUPPORTED`: the one escape the guide's rule 2.2.3 allows, from `"<action>"` or
/// `"<action>.<key path>"` to the reason the capability is not built.
///
/// A case it matches is counted as skipped, never as passed, and an entry that matches no case
/// fails the run: an escape hatch nobody uses is a hatch that should be removed.
struct Unsupported {
    entries: &'static [(&'static str, &'static str)],
    matched: Vec<bool>,
}

impl Unsupported {
    fn new(entries: &'static [(&'static str, &'static str)]) -> Unsupported {
        Unsupported {
            entries,
            matched: vec![false; entries.len()],
        }
    }

    /// The reason this case is skipped, or `None` when no entry matches it.
    fn reason_for(&mut self, case: &Case) -> Option<&'static str> {
        for (index, (key, reason)) in self.entries.iter().enumerate() {
            if key_matches(key, case) {
                self.matched[index] = true;
                return Some(reason);
            }
        }
        None
    }

    /// The entries that matched no case.
    fn unmatched(&self) -> Vec<String> {
        self.entries
            .iter()
            .zip(&self.matched)
            .filter_map(|((key, _), matched)| {
                if *matched {
                    None
                } else {
                    Some((*key).to_string())
                }
            })
            .collect()
    }
}

/// Whether an `UNSUPPORTED` key names this case: the action itself, or the action and a dotted
/// path of keys the case actually holds.
fn key_matches(key: &str, case: &Case) -> bool {
    match key.split_once('.') {
        None => case.action() == Some(key),
        Some((action, path)) => case.action() == Some(action) && holds_path(case, path),
    }
}

/// Whether the case holds every key of the dotted `path`.
fn holds_path(case: &Case, path: &str) -> bool {
    let mut node = Some(&case.json);
    for key in path.split('.') {
        node = node.and_then(|value| value.get(key));
    }
    node.is_some()
}

/// What one suite run produced.
pub struct Summary {
    /// The suite path, as the oracle names it.
    pub suite: String,
    /// Cases loaded.
    pub cases: usize,
    /// Cases whose every expectation held.
    pub passed: usize,
    /// Cases skipped by `UNSUPPORTED`, with their reason.
    pub skipped: Vec<(String, String)>,
    /// Cases that failed, with the first difference of each.
    pub failures: Vec<(String, String)>,
    /// `UNSUPPORTED` entries that matched no case.
    pub unused: Vec<String>,
}

/// Runs the cases of the suite `rel` through `handlers`, dispatching on each case's `action`.
///
/// The order is the original harness's: an `UNSUPPORTED` match skips the case before its keys are
/// looked at, a case without an `action` fails, an `action` no handler declares fails as unknown,
/// and everything else runs its handler.
pub fn run_suite(
    rel: &str,
    handlers: &[(&str, Handler)],
    unsupported: &'static [(&'static str, &'static str)],
) -> Summary {
    let cases = load_suite(rel).unwrap_or_else(|problem| panic!("{rel}: {problem}"));
    assert!(
        !cases.is_empty(),
        "{rel}: the suite loaded no cases; a broken path must not read as a pass"
    );
    let mut unsupported = Unsupported::new(unsupported);
    let mut summary = Summary {
        suite: rel.to_string(),
        cases: cases.len(),
        passed: 0,
        skipped: Vec::new(),
        failures: Vec::new(),
        unused: Vec::new(),
    };
    for case in &cases {
        if let Some(reason) = unsupported.reason_for(case) {
            summary.skipped.push((case.id.clone(), reason.to_string()));
            continue;
        }
        let Some(action) = case.action() else {
            summary.failures.push((
                case.id.clone(),
                format!("{}: the case names no 'action'", case.id),
            ));
            continue;
        };
        let Some((_, handler)) = handlers.iter().find(|(named, _)| *named == action) else {
            summary.failures.push((
                case.id.clone(),
                format!("Unknown action '{action}' in {}", case.id),
            ));
            continue;
        };
        match handler(case) {
            Ok(()) => summary.passed += 1,
            Err(problem) => summary.failures.push((case.id.clone(), problem)),
        }
    }
    summary.unused = unsupported.unmatched();
    summary
}

impl Summary {
    /// The one line the run is judged by.
    pub fn line(&self) -> String {
        format!(
            "conformance {}: {} passed, {} skipped, {} failed",
            self.suite,
            self.passed,
            self.skipped.len(),
            self.failures.len()
        )
    }

    /// Prints the summary line, then fails the test on any failing case, any `UNSUPPORTED` entry
    /// that matched nothing, and any count that is not the one the step promises.
    ///
    /// The line comes first so that a failing run still reports what it counted.
    pub fn report(&self, expected_passed: usize, expected_skipped: usize) {
        println!("{}", self.line());
        if !self.failures.is_empty() {
            let list = self
                .failures
                .iter()
                .map(|(id, problem)| format!("  {id}: {problem}"))
                .collect::<Vec<_>>()
                .join("\n");
            panic!(
                "{} case(s) failed in {}:\n{}",
                self.failures.len(),
                self.suite,
                list
            );
        }
        if !self.unused.is_empty() {
            panic!(
                "UNSUPPORTED entries that matched no case in {}: {}",
                self.suite,
                self.unused.join(", ")
            );
        }
        assert_eq!(
            self.cases,
            self.passed + self.skipped.len() + self.failures.len(),
            "{}: a case of the {} loaded is accounted for neither as passed, skipped nor failed",
            self.suite,
            self.cases
        );
        assert_eq!(
            (self.passed, self.skipped.len(), self.failures.len()),
            (expected_passed, expected_skipped, 0),
            "{}: the counts this step promises",
            self.suite
        );
    }
}
