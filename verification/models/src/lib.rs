//! Stateright models of the Secretary / Lucina multi-process protocols.
//!
//! Every protocol has a module here and a TLA+ twin in `formal/<protocol>/`
//! (checked by TLC). `docs/verification.md` is the policy, the inventory and
//! the "model action → code location" tables.
//!
//! A model module exposes `cases(deep)`: named configurations of the model
//! (bounds plus switches such as "the current code" or "the proposed fix")
//! and, per property, what the checker must find:
//!
//! - [`Expect::Holds`]: an `always` property with no counterexample;
//! - [`Expect::Violated`]: a known bug, kept as a regression witness (the
//!   model is strong enough to see it); when the fix lands in the code, the
//!   case's switch flips and the expectation becomes `Holds`;
//! - [`Expect::Reached`]: a `sometimes` property (the model reaches an
//!   interesting state, so the invariants are not vacuous).
//!
//! The tests in `tests/` and the `verify-models` binary run the same cases.

use std::fmt::Debug;
use std::hash::Hash;
use std::time::{Duration, Instant};

use stateright::{Checker, Expectation, Model};

pub mod gates;
pub mod ink_sync;
pub mod oauth;
pub mod ownership;
pub mod shopping;
pub mod timeline;

/// What a case expects from one property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    Holds,
    Violated,
    Reached,
}

/// The checker's verdict on one property.
#[derive(Debug, Clone)]
pub struct PropResult {
    pub name: &'static str,
    pub expectation: &'static str,
    /// A counterexample (always/eventually) or a witness (sometimes) exists.
    pub discovered: bool,
    /// The discovery as one action per line (shortest path: BFS).
    pub trace: Option<String>,
    /// The discovery's last state (`{:#?}`).
    pub last_state: Option<String>,
}

/// One checker run.
#[derive(Debug, Clone)]
pub struct Report {
    pub unique_states: usize,
    pub total_states: usize,
    pub max_depth: usize,
    pub complete: bool,
    pub elapsed: Duration,
    pub props: Vec<PropResult>,
}

impl Report {
    pub fn prop(&self, name: &str) -> &PropResult {
        self.props
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no property {name}"))
    }
}

/// Breadth-first search to `max_depth` (or the whole space with `None`).
pub fn check<M>(model: M, max_depth: Option<usize>) -> Report
where
    M: Model + Send + Sync + 'static,
    M::State: Hash + Debug + Clone + PartialEq + Send + Sync + 'static,
    M::Action: Debug + Clone + PartialEq + Send + Sync + 'static,
{
    let started = Instant::now();
    let props: Vec<(&'static str, Expectation)> = model
        .properties()
        .iter()
        .map(|p| (p.name, p.expectation.clone()))
        .collect();
    let mut builder = model.checker().threads(1);
    if let Some(d) = max_depth {
        builder = builder.target_max_depth(d);
    }
    let checker = builder.spawn_bfs().join();
    let discoveries = checker.discoveries();
    let model = checker.model();
    let props = props
        .into_iter()
        .map(|(name, exp)| {
            let path = discoveries.get(name);
            PropResult {
                name,
                expectation: match exp {
                    Expectation::Always => "always",
                    Expectation::Eventually => "eventually",
                    Expectation::Sometimes => "sometimes",
                },
                discovered: path.is_some(),
                // Stateright keeps the last violating path it saw; BFS again
                // for the shortest one (what the docs quote).
                last_state: path.map(|p| format!("{:#?}", p.last_state())),
                trace: path.map(|p| {
                    let actions = match exp {
                        Expectation::Always => shortest_counterexample(model, name, 5_000_000)
                            .unwrap_or_else(|| p.clone().into_actions()),
                        _ => p.clone().into_actions(),
                    };
                    actions
                        .iter()
                        .enumerate()
                        .map(|(i, a)| format!("{:>3}. {a:?}", i + 1))
                        .collect::<Vec<_>>()
                        .join("\n")
                }),
            }
        })
        .collect();
    Report {
        unique_states: checker.unique_state_count(),
        total_states: checker.state_count(),
        max_depth: checker.max_depth(),
        complete: max_depth.is_none_or(|d| checker.max_depth() < d),
        elapsed: started.elapsed(),
        props,
    }
}

/// Plain BFS with parent pointers to the first state violating the
/// `always` property `name` (at most `limit` states).
pub fn shortest_counterexample<M>(model: &M, name: &str, limit: usize) -> Option<Vec<M::Action>>
where
    M: Model,
    M::State: Hash + Clone,
    M::Action: Clone,
{
    use std::collections::hash_map::DefaultHasher;
    use std::collections::{HashSet, VecDeque};
    use std::hash::Hasher;
    let prop = model.properties().into_iter().find(|p| p.name == name)?;
    let fp = |s: &M::State| {
        let mut h = DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    };
    // (state, parent index, action from parent)
    let mut nodes: Vec<(M::State, usize, Option<M::Action>)> = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = VecDeque::new();
    for s in model.init_states() {
        if seen.insert(fp(&s)) {
            nodes.push((s, usize::MAX, None));
            queue.push_back(nodes.len() - 1);
        }
    }
    let mut actions = Vec::new();
    while let Some(i) = queue.pop_front() {
        if !(prop.condition)(model, &nodes[i].0) {
            let mut path = Vec::new();
            let mut j = i;
            while let Some(a) = nodes[j].2.clone() {
                path.push(a);
                j = nodes[j].1;
            }
            path.reverse();
            return Some(path);
        }
        if nodes.len() > limit {
            return None;
        }
        actions.clear();
        let state = nodes[i].0.clone();
        model.actions(&state, &mut actions);
        for a in actions.drain(..) {
            if let Some(next) = model.next_state(&state, a.clone()) {
                if seen.insert(fp(&next)) {
                    nodes.push((next, i, Some(a)));
                    queue.push_back(nodes.len() - 1);
                }
            }
        }
    }
    None
}

/// A named configuration of a model plus its expectations.
pub struct Case {
    pub protocol: &'static str,
    pub name: &'static str,
    pub run: Box<dyn Fn() -> Report + Send + Sync>,
    pub expect: Vec<(&'static str, Expect)>,
}

impl Case {
    pub fn new<M>(
        protocol: &'static str,
        name: &'static str,
        model: M,
        max_depth: Option<usize>,
        expect: Vec<(&'static str, Expect)>,
    ) -> Self
    where
        M: Model + Clone + Send + Sync + 'static,
        M::State: Hash + Debug + Clone + PartialEq + Send + Sync + 'static,
        M::Action: Debug + Clone + PartialEq + Send + Sync + 'static,
    {
        Case {
            protocol,
            name,
            run: Box::new(move || check(model.clone(), max_depth)),
            expect,
        }
    }

    /// Run and compare with the expectations. `Err` lists the mismatches.
    pub fn verify(&self) -> (Report, Result<(), String>) {
        let report = (self.run)();
        let mut errors = Vec::new();
        for (name, want) in &self.expect {
            let got = report.prop(name);
            let ok = match want {
                Expect::Holds => !got.discovered && got.expectation != "sometimes",
                Expect::Violated => got.discovered && got.expectation != "sometimes",
                Expect::Reached => got.discovered && got.expectation == "sometimes",
            };
            if !ok {
                errors.push(match (want, &got.trace) {
                    (Expect::Holds, Some(t)) => {
                        format!("{name}: expected to hold, counterexample:\n{t}")
                    }
                    (Expect::Holds, None) => format!("{name}: expected an always property"),
                    (Expect::Violated, _) => format!(
                        "{name}: expected a counterexample (known bug witness), none found: model too weak or bug fixed (flip the case)"
                    ),
                    (Expect::Reached, _) => format!("{name}: witness state never reached"),
                });
            }
        }
        let res = if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        };
        (report, res)
    }
}

/// Every case of every protocol.
pub fn all_cases(deep: bool) -> Vec<Case> {
    let mut v = Vec::new();
    v.extend(timeline::cases(deep));
    v.extend(ownership::cases(deep));
    v.extend(shopping::cases(deep));
    v.extend(oauth::cases(deep));
    v.extend(gates::cases(deep));
    v.extend(ink_sync::cases(deep));
    v
}

/// Small sorted-set helpers on `Vec` (states must be `Hash`).
pub fn set_insert<T: Ord>(v: &mut Vec<T>, x: T) -> bool {
    match v.binary_search(&x) {
        Ok(_) => false,
        Err(i) => {
            v.insert(i, x);
            true
        }
    }
}

pub fn set_has<T: Ord>(v: &[T], x: &T) -> bool {
    v.binary_search(x).is_ok()
}
