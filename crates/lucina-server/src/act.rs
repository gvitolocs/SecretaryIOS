//! Acting through an agent: what the owner asks for on one of his devices or machines, which she
//! has no tool of her own for ("turn on nespc and put Pokemon Diamond on this DSi"), goes to a
//! Lucina Agents run. `chat::act_turn` turns Jev's `agent_task` into a job; the job body is [`job`].
//!
//! - [`brief`]: the agent's prompt, from his words and the device he asked from. Pure.
//! - [`run`]: start the agent, poll it until it finishes or the time cap, read its final message
//!   through agentd (`POST /agents/{id}/wait` has it as `lastMessage`), word it for the owner.
//! - [`crate::config::ActConfig`]: what she runs it with (`LUCINA_ACT_*`): a free opencode model
//!   and a mode that doesn't ask, never the flaw dispatcher's Claude policy.
//! - [`Hands`]: what [`run`] does to the world, so a test can stand in for agentd.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use lucina_tools::tools::reach::{agentd_request, allowed_model};
use serde_json::{json, Value};

use crate::config::ActConfig;
use crate::AppState;

/// The planner action and job action.
pub const ACTION: &str = "agent_task";
/// The agentd origin of every run started here is `service:lucina` (`agentd_request`).
const POLL: Duration = Duration::from_secs(5);
/// Failed status reads in a row before the run is given up.
const MAX_STATUS_ERRORS: u32 = 6;

/// The labels of every act run: who started it, and that it is an act run. agentd reads every
/// label value as a string, so `act` is the text `"true"` and never a boolean (LA-C049).
pub fn labels() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("from".to_owned(), "lucina".to_owned()),
        ("act".to_owned(), "true".to_owned()),
    ])
}

/// What [`AgentdHands::start_agent`] sends agentd: only the keys the settings carry.
pub fn agentd_body(
    act: &ActConfig,
    cwd: &str,
    prompt: &str,
    title: &str,
    labels: BTreeMap<String, String>,
) -> Value {
    let mut body = json!({
        "provider": act.provider,
        "model": act.model,
        "cwd": cwd,
        "prompt": prompt,
        "title": clip(title, 80),
        "labels": labels,
    });
    if let Some(effort) = &act.effort {
        body["thinkingOptionId"] = json!(effort);
    }
    if let Some(mode) = &act.mode {
        body["modeId"] = json!(mode);
    }
    body
}

/// What the run does to the world: agentd on its socket, or a test's stand-in.
#[async_trait::async_trait]
pub trait Hands: Send + Sync {
    /// An agent working in `cwd`: its id.
    async fn start_agent(
        &self,
        act: &ActConfig,
        cwd: &str,
        prompt: &str,
        title: &str,
        labels: BTreeMap<String, String>,
    ) -> Result<String, String>;
    /// The agent's status; `Ok(None)`: it no longer exists; `Err`: agentd can't say now.
    async fn agent_status(&self, id: &str) -> Result<Option<String>, String>;
    /// The agent's final message (and its error, if it ended on one) once it is not running.
    async fn agent_answer(&self, id: &str) -> Result<Answer, String>;
    async fn stop_agent(&self, id: &str);
}

/// What a finished agent left.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    pub text: Option<String>,
    pub error: Option<String>,
}

/// The real hands: agentd on its socket, as `service:lucina`.
pub struct AgentdHands;

impl AgentdHands {
    async fn agentd(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        agentd_request(&crate::agentd::socket_path(), method, path, body).await
    }
}

/// The statuses agentd gives a run that is still working: everything else ended it.
pub const WORKING: [&str; 2] = ["running", "initializing"];

/// What agentd's `GET /agents/{id}` says the run's status is; `None`: the answer names none.
pub fn status_of(v: &Value) -> Option<String> {
    v["status"].as_str().map(str::to_owned)
}

/// What agentd's `POST /agents/{id}/wait` answer left of the run: its last message, and the error
/// it ended on if it ended on one.
pub fn answer_of(v: &Value) -> Answer {
    Answer {
        text: v["lastMessage"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        error: v["error"].as_str().map(str::to_owned),
    }
}

#[async_trait::async_trait]
impl Hands for AgentdHands {
    async fn start_agent(
        &self,
        act: &ActConfig,
        cwd: &str,
        prompt: &str,
        title: &str,
        labels: BTreeMap<String, String>,
    ) -> Result<String, String> {
        let body = agentd_body(act, cwd, prompt, title, labels);
        let v = self.agentd("POST", "/agents", Some(&body)).await?;
        v["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "agentd returned no agent id".to_owned())
    }

    async fn agent_status(&self, id: &str) -> Result<Option<String>, String> {
        match self.agentd("GET", &format!("/agents/{id}"), None).await {
            Ok(v) => Ok(status_of(&v)),
            Err(e) if e.starts_with("agentd 404") => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn agent_answer(&self, id: &str) -> Result<Answer, String> {
        // The agent is not running, so the wait answers at once with `{status, error?, lastMessage?}`.
        let v = self
            .agentd(
                "POST",
                &format!("/agents/{id}/wait?timeoutMs=1000"),
                Some(&json!({})),
            )
            .await?;
        Ok(answer_of(&v))
    }

    async fn stop_agent(&self, id: &str) {
        if let Err(e) = self
            .agentd("POST", &format!("/agents/{id}/stop"), Some(&json!({})))
            .await
        {
            tracing::warn!(agent = id, error = %e, "act: couldn't stop the agent");
        }
    }
}

fn clip(s: &str, max: usize) -> String {
    lucina_delegate::types::clip(s, max)
}

/// How often the run looks at the agent, and when it gives up.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub poll: Duration,
    pub cap: Duration,
}

impl Timing {
    /// The act settings' run minutes, looked at every few seconds.
    pub fn of(act: &ActConfig) -> Self {
        Self {
            poll: POLL,
            cap: Duration::from_secs(act.minutes.max(1) * 60),
        }
    }
}

/// The agent's prompt: his request as he wrote it, the device he asked from and that it is on and
/// connected right now, what is known about that device and his machines, and the rules.
pub fn brief(message: &str, device_channel: &str) -> String {
    let device = crate::device::label(device_channel);
    let mut out = String::new();
    out.push_str("You are an agent started by Lucina, the owner's personal assistant, to carry out one request for him. He asked in his own words:\n\n");
    out.push_str(message.trim());
    out.push_str("\n\n");
    out.push_str(&format!(
        "He sent it from his {device} (channel `{device_channel}`). The {device} is switched on and connected to Lucina right now: this message came from it.\n\n"
    ));
    let notes = crate::device::agent_notes(device_channel);
    if !notes.is_empty() {
        out.push_str(&format!("About the {device}: {notes}\n\n"));
    }
    out.push_str(crate::device::MACHINES);
    out.push_str("\n\nRules:\n");
    out.push_str("- Do the thing he asked. Do not write code into repositories, and do not change anything he did not ask about.\n");
    out.push_str("- Kill nothing you did not start yourself.\n");
    out.push_str("- STOP and ask through `lucina_decide` (never proceed on your own) for anything that costs money, needs credentials or a password, accepts terms or an agreement, or sends a message in his name.\n");
    out.push_str(
        "- Finish with 1-3 plain sentences saying what was done, what was not, and why. No logs.\n",
    );
    out
}

/// Start the agent, wait for it, and word what it did. `word` turns the agent's final message into
/// the reply the owner reads. The agent is stopped when it overruns `timing.cap`.
/// `Ok((reply, raw))`: the reply and the agent's own final message.
pub async fn run<W, F>(
    hands: &dyn Hands,
    act: &ActConfig,
    prompt: &str,
    title: &str,
    cwd: &std::path::Path,
    timing: Timing,
    word: W,
) -> Result<(String, String), String>
where
    W: FnOnce(String) -> F,
    F: std::future::Future<Output = Result<String, String>>,
{
    allowed_model(&act.provider, Some(&act.model))?;
    tokio::fs::create_dir_all(cwd)
        .await
        .map_err(|e| format!("no working directory for the agent: {e}"))?;
    let cwd = cwd.display().to_string();
    let id = hands.start_agent(act, &cwd, prompt, title, labels()).await?;
    tracing::info!(agent = %id, "act: agent started");
    let started = Instant::now();
    let mut errors = 0u32;
    loop {
        tokio::time::sleep(timing.poll).await;
        match hands.agent_status(&id).await {
            Ok(Some(s)) if WORKING.contains(&s.as_str()) => errors = 0,
            Ok(Some(_)) => break,
            Ok(None) => return Err("the agent is gone: Lucina Agents no longer knows it".into()),
            Err(e) => {
                errors += 1;
                tracing::warn!(agent = %id, error = %e, "act: agentd can't say");
                if errors >= MAX_STATUS_ERRORS {
                    return Err(format!(
                        "I lost contact with Lucina Agents while the agent was working ({e})"
                    ));
                }
            }
        }
        if started.elapsed() >= timing.cap {
            hands.stop_agent(&id).await;
            return Err(format!(
                "the agent did not finish within {} minutes, so I stopped it",
                (timing.cap.as_secs() / 60).max(1)
            ));
        }
    }
    let answer = hands.agent_answer(&id).await?;
    let raw = match (answer.text, answer.error) {
        (Some(text), _) => text,
        (None, Some(e)) => return Err(format!("the agent ended on an error: {}", clip(&e, 200))),
        (None, None) => return Err("the agent finished without saying what it did".into()),
    };
    let reply = word(raw.clone()).await?;
    Ok((reply, raw))
}

/// The job body of an `agent_task`: [`run`] over agentd, worded by the planner.
pub async fn job(
    st: &AppState,
    t: &crate::planner::TurnInput<'_>,
    plan: &Value,
    job_id: &str,
) -> Result<(String, String), String> {
    let message = plan["task"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(t.user_text);
    let channel = plan["channel"].as_str().unwrap_or("web");
    let prompt = plan["brief"]
        .as_str()
        .map(str::to_owned)
        .filter(|b| !b.trim().is_empty())
        .unwrap_or_else(|| brief(message, channel));
    let act = &st.cfg.act;
    let cwd = st.cfg.data_dir.join("act").join(job_id);
    let title = format!("Lucina act: {}", clip(message, 60));
    run(&AgentdHands, act, &prompt, &title, &cwd, Timing::of(act), |answer| async move {
        st.planner
            .generate_skill_reply(
                t,
                "Lucina Agents",
                &answer,
                "Answer in 1-3 short sentences. Say what the agent did, or what blocked it. No raw logs.",
            )
            .await
            .map_err(|e| e.to_string())
    })
    .await
}

/// `jobs::blocker` for an `agent_task`: the agents' daemon has to be there.
pub fn blocker() -> Option<String> {
    if crate::agentd::socket_path().exists() {
        None
    } else {
        Some(
            "Lucina Agents (agentd) isn't running, so I can't start an agent for that right now."
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DEFAULT_ACT_MODEL, DEFAULT_ACT_PROVIDER};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        /// What each started agent was asked for, in the shape agentd gets.
        started: Mutex<Vec<Value>>,
        stopped: Mutex<Vec<String>>,
        /// Statuses to give, in order; the last one repeats.
        statuses: Mutex<Vec<Result<Option<String>, String>>>,
        answer: Mutex<Answer>,
        down: bool,
    }

    #[async_trait::async_trait]
    impl Hands for Fake {
        async fn start_agent(
            &self,
            act: &ActConfig,
            cwd: &str,
            prompt: &str,
            title: &str,
            labels: BTreeMap<String, String>,
        ) -> Result<String, String> {
            if self.down {
                return Err("Lucina Agents (agentd) is not reachable: no such file".into());
            }
            self.started
                .lock()
                .unwrap()
                .push(agentd_body(act, cwd, prompt, title, labels));
            Ok("ag-1".into())
        }
        async fn agent_status(&self, _id: &str) -> Result<Option<String>, String> {
            let mut s = self.statuses.lock().unwrap();
            if s.len() > 1 {
                s.remove(0)
            } else {
                s[0].clone()
            }
        }
        async fn agent_answer(&self, _id: &str) -> Result<Answer, String> {
            Ok(self.answer.lock().unwrap().clone())
        }
        async fn stop_agent(&self, id: &str) {
            self.stopped.lock().unwrap().push(id.into());
        }
    }

    /// The act settings with nothing set: what Lucina runs on her own.
    fn act() -> ActConfig {
        ActConfig::from_lookup(&|_| None)
    }

    fn quick() -> Timing {
        Timing {
            poll: Duration::from_millis(5),
            cap: Duration::from_millis(80),
        }
    }

    fn running() -> Result<Option<String>, String> {
        Ok(Some("running".into()))
    }

    fn idle() -> Result<Option<String>, String> {
        Ok(Some("idle".into()))
    }

    async fn worded(answer: String) -> Result<String, String> {
        Ok(format!("worded: {answer}"))
    }

    #[tokio::test]
    async fn act_run_starts_one_agent_with_the_brief_and_returns_its_answer_worded() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("act").join("job-1");
        let h = Fake {
            statuses: Mutex::new(vec![running(), running(), idle()]),
            answer: Mutex::new(Answer {
                text: Some("Pokemon Diamond is on the DSi's SD card.".into()),
                error: None,
            }),
            ..Fake::default()
        };
        let b = brief(
            "Accendi nespc e passami Pokemon Diamante su questo DSi",
            "dsi",
        );
        let (reply, raw) = run(&h, &act(), &b, "t", &cwd, quick(), worded)
            .await
            .unwrap();
        assert_eq!(raw, "Pokemon Diamond is on the DSi's SD card.");
        assert_eq!(reply, "worded: Pokemon Diamond is on the DSi's SD card.");
        let started = h.started.lock().unwrap();
        assert_eq!(started.len(), 1, "exactly one agent");
        assert_eq!(started[0]["prompt"], b, "the brief is the prompt");
        assert_eq!(started[0]["cwd"], cwd.display().to_string());
        assert_eq!(
            started[0]["provider"], DEFAULT_ACT_PROVIDER,
            "her own settings, not the flaw dispatcher's Claude"
        );
        assert_eq!(started[0]["model"], DEFAULT_ACT_MODEL, "a free model");
        assert_eq!(
            started[0]["modeId"], "build",
            "a mode that runs its tools without asking: nobody answers a prompt here"
        );
        assert!(
            started[0].get("thinkingOptionId").is_none(),
            "no effort set, so the key isn't sent"
        );
        assert!(cwd.is_dir(), "the per-job directory was made");
        assert!(h.stopped.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn act_run_that_overruns_stops_the_agent_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let h = Fake {
            statuses: Mutex::new(vec![running()]),
            ..Fake::default()
        };
        let e = run(&h, &act(), "b", "t", dir.path(), quick(), worded)
            .await
            .unwrap_err();
        assert!(
            e.contains("did not finish") && e.contains("stopped it"),
            "{e}"
        );
        assert_eq!(*h.stopped.lock().unwrap(), vec!["ag-1".to_owned()]);
    }

    #[tokio::test]
    async fn act_run_with_agentd_down_is_an_error_and_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let h = Fake {
            down: true,
            statuses: Mutex::new(vec![idle()]),
            ..Fake::default()
        };
        let e = run(&h, &act(), "b", "t", dir.path(), quick(), worded)
            .await
            .unwrap_err();
        assert!(e.contains("not reachable"), "{e}");
        assert!(h.started.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn act_run_reports_an_agent_that_ended_on_an_error_or_said_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let h = Fake {
            statuses: Mutex::new(vec![Ok(Some("error".into()))]),
            answer: Mutex::new(Answer {
                text: None,
                error: Some("quota".into()),
            }),
            ..Fake::default()
        };
        let e = run(&h, &act(), "b", "t", dir.path(), quick(), worded)
            .await
            .unwrap_err();
        assert!(
            e.contains("ended on an error") && e.contains("quota"),
            "{e}"
        );
        let h = Fake {
            statuses: Mutex::new(vec![idle()]),
            ..Fake::default()
        };
        let e = run(&h, &act(), "b", "t", dir.path(), quick(), worded)
            .await
            .unwrap_err();
        assert!(e.contains("without saying"), "{e}");
        let h = Fake {
            statuses: Mutex::new(vec![Ok(None)]),
            ..Fake::default()
        };
        let e = run(&h, &act(), "b", "t", dir.path(), quick(), worded)
            .await
            .unwrap_err();
        assert!(e.contains("gone"), "{e}");
    }

    #[test]
    fn act_brief_has_the_request_the_device_and_the_four_stops() {
        let b = brief(
            "Accendi i Anespici e passami Pokémon Diamante su questo DSi",
            "dsi",
        );
        assert!(
            b.contains("Accendi i Anespici e passami Pokémon Diamante su questo DSi"),
            "verbatim"
        );
        assert!(b.contains("Nintendo DSi"));
        assert!(b.contains("connected"));
        assert!(b.contains("switched on"));
        for stop in [
            "costs money",
            "credentials or a password",
            "accepts terms",
            "sends a message in his name",
        ] {
            assert!(b.contains(stop), "{stop}");
        }
        assert!(b.contains("lucina_decide"));
        assert!(
            b.contains("dsi.sh") && b.contains("SITE BOOT"),
            "device notes"
        );
        assert!(b.contains("nespc") && b.contains("nezopt"));
        assert!(
            b.contains("do not write code into repositories")
                || b.contains("Do not write code into repositories")
        );
        assert!(b.contains("Kill nothing you did not start"));
        let mac = brief("apri Safari", "mac");
        assert!(mac.contains("Mac") && mac.contains("connected") && !mac.contains("dsi.sh"));
    }
}
