//! `delegate.toml` (`LUCINA_DELEGATE_CONFIG`): every key optional, the
//! defaults below are the nezopt values. No secrets in this file: API keys
//! are named by a secrets-skill setting or a key file path.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::types::{AgentId, Kind};

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/nez"))
}

/// How delegated processes are isolated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    /// bubblewrap: read-only system, the sandbox dir writable, other repos,
    /// SSH keys, `~/.config`, systemd and docker hidden (default).
    Bwrap,
    /// No isolation (tests). Code tasks are refused in this mode.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentBudget {
    pub calls_per_day: u32,
    pub wall_minutes_per_day: u32,
    /// Claude only: model turns per day.
    pub turns_per_day: u32,
}

impl AgentBudget {
    fn new(calls: u32, minutes: u32, turns: u32) -> Self {
        Self {
            calls_per_day: calls,
            wall_minutes_per_day: minutes,
            turns_per_day: turns,
        }
    }
}

impl Default for AgentBudget {
    fn default() -> Self {
        Self::new(80, 240, 0)
    }
}

/// The runaway guard: generous caps, a pause (and one message) when hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budget {
    pub total_calls_per_day: u32,
    pub claude: AgentBudget,
    pub grok: AgentBudget,
    pub chatgpt: AgentBudget,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            total_calls_per_day: 200,
            claude: AgentBudget::new(80, 480, 1500),
            grok: AgentBudget::new(80, 240, 0),
            chatgpt: AgentBudget::new(80, 240, 0),
        }
    }
}

impl Budget {
    pub fn of(&self, a: AgentId) -> &AgentBudget {
        match a {
            AgentId::Claude => &self.claude,
            AgentId::Grok => &self.grok,
            AgentId::Chatgpt => &self.chatgpt,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeConfig {
    pub bin: PathBuf,
    /// The owner's rule: Opus 5.5 at medium effort, never silently changed.
    pub model: String,
    pub effort: String,
    pub max_turns_per_job: u32,
    pub timeout_secs: u64,
    pub code_timeout_secs: u64,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            bin: home().join(".local/bin/claude"),
            model: "claude-opus-5-5".into(),
            effort: "medium".into(),
            max_turns_per_job: 40,
            timeout_secs: 900,
            code_timeout_secs: 2700,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GrokConfig {
    /// `auto` (gbot when its session is signed in, else xAI), `gbot`, `xai`.
    pub backend: String,
    pub gbot_bin: PathBuf,
    /// The Grok Bot bot that takes Lucina's questions (`gbot bots list`).
    pub bot: String,
    pub xai_url: String,
    pub xai_model: String,
    /// Secrets-skill setting holding the xAI key (`[refs]` in secrets.toml).
    pub xai_key_setting: String,
    /// Or a key file (one line, 0600).
    pub xai_key_file: Option<PathBuf>,
    pub timeout_secs: u64,
    pub poll_secs: u64,
    /// The answer is complete after this long without new bot entries.
    pub quiet_secs: u64,
}

impl Default for GrokConfig {
    fn default() -> Self {
        Self {
            backend: "auto".into(),
            gbot_bin: home().join(".local/bin/gbot"),
            bot: "Grok Bot".into(),
            xai_url: "https://api.x.ai".into(),
            xai_model: "grok-4".into(),
            xai_key_setting: "delegate.xai_api_key".into(),
            xai_key_file: None,
            timeout_secs: 900,
            poll_secs: 5,
            quiet_secs: 25,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChatgptConfig {
    /// `auto` (OpenAI API when a key is configured, else Codex), `openai`, `codex`.
    pub backend: String,
    pub codex_bin: PathBuf,
    /// Codex's sign-in (the owner's ChatGPT account).
    pub codex_auth: PathBuf,
    pub codex_model: String,
    pub codex_effort: String,
    pub openai_url: String,
    pub openai_model: String,
    pub openai_key_setting: String,
    pub openai_key_file: Option<PathBuf>,
    pub timeout_secs: u64,
}

impl Default for ChatgptConfig {
    fn default() -> Self {
        Self {
            backend: "auto".into(),
            codex_bin: home().join(".local/bin/codex"),
            codex_auth: home().join(".codex/auth.json"),
            codex_model: "gpt-6.1-sol".into(),
            codex_effort: "medium".into(),
            openai_url: "https://api.openai.com".into(),
            openai_model: "gpt-5.5".into(),
            openai_key_setting: "delegate.openai_api_key".into(),
            openai_key_file: None,
            timeout_secs: 900,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DelegateConfig {
    pub enabled: bool,
    /// Hard limit; values above 1 are clamped to 1.
    pub max_depth: u8,
    /// How long a chat turn waits for a quick answer before it says "I'll tell you".
    pub inline_wait_secs: u64,
    /// Answers longer than this are saved as a note; the chat gets a summary.
    pub note_threshold_chars: usize,
    pub sandbox_root: PathBuf,
    /// Code tasks may name a repo only under these roots (it is cloned into the sandbox).
    pub allowed_repo_roots: Vec<PathBuf>,
    pub isolation: Isolation,
    /// Self-review: one JSON-mode model call before each delegation.
    pub judge: bool,
    pub judge_timeout_secs: u64,
    /// State (budget use, toggles) and jobs; default under the data dir.
    pub state_path: Option<PathBuf>,
    pub jobs_path: Option<PathBuf>,
    pub audit_path: Option<PathBuf>,
    pub budget: Budget,
    pub claude: ClaudeConfig,
    pub grok: GrokConfig,
    pub chatgpt: ChatgptConfig,
}

impl Default for DelegateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_depth: 1,
            inline_wait_secs: 20,
            note_threshold_chars: 1200,
            sandbox_root: home().join("scratch/lucina-delegate"),
            allowed_repo_roots: vec![home().join("Projects")],
            isolation: Isolation::Bwrap,
            judge: true,
            judge_timeout_secs: 25,
            state_path: None,
            jobs_path: None,
            audit_path: None,
            budget: Budget::default(),
            claude: ClaudeConfig::default(),
            grok: GrokConfig::default(),
            chatgpt: ChatgptConfig::default(),
        }
    }
}

impl DelegateConfig {
    /// The file, or the defaults when `path` is `None` or missing.
    pub fn load(path: Option<&Path>, data_dir: &Path) -> Result<Self, String> {
        let mut cfg = match path {
            Some(p) if p.exists() => {
                let text =
                    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
                toml::from_str::<DelegateConfig>(&text)
                    .map_err(|e| format!("{}: {e}", p.display()))?
            }
            _ => DelegateConfig::default(),
        };
        cfg.max_depth = cfg.max_depth.min(1);
        let dir = data_dir.join("lucina-rs");
        cfg.state_path
            .get_or_insert_with(|| dir.join("delegate-state.json"));
        cfg.jobs_path
            .get_or_insert_with(|| dir.join("delegate-jobs.json"));
        cfg.audit_path
            .get_or_insert_with(|| dir.join("delegate-audit.jsonl"));
        Ok(cfg)
    }

    pub fn timeout(&self, agent: AgentId, kind: Kind) -> Duration {
        Duration::from_secs(match agent {
            AgentId::Claude if kind == Kind::Code => self.claude.code_timeout_secs,
            AgentId::Claude => self.claude.timeout_secs,
            AgentId::Grok => self.grok.timeout_secs,
            AgentId::Chatgpt => self.chatgpt.timeout_secs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let c = DelegateConfig::load(None, dir.path()).unwrap();
        assert_eq!(c.claude.model, "claude-opus-5-5");
        assert_eq!(c.claude.effort, "medium");
        assert_eq!(c.budget.total_calls_per_day, 200);
        assert_eq!(
            c.state_path.unwrap(),
            dir.path().join("lucina-rs/delegate-state.json")
        );
        let p = dir.path().join("d.toml");
        std::fs::write(
            &p,
            "max_depth = 5\n[budget]\ntotal_calls_per_day = 7\n[grok]\nbot = \"Researcher\"\n",
        )
        .unwrap();
        let c = DelegateConfig::load(Some(&p), dir.path()).unwrap();
        assert_eq!(c.max_depth, 1, "depth is a hard limit");
        assert_eq!(c.budget.total_calls_per_day, 7);
        assert_eq!(c.budget.claude.turns_per_day, 1500);
        assert_eq!(c.grok.bot, "Researcher");
        std::fs::write(&p, "nope = 1\n").unwrap();
        assert!(DelegateConfig::load(Some(&p), dir.path()).is_err());
    }

    #[test]
    fn the_deploy_example_parses_to_the_defaults() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/lucina-delegate/delegate.example.toml");
        let dir = tempfile::tempdir().unwrap();
        let c = DelegateConfig::load(Some(&p), dir.path()).unwrap();
        let d = DelegateConfig::default();
        assert_eq!(c.budget.total_calls_per_day, d.budget.total_calls_per_day);
        assert_eq!(c.budget.claude.turns_per_day, d.budget.claude.turns_per_day);
        assert_eq!(c.claude.model, d.claude.model);
        assert_eq!(c.chatgpt.codex_model, d.chatgpt.codex_model);
        assert_eq!(c.grok.bot, d.grok.bot);
        assert_eq!(c.isolation, Isolation::Bwrap);
    }
}
