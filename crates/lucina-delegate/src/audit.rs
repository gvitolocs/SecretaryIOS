//! The delegation audit log (`delegate-audit.jsonl`): agent, backend, task
//! kind, origin, decision, risk, duration, outcome and the budget used.
//! Never prompt contents, answers or secrets.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::state::Usage;
use crate::types::{now_iso, JobRecord};

const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug)]
pub struct AuditLog {
    path: Option<PathBuf>,
    lock: Mutex<()>,
}

impl AuditLog {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn entry(job: &JobRecord, outcome: &str, usage: Usage, total_calls: u32) -> Value {
        json!({
            "ts": now_iso(),
            "job": job.id,
            "agent": job.agent.as_str(),
            "backend": job.backend,
            "kind": job.kind.as_str(),
            "origin": job.origin.as_str(),
            "outcome": outcome,
            "decision": job.judgement.as_ref().map(|j| j.decision.as_str()),
            "risk": job.judgement.as_ref().map(|j| j.risk.as_str()),
            "reversible": job.judgement.as_ref().map(|j| j.reversible),
            "judge": job.judgement.as_ref().map(|j| j.source.clone()),
            "durationMs": job.duration_ms,
            "turns": job.turns,
            "note": job.note_slug.is_some(),
            "branch": job.branch,
            "budget": {"calls": usage.calls, "wallMs": usage.wall_ms, "turns": usage.turns, "totalCalls": total_calls},
        })
    }

    pub fn write(&self, mut line: Value) {
        lucina_secrets::redactor().redact_json(&mut line);
        tracing::info!(target: "lucina_delegate::audit", "{line}");
        let Some(p) = &self.path else { return };
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if std::fs::metadata(p)
            .map(|m| m.len() > MAX_BYTES)
            .unwrap_or(false)
        {
            // Keep the newer half.
            if let Ok(text) = std::fs::read_to_string(p) {
                let lines: Vec<&str> = text.lines().collect();
                let keep = lines[lines.len() / 2..].join("\n") + "\n";
                let _ = lucina_store::write_atomic(p, keep.as_bytes());
            }
        }
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p);
        match file {
            Ok(mut f) => {
                let _ = writeln!(f, "{line}");
            }
            Err(e) => tracing::warn!(error = %e, "delegate audit write failed"),
        }
    }
}
