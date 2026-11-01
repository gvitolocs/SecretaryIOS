//! Claude: the Claude Code CLI on nezopt, the owner's way (`claude -p
//! --model claude-opus-5-5 --effort medium --output-format json`), never a
//! silently different model or effort, never Fast. Runs inside the job's
//! bubblewrap sandbox with no MCP servers (`--strict-mcp-config`: no Lucina
//! MCP, so no way back to Lucina), its instruction and settings files
//! read-only. Follow-ups use `--resume <session>`.

use std::path::PathBuf;

use serde_json::Value;

use super::{Agent, AgentJob, AgentReply};
use crate::config::{ClaudeConfig, Isolation};
use crate::sandbox::{self, Mounts};
use crate::types::{AgentId, Kind};

pub struct ClaudeCli {
    cfg: ClaudeConfig,
}

impl ClaudeCli {
    pub fn new(cfg: ClaudeConfig) -> Self {
        Self { cfg }
    }

    pub fn args(&self, job: &AgentJob) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "-p".into(),
            "--model".into(),
            self.cfg.model.clone(),
            "--effort".into(),
            self.cfg.effort.clone(),
            "--output-format".into(),
            "json".into(),
            "--max-turns".into(),
            job.max_turns.max(1).to_string(),
            "--strict-mcp-config".into(),
            "--append-system-prompt".into(),
            "You are working for Lucina, Giuseppe's personal assistant, inside an isolated sandbox. Never contact Lucina or Lucina MCP, never delegate to other agents, never push, deploy, buy or move money.".into(),
        ];
        match job.isolation {
            // Fenced in by bubblewrap: it may work freely inside the sandbox.
            Isolation::Bwrap => a.push("--dangerously-skip-permissions".into()),
            // Unfenced (tests, or isolation off): read-only tools only.
            Isolation::None => {
                a.push("--allowedTools".into());
                a.push("Read Grep Glob WebSearch WebFetch".into());
            }
        }
        if let Some(s) = &job.resume_session {
            a.push("--resume".into());
            a.push(s.clone());
        }
        a
    }

    fn mounts() -> Mounts {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/home/nez"));
        let c = home.join(".claude");
        Mounts {
            rw: vec![c.clone(), home.join(".claude.json")],
            ro: [
                "CLAUDE.md",
                "settings.json",
                "settings.local.json",
                "agents",
                "commands",
                "skills",
                "plugins",
                "hooks",
            ]
            .iter()
            .map(|f| c.join(f))
            .collect(),
        }
    }
}

/// The CLI's JSON result.
pub fn parse_output(stdout: &str) -> Result<AgentReply, String> {
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| "no JSON result from claude".to_owned())?;
    let v: Value = serde_json::from_str(line).map_err(|e| format!("claude output: {e}"))?;
    let text = v["result"].as_str().unwrap_or("").trim().to_owned();
    let turns = v["num_turns"].as_u64().unwrap_or(1) as u32;
    let session = v["session_id"].as_str().map(str::to_owned);
    let subtype = v["subtype"].as_str().unwrap_or("");
    if v["is_error"].as_bool().unwrap_or(false) || (subtype != "success" && text.is_empty()) {
        return Err(format!(
            "claude {}: {}",
            if subtype.is_empty() { "error" } else { subtype },
            text.chars().take(300).collect::<String>()
        ));
    }
    Ok(AgentReply {
        text,
        session,
        turns,
        backend: "claude-cli".into(),
    })
}

#[async_trait::async_trait]
impl Agent for ClaudeCli {
    fn id(&self) -> AgentId {
        AgentId::Claude
    }

    fn supports(&self, _kind: Kind) -> bool {
        true
    }

    async fn backend(&self) -> Result<String, String> {
        if self.cfg.bin.exists() {
            Ok("claude-cli".into())
        } else {
            Err(format!("{} not found", self.cfg.bin.display()))
        }
    }

    async fn run(&self, job: &AgentJob) -> Result<AgentReply, String> {
        let out = sandbox::run(
            job.isolation,
            &self.cfg.bin,
            &self.args(job),
            &job.workdir,
            &job.writable,
            &Self::mounts(),
            &[],
            &job.prompt,
            job.timeout,
        )
        .await?;
        if out.timed_out {
            return Err(format!("claude timed out after {}s", job.timeout.as_secs()));
        }
        parse_output(&out.stdout).map_err(|e| {
            let tail: String = out
                .stderr
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .join(" | ");
            if tail.is_empty() {
                e
            } else {
                format!("{e} ({})", tail.chars().take(300).collect::<String>())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn job(iso: Isolation) -> AgentJob {
        AgentJob {
            job_id: "j".into(),
            kind: Kind::Analysis,
            prompt: "p".into(),
            workdir: "/tmp".into(),
            writable: "/tmp".into(),
            timeout: Duration::from_secs(1),
            max_turns: 12,
            resume_session: Some("s-1".into()),
            isolation: iso,
        }
    }

    #[test]
    fn the_owners_model_and_effort() {
        let c = ClaudeCli::new(ClaudeConfig::default());
        let a = c.args(&job(Isolation::Bwrap)).join(" ");
        assert!(a.contains("-p --model claude-opus-5-5 --effort medium --output-format json --max-turns 12 --strict-mcp-config"), "{a}");
        assert!(a.contains("--dangerously-skip-permissions"));
        assert!(a.ends_with("--resume s-1"));
        assert!(!a.to_lowercase().contains("fast"));
        let a = c.args(&job(Isolation::None)).join(" ");
        assert!(!a.contains("dangerously"));
        assert!(a.contains("--allowedTools Read Grep Glob WebSearch WebFetch"));
    }

    #[test]
    fn parses_results() {
        let r = parse_output(r#"{"type":"result","subtype":"success","is_error":false,"result":" 42 ","session_id":"abc","num_turns":3}"#).unwrap();
        assert_eq!(
            (r.text.as_str(), r.session.as_deref(), r.turns),
            ("42", Some("abc"), 3)
        );
        assert!(parse_output(
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":""}"#
        )
        .unwrap_err()
        .contains("error_max_turns"));
        assert!(parse_output("garbage").is_err());
    }
}
