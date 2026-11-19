//! Quota hand-off: a run whose provider stops on a quota, usage or rate limit (or is
//! unavailable) continues on another agent (owner, 2026-11-08: "then another agents
//! should continue them").
//!
//! - **Detection**: the failure text (the error and the last system error item) goes
//!   through the owner's lane detectors (`~/.config/lane/chains.toml` `[[detectors]]`),
//!   for the lane the agent's provider stands for ([`LaneConfig::lane_of_provider`]). A
//!   provider reply carrying `code: quota` counts as a quota too (as in `run_chain`).
//! - **Decision**: [`lucina_agents::handoff::decide`] (Verus-proven,
//!   `verification/verus/agent_handoff.rs`): real failures, helpers, external agents,
//!   explicit `--chain` runs, archived agents and runs carrying [`HANDOFF_OFF_LABEL`] stay;
//!   otherwise the first lane of the `deep-lucina` chain (lane's order; `deep` without Codex when
//!   it is missing) whose provider is registered, isn't Codex, isn't forbidden and wasn't tried by
//!   this run or the runs it continues.
//! - **Continuation**: a new agent on that lane's provider and model, in the same cwd and
//!   workspace (uncommitted changes stay as they are), seeded with the chat history
//!   ([`lucina_agents::fork_context`]) and sent the original task with a note on what
//!   happened. Labels link the two: [`CONTINUED_FROM_LABEL`] on the new run,
//!   [`CONTINUED_ON_LABEL`] / [`CONTINUED_PROVIDER_LABEL`] on the old one, whose list
//!   bucket is then `done` instead of `failed`. A continuation that hits a limit hands on
//!   again, with the providers already tried left out ([`TRIED_LABEL`]).
//! - **Followers**: `wait`, orchestration finish notices and Lucina's reach watcher (a
//!   reach thread's reply) move to the continuation; while the hand-off is being
//!   decided the old run carries [`HANDOFF_LABEL`] = `pending` and counts as busy.

use lucina_agents::fork_context::{self, Boundary};
use lucina_agents::handoff::{self, Candidate, FailureClass, RunFacts};
use lucina_agents::timeline::NotificationLevel::{Info, Warning};
use lucina_agents::{AgentStatus, TimelineItem};

use super::{rewind::FORKED_LABEL, Agent, CreateRequest, Manager, JUNO_LABEL};
use crate::lane::{self, Outcome};
use lucina_agents::external as ext;

/// On the old run while the hand-off is decided (`pending`), or why none happened.
pub const HANDOFF_LABEL: &str = "lucina.handoff";
/// On the old run: the continuation's agent id.
pub const CONTINUED_ON_LABEL: &str = "lucina.continued-on";
/// On the old run: the continuation's provider.
pub const CONTINUED_PROVIDER_LABEL: &str = "lucina.continued-provider";
/// On the continuation: the run it continues.
pub const CONTINUED_FROM_LABEL: &str = "lucina.continued-from";
/// On the continuation: the providers the hand-off sequence already used (comma list).
pub const TRIED_LABEL: &str = "lucina.handoff.tried";
/// On the continuation: how many hand-offs led to it.
pub const COUNT_LABEL: &str = "lucina.handoff.count";
/// On a run that opts out of the hand-off: it stays where its provider stopped. Lucina's flaw
/// dispatcher labels its agents with this (owner, 2026-11-13: a triage handed on to an expensive
/// agent is a triage that escaped the cheap end of the chain).
pub const HANDOFF_OFF_LABEL: &str = "lucina.handoff-off";

const PENDING: &str = "pending";
const TASK_OPEN: &str = "<original-task>";
const TASK_CLOSE: &str = "</original-task>";

/// Modes that run without asking before each tool call, per provider family. The
/// continuation keeps the owner's choice: a run started in one gets one too when its
/// new provider offers one, else the provider's default.
const NO_ASK_MODES: &[&str] = &[
    "bypassPermissions",
    "full-access",
    "auto-approve",
    "yolo",
    "dontAsk",
];

/// Labels the continuation doesn't inherit.
fn inherited(key: &str) -> bool {
    !(key.starts_with("lucina.handoff")
        || key.starts_with("lucina.continued-")
        || key == "lucina.lane"
        || key == "lucina.chain"
        || key == FORKED_LABEL)
}

/// Was this run started with the hand-off turned off (`lucina.handoff-off=1`)? Anything else, and
/// an absent label, keeps the hand-off on.
fn handoff_off(labels: &std::collections::BTreeMap<String, String>) -> bool {
    labels.get(HANDOFF_OFF_LABEL).map(String::as_str) == Some("1")
}

/// The text a failure is classified on: the error and the last system error item.
fn failure_text(a: &Agent) -> String {
    let last_system = a.rows.iter().rev().find_map(|r| match &r.item {
        TimelineItem::AssistantMessage { text, .. }
            if text.starts_with(lucina_agents::events::SYSTEM_ERROR_PREFIX) =>
        {
            Some(text.clone())
        }
        _ => None,
    });
    format!(
        "{}\n{}",
        a.record.last_error.clone().unwrap_or_default(),
        last_system.unwrap_or_default()
    )
}

/// The task the run was given: the first user message, or the original task a
/// continuation's prompt carries.
pub(super) fn original_task(a: &Agent) -> Option<String> {
    let first = a.rows.iter().find_map(|r| match &r.item {
        TimelineItem::UserMessage { text, .. } => Some(text.clone()),
        _ => None,
    })?;
    Some(extract_task(&first))
}

pub fn extract_task(prompt: &str) -> String {
    match (prompt.find(TASK_OPEN), prompt.rfind(TASK_CLOSE)) {
        (Some(a), Some(b)) if a + TASK_OPEN.len() <= b => {
            prompt[a + TASK_OPEN.len()..b].trim().to_string()
        }
        _ => prompt.trim().to_string(),
    }
}

/// The prompt the continuation gets (after the chat history).
pub fn continuation_prompt(
    from_provider: &str,
    why: &str,
    task: &str,
    status: Option<&str>,
) -> String {
    // Porcelain's leading space is meaningful (` M` = changed, not staged): trim the end only.
    let status = match status.map(str::trim_end) {
        None => "(not a git repository, or git status failed)".to_string(),
        Some(s) if s.trim().is_empty() => "clean: no uncommitted changes".to_string(),
        Some(s) => {
            let mut clipped: String = s.chars().take(4000).collect();
            if clipped.len() < s.len() {
                clipped.push_str("\n…");
            }
            clipped
        }
    };
    format!(
        "You are continuing a run that another agent ({from_provider}) could not finish: it stopped on {why}. \
The chat history above is that run so far. You work in the same directory. Any uncommitted changes there are its \
partial work: keep them, read `git status` and `git diff` first, and carry on from where it stopped. Do not reset, \
stash or start over. Finish the original task as it asks, including any reply or report it asks you to send; \
where it names a tool, path or command, use the same one. Its rules still hold.\n\n\
Workspace status when the run stopped:\n{status}\n\n{TASK_OPEN}\n{task}\n{TASK_CLOSE}\n"
    )
}

/// The mode the continuation gets on `modes` (the target's), given the source's mode.
pub fn carry_mode(source: Option<&str>, target_modes: &[String]) -> Option<String> {
    let no_ask = source.is_some_and(|m| NO_ASK_MODES.contains(&m));
    if !no_ask {
        return None;
    }
    NO_ASK_MODES
        .iter()
        .find(|m| target_modes.iter().any(|t| t == *m))
        .map(|m| m.to_string())
}

impl Manager {
    /// Called with the agent's lock held, right after a turn failed: classify the
    /// failure, and when it may be handed on, mark it pending. Returns what the
    /// hand-off task needs (spawn [`Manager::hand_off`] with it).
    pub(super) fn handoff_check(&self, agent: &mut Agent) -> Option<(FailureClass, String)> {
        let r = &agent.record;
        if r.internal
            || r.external
            || ext::is_session(&r.provider)
            || r.provider == ext::PROVIDER
            || agent.chain_active
            || r.archived_at.is_some()
            || r.labels.contains_key(CONTINUED_ON_LABEL)
            || handoff_off(&r.labels)
        {
            return None;
        }
        let text = failure_text(agent);
        let outcome = if text.contains("code: quota") {
            Outcome::Quota("provider-quota-reply".into())
        } else {
            match self
                .inner
                .lanes
                .lane_of_provider(&r.provider, r.labels.get("lucina.lane").map(String::as_str))
            {
                Some((name, l)) => self.inner.lanes.classify(&name, &l.kind, &text, None),
                None => Outcome::Failed,
            }
        };
        let (class, detector) = match outcome {
            Outcome::Quota(d) => (FailureClass::Quota, d),
            Outcome::Unavailable(d) => (FailureClass::Unavailable, d),
            Outcome::Failed => return None,
        };
        agent
            .record
            .labels
            .insert(HANDOFF_LABEL.into(), PENDING.into());
        tracing::info!(agent = %agent.record.id, provider = %agent.record.provider, %detector, "run stopped on a limit; handing it on");
        Some((class, detector))
    }

    pub(super) fn spawn_handoff(&self, id: String, class: FailureClass, detector: String) {
        let manager = self.clone();
        tokio::spawn(async move { manager.hand_off(id, class, detector).await });
    }

    fn set_handoff_result(
        &self,
        id: &str,
        labels: &[(&str, Option<String>)],
        note: Option<(lucina_agents::timeline::NotificationLevel, String)>,
    ) {
        {
            let mut agents = self.lock();
            if let Some(agent) = agents.get_mut(id) {
                for (k, v) in labels {
                    match v {
                        Some(v) => agent.record.labels.insert(k.to_string(), v.clone()),
                        None => agent.record.labels.remove(*k),
                    };
                }
                if let Some((level, message)) = note {
                    self.append(agent, TimelineItem::Notification { level, message }, None);
                }
                // A hand-off that ended with nobody to take the run over leaves the agent in error for
                // good: it gives up its provider process now (AGENT-01). One that went on to a
                // continuation keeps its process as before.
                if labels.iter().any(|(k, v)| {
                    *k == HANDOFF_LABEL
                        && v.as_deref()
                            .is_some_and(|v| v == "exhausted" || v.starts_with("none"))
                }) {
                    self.release_if_errored(agent);
                }
                self.persist(agent);
                self.publish_state(agent);
            }
        }
        self.bump();
    }

    fn provider_label(&self, id: &str) -> String {
        self.inner
            .providers
            .get(id)
            .map(|p| p.info().label)
            .unwrap_or_else(|| id.to_string())
    }

    /// Start the continuation of `id` (module docs).
    pub(super) async fn hand_off(&self, id: String, class: FailureClass, detector: String) {
        let Some((record, task, history)) = ({
            let agents = self.lock();
            agents.get(&id).map(|a| {
                let history = fork_context::build(
                    &a.rows,
                    &a.record.epoch,
                    Boundary::default(),
                    a.record.title.as_deref(),
                    Some(&a.record.cwd),
                )
                .map(|c| c.attachment.text)
                .ok();
                (a.record.clone(), original_task(a), history)
            })
        }) else {
            return;
        };
        let what = match class {
            FailureClass::Quota => "a usage or rate limit",
            _ => "an unavailable provider",
        };
        let why = format!("{what} ({detector})");
        let from_label = self.provider_label(&record.provider);
        let Some(task) = task.filter(|t| !t.trim().is_empty()) else {
            self.set_handoff_result(
                &id,
                &[(HANDOFF_LABEL, Some("none: no task to continue".into()))],
                Some((
                    Warning,
                    format!("{from_label} stopped on {why}; there is no task to hand on"),
                )),
            );
            return;
        };
        let status = Self::worktree_status(&record.cwd).await;
        let handoffs: u32 = record
            .labels
            .get(COUNT_LABEL)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let mut tried: Vec<String> = record
            .labels
            .get(TRIED_LABEL)
            .map(|t| {
                t.split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if !tried.contains(&record.provider) {
            tried.push(record.provider.clone());
        }
        let facts = RunFacts {
            class,
            internal: record.internal,
            external: record.external,
            chain_run: record.labels.contains_key("lucina.chain"),
            archived: record.archived_at.is_some(),
            handoffs,
            dirty: status.as_deref().is_some_and(|s| !s.trim().is_empty()),
        };
        let category = if self.inner.lanes.categories.contains_key("deep-lucina") {
            "deep-lucina"
        } else {
            "deep"
        };
        let lanes = self.inner.lanes.chain(category);
        let mut skipped: Vec<String> = Vec::new();
        loop {
            let candidates: Vec<Candidate> = lanes
                .iter()
                .map(|(name, p, l)| Candidate {
                    registered: self.inner.providers.contains_key(*p),
                    codex: *p == "codex",
                    forbidden: lane::is_forbidden(name, l),
                    tried: tried.iter().any(|t| t == p),
                })
                .collect();
            let Some(i) = handoff::decide(&facts, &candidates) else {
                let left = if skipped.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", skipped.join("; "))
                };
                self.set_handoff_result(
                    &id,
                    &[(HANDOFF_LABEL, Some("exhausted".into()))],
                    Some((Warning, format!("{from_label} stopped on {why}; no other agent could take it over{left}"))),
                );
                return;
            };
            let (lane_name, provider, lane) = lanes[i].clone();
            tried.push(provider.to_string());
            let target_modes: Vec<String> = self
                .inner
                .providers
                .get(provider)
                .map(|p| p.info().modes.into_iter().map(|m| m.id).collect())
                .unwrap_or_default();
            let mut labels: std::collections::BTreeMap<String, String> = record
                .labels
                .iter()
                .filter(|(k, _)| inherited(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if provider != "claude" {
                labels.remove(JUNO_LABEL);
            }
            labels.insert(CONTINUED_FROM_LABEL.into(), id.clone());
            labels.insert(TRIED_LABEL.into(), tried.join(","));
            labels.insert(COUNT_LABEL.into(), (handoffs + 1).to_string());
            labels.insert("lucina.lane".into(), lane_name.clone());
            let req = CreateRequest {
                provider: provider.to_string(),
                model: lane.model.clone(),
                thinking_option_id: lane.effort.clone(),
                mode_id: carry_mode(
                    record
                        .last_mode_id
                        .as_deref()
                        .or(record.config.mode_id.as_deref()),
                    &target_modes,
                ),
                cwd: record.cwd.clone(),
                workspace_id: record.workspace_id.clone(),
                title: record.title.clone(),
                labels,
                tool_policy: record.config.tool_policy.clone(),
                // The run already passed the start guard in this directory.
                allow_main_checkout: true,
                seed_context: history.clone(),
                ..CreateRequest::default()
            };
            let to_label = self.provider_label(provider);
            match Box::pin(self.create(req)).await {
                Ok(snap) => {
                    let short: String = snap.id.chars().take(8).collect();
                    let from_short: String = id.chars().take(8).collect();
                    self.note(
                        &snap.id,
                        Info,
                        format!(
                            "Continued from agent {from_short} ({from_label} stopped on {why})"
                        ),
                    );
                    let prompt = continuation_prompt(&from_label, &why, &task, status.as_deref());
                    // The link goes on the old run only once the new one is running (or has failed
                    // and is handing on itself): a follower that moved to an idle, not yet started
                    // continuation would take it for finished. A failure here hands on again from
                    // the new agent (start_turn's own check).
                    if let Err(e) = self.start_turn(&snap.id, &prompt).await {
                        tracing::info!(agent = %snap.id, error = %e, "continuation's first turn failed");
                    }
                    self.set_handoff_result(
                        &id,
                        &[
                            (HANDOFF_LABEL, None),
                            (CONTINUED_ON_LABEL, Some(snap.id.clone())),
                            (CONTINUED_PROVIDER_LABEL, Some(provider.to_string())),
                        ],
                        Some((Info, format!("{from_label} stopped on {why}; continued on {to_label} as agent {short}"))),
                    );
                    return;
                }
                Err(e) => {
                    tracing::warn!(agent = %id, provider, error = %e, "continuation not started");
                    skipped.push(format!(
                        "{to_label}: {}",
                        e.to_string().chars().take(160).collect::<String>()
                    ));
                    // A half-made agent (its session didn't start) goes: it never ran anything.
                    let stray: Vec<String> = self
                        .lock()
                        .values()
                        .filter(|a| {
                            a.record.labels.get(CONTINUED_FROM_LABEL) == Some(&id)
                                && a.record.provider == provider
                                && a.k.status == AgentStatus::Error
                                && a.rows.is_empty()
                        })
                        .map(|a| a.record.id.clone())
                        .collect();
                    for s in stray {
                        let _ = self.delete(&s).await;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// A run the dispatcher labelled `lucina.handoff-off=1` is not handed on: [`Manager::handoff_check`]
    /// returns `None` on this predicate before it looks at the failure at all, which is what keeps a
    /// triage that stopped on the free lane from carrying on with an expensive agent.
    #[test]
    fn a_run_opted_out_of_the_hand_off_stays() {
        assert!(handoff_off(&labels(&[(HANDOFF_OFF_LABEL, "1")])));
        // No label, another value, or a hand-off label of its own: the hand-off stays on.
        assert!(!handoff_off(&labels(&[])));
        assert!(!handoff_off(&labels(&[(HANDOFF_OFF_LABEL, "0")])));
        assert!(!handoff_off(&labels(&[(HANDOFF_OFF_LABEL, "yes")])));
        assert!(!handoff_off(&labels(&[
            (HANDOFF_LABEL, "pending"),
            (CONTINUED_FROM_LABEL, "ag-1")
        ])));
    }

    #[test]
    fn task_survives_a_second_hand_off() {
        let p = continuation_prompt(
            "Claude",
            "a limit (x)",
            "Fix the bug.\nThen reply.",
            Some(" M a.rs\n"),
        );
        assert!(p.contains(" M a.rs"));
        assert_eq!(extract_task(&p), "Fix the bug.\nThen reply.");
        let p2 = continuation_prompt("Cursor", "a limit (y)", &extract_task(&p), Some(""));
        assert_eq!(extract_task(&p2), "Fix the bug.\nThen reply.");
        assert!(p2.contains("clean: no uncommitted changes"));
        assert_eq!(extract_task("  plain task "), "plain task");
    }

    #[test]
    fn modes_carry_only_when_both_skip_prompts() {
        let vibe = vec![
            "default".to_string(),
            "plan".into(),
            "accept-edits".into(),
            "auto-approve".into(),
        ];
        assert_eq!(
            carry_mode(Some("bypassPermissions"), &vibe).as_deref(),
            Some("auto-approve")
        );
        assert_eq!(carry_mode(Some("default"), &vibe), None);
        assert_eq!(carry_mode(None, &vibe), None);
        assert_eq!(
            carry_mode(Some("bypassPermissions"), &["default".to_string()]),
            None
        );
    }

    #[test]
    fn handoff_labels_are_not_inherited() {
        for k in [
            HANDOFF_LABEL,
            CONTINUED_ON_LABEL,
            CONTINUED_FROM_LABEL,
            TRIED_LABEL,
            COUNT_LABEL,
            "lucina.lane",
            "lucina.chain",
            FORKED_LABEL,
        ] {
            assert!(!inherited(k), "{k}");
        }
        for k in ["reach", "from", super::super::PARENT_LABEL] {
            assert!(inherited(k), "{k}");
        }
    }
}
