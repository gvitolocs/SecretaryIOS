//! Grok: the owner's Grok Bot (desktop app session on nezopt, through the
//! `gbot` CLI: send to a bot, then read its thread until the reply settles),
//! or the xAI API (Responses API with web and X search) when a key is
//! configured. `backend = "auto"` prefers Grok Bot when its session is
//! signed in.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::{api_key, has_key, responses, Agent, AgentJob, AgentReply};
use crate::config::GrokConfig;
use crate::types::{AgentId, Kind};

pub struct Grok {
    cfg: GrokConfig,
    http: reqwest::Client,
    secrets: Option<Arc<lucina_secrets::store::SecretStore>>,
}

fn grok_bot_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/home/nez/.config"))
        .join("Grok Bot")
}

/// The Grok Bot app writes its gateway descriptor while signed in.
pub fn gbot_session_ready() -> Result<(), String> {
    let dir = grok_bot_dir();
    if !dir.join("gateway-descriptor.json").is_file() {
        let signed_in = std::fs::read(dir.join("desktop-status.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v["signedIn"].as_bool());
        return Err(match signed_in {
            Some(false) => "the Grok Bot app on nezopt is signed out".into(),
            _ => "no Grok Bot session on nezopt (open the Grok Bot app and sign in)".into(),
        });
    }
    Ok(())
}

/// A thread entry written by the bot (not by us).
pub fn is_bot_entry(e: &Value) -> bool {
    let kind = e["kind"].as_str().unwrap_or("");
    let role = e["role"]
        .as_str()
        .or_else(|| e["sender"].as_str())
        .or_else(|| e["type"].as_str())
        .unwrap_or("")
        .to_lowercase();
    kind == "send-message"
        || ["assistant", "bot", "agent", "grok", "model"].contains(&role.as_str())
}

/// The text of an entry (same order as gbot's `entryText`).
pub fn entry_text(e: &Value) -> String {
    for k in ["text", "prompt"] {
        if let Some(s) = e[k].as_str().filter(|s| !s.is_empty()) {
            return s.to_owned();
        }
    }
    if let Some(s) = e["message"].as_str().filter(|s| !s.is_empty()) {
        return s.to_owned();
    }
    match &e["content"] {
        Value::String(s) => return s.clone(),
        Value::Array(parts) => {
            return parts
                .iter()
                .filter_map(|p| {
                    p.as_str()
                        .map(str::to_owned)
                        .or_else(|| p["text"].as_str().map(str::to_owned))
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        Value::Object(o) => {
            if let Some(t) = o.get("text").and_then(Value::as_str) {
                return t.to_owned();
            }
        }
        _ => {}
    }
    if let Some(s) = e["message"]["content"].as_str() {
        return s.to_owned();
    }
    e["preview"].as_str().unwrap_or("").to_owned()
}

/// `{cursor, entries}` from `gbot thread --json` (top level or under `result`).
pub fn thread_page(v: &Value) -> (Option<String>, Vec<Value>) {
    // gbot 0.12 puts them under `transcript` (with `target` beside it).
    let root = if v.get("entries").is_some() {
        v
    } else if v["transcript"].get("entries").is_some() {
        &v["transcript"]
    } else if v["result"]["transcript"].get("entries").is_some() {
        &v["result"]["transcript"]
    } else {
        &v["result"]
    };
    (
        root["cursor"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(str::to_owned),
        root["entries"].as_array().cloned().unwrap_or_default(),
    )
}

impl Grok {
    pub fn new(
        cfg: GrokConfig,
        http: reqwest::Client,
        secrets: Option<Arc<lucina_secrets::store::SecretStore>>,
    ) -> Self {
        Self { cfg, http, secrets }
    }

    fn xai_configured(&self) -> bool {
        has_key(
            self.secrets.as_deref(),
            &self.cfg.xai_key_setting,
            self.cfg.xai_key_file.as_ref(),
        )
    }

    fn pick(&self) -> Result<&'static str, String> {
        let gbot = || {
            if !self.cfg.gbot_bin.exists() {
                return Err(format!("{} not found", self.cfg.gbot_bin.display()));
            }
            gbot_session_ready()
        };
        match self.cfg.backend.as_str() {
            "gbot" => gbot().map(|_| "gbot"),
            "xai" => {
                if self.xai_configured() {
                    Ok("xai-api")
                } else {
                    Err("no xAI API key configured".into())
                }
            }
            _ => match gbot() {
                Ok(()) => Ok("gbot"),
                Err(why) if !self.xai_configured() => {
                    Err(format!("{why}; no xAI API key configured either"))
                }
                Err(_) => Ok("xai-api"),
            },
        }
    }

    async fn gbot(&self, args: &[&str], timeout: Duration) -> Result<Value, String> {
        let out = tokio::time::timeout(
            timeout,
            tokio::process::Command::new(&self.cfg.gbot_bin)
                .args(args)
                .arg("--json")
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| "gbot timed out".to_owned())?
        .map_err(|e| format!("gbot: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let v: Value = serde_json::from_str(stdout.trim()).map_err(|_| {
            format!(
                "gbot {}: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr)
                    .trim()
                    .chars()
                    .take(200)
                    .collect::<String>()
            )
        })?;
        if let Some(msg) = v["error"]["message"].as_str() {
            return Err(format!(
                "gbot: {}",
                msg.chars().take(200).collect::<String>()
            ));
        }
        Ok(v)
    }

    async fn run_gbot(&self, job: &AgentJob) -> Result<AgentReply, String> {
        let bot = self.cfg.bot.as_str();
        let short = Duration::from_secs(60);
        let (mut cursor, before) = thread_page(
            &self
                .gbot(&["thread", bot, "--limit", "5", "--gateway"], short)
                .await?,
        );
        let mut seen: std::collections::HashSet<String> = before
            .iter()
            .filter_map(|e| e["id"].as_str().map(str::to_owned))
            .collect();
        self.gbot(
            &[
                "send",
                bot,
                &job.prompt,
                "--gateway",
                "--reply-mode",
                "manual",
            ],
            short,
        )
        .await?;
        let deadline = tokio::time::Instant::now() + job.timeout;
        let quiet = Duration::from_secs(self.cfg.quiet_secs.max(5));
        let poll = Duration::from_secs(self.cfg.poll_secs.max(1));
        let mut answer: Vec<String> = Vec::new();
        let mut last_new = tokio::time::Instant::now();
        loop {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(poll).await;
            let mut args = vec!["thread", bot, "--limit", "50", "--full", "--gateway"];
            if let Some(c) = cursor.as_deref() {
                args.extend(["--after", c]);
            }
            let page = match self.gbot(&args, short).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "gbot thread poll failed");
                    continue;
                }
            };
            let (next, entries) = thread_page(&page);
            if next.is_some() {
                cursor = next;
            }
            for e in entries {
                let id = e["id"].as_str().unwrap_or("").to_owned();
                if !id.is_empty() && !seen.insert(id) {
                    continue;
                }
                if is_bot_entry(&e) {
                    let t = entry_text(&e).trim().to_owned();
                    if !t.is_empty() && t != job.prompt.trim() {
                        answer.push(t);
                        last_new = tokio::time::Instant::now();
                    }
                }
            }
            if !answer.is_empty() && last_new.elapsed() >= quiet {
                break;
            }
        }
        if answer.is_empty() {
            return Err(format!(
                "no reply from Grok Bot \"{bot}\" within {}s",
                job.timeout.as_secs()
            ));
        }
        Ok(AgentReply {
            text: answer.join("\n\n"),
            session: None,
            turns: 1,
            backend: "gbot".into(),
        })
    }

    async fn run_xai(&self, job: &AgentJob) -> Result<AgentReply, String> {
        let key = api_key(
            self.secrets.as_deref(),
            &self.cfg.xai_key_setting,
            self.cfg.xai_key_file.as_ref(),
            "delegate.grok",
        )
        .await
        .ok_or_else(|| "no xAI API key".to_owned())?;
        let tools = if matches!(job.kind, Kind::Research | Kind::Question) {
            vec![json!({"type": "web_search"}), json!({"type": "x_search"})]
        } else {
            Vec::new()
        };
        let text = responses::call(
            &self.http,
            &self.cfg.xai_url,
            &key,
            &self.cfg.xai_model,
            &job.prompt,
            tools,
            job.timeout,
        )
        .await
        .map_err(|e| format!("xAI: {e}"))?;
        Ok(AgentReply {
            text,
            session: None,
            turns: 1,
            backend: "xai-api".into(),
        })
    }
}

#[async_trait::async_trait]
impl Agent for Grok {
    fn id(&self) -> AgentId {
        AgentId::Grok
    }

    fn supports(&self, kind: Kind) -> bool {
        kind != Kind::Code
    }

    async fn backend(&self) -> Result<String, String> {
        self.pick().map(str::to_owned)
    }

    async fn run(&self, job: &AgentJob) -> Result<AgentReply, String> {
        match self.pick()? {
            "gbot" => self.run_gbot(job).await,
            _ => self.run_xai(job).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_gbot_threads() {
        let page = json!({"cursor": "e3", "entries": [
            {"id": "e1", "role": "user", "text": "the question"},
            {"id": "e2", "kind": "send-message", "message": {"type": "text", "content": "Rust 1.95 shipped."}},
            {"id": "e3", "role": "assistant", "content": [{"type": "text", "text": "Sources: …"}]}
        ], "entryCount": 3, "gapReset": false});
        let (cursor, entries) = thread_page(&page);
        assert_eq!(cursor.as_deref(), Some("e3"));
        let bot: Vec<String> = entries
            .iter()
            .filter(|e| is_bot_entry(e))
            .map(entry_text)
            .collect();
        assert_eq!(bot, vec!["Rust 1.95 shipped.", "Sources: …"]);
        let (c, e) = thread_page(&json!({"result": {"cursor": "x", "entries": []}}));
        assert_eq!((c.as_deref(), e.len()), (Some("x"), 0));
        // gbot 0.12: `{target, transcript: {entries, nextBeforeSeq}}`.
        let v = json!({"target": {"id": "b1"}, "transcript": {"entries": [
            {"kind": "message", "id": "t1u", "role": "user", "seq": 12},
            {"kind": "send-message", "id": "t1s13", "seq": 13, "message": {"type": "text", "content": "pong"}}
        ], "nextBeforeSeq": 10}});
        let (c, e) = thread_page(&v);
        assert!(c.is_none());
        let bot: Vec<String> = e.iter().filter(|e| is_bot_entry(e)).map(entry_text).collect();
        assert_eq!(bot, vec!["pong"]);
    }

    #[tokio::test]
    async fn backend_choice() {
        let g = Grok::new(
            GrokConfig {
                backend: "xai".into(),
                ..GrokConfig::default()
            },
            reqwest::Client::new(),
            None,
        );
        assert!(g.backend().await.unwrap_err().contains("xAI"));
        let t = tempfile::tempdir().unwrap();
        let key = t.path().join("xai.key");
        std::fs::write(&key, "xai-test-key-0123456789\n").unwrap();
        let g = Grok::new(
            GrokConfig {
                backend: "auto".into(),
                gbot_bin: t.path().join("missing-gbot"),
                xai_key_file: Some(key),
                ..GrokConfig::default()
            },
            reqwest::Client::new(),
            None,
        );
        assert_eq!(g.backend().await.unwrap(), "xai-api");
        assert!(!g.supports(Kind::Code));
    }
}
