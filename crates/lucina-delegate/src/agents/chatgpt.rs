//! ChatGPT: the OpenAI API (Responses API, web search for research) when a
//! key is configured; otherwise the Codex CLI on nezopt, which is signed in
//! with the owner's ChatGPT account (`codex exec`, read-only sandbox, live
//! web search for research, no MCP servers). It answers questions, research
//! and analysis; code goes to Claude.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use super::{api_key, has_key, responses, Agent, AgentJob, AgentReply};
use crate::config::ChatgptConfig;
use crate::sandbox::{self, Mounts};
use crate::types::{AgentId, Kind};

pub struct Chatgpt {
    cfg: ChatgptConfig,
    http: reqwest::Client,
    secrets: Option<Arc<lucina_secrets::store::SecretStore>>,
}

const ANSWER_FILE: &str = ".lucina-answer.md";

impl Chatgpt {
    pub fn new(
        cfg: ChatgptConfig,
        http: reqwest::Client,
        secrets: Option<Arc<lucina_secrets::store::SecretStore>>,
    ) -> Self {
        Self { cfg, http, secrets }
    }

    fn openai_configured(&self) -> bool {
        has_key(
            self.secrets.as_deref(),
            &self.cfg.openai_key_setting,
            self.cfg.openai_key_file.as_ref(),
        )
    }

    fn codex_ready(&self) -> Result<(), String> {
        if !self.cfg.codex_bin.exists() {
            return Err(format!("{} not found", self.cfg.codex_bin.display()));
        }
        if !self.cfg.codex_auth.is_file() {
            return Err("Codex is not signed in on nezopt".into());
        }
        Ok(())
    }

    fn pick(&self) -> Result<&'static str, String> {
        match self.cfg.backend.as_str() {
            "openai" if self.openai_configured() => Ok("openai-api"),
            "openai" => Err("no OpenAI API key configured".into()),
            "codex" => self.codex_ready().map(|_| "codex-cli"),
            _ if self.openai_configured() => Ok("openai-api"),
            _ => self.codex_ready().map(|_| "codex-cli"),
        }
    }

    /// `codex [--search] exec …` arguments (prompt on stdin).
    pub fn codex_args(&self, job: &AgentJob) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        if matches!(job.kind, Kind::Research | Kind::Question) {
            a.push("--search".into());
        }
        a.extend([
            "exec".to_owned(),
            "--skip-git-repo-check".into(),
            "--ephemeral".into(),
            "-m".into(),
            self.cfg.codex_model.clone(),
            "-c".into(),
            format!("model_reasoning_effort={}", self.cfg.codex_effort),
            "-c".into(),
            "mcp_servers={}".into(),
            "-s".into(),
            "read-only".into(),
            "-C".into(),
            job.workdir.to_string_lossy().into_owned(),
            "-o".into(),
            job.writable
                .join(ANSWER_FILE)
                .to_string_lossy()
                .into_owned(),
            "-".into(),
        ]);
        a
    }

    async fn run_codex(&self, job: &AgentJob) -> Result<AgentReply, String> {
        let h = home();
        let mounts = Mounts {
            rw: vec![h.join(".codex")],
            ro: vec![h.join(".codex/config.toml"), h.join(".codex/AGENTS.md")],
        };
        let out = sandbox::run(
            job.isolation,
            &self.cfg.codex_bin,
            &self.codex_args(job),
            &job.workdir,
            &job.writable,
            &mounts,
            &[],
            &job.prompt,
            job.timeout,
        )
        .await?;
        if out.timed_out {
            return Err(format!("codex timed out after {}s", job.timeout.as_secs()));
        }
        let answer = std::fs::read_to_string(job.writable.join(ANSWER_FILE))
            .unwrap_or_default()
            .trim()
            .to_owned();
        if answer.is_empty() {
            let tail = codex_error(&out.stderr, &out.stdout);
            return Err(format!(
                "codex gave no answer (exit {:?}){}",
                out.status,
                if tail.is_empty() {
                    String::new()
                } else {
                    format!(": {}", tail.chars().take(300).collect::<String>())
                }
            ));
        }
        Ok(AgentReply {
            text: answer,
            session: None,
            turns: 1,
            backend: "codex-cli".into(),
        })
    }

    async fn run_openai(&self, job: &AgentJob) -> Result<AgentReply, String> {
        let key = api_key(
            self.secrets.as_deref(),
            &self.cfg.openai_key_setting,
            self.cfg.openai_key_file.as_ref(),
            "delegate.chatgpt",
        )
        .await
        .ok_or_else(|| "no OpenAI API key".to_owned())?;
        let tools = if job.kind == Kind::Research {
            vec![json!({"type": "web_search"})]
        } else {
            Vec::new()
        };
        let text = responses::call(
            &self.http,
            &self.cfg.openai_url,
            &key,
            &self.cfg.openai_model,
            &job.prompt,
            tools,
            job.timeout,
        )
        .await
        .map_err(|e| format!("OpenAI: {e}"))?;
        Ok(AgentReply {
            text,
            session: None,
            turns: 1,
            backend: "openai-api".into(),
        })
    }
}

/// Why codex failed: its own `ERROR: …` lines ("Your workspace is out of credits"), else any line
/// that mentions an error. Its INFO trace lines (`auth_retry_after_error=…`) are not the reason.
fn codex_error(stderr: &str, stdout: &str) -> String {
    let lines = || stderr.lines().chain(stdout.lines()).map(str::trim);
    let mut own: Vec<&str> = lines()
        .filter_map(|l| l.strip_prefix("ERROR:").map(str::trim))
        .collect();
    own.dedup();
    if !own.is_empty() {
        return own.into_iter().take(2).collect::<Vec<_>>().join(" | ");
    }
    lines()
        .filter(|l| (l.contains("ERROR") || l.contains("error")) && !l.contains(" INFO "))
        .take(2)
        .collect::<Vec<_>>()
        .join(" | ")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/nez"))
}

#[async_trait::async_trait]
impl Agent for Chatgpt {
    fn id(&self) -> AgentId {
        AgentId::Chatgpt
    }

    fn supports(&self, kind: Kind) -> bool {
        kind != Kind::Code
    }

    async fn backend(&self) -> Result<String, String> {
        self.pick().map(str::to_owned)
    }

    async fn run(&self, job: &AgentJob) -> Result<AgentReply, String> {
        match self.pick()? {
            "openai-api" => self.run_openai(job).await,
            _ => self.run_codex(job).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Isolation;
    use std::time::Duration;

    #[test]
    fn codex_args_are_read_only_without_mcp() {
        let c = Chatgpt::new(ChatgptConfig::default(), reqwest::Client::new(), None);
        let job = AgentJob {
            job_id: "j".into(),
            kind: Kind::Research,
            prompt: "p".into(),
            workdir: "/s/j".into(),
            writable: "/s/j".into(),
            timeout: Duration::from_secs(1),
            max_turns: 1,
            resume_session: None,
            isolation: Isolation::Bwrap,
        };
        let a = c.codex_args(&job).join(" ");
        assert!(a.starts_with("--search exec --skip-git-repo-check --ephemeral -m gpt-6.1-sol -c model_reasoning_effort=medium -c mcp_servers={} -s read-only -C /s/j -o /s/j/.lucina-answer.md -"), "{a}");
        let job = AgentJob {
            kind: Kind::Analysis,
            ..job
        };
        assert!(c.codex_args(&job)[0] == "exec");
        assert!(!c.supports(Kind::Code));
    }

    #[test]
    fn codex_error_is_its_own_error_line_not_the_trace() {
        // 2026-11-01: the reply to ChatGPT carried an INFO trace line instead of the reason.
        let stderr = "2026-11-01T12:12:41.639433Z  INFO list_models{refresh_strategy=online}: feedback_tags: auth_mode=\"Chatgpt\" auth_retry_after_error=false\nERROR: Your workspace is out of credits. Add credits to continue.\nERROR: Your workspace is out of credits. Add credits to continue.\n";
        assert_eq!(
            codex_error(stderr, ""),
            "Your workspace is out of credits. Add credits to continue."
        );
        assert_eq!(
            codex_error("2026 INFO x auth_retry_after_error=false\n", "thread error: boom"),
            "thread error: boom"
        );
        assert_eq!(codex_error("2026 INFO x error=none\n", ""), "");
    }
}
