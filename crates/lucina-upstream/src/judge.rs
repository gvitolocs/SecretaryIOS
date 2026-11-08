//! JEV's questions about a change set, the facts it reads ([`state`]) and its [`Verdict`].
//! The same state template for every entry: facts only, no guesses or opinion text.

use lucina_jev::{Decision, Question};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::clip;
use crate::fetch::ChangeSet;
use crate::registry::{Settings, Upstream};
use crate::summary::Summary;

/// How much a change set matters. Snake_case JSON (the CLI, the run record and `jev/upstream`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    /// 0-4 (see [`questions`]).
    pub importance: u32,
    pub importance_conf: f64,
    pub breaking_p: f64,
    pub security_p: f64,
    /// The changes touch a feature in `rely_on`.
    pub relies_p: f64,
    /// small | medium | large.
    pub effort: String,
    pub effort_conf: f64,
    /// jev | heuristic.
    pub judged_by: String,
    pub model: String,
}

impl Verdict {
    /// `importance 3/4 (0.71), security 0.08, breaking 0.20, relies 0.64, effort medium`.
    pub fn line(&self) -> String {
        format!(
            "importance {}/4 ({:.2}), security {:.2}, breaking {:.2}, relies {:.2}, effort {}",
            self.importance,
            self.importance_conf,
            self.security_p,
            self.breaking_p,
            self.relies_p,
            self.effort
        )
    }
}

pub fn questions() -> Vec<(&'static str, Question)> {
    vec![
        (
            "importance",
            Question::score(
                "How important is this upstream change set for a project that reimplemented this upstream in Rust and wants to stay at parity?",
                [
                    "0: nothing to port (docs, CI, chores)",
                    "1: minor fixes",
                    "2: notable fixes or small features",
                    "3: significant features or many fixes worth porting",
                    "4: major release or architecture change",
                ],
            ),
        ),
        (
            "breaking",
            Question::noul(
                "Do these changes break or change behaviour, formats or APIs that a reimplementation copies?",
            ),
        ),
        (
            "security",
            Question::noul(
                "Does this change set fix a security vulnerability (CVE, memory safety, auth, injection, data exposure)?",
            ),
        ),
        (
            "relies",
            Question::noul("Do these changes touch any of the features listed in `we_rely_on`?"),
        ),
        (
            "effort",
            Question::choice(
                "How much work to port these changes into the Rust reimplementation?",
                [
                    ("small", "under a day"),
                    ("medium", "a few days"),
                    ("large", "a week or more"),
                ],
            ),
        ),
    ]
}

/// The facts JEV reads.
pub fn state(up: &Upstream, cs: &ChangeSet, s: &Summary) -> Value {
    json!({
        "upstream": up.name,
        "relation": up.relation,
        "our_counterpart": up.ours.iter().map(|o| o.label()).collect::<Vec<_>>(),
        "we_rely_on": up.rely_on,
        "pinned": cs.pin,
        "latest": cs.latest,
        "releases": cs.releases.iter().take(5).map(|r| json!({
            "tag": r.tag,
            "date": r.date,
            "prerelease": r.prerelease,
            "notes": clip(&r.notes, 600),
        })).collect::<Vec<_>>(),
        "commits_since_pin": cs.commits_total,
        "commit_subjects": cs.commits.iter().take(60).map(|c| c.subject.as_str()).collect::<Vec<_>>(),
        "paths_touched": cs.files_touched.iter().take(20).collect::<Vec<_>>(),
        "counts": s.counts,
    })
}

/// JEV's answers as a verdict; `None` when one is missing.
pub fn verdict(d: &Decision) -> Option<Verdict> {
    let (importance, importance_conf) = d.score("importance")?;
    let (effort, effort_conf) = d.choice("effort")?;
    Some(Verdict {
        importance: importance.min(4),
        importance_conf,
        breaking_p: d.yes("breaking")?,
        security_p: d.yes("security")?,
        relies_p: d.yes("relies")?,
        effort: effort.to_owned(),
        effort_conf,
        judged_by: "jev".into(),
        model: d.model.clone(),
    })
}

/// Worth asking the owner about.
pub fn significant(v: &Verdict, s: &Settings) -> bool {
    v.security_p >= s.security_min || v.importance >= s.notify_min_importance
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::verdict_of;
    use lucina_jev::Answer;

    #[test]
    fn questions_are_valid() {
        let q = questions();
        assert_eq!(q.len(), 5);
        for (name, q) in &q {
            q.validate(name).unwrap();
        }
    }

    #[test]
    fn verdict_from_answers() {
        let mut d = Decision {
            model: "jev-1.14.0".into(),
            ..Decision::default()
        };
        d.answers.insert(
            "importance".into(),
            Answer::Score {
                score: 3,
                confidence: 0.71,
            },
        );
        d.answers.insert("breaking".into(), Answer::Noul(0.2));
        d.answers.insert("security".into(), Answer::Noul(0.08));
        assert_eq!(verdict(&d), None, "relies and effort missing");
        d.answers.insert("relies".into(), Answer::Noul(0.64));
        d.answers.insert(
            "effort".into(),
            Answer::Choice {
                choice: "medium".into(),
                confidence: 0.6,
                probabilities: Default::default(),
            },
        );
        let v = verdict(&d).unwrap();
        assert_eq!(v.importance, 3);
        assert_eq!(v.judged_by, "jev");
        assert_eq!(
            v.line(),
            "importance 3/4 (0.71), security 0.08, breaking 0.20, relies 0.64, effort medium"
        );
    }

    #[test]
    fn significance() {
        let s = Settings::default();
        assert!(significant(&verdict_of(3, 0.1, 0.1), &s));
        assert!(significant(&verdict_of(1, 0.5, 0.1), &s));
        assert!(!significant(&verdict_of(2, 0.49, 0.9), &s));
    }
}
