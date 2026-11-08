//! The owner's lane config (`~/.config/lane/chains.toml`, or `$LANE_CONFIG`), read-only.
//! It stays the single source of truth for models, efforts, work categories and
//! quota detectors. agentd never changes a lane's model or effort and never turns
//! on Fast (owner rule).
//!
//! A chain falls through to its next lane only when the failure matches a detector
//! with outcome `quota` or `unavailable`; an unmatched failure stops it (lane's rules).

use std::collections::BTreeMap;
use std::path::PathBuf;

use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Lane {
    pub kind: String,
    #[serde(default)]
    pub bin: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub worker: Option<String>,
}

/// Owner rule (2026-10-27): the paid DeepSeek key is never used, through any lane.
pub fn is_forbidden(name: &str, lane: &Lane) -> bool {
    name == "deepseek-paid" || lane.worker.as_deref() == Some("deepseek-flash") || lane.model.as_deref().is_some_and(|m| m.contains("paid"))
}

#[derive(Debug, Clone, Deserialize)]
pub struct Detector {
    pub name: String,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub lanes: Vec<String>,
    pub outcome: String,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub empty_stdout: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LaneConfig {
    #[serde(default)]
    pub lanes: BTreeMap<String, Lane>,
    #[serde(default)]
    pub categories: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub detectors: Vec<Detector>,
}

/// How a failed attempt is classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Out of quota or credits: fall through.
    Quota(String),
    /// Login missing, server down: fall through.
    Unavailable(String),
    /// Anything else: the chain stops.
    Failed,
}

/// The agentd provider for a lane kind, when agentd can run it interactively.
pub fn provider_of_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "claude" => "claude",
        "codex" => "codex",
        "cursor" => "cursor",
        "zcode" => "zcode",
        // Development only: exist when the mock providers are registered (tests).
        "mock" => "mock",
        "mock-alt" => "mock-alt",
        _ => return None,
    })
}

/// The agentd provider and model that run a lane, when agentd has one. Beyond the
/// kinds of [`provider_of_kind`]:
/// - a `command` lane named `mistral-*` (the owner's Vibe sign-in) runs on `vibe`
///   (`~/.config/lucina-agentd/acp.toml`, CodeCor's coding Vibe) with the lane's model;
/// - an `orchestra` lane on a free Zen worker runs on `opencode` with `opencode/<model>`;
/// - a `qcode` lane (the local Qwen) runs on `lucinacode`.
///
/// The paid DeepSeek lane never maps ([`is_forbidden`]).
pub fn agentd_provider(name: &str, lane: &Lane) -> Option<(&'static str, Option<String>)> {
    if is_forbidden(name, lane) {
        return None;
    }
    if let Some(p) = provider_of_kind(&lane.kind) {
        return Some((p, lane.model.clone()));
    }
    match lane.kind.as_str() {
        "command" if name.starts_with("mistral-") => Some(("vibe", lane.model.clone())),
        "orchestra" if lane.worker.as_deref().is_some_and(|w| w.starts_with("zen-")) => {
            let model = lane.model.as_deref().filter(|m| !m.is_empty())?;
            Some(("opencode", Some(format!("opencode/{model}"))))
        }
        "qcode" => Some(("lucinacode", None)),
        _ => None,
    }
}

pub fn default_path() -> PathBuf {
    if let Some(p) = std::env::var_os("LANE_CONFIG") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/lane/chains.toml")
}

impl LaneConfig {
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// The config at [`default_path`], or an empty one when it's missing or broken
    /// (logged; agentd still runs, without defaults).
    pub fn load() -> Self {
        let path = default_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "unreadable lane config");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// The first lane of a kind (its model and effort are the provider's defaults).
    pub fn first_of_kind(&self, kind: &str) -> Option<(&str, &Lane)> {
        // Prefer the order of the `deep` category, then name order.
        let deep = self.categories.get("deep").cloned().unwrap_or_default();
        deep.iter()
            .filter_map(|name| self.lanes.get_key_value(name.as_str()))
            .chain(self.lanes.iter())
            .find(|(_, lane)| lane.kind == kind)
            .map(|(n, l)| (n.as_str(), l))
    }

    /// Classify a failure of a lane, from its kind and the error text.
    pub fn classify(&self, lane: &str, kind: &str, text: &str, exit_code: Option<i32>) -> Outcome {
        for d in &self.detectors {
            if !d.kinds.is_empty() && !d.kinds.iter().any(|k| k == kind) {
                continue;
            }
            if !d.lanes.is_empty() && !d.lanes.iter().any(|l| l == lane) {
                continue;
            }
            if let Some(p) = &d.pattern {
                match Regex::new(p) {
                    Ok(re) if re.is_match(text) => {}
                    _ => continue,
                }
            }
            if let Some(code) = d.exit_code {
                if exit_code != Some(code) {
                    continue;
                }
            }
            if d.empty_stdout == Some(true) && !text.trim().is_empty() {
                continue;
            }
            if d.pattern.is_none() && d.exit_code.is_none() && d.empty_stdout.is_none() {
                continue;
            }
            return match d.outcome.as_str() {
                "quota" => Outcome::Quota(d.name.clone()),
                "unavailable" => Outcome::Unavailable(d.name.clone()),
                _ => Outcome::Failed,
            };
        }
        Outcome::Failed
    }

    /// The lanes of a category that agentd can run, as `(lane, provider, lane config)`;
    /// the config's `model` is the one the provider gets ([`agentd_provider`]).
    pub fn chain(&self, category: &str) -> Vec<(String, &'static str, Lane)> {
        self.categories
            .get(category)
            .into_iter()
            .flatten()
            .filter_map(|name| {
                let lane = self.lanes.get(name)?;
                let (p, model) = agentd_provider(name, lane)?;
                Some((name.clone(), p, Lane { model, ..lane.clone() }))
            })
            .collect()
    }

    /// The lane an agent of `provider` stands for, for its detectors: the agent's own
    /// `lucina.lane` label when it names a lane of that provider, else the first lane
    /// (in `deep-lucina`, then `deep`, then name order) that maps to the provider.
    pub fn lane_of_provider(&self, provider: &str, label: Option<&str>) -> Option<(String, Lane)> {
        if let Some(name) = label {
            if let Some(lane) = self.lanes.get(name) {
                if agentd_provider(name, lane).is_some_and(|(p, _)| p == provider) {
                    return Some((name.to_string(), lane.clone()));
                }
            }
        }
        let order: Vec<String> = ["deep-lucina", "routine-lucina", "deep", "routine"]
            .iter()
            .flat_map(|c| self.categories.get(*c).cloned().unwrap_or_default())
            .chain(self.lanes.keys().cloned())
            .collect();
        order.into_iter().find_map(|name| {
            let lane = self.lanes.get(&name)?;
            agentd_provider(&name, lane).filter(|(p, _)| *p == provider).map(|_| (name.clone(), lane.clone()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[lanes.claude-cli]
kind = "claude"
model = "claude-opus-5-5"
effort = "medium"
[lanes.codex]
kind = "codex"
model = "gpt-6.1-sol"
effort = "medium"
[lanes.zen]
kind = "orchestra"
[lanes.deepseek-paid]
kind = "codex"
worker = "deepseek-flash"
model = "deepseek-flash (paid API)"
[categories]
deep = ["claude-cli", "codex", "zen", "deepseek-paid"]
[[detectors]]
name = "claude-usage-limit"
kinds = ["claude"]
outcome = "quota"
pattern = '(?i)you.ve hit your (session )?limit'
[[detectors]]
name = "codex-auth"
kinds = ["codex"]
outcome = "unavailable"
pattern = '(?i)not logged in'
"#;

    #[test]
    fn parses_and_classifies() {
        let c = LaneConfig::parse(SAMPLE).unwrap();
        assert_eq!(c.first_of_kind("codex").unwrap().1.model.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(c.classify("claude-cli", "claude", "You've hit your session limit · resets 8pm", None), Outcome::Quota("claude-usage-limit".into()));
        assert_eq!(c.classify("codex", "codex", "Not logged in", None), Outcome::Unavailable("codex-auth".into()));
        assert_eq!(c.classify("codex", "codex", "syntax error", None), Outcome::Failed);
        let chain: Vec<&str> = c.chain("deep").iter().map(|(_, p, _)| *p).collect();
        assert_eq!(chain, ["claude", "codex"], "orchestra lanes are not agentd providers, and deepseek-paid never runs");
    }

    /// The failure texts of real stopped runs, as agentd's `failure_text` builds them,
    /// against the owner's detectors (the quota hand-off reads the same).
    #[test]
    fn real_limit_stops_classify_through_the_owner_detectors() {
        let Ok(text) = std::fs::read_to_string(default_path()) else {
            eprintln!("no lane config");
            return;
        };
        let c = LaneConfig::parse(&text).unwrap();
        let class = |provider: &str, failure: &str| {
            let (name, lane) = c.lane_of_provider(provider, None).unwrap_or_else(|| panic!("no lane for {provider}"));
            c.classify(&name, &lane.kind, failure, None)
        };
        // Agents 16d5ad63 and cdecfba5 (2026-11-01/02), word for word.
        let claude = "You've hit your weekly limit · resets Nov 4, 12pm (UTC)\n[System Error] You've hit your weekly limit · resets Nov 4, 12pm (UTC)\n\ncode: usage_limit_reached\n\nHTTP 429";
        assert!(matches!(class("claude", claude), Outcome::Quota(_)), "{:?}", class("claude", claude));
        // Vibe (2026-11-08 probe) and ZCode's expired sign-in.
        let vibe = "session/prompt: API error occurred: Status 402. Body: {\"object\":\"error\",\"message\":\"Vibe budget exhausted.\",\"type\":\"billing_vibe_budget_exhausted\",\"code\":\"2304\",\"raw_status_code\":402}";
        assert!(matches!(class("vibe", vibe), Outcome::Quota(_)), "{:?}", class("vibe", vibe));
        let zcode = "[project-access-token] { family: 'zai', stage: 'resolve', code: 'project_token_login_failed' }";
        assert!(matches!(class("zcode", zcode), Outcome::Unavailable(_)));
        // A task's own failure is not a limit.
        assert_eq!(class("claude", "error[E0308]: mismatched types\n[System Error] build failed"), Outcome::Failed);
    }

    /// The Lucina chain agentd hands on along: lane's `deep-lucina`, never Codex or paid DeepSeek.
    #[test]
    fn owner_deep_lucina_chain_maps_to_agentd_providers() {
        let Ok(text) = std::fs::read_to_string(default_path()) else {
            eprintln!("no lane config");
            return;
        };
        let c = LaneConfig::parse(&text).unwrap();
        let chain = c.chain("deep-lucina");
        let providers: Vec<&str> = chain.iter().map(|(_, p, _)| *p).collect();
        assert!(!providers.contains(&"codex"), "{providers:?}");
        assert_eq!(providers.first(), Some(&"claude"), "{providers:?}");
        for (name, p, lane) in &chain {
            assert!(!is_forbidden(name, lane), "{name}");
            if *p == "opencode" {
                assert!(lane.model.as_deref().is_some_and(|m| m.starts_with("opencode/") && m.ends_with("-free")), "{name}: {:?}", lane.model);
            }
        }
    }

    #[test]
    fn lanes_map_to_agentd_providers() {
        let t = r#"
[lanes.mistral-large]
kind = "command"
model = "mistral-large"
[lanes.zen-deepseek-flash]
kind = "orchestra"
worker = "zen-deepseek-flash"
model = "deepseek-v4.1-flash-free"
[lanes.deepseek-paid]
kind = "orchestra"
worker = "deepseek-flash"
model = "deepseek-flash (paid API)"
[lanes.qwen]
kind = "qcode"
model = "qwen3.8:27b"
[lanes.other]
kind = "command"
"#;
        let c = LaneConfig::parse(t).unwrap();
        let p = |n: &str| agentd_provider(n, &c.lanes[n]);
        assert_eq!(p("mistral-large"), Some(("vibe", Some("mistral-large".into()))));
        assert_eq!(p("zen-deepseek-flash"), Some(("opencode", Some("opencode/deepseek-v4.1-flash-free".into()))));
        assert_eq!(p("deepseek-paid"), None);
        assert_eq!(p("qwen"), Some(("lucinacode", None)));
        assert_eq!(p("other"), None);
        assert_eq!(c.lane_of_provider("vibe", None).map(|(n, _)| n).as_deref(), Some("mistral-large"));
        assert_eq!(c.lane_of_provider("opencode", None).map(|(n, _)| n).as_deref(), Some("zen-deepseek-flash"));
    }

    /// The owner's real config must keep parsing: a schema change in lane breaks this
    /// test, not a run. Skipped where the file doesn't exist (CI, other machines).
    #[test]
    fn owner_config_parses() {
        let path = default_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("no lane config at {}", path.display());
            return;
        };
        let c = LaneConfig::parse(&text).expect("chains.toml parses");
        assert!(!c.lanes.is_empty());
        for d in &c.detectors {
            if let Some(p) = &d.pattern {
                Regex::new(p).unwrap_or_else(|e| panic!("detector {} has a bad regex: {e}", d.name));
            }
        }
    }
}
