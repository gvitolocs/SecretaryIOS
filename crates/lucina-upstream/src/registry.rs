//! The upstream registry (`docs/upstreams.toml`; the format is in its header comment).
//! Unknown keys are refused so a typo fails the tests instead of being ignored.

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("registry does not parse: {0}")]
    Parse(#[from] toml::de::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub upstream: Vec<Upstream>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    /// Ask the owner from this JEV importance (0-4) up.
    pub notify_min_importance: u32,
    /// …or from this probability of a security fix.
    pub security_min: f64,
    pub max_decisions_per_run: usize,
    pub max_contrib_per_run: usize,
    /// On "Port into ours", also start an agent in the new workspace.
    pub auto_run: bool,
    pub data_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            notify_min_importance: 3,
            security_min: 0.5,
            max_decisions_per_run: 3,
            max_contrib_per_run: 1,
            auto_run: false,
            data_dir: "/home/nez/Projects/Lucina/data/upstream-watch".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Github,
    Git,
    Debian,
    Frozen,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Track {
    #[default]
    Releases,
    Tags,
    Commits,
}

/// Whether a significant change is asked about or only reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Notify {
    /// Ask the owner when significant.
    #[default]
    Auto,
    /// Weekly report only, never ask.
    Report,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Relation {
    Port,
    Fork,
    Vendored,
    Reference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Confirmed,
    Likely,
    Unconfirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContribStatus {
    #[default]
    Candidate,
    Asked,
    Drafting,
    Offered,
    Sent,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
    pub id: String,
    pub name: String,
    pub url: String,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<Track>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_date: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub ours: Vec<Ours>,
    pub licence_upstream: String,
    pub licence_ours: String,
    pub relation: Relation,
    pub confidence: Confidence,
    #[serde(default)]
    pub rely_on: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<Notify>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
    #[serde(default)]
    pub contrib: Vec<Contrib>,
}

impl Upstream {
    pub fn branch(&self) -> &str {
        self.branch.as_deref().unwrap_or("main")
    }
    pub fn track(&self) -> Track {
        self.track.unwrap_or_default()
    }
    /// The owner may be asked about it (`notify` is not `report`).
    pub fn asks(&self) -> bool {
        self.notify.unwrap_or_default() == Notify::Auto
    }
    /// Where our counterpart lives, for messages (`<repo>/<path>`).
    pub fn ours_label(&self) -> String {
        self.ours
            .first()
            .map(Ours::label)
            .unwrap_or_else(|| "our counterpart".into())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ours {
    pub repo: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

impl Ours {
    pub fn label(&self) -> String {
        let repo = self.repo.trim_end_matches('/');
        if self.path.is_empty() || self.path == "." {
            repo.to_owned()
        } else {
            format!("{repo}/{}", self.path)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contrib {
    pub id: String,
    pub title: String,
    /// Our files or commits that hold the improvement.
    pub ours: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_target: Option<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub status: ContribStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub level: Level,
    /// The entry's id (or `settings`).
    pub id: String,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let level = match self.level {
            Level::Error => "error",
            Level::Warning => "warning",
        };
        write!(f, "{level}: {}: {}", self.id, self.message)
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_date(d: &str) -> bool {
    chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok()
}

impl Registry {
    pub fn parse(text: &str) -> Result<Self, RegistryError> {
        Ok(toml::from_str(text)?)
    }

    pub fn get(&self, id: &str) -> Option<&Upstream> {
        self.upstream.iter().find(|u| u.id == id)
    }

    /// Every problem, errors and warnings.
    pub fn validate(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        let mut push = |level, id: &str, message: String| {
            out.push(Problem {
                level,
                id: id.to_owned(),
                message,
            })
        };
        let s = &self.settings;
        if s.notify_min_importance > 4 {
            push(
                Level::Error,
                "settings",
                "notify_min_importance is 0-4".into(),
            );
        }
        if !(0.0..=1.0).contains(&s.security_min) {
            push(Level::Error, "settings", "security_min is 0-1".into());
        }
        if s.data_dir.trim().is_empty() {
            push(Level::Error, "settings", "data_dir is empty".into());
        }
        let mut ids = std::collections::HashSet::new();
        let mut contrib_ids = std::collections::HashSet::new();
        for u in &self.upstream {
            let id = u.id.as_str();
            if !valid_id(id) {
                push(Level::Error, id, "id must be [a-z0-9-]".into());
            }
            if !ids.insert(id) {
                push(Level::Error, id, "duplicate id".into());
            }
            if u.name.trim().is_empty() {
                push(Level::Error, id, "name is empty".into());
            }
            match u.source {
                Source::Github => {
                    let ok = u.repo.as_deref().is_some_and(|r| {
                        let mut p = r.split('/');
                        matches!((p.next(), p.next(), p.next()), (Some(a), Some(b), None) if !a.is_empty() && !b.is_empty())
                    });
                    if !ok {
                        push(
                            Level::Error,
                            id,
                            "github needs repo = \"owner/name\"".into(),
                        );
                    }
                }
                Source::Git => {
                    if u.url.trim().is_empty() && u.local.is_none() {
                        push(Level::Error, id, "git needs url or local".into());
                    }
                }
                Source::Debian => {
                    if u.package.as_deref().is_none_or(|p| p.trim().is_empty()) {
                        push(Level::Error, id, "debian needs package".into());
                    }
                }
                Source::Frozen | Source::Manual => {}
            }
            if u.track.is_some() && u.source != Source::Github {
                push(Level::Warning, id, "track is only read for github".into());
            }
            if matches!(u.source, Source::Github | Source::Git)
                && u.pin.is_none()
                && u.pin_date.is_none()
            {
                let level = if u.confidence == Confidence::Unconfirmed {
                    Level::Warning
                } else {
                    Level::Error
                };
                push(level, id, "needs pin or pin_date".into());
            }
            if let Some(d) = &u.pin_date {
                if !valid_date(d) {
                    push(
                        Level::Error,
                        id,
                        format!("pin_date {d:?} is not YYYY-MM-DD"),
                    );
                }
            }
            if u.ours.is_empty() {
                push(Level::Error, id, "needs at least one ours".into());
            }
            for o in &u.ours {
                if o.repo.trim().is_empty() {
                    push(Level::Error, id, "ours.repo is empty".into());
                }
            }
            for c in &u.contrib {
                if !valid_id(&c.id) {
                    push(
                        Level::Error,
                        id,
                        format!("contrib id {:?} must be [a-z0-9-]", c.id),
                    );
                }
                if !contrib_ids.insert(c.id.as_str()) {
                    push(Level::Error, id, format!("duplicate contrib id {:?}", c.id));
                }
                if c.title.trim().is_empty() {
                    push(Level::Error, id, format!("contrib {} has no title", c.id));
                }
            }
        }
        out
    }

    /// Only the errors of [`Registry::validate`].
    pub fn errors(&self) -> Vec<Problem> {
        self.validate()
            .into_iter()
            .filter(|p| p.level == Level::Error)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: &str = r#"
[settings]
max_decisions_per_run = 2

[[upstream]]
id = "foo"
name = "Foo"
url = "https://github.com/acme/foo"
source = "github"
repo = "acme/foo"
pin = "v1.2.0"
ours = [{ repo = "/r", path = "crates/foo" }]
licence_upstream = "MIT"
licence_ours = "MIT"
relation = "port"
confidence = "confirmed"

[[upstream.contrib]]
id = "foo-faster"
title = "a faster parser"
ours = "crates/foo/src/parse.rs"
"#;

    #[test]
    fn parses_with_defaults() {
        let r = Registry::parse(SMALL).unwrap();
        assert_eq!(r.settings.max_decisions_per_run, 2);
        assert_eq!(r.settings.notify_min_importance, 3);
        assert_eq!(r.settings.security_min, 0.5);
        assert!(!r.settings.auto_run);
        let u = &r.upstream[0];
        assert_eq!(u.branch(), "main");
        assert_eq!(u.track(), Track::Releases);
        assert_eq!(u.contrib[0].status, ContribStatus::Candidate);
        assert_eq!(u.ours_label(), "/r/crates/foo");
        assert!(u.asks());
        let quiet = SMALL.replace("pin = ", "notify = \"report\"\npin = ");
        assert!(!Registry::parse(&quiet).unwrap().upstream[0].asks());
        assert!(r.validate().is_empty(), "{:?}", r.validate());
    }

    #[test]
    fn unknown_keys_fail() {
        let typo = SMALL.replace("pin = ", "pinn = ");
        assert!(Registry::parse(&typo).is_err());
        let typo = SMALL.replace("max_decisions_per_run", "max_decision_per_run");
        assert!(Registry::parse(&typo).is_err());
        let bad = SMALL.replace("relation = \"port\"", "relation = \"copy\"");
        assert!(Registry::parse(&bad).is_err());
    }

    #[test]
    fn validation_problems() {
        let mut r = Registry::parse(SMALL).unwrap();
        let mut dup = r.upstream[0].clone();
        dup.contrib.clear();
        r.upstream.push(dup);
        let mut u = r.upstream[0].clone();
        u.id = "Bad_Id".into();
        u.repo = Some("nope".into());
        u.pin = None;
        u.ours.clear();
        u.contrib.clear();
        r.upstream.push(u);
        let mut d = r.upstream[0].clone();
        d.id = "deb".into();
        d.source = Source::Debian;
        d.contrib.clear();
        r.upstream.push(d);
        let mut g = r.upstream[0].clone();
        g.id = "unsure".into();
        g.pin = None;
        g.confidence = Confidence::Unconfirmed;
        g.contrib.clear();
        r.upstream.push(g);
        let p = r.validate();
        let has = |id: &str, level: Level, m: &str| {
            p.iter()
                .any(|x| x.id == id && x.level == level && x.message.contains(m))
        };
        assert!(has("foo", Level::Error, "duplicate id"), "{p:?}");
        assert!(has("Bad_Id", Level::Error, "[a-z0-9-]"));
        assert!(has("Bad_Id", Level::Error, "owner/name"));
        assert!(has("Bad_Id", Level::Error, "pin or pin_date"));
        assert!(has("Bad_Id", Level::Error, "at least one ours"));
        assert!(has("deb", Level::Error, "package"));
        assert!(has("unsure", Level::Warning, "pin or pin_date"));
        assert!(!has("unsure", Level::Error, "pin or pin_date"));
    }

    #[test]
    fn notify_paths_bases_and_unpinned_sources() {
        let bad = SMALL.replace("pin = ", "notify = \"never\"\npin = ");
        assert!(Registry::parse(&bad).is_err(), "notify is auto | report");
        let odd = SMALL.replace(
            "ours = [{ repo = \"/r\", path = \"crates/foo\" }]",
            "ours = [{ repo = \"/r\", path = \".\", base = \"usb-dev-loop\" }]",
        );
        let r = Registry::parse(&odd).unwrap();
        assert_eq!(r.upstream[0].ours[0].base.as_deref(), Some("usb-dev-loop"));
        assert_eq!(r.upstream[0].ours_label(), "/r");
        assert!(r.errors().is_empty());
        let short = SMALL.replace("pin = \"v1.2.0\"", "pin = \"29e36ae1\"");
        assert!(Registry::parse(&short).unwrap().errors().is_empty());
        for source in ["manual\"\ncheck_hint = \"look by hand", "frozen"] {
            let t = SMALL
                .replace("source = \"github\"", &format!("source = \"{source}\""))
                .replace("repo = \"acme/foo\"\n", "")
                .replace("pin = \"v1.2.0\"\n", "");
            let r = Registry::parse(&t).unwrap();
            assert!(r.validate().is_empty(), "{source}: {:?}", r.validate());
        }
    }

    #[test]
    fn the_real_registry_is_valid() {
        let r = Registry::parse(include_str!("../../../docs/upstreams.toml")).unwrap();
        assert!(!r.upstream.is_empty());
        assert_eq!(r.errors(), vec![]);
    }
}
