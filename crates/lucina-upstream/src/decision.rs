//! Bodies for `POST /api/secretary/decisions` (docs/design/decisions-api.md) and what the owner chose.

use serde_json::{json, Value};

use crate::clip;
use crate::fetch::ChangeSet;
use crate::judge::Verdict;
use crate::registry::{Contrib, Settings, Upstream};
use crate::summary::Summary;

pub const QUESTION_MAX: usize = 2000;
pub const CONTEXT_MAX: usize = 8000;
pub const PORT_OPTIONS: [&str; 3] = ["Port into ours", "Later", "Ignore"];
pub const CONTRIB_OPTIONS: [&str; 3] = ["Draft a contribution (no push)", "Later", "Never"];

/// "Port into ours" for a security fix, an important change to a feature we rely on, or a major
/// release; else "Later".
pub fn recommendation(v: &Verdict, s: &Settings) -> &'static str {
    if v.security_p >= s.security_min
        || (v.importance >= 3 && v.relies_p >= 0.5)
        || v.importance == 4
    {
        PORT_OPTIONS[0]
    } else {
        PORT_OPTIONS[1]
    }
}

pub fn body(
    up: &Upstream,
    cs: &ChangeSet,
    summary: &Summary,
    v: &Verdict,
    settings: &Settings,
    report_path: &str,
) -> Value {
    let ours = up
        .ours
        .first()
        .map(|o| o.path.clone())
        .unwrap_or_else(|| "ours".into());
    let question = clip(
        &format!(
            "New upstream {}: {}. Port the changes into {ours}?",
            up.name, summary.headline
        ),
        QUESTION_MAX,
    );
    let mut ctx = Vec::new();
    ctx.extend(summary.lines.iter().map(|l| format!("- {l}")));
    ctx.push(String::new());
    ctx.push(format!(
        "{}: {}",
        if v.judged_by == "jev" {
            "JEV"
        } else {
            "Heuristic (JEV unavailable)"
        },
        v.line()
    ));
    ctx.push(format!(
        "Pinned {} → latest {}.",
        cs.pin.as_deref().unwrap_or("?"),
        cs.latest.as_deref().unwrap_or("?")
    ));
    if let Some(u) = &cs.compare_url {
        ctx.push(format!("Compare: {u}"));
    }
    ctx.push(format!(
        "Licences: upstream {}, ours {}.",
        up.licence_upstream, up.licence_ours
    ));
    ctx.push(format!("Our counterpart: {}.", up.ours_label()));
    ctx.push(format!("Report: {report_path}"));
    ctx.push("On 'Port into ours' Lucina Agents gets a workspace and a porting brief that follows docs/porting-guidelines.md.".into());
    let security = v.security_p >= settings.security_min;
    json!({
        "question": question,
        "options": PORT_OPTIONS,
        "recommendation": recommendation(v, settings),
        "context": clip(&ctx.join("\n"), CONTEXT_MAX),
        // The owner chooses ports himself: the decide API always asks him about high impact.
        "impact": "high",
        "reversible": true,
        "urgency": if security { "high" } else { "low" },
        "topic": "upstream-watch",
        "asking_session": "upstream-watch",
        "asking_client": "upstream-watch",
    })
}

/// "Contribute our … back?": draft only.
pub fn contrib_body(up: &Upstream, c: &Contrib) -> Value {
    let mut ctx = vec![
        format!("Ours: {}", c.ours),
        format!("Upstream: {} ({})", up.name, up.url),
    ];
    if let Some(t) = &c.upstream_target {
        ctx.push(format!("Upstream target: {t}"));
    }
    if !c.notes.is_empty() {
        ctx.push(format!("Notes: {}", c.notes));
    }
    ctx.push(format!(
        "Licences: upstream {}, ours {}.",
        up.licence_upstream, up.licence_ours
    ));
    ctx.push(
        "A draft plan goes to the upstream-watch data directory; nothing leaves this machine."
            .into(),
    );
    json!({
        "question": clip(&format!(
            "Contribute our {} back to {} upstream? Draft only: nothing is pushed or opened upstream without your OK.",
            c.title, up.name
        ), QUESTION_MAX),
        "options": CONTRIB_OPTIONS,
        "recommendation": "Later",
        "context": clip(&ctx.join("\n"), CONTEXT_MAX),
        "impact": "high",
        "reversible": true,
        "urgency": "low",
        "topic": "upstream-contrib",
        "asking_session": "upstream-watch",
        "asking_client": "upstream-watch",
    })
}

/// The answer to either question: the first option (port / draft), the second (later), the third
/// (ignore / never), or his own words.
#[derive(Debug, Clone, PartialEq)]
pub enum Chosen {
    Port,
    Later,
    Ignore,
    OwnerReply(String),
}

impl Chosen {
    /// The state's word for it (`port`, `later`, `ignore`, `reply`).
    pub fn word(&self) -> &'static str {
        match self {
            Self::Port => "port",
            Self::Later => "later",
            Self::Ignore => "ignore",
            Self::OwnerReply(_) => "reply",
        }
    }
}

/// What a decided decision chose; `None` while it is not decided.
pub fn chosen(d: &Value) -> Option<Chosen> {
    if d["status"].as_str().is_some_and(|s| s != "decided") {
        return None;
    }
    if d["decision"] == "owner_reply" {
        return Some(Chosen::OwnerReply(
            d["reason"].as_str().unwrap_or("").to_owned(),
        ));
    }
    let by_index = match d["chosen_index"].as_u64() {
        Some(1) => Some(Chosen::Port),
        Some(2) => Some(Chosen::Later),
        Some(3) => Some(Chosen::Ignore),
        _ => None,
    };
    by_index.or_else(|| {
        let opt = d["chosen_option"].as_str()?;
        match opt {
            o if o == PORT_OPTIONS[0] || o == CONTRIB_OPTIONS[0] => Some(Chosen::Port),
            "Later" => Some(Chosen::Later),
            "Ignore" | "Never" => Some(Chosen::Ignore),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summary::summarize;
    use crate::testutil::{changes, upstream, verdict_of};

    #[test]
    fn body_fields_and_limits() {
        let up = upstream();
        let s = Settings::default();
        let cs = changes("v0.20.0", 30);
        let sum = summarize(&cs);
        let b = body(
            &up,
            &cs,
            &sum,
            &verdict_of(3, 0.08, 0.64),
            &s,
            "/d/runs/2026-11-02.md",
        );
        assert_eq!(
            b["question"],
            "New upstream X: 30 commits (30 fixes). Port the changes into k?"
        );
        assert_eq!(b["options"], json!(["Port into ours", "Later", "Ignore"]));
        assert_eq!(b["recommendation"], "Port into ours");
        assert_eq!(b["impact"], "high");
        assert_eq!(b["reversible"], true);
        assert_eq!(b["urgency"], "low");
        assert_eq!(b["topic"], "upstream-watch");
        let ctx = b["context"].as_str().unwrap();
        assert!(ctx.contains("JEV: importance 3/4 (0.70), security 0.08, breaking 0.20, relies 0.64, effort medium"), "{ctx}");
        assert!(ctx.contains("Pinned v0.19.0 → latest v0.20.0."));
        assert!(ctx.contains("Licences: upstream MPL-2.0, ours MPL-2.0."));
        assert!(ctx.contains("/d/runs/2026-11-02.md"));
        assert!(ctx.contains("docs/porting-guidelines.md"));

        let mut long = sum.clone();
        long.headline = "x".repeat(5000);
        long.lines = vec!["y".repeat(3000); 15];
        let b = body(&up, &cs, &long, &verdict_of(1, 0.9, 0.1), &s, "r");
        assert!(b["question"].as_str().unwrap().chars().count() <= QUESTION_MAX);
        assert!(b["context"].as_str().unwrap().chars().count() <= CONTEXT_MAX);
        assert_eq!(b["urgency"], "high");
    }

    #[test]
    fn recommendation_rules() {
        let s = Settings::default();
        let r = |i, sec, rel| recommendation(&verdict_of(i, sec, rel), &s);
        assert_eq!(r(1, 0.6, 0.0), "Port into ours", "security");
        assert_eq!(r(3, 0.1, 0.5), "Port into ours", "important and relied on");
        assert_eq!(r(3, 0.1, 0.4), "Later");
        assert_eq!(r(4, 0.0, 0.0), "Port into ours", "major");
        assert_eq!(r(2, 0.49, 0.9), "Later");
    }

    #[test]
    fn contrib_question() {
        let up = upstream();
        let b = contrib_body(&up, &up.contrib[0]);
        assert_eq!(b["question"], "Contribute our a faster boot path back to X upstream? Draft only: nothing is pushed or opened upstream without your OK.");
        assert_eq!(b["options"][0], "Draft a contribution (no push)");
        assert_eq!(b["recommendation"], "Later");
        assert_eq!(b["topic"], "upstream-contrib");
        assert_eq!(b["impact"], "high");
    }

    #[test]
    fn chosen_mapping() {
        let d = |v: Value| chosen(&v);
        assert_eq!(d(json!({"status": "pending"})), None);
        assert_eq!(
            d(json!({"status": "decided", "chosen_index": 1})),
            Some(Chosen::Port)
        );
        assert_eq!(
            d(json!({"status": "decided", "chosen_index": 2})),
            Some(Chosen::Later)
        );
        assert_eq!(
            d(json!({"status": "decided", "chosen_index": 3})),
            Some(Chosen::Ignore)
        );
        assert_eq!(
            d(json!({"status": "decided", "chosen_option": "Ignore"})),
            Some(Chosen::Ignore)
        );
        assert_eq!(
            d(json!({"status": "decided", "chosen_option": "Draft a contribution (no push)"})),
            Some(Chosen::Port)
        );
        assert_eq!(
            d(json!({"status": "decided", "chosen_option": "Never"})),
            Some(Chosen::Ignore)
        );
        assert_eq!(
            d(
                json!({"status": "decided", "decision": "owner_reply", "reason": "only the vfs part", "chosen_index": null})
            ),
            Some(Chosen::OwnerReply("only the vfs part".into()))
        );
        assert_eq!(d(json!({"status": "expired"})), None);
    }
}
