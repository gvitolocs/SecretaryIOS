//! `root_exec`: commands as root on the nezopt host, only after the owner
//! approved exactly those commands.
//!
//! The plan is a list of argv arrays (no shell). The approval (`root.exec`,
//! sensitive, risk high) shows the commands one per line, shell-quoted, and
//! binds the canonical JSON of the argv list as its payload hash. With a
//! verified, unspent grant each argv runs through `nezopt-sudo` (path:
//! `LUCINA_ROOT_SUDO`) with a per-command timeout and capped output; the run
//! stops at the first failure. Every run, denial and refusal is appended to
//! the audit file.

use std::future::Future;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::canon::{plan_hash, shell_line};
use crate::client::{clip, NewApproval, RequesterInfo, MAX_SUMMARY};
use crate::flow::{Approvals, Begun, Outcome};
use crate::{now_unix, ApprovalError};

/// Most commands in one plan.
pub const MAX_COMMANDS: usize = 40;
const MAX_ARG_LEN: usize = 4096;
pub const ACTION: &str = "root.exec";

/// Who is asking, as the owner sees it.
#[derive(Debug, Clone)]
pub struct Who {
    pub kind: String,
    pub name: String,
    pub detail: String,
}

impl Who {
    pub fn lucina(detail: impl Into<String>) -> Self {
        Self {
            kind: "lucina".into(),
            name: "Lucina".into(),
            detail: detail.into(),
        }
    }
}

/// One command's result.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StepResult {
    pub argv: Vec<String>,
    pub ok: bool,
    #[serde(rename = "exitCode")]
    pub exit_code: Option<i32>,
    #[serde(rename = "timedOut")]
    pub timed_out: bool,
    pub error: Option<String>,
    pub stdout: String,
    pub stderr: String,
    /// Output was cut at the cap.
    pub truncated: bool,
    pub ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RootOutcome {
    /// Every command ran and succeeded.
    Done,
    /// Command `index` (0-based) failed; later ones did not run.
    Failed {
        index: usize,
    },
    Denied {
        decided_by: Option<String>,
    },
    Expired,
    /// No answer yet; nothing ran.
    Pending {
        expires_at: i64,
    },
    /// Nothing ran: the request could not be made or the grant was not acceptable.
    Refused(String),
}

#[derive(Debug, Clone)]
pub struct RootReport {
    pub outcome: RootOutcome,
    pub approval_id: Option<String>,
    pub payload_hash: String,
    /// The steps that ran (a failed one is the last).
    pub steps: Vec<StepResult>,
}

impl RootReport {
    pub fn is_done(&self) -> bool {
        self.outcome == RootOutcome::Done
    }

    /// A short, value-free line for notifications and logs.
    pub fn summary(&self) -> String {
        let n = self.steps.len();
        match &self.outcome {
            RootOutcome::Done => format!("Root commands approved and run: {n} of {n} ok."),
            RootOutcome::Failed { index } => {
                let s = &self.steps[*index];
                let why = match (s.timed_out, s.exit_code, &s.error) {
                    (true, _, _) => "timed out".to_owned(),
                    (_, Some(c), _) => format!("exit {c}"),
                    (_, _, Some(e)) => e.clone(),
                    _ => "failed".to_owned(),
                };
                format!(
                    "Root commands stopped at step {} ({why}): {}",
                    index + 1,
                    clip(&shell_line(&s.argv), 160)
                )
            }
            RootOutcome::Denied { decided_by } => match decided_by {
                Some(d) => format!("Root commands denied by {d}; nothing ran."),
                None => "Root commands denied; nothing ran.".to_owned(),
            },
            RootOutcome::Expired => "Root commands not approved in time; nothing ran.".to_owned(),
            RootOutcome::Pending { .. } => {
                "Root commands still waiting for approval; nothing ran.".to_owned()
            }
            RootOutcome::Refused(why) => format!("Root commands not run: {why}"),
        }
    }
}

/// A root plan whose approval exists on the server.
#[derive(Debug, Clone)]
pub struct RootPending {
    pub request: NewApproval,
    pub begun: Begun,
    pub plan: Vec<Vec<String>>,
    pub reason: String,
    pub who: Who,
}

/// What [`Approvals::root_spawn`] returns at once.
#[derive(Debug, Clone)]
pub struct Spawned {
    pub id: String,
    pub expires_at: i64,
    /// The same plan was already being waited for; nothing new was started.
    pub already_waiting: bool,
}

/// Reject plans that cannot be shown or run faithfully.
pub fn validate_plan(plan: &[Vec<String>]) -> Result<(), ApprovalError> {
    let bad = |m: String| Err(ApprovalError::Invalid(m));
    if plan.is_empty() {
        return bad("the plan has no commands".into());
    }
    if plan.len() > MAX_COMMANDS {
        return bad(format!("the plan has more than {MAX_COMMANDS} commands"));
    }
    for (i, argv) in plan.iter().enumerate() {
        if argv.first().is_none_or(|a| a.is_empty()) {
            return bad(format!("command {} is empty", i + 1));
        }
        if argv
            .iter()
            .any(|a| a.contains('\0') || a.len() > MAX_ARG_LEN)
        {
            return bad(format!(
                "command {} has an argument with a NUL byte or over {MAX_ARG_LEN} bytes",
                i + 1
            ));
        }
    }
    Ok(())
}

/// The approval's `details`: the commands, one per line, shell-quoted.
pub fn plan_details(plan: &[Vec<String>]) -> String {
    plan.iter()
        .map(|a| shell_line(a))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The approval request for a plan.
pub fn root_request(
    plan: &[Vec<String>],
    reason: &str,
    who: &Who,
    ttl_secs: u64,
) -> Result<NewApproval, ApprovalError> {
    validate_plan(plan)?;
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(ApprovalError::Invalid("a reason is required".into()));
    }
    let n = plan.len();
    Ok(NewApproval {
        requester: RequesterInfo {
            kind: who.kind.clone(),
            name: who.name.clone(),
            detail: clip(&who.detail, 200),
        },
        action: ACTION.into(),
        summary: clip(
            &format!(
                "Run {n} command{} as root on nezopt: {reason}",
                if n == 1 { "" } else { "s" }
            ),
            MAX_SUMMARY,
        ),
        details: plan_details(plan),
        risk: "high".into(),
        sensitive: true,
        payload_hash: plan_hash(plan),
        ttl_secs,
    })
}

async fn read_capped<R: AsyncRead + Unpin>(r: Option<R>, cap: usize) -> (String, bool) {
    let Some(mut r) = r else {
        return (String::new(), false);
    };
    let mut buf = Vec::new();
    let _ = (&mut r).take(cap as u64).read_to_end(&mut buf).await;
    // Keep draining so the child never blocks on a full pipe.
    let mut sink = [0u8; 8192];
    let mut truncated = false;
    loop {
        match r.read(&mut sink).await {
            Ok(0) | Err(_) => break,
            Ok(_) => truncated = true,
        }
    }
    (String::from_utf8_lossy(&buf).into_owned(), truncated)
}

impl Approvals {
    fn audit(&self, mut line: serde_json::Value) {
        line["ts"] = json!(chrono::Utc::now().to_rfc3339());
        let path = &self.config().root_audit;
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)?;
            writeln!(f, "{line}")
        };
        if let Err(e) = write() {
            tracing::error!(path = %path.display(), error = %e.kind(), "root exec audit line not written");
        }
    }

    /// The audit file can be opened for append (checked before asking).
    fn audit_ready(&self) -> Result<(), ApprovalError> {
        let path = &self.config().root_audit;
        let open = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)
                .map(|_| ())
        };
        open().map_err(|e| {
            ApprovalError::Io(format!(
                "the audit file {} is not writable ({}); root commands need it",
                path.display(),
                e.kind()
            ))
        })
    }

    /// Create (or reuse) the approval for a plan.
    pub async fn root_begin(
        &self,
        plan: &[Vec<String>],
        reason: &str,
        who: &Who,
    ) -> Result<RootPending, ApprovalError> {
        let request = root_request(plan, reason, who, self.config().ttl_secs)?;
        self.audit_ready()?;
        let begun = self.begin(&request).await?;
        if !begun.reused {
            self.audit(json!({
                "event": "requested", "approvalId": begun.id,
                "payloadHash": request.payload_hash, "reason": reason,
                "requester": {"kind": who.kind, "name": who.name, "detail": who.detail},
                "commands": plan.len(),
            }));
        }
        Ok(RootPending {
            request,
            begun,
            plan: plan.to_vec(),
            reason: reason.trim().to_owned(),
            who: who.clone(),
        })
    }

    /// Wait up to `wait` for the owner; on a verified grant run the plan.
    pub async fn root_finish(&self, p: &RootPending, wait: Duration) -> RootReport {
        let hash = p.request.payload_hash.clone();
        let id = p.begun.id.clone();
        let report = |outcome, steps| RootReport {
            outcome,
            approval_id: Some(id.clone()),
            payload_hash: hash.clone(),
            steps,
        };
        let line = |event: &str, extra: serde_json::Value| {
            let mut v = json!({
                "event": event, "approvalId": id, "payloadHash": hash,
                "reason": p.reason,
                "requester": {"kind": p.who.kind, "name": p.who.name, "detail": p.who.detail},
            });
            if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            v
        };
        let outcome = match self.finish(&p.request, &p.begun, wait).await {
            Ok(o) => o,
            Err(e) => {
                let why = e.to_string();
                self.audit(line("refused", json!({"why": why})));
                return report(RootOutcome::Refused(why), Vec::new());
            }
        };
        match outcome {
            Outcome::Pending { expires_at, .. } => {
                report(RootOutcome::Pending { expires_at }, Vec::new())
            }
            Outcome::Denied { decided_by, .. } => {
                self.audit(line("denied", json!({"decidedBy": decided_by})));
                report(RootOutcome::Denied { decided_by }, Vec::new())
            }
            Outcome::Expired { .. } => {
                self.audit(line("expired", json!({})));
                report(RootOutcome::Expired, Vec::new())
            }
            Outcome::Approved(claims) => {
                self.audit(line(
                    "approved",
                    json!({"decidedBy": claims.decided_by, "biometric": claims.biometric,
                           "grantExp": claims.exp, "commands": p.plan.len()}),
                ));
                let mut steps = Vec::new();
                let mut failed = None;
                for (i, argv) in p.plan.iter().enumerate() {
                    let s = self.run_step(argv).await;
                    let ok = s.ok;
                    steps.push(s);
                    if !ok {
                        failed = Some(i);
                        break;
                    }
                }
                let brief: Vec<_> = steps
                    .iter()
                    .map(|s| {
                        json!({"argv": s.argv, "ok": s.ok, "exitCode": s.exit_code,
                               "timedOut": s.timed_out, "error": s.error, "ms": s.ms})
                    })
                    .collect();
                let outcome = match failed {
                    Some(index) => RootOutcome::Failed { index },
                    None => RootOutcome::Done,
                };
                self.audit(line(
                    if failed.is_some() { "failed" } else { "ran" },
                    json!({"steps": brief, "planned": p.plan.len(),
                           "failedIndex": failed}),
                ));
                report(outcome, steps)
            }
        }
    }

    /// Ask for a plan and wait up to `wait` for the owner. Setup problems come
    /// back as [`RootOutcome::Refused`].
    pub async fn root_exec_wait(
        &self,
        plan: &[Vec<String>],
        reason: &str,
        who: &Who,
        wait: Duration,
    ) -> RootReport {
        match self.root_begin(plan, reason, who).await {
            Ok(p) => self.root_finish(&p, wait).await,
            Err(e) => RootReport {
                outcome: RootOutcome::Refused(e.to_string()),
                approval_id: None,
                payload_hash: plan_hash(plan),
                steps: Vec::new(),
            },
        }
    }

    /// [`root_exec_wait`](Self::root_exec_wait) with `LUCINA_ROOT_WAIT_SECS`.
    pub async fn root_exec(&self, plan: &[Vec<String>], reason: &str, who: &Who) -> RootReport {
        self.root_exec_wait(plan, reason, who, self.config().root_wait)
            .await
    }

    /// Create the approval now and wait for it in the background (the chat
    /// tool: a tool call may not block for minutes). `on_done` gets the final
    /// report. The same plan asked twice while one waiter is alive starts
    /// nothing new.
    pub async fn root_spawn<F, Fut>(
        self: &Arc<Self>,
        plan: Vec<Vec<String>>,
        reason: String,
        who: Who,
        on_done: F,
    ) -> Result<Spawned, ApprovalError>
    where
        F: FnOnce(RootReport) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let p = self.root_begin(&plan, &reason, &who).await?;
        let spawned = Spawned {
            id: p.begun.id.clone(),
            expires_at: p.begun.expires_at,
            already_waiting: false,
        };
        let hash = p.request.payload_hash.clone();
        if !self
            .inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(hash.clone())
        {
            return Ok(Spawned {
                already_waiting: true,
                ..spawned
            });
        }
        let me = self.clone();
        tokio::spawn(async move {
            let wait = me.config().root_wait;
            let report = me.root_finish(&p, wait).await;
            me.inflight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&hash);
            on_done(report).await;
        });
        Ok(spawned)
    }

    async fn run_step(&self, argv: &[String]) -> StepResult {
        let started = Instant::now();
        let cfg = self.config();
        let ms = |s: Instant| s.elapsed().as_millis() as u64;
        let mut cmd = tokio::process::Command::new(&cfg.root_sudo);
        cmd.args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return StepResult {
                    argv: argv.to_vec(),
                    ok: false,
                    exit_code: None,
                    timed_out: false,
                    error: Some(format!(
                        "could not start {}: {}",
                        cfg.root_sudo.display(),
                        e.kind()
                    )),
                    stdout: String::new(),
                    stderr: String::new(),
                    truncated: false,
                    ms: ms(started),
                }
            }
        };
        let (out, err) = (child.stdout.take(), child.stderr.take());
        let cap = cfg.output_cap;
        let work =
            async { tokio::join!(child.wait(), read_capped(out, cap), read_capped(err, cap)) };
        match tokio::time::timeout(cfg.root_cmd_timeout, work).await {
            Ok((status, (stdout, t1), (stderr, t2))) => {
                let (exit_code, error) = match &status {
                    Ok(s) => (
                        s.code(),
                        s.code()
                            .is_none()
                            .then(|| "terminated by a signal".to_owned()),
                    ),
                    Err(e) => (None, Some(format!("wait failed: {}", e.kind()))),
                };
                StepResult {
                    argv: argv.to_vec(),
                    ok: exit_code == Some(0),
                    exit_code,
                    timed_out: false,
                    error,
                    stdout,
                    stderr,
                    truncated: t1 || t2,
                    ms: ms(started),
                }
            }
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                StepResult {
                    argv: argv.to_vec(),
                    ok: false,
                    exit_code: None,
                    timed_out: true,
                    error: Some(format!(
                        "timed out after {} s",
                        cfg.root_cmd_timeout.as_secs()
                    )),
                    stdout: String::new(),
                    stderr: String::new(),
                    truncated: false,
                    ms: ms(started),
                }
            }
        }
    }
}

/// Seconds until `expires_at` (for messages).
pub fn secs_left(expires_at: i64) -> i64 {
    (expires_at - now_unix()).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(cmds: &[&[&str]]) -> Vec<Vec<String>> {
        cmds.iter()
            .map(|c| c.iter().map(|s| (*s).to_owned()).collect())
            .collect()
    }

    #[test]
    fn validates_plans() {
        assert!(validate_plan(&p(&[&["systemctl", "daemon-reload"]])).is_ok());
        assert!(validate_plan(&[]).is_err());
        assert!(validate_plan(&p(&[&[]])).is_err());
        assert!(validate_plan(&p(&[&[""]])).is_err());
        assert!(validate_plan(&p(&[&["echo", "a\0b"]])).is_err());
        let many = vec![vec!["true".to_owned()]; MAX_COMMANDS + 1];
        assert!(validate_plan(&many).is_err());
    }

    #[test]
    fn request_shows_exact_quoted_commands_and_binds_the_hash() {
        let plan = p(&[
            &["install", "-m", "644", "/a b/unit.service", "/etc/x"],
            &["systemctl", "restart", "oracle-poko-tunnel"],
        ]);
        let r = root_request(&plan, "fix the tunnel", &Who::lucina("test"), 300).unwrap();
        assert_eq!(r.action, "root.exec");
        assert!(r.sensitive && r.risk == "high");
        assert_eq!(
            r.details,
            "install -m 644 '/a b/unit.service' /etc/x\nsystemctl restart oracle-poko-tunnel"
        );
        assert_eq!(r.payload_hash, plan_hash(&plan));
        assert!(root_request(&plan, "  ", &Who::lucina("t"), 300).is_err());
    }
}
