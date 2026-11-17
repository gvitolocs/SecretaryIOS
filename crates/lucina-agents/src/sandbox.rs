//! The worker sandbox (LA-C013): a [`Policy`] read from `deploy/lucina-agentd/sandbox-policy.toml`
//! and [`to_bwrap_args`], the bubblewrap argument list for one run. Design, the view a worker gets
//! and the limits: `docs/design/agents-sandbox.md`.
//!
//! Pure: no file system, no environment, no clock. The caller passes real paths (symlinks
//! resolved); this module compares them after lexical normalisation (`.` and `..` removed without
//! touching the file system) and binds exactly the normalised strings it checked.
//!
//! What can never be bound, whatever the policy says, is the fixed list in [`Hidden`] (in this file,
//! not in the policy). It is applied three times: to the policy when it is read
//! ([`Policy::validate`]), to every run input, and to every bind of the finished argument list
//! ([`audit`]) before [`to_bwrap_args`] returns it. A caller that resolves the bound sources on the
//! host can check them once more with [`bound_paths`] and [`Hidden::check`].

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

/// The policy format this build reads.
pub const POLICY_VERSION: u32 = 1;

/// Why a policy or a run was refused. Every path variant names the path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxError {
    #[error("sandbox policy: {0}")]
    Policy(String),
    #[error("{what} {path}: {why}")]
    BadPath {
        what: String,
        path: String,
        why: String,
    },
    #[error("{what} {path} is refused: it would expose {hidden}")]
    Hidden {
        what: String,
        path: String,
        hidden: String,
    },
    #[error("{a_what} {a} and {b_what} {b} overlap (one is, or holds, the other)")]
    Overlap {
        a_what: String,
        a: String,
        b_what: String,
        b: String,
    },
    #[error("lane {lane}: {why}")]
    Lane { lane: String, why: String },
    #[error("environment variable {name}: {why}")]
    Env { name: String, why: String },
    #[error("git directory {path}: {why}")]
    Git { path: String, why: String },
    #[error("argument list: {0}")]
    Args(String),
}

type Result<T> = std::result::Result<T, SandboxError>;

/// The policy file (see `deploy/lucina-agentd/sandbox-policy.toml`). Read it with
/// [`Policy::from_toml`]; [`to_bwrap_args`] validates it again, so a value built by hand gets
/// the same refusals.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub version: u32,
    pub system: SystemView,
    pub home: HomeView,
    /// The lane for a name no key matches. Its `home` ends with `/{lane}`.
    pub default: LanePolicy,
    /// Per lane: an exact name (`qoder`) or a prefix pattern (`zen-*`).
    #[serde(default)]
    pub lanes: BTreeMap<String, LanePolicy>,
}

/// The system half of the view (absolute paths, never under `/home` or `/root`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemView {
    /// Bound read-only when they exist (`--ro-bind-try`).
    pub ro: Vec<String>,
    /// Bound read-only for a lane whose network is `host` (name resolution).
    #[serde(default)]
    pub net_ro: Vec<String>,
    #[serde(default)]
    pub symlinks: Vec<Symlink>,
    /// `PATH` after the shim directory (`~/...` or absolute).
    pub path: Vec<String>,
}

/// A symlink created in the view (`link` points at `target`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Symlink {
    pub target: String,
    pub link: String,
}

/// The home half of the view (every path `~/...`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeView {
    pub toolchains_ro: Vec<String>,
    pub caches_rw: Vec<String>,
    /// Read-write for a build run only; also its `CARGO_TARGET_DIR`.
    pub check_target: String,
    /// Read-only, first on `PATH`; must exist.
    pub shims: String,
}

/// One lane's entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanePolicy {
    /// Its own sign-in home (`~/...`), bound read-write at its real path when it exists.
    pub home: String,
    pub network: Network,
}

/// A lane's network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    /// The host's network namespace (loopback included).
    Host,
    /// A new, empty network namespace.
    None,
}

/// A run's cost class: a light run cannot build, a build run gets the shared check target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostClass {
    Light,
    Build,
}

/// What one run needs, all real paths (the caller resolves symlinks first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInputs {
    /// The real home: an empty tmpfs inside, and `HOME`.
    pub home: PathBuf,
    /// Read-write, the working directory. A worktree's top, or a plain repository's top.
    pub workspace: PathBuf,
    /// The repository's common git directory (`<repo>/.git`, or `<name>.git`).
    pub common_git_dir: PathBuf,
    /// The worktree's own git directory (`<common>/worktrees/<name>`), or `None` for a plain
    /// repository (then `common_git_dir` must be `<workspace>/.git`).
    pub worktree_git_dir: Option<PathBuf>,
    pub lane: String,
    pub cost: CostClass,
    /// The per-run temp directory: read-write, and `TMPDIR`.
    pub tmpdir: PathBuf,
    /// Further read-only paths the run named (a sibling checkout, say).
    pub extra_ro: Vec<PathBuf>,
    /// The environment the caller already filtered (names and values). Nothing else gets in.
    pub env: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------------------------
// What is never bound.

/// Paths relative to the home that may not be bound, nor any ancestor of them; what lies inside
/// may be (`~/.cargo/registry`, a worktree under `~/Projects`).
const HOME_WHOLE: &[&str] = &[
    "",
    ".cargo",
    ".cache",
    ".local/bin",
    ".local/share/lane-homes",
    "Projects",
    "Projects/.worktrees",
    "scratch",
    "scratch/tmp",
];

/// Paths relative to the home that may not be bound, nor anything inside them, nor any ancestor.
const HOME_TREES: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".config",
    ".claude",
    ".claude.json",
    ".cargo/credentials.toml",
    ".cargo/credentials",
    ".git-credentials",
    ".gitconfig",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".docker",
    ".kube",
    ".aws",
    ".local/share/lucina-agentd",
    ".local/state/lucina-agentd",
    ".qoder",
    ".zcode",
    ".codex",
    ".qwen",
    ".opencode",
    ".local/share/opencode",
    ".vibe",
    ".cursor",
    ".gemini",
    ".local/share/zcode-lane-home",
    ".local/share/keyrings",
    ".password-store",
    ".infisical",
    ".mozilla",
    ".bash_history",
    ".zsh_history",
    ".python_history",
    "secrets",
    "mnt",
];

/// Absolute paths that may not be bound, nor anything inside, nor any ancestor (so `/` and `/run`
/// are refused too). `/tmp` holds other programs' sockets (tmux, ssh-agent).
const ABS_TREES: &[&str] = &[
    "/proc",
    "/sys",
    "/dev",
    "/root",
    "/tmp",
    "/var/tmp",
    "/run/user",
    "/var/run/user",
    "/run/docker.sock",
    "/var/run/docker.sock",
    "/run/containerd",
    "/var/run/containerd",
    "/run/dbus",
    "/var/run/dbus",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// The path and its ancestors.
    Whole,
    /// The path, its ancestors and everything inside.
    Tree,
    /// The path, its ancestors, and every child but the named one (other lanes' homes).
    ChildrenExcept(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    kind: Kind,
    path: PathBuf,
    label: String,
}

impl Rule {
    fn covers(&self, p: &Path) -> bool {
        let ancestor_or_equal = self.path.starts_with(p);
        match &self.kind {
            Kind::Whole => ancestor_or_equal,
            Kind::Tree => ancestor_or_equal || p.starts_with(&self.path),
            Kind::ChildrenExcept(own) => {
                ancestor_or_equal
                    || match p.strip_prefix(&self.path) {
                        Ok(rest) => match rest.components().next() {
                            Some(Component::Normal(c)) => own.as_deref() != c.to_str(),
                            _ => false,
                        },
                        Err(_) => false,
                    }
            }
        }
    }
}

/// The paths one run may never see. Built from the fixed lists above, the home, and the policy's
/// lanes (every lane home but the run's own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hidden {
    rules: Vec<Rule>,
}

impl Hidden {
    fn base(home: &Path) -> Hidden {
        let mut rules = Vec::new();
        for rel in HOME_WHOLE {
            rules.push(Rule {
                kind: Kind::Whole,
                path: home.join(rel),
                label: if rel.is_empty() {
                    "the home as a whole (~)".to_owned()
                } else {
                    format!("~/{rel} as a whole")
                },
            });
        }
        for rel in HOME_TREES {
            rules.push(Rule {
                kind: Kind::Tree,
                path: home.join(rel),
                label: format!("~/{rel}"),
            });
        }
        for abs in ABS_TREES {
            rules.push(Rule {
                kind: Kind::Tree,
                path: PathBuf::from(abs),
                label: (*abs).to_owned(),
            });
        }
        Hidden { rules }
    }

    /// The hidden set for a run of `lane` with this home (absolute; it is normalised here).
    pub fn for_run(policy: &Policy, home: &Path, lane: &str) -> Result<Hidden> {
        let home = run_home(home)?;
        let choice = policy.lane(lane)?;
        let own_home = choice.home(&home, lane)?;
        let mut h = Hidden::base(&home);
        for (key, lp) in &policy.lanes {
            if choice.key.as_deref() == Some(key.as_str()) {
                continue;
            }
            h.rules.push(Rule {
                kind: Kind::Tree,
                path: home_path("the sign-in home of a lane", &lp.home, &home)?,
                label: format!("the sign-in home of lane {key}"),
            });
        }
        let parent = template_parent(&policy.default.home, &home)?;
        let own_child = if own_home.parent() == Some(parent.as_path()) {
            own_home
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
        } else {
            None
        };
        h.rules.push(Rule {
            kind: Kind::ChildrenExcept(own_child),
            path: parent,
            label: "the sign-in home of another lane".to_owned(),
        });
        Ok(h)
    }

    /// Refuses `path` (normalised here) when it is, holds, or lies inside a hidden path.
    pub fn check(&self, what: &str, path: &Path) -> Result<()> {
        let p = normalize(path);
        match self.rules.iter().find(|r| r.covers(&p)) {
            Some(r) => Err(SandboxError::Hidden {
                what: what.to_owned(),
                path: show(&p),
                hidden: r.label.clone(),
            }),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Paths.

/// Lexical normalisation: `.` dropped, `..` pops a component (never above `/`).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::RootDir => out.push("/"),
            Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
        }
    }
    out
}

fn show(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn bad(what: &str, path: &str, why: &str) -> SandboxError {
    SandboxError::BadPath {
        what: what.to_owned(),
        path: path.to_owned(),
        why: why.to_owned(),
    }
}

/// A run input: absolute, UTF-8 (a lossy conversion would bind another path), no NUL.
fn input_path(what: &str, p: &Path) -> Result<PathBuf> {
    let Some(text) = p.to_str() else {
        return Err(bad(what, &p.to_string_lossy(), "is not UTF-8"));
    };
    if text.contains('\0') {
        return Err(bad(what, text, "contains a NUL byte"));
    }
    if !p.is_absolute() {
        return Err(bad(
            what,
            text,
            "must be absolute (the caller passes real paths)",
        ));
    }
    Ok(normalize(p))
}

fn run_home(home: &Path) -> Result<PathBuf> {
    let h = input_path("the home", home)?;
    if h.parent().is_none() {
        return Err(bad("the home", &show(&h), "cannot be /"));
    }
    Ok(h)
}

/// A policy path under the home: `~` or `~/...`, staying inside the home after normalisation.
fn home_path(what: &str, spec: &str, home: &Path) -> Result<PathBuf> {
    if spec.contains('\0') {
        return Err(bad(what, spec, "contains a NUL byte"));
    }
    let rest = if spec == "~" {
        ""
    } else if let Some(r) = spec.strip_prefix("~/") {
        r
    } else {
        return Err(bad(
            what,
            spec,
            "must start with ~/ (a path under the home)",
        ));
    };
    let p = normalize(&home.join(rest));
    if !p.starts_with(home) {
        return Err(bad(what, spec, "leaves the home"));
    }
    Ok(p)
}

/// A system path of the policy: absolute, not under `/home` or `/root` (those are written `~/`).
fn system_path(what: &str, spec: &str) -> Result<PathBuf> {
    if spec.contains('\0') {
        return Err(bad(what, spec, "contains a NUL byte"));
    }
    if !spec.starts_with('/') {
        return Err(bad(what, spec, "must be absolute"));
    }
    let p = normalize(Path::new(spec));
    if p.starts_with("/home") || p.starts_with("/root") {
        return Err(bad(
            what,
            spec,
            "is under /home or /root (home paths are written ~/...)",
        ));
    }
    Ok(p)
}

/// A `PATH` entry: `~/...` or absolute, no `:`.
fn path_entry(spec: &str, home: &Path) -> Result<PathBuf> {
    if spec.contains(':') {
        return Err(bad("the PATH entry", spec, "contains ':'"));
    }
    if spec.starts_with('~') {
        home_path("the PATH entry", spec, home)
    } else {
        system_path("the PATH entry", spec)
    }
}

/// The directory that holds the default lane's homes (`<dir>/{lane}`).
fn template_parent(spec: &str, home: &Path) -> Result<PathBuf> {
    let Some(dir) = spec.strip_suffix("/{lane}") else {
        return Err(SandboxError::Policy(format!(
            "default.home {spec} must end with /{{lane}} (each unnamed lane gets its own home)"
        )));
    };
    if dir.contains('{') || dir.contains('}') {
        return Err(SandboxError::Policy(format!(
            "default.home {spec}: {{lane}} may appear only once, at the end"
        )));
    }
    home_path("default.home", dir, home)
}

fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn disjoint(roots: &[(String, PathBuf)]) -> Result<()> {
    for (i, (aw, a)) in roots.iter().enumerate() {
        for (bw, b) in &roots[i + 1..] {
            if overlap(a, b) {
                return Err(SandboxError::Overlap {
                    a_what: aw.clone(),
                    a: show(a),
                    b_what: bw.clone(),
                    b: show(b),
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Lanes.

fn valid_lane_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 64
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// `name` or `prefix-*`: returns the prefix (with its `-`) for a pattern.
fn lane_key(key: &str) -> Result<Option<&str>> {
    if let Some(prefix) = key.strip_suffix('*') {
        if let Some(stem) = prefix.strip_suffix('-') {
            if valid_lane_name(stem) {
                return Ok(Some(prefix));
            }
        }
    } else if valid_lane_name(key) {
        return Ok(None);
    }
    Err(SandboxError::Policy(format!(
        "lane key {key:?}: a lane name ([a-z0-9][a-z0-9._-]*) or a prefix pattern ending in -*"
    )))
}

/// The policy entry a lane name resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LaneChoice<'a> {
    /// The matching key, `None` for the default.
    key: Option<String>,
    lane: &'a LanePolicy,
}

impl LaneChoice<'_> {
    fn home(&self, home: &Path, lane: &str) -> Result<PathBuf> {
        match self.key {
            Some(_) => home_path("the lane home", &self.lane.home, home),
            None => {
                let dir = template_parent(&self.lane.home, home)?;
                Ok(dir.join(lane))
            }
        }
    }
}

impl Policy {
    /// Parses and validates the policy text.
    pub fn from_toml(text: &str) -> Result<Policy> {
        let p: Policy = toml::from_str(text).map_err(|e| SandboxError::Policy(e.to_string()))?;
        p.validate()?;
        Ok(p)
    }

    /// Everything that can be checked without a run: the version, the path forms, every path
    /// against the hidden list (with a stand-in home: all home paths are written `~/`), the lane
    /// keys, and that no two policy paths overlap.
    pub fn validate(&self) -> Result<()> {
        if self.version != POLICY_VERSION {
            return Err(SandboxError::Policy(format!(
                "version {} (this build reads {POLICY_VERSION})",
                self.version
            )));
        }
        // The stand-in home is the relative path `~`: rules and messages then read `~/.ssh`,
        // exactly as the policy writes its paths, and no absolute path can fall under it.
        let home = Path::new("~");
        let base = Hidden::base(home);
        if self.system.ro.is_empty() {
            return Err(SandboxError::Policy("system.ro is empty".to_owned()));
        }
        for spec in self.system.ro.iter().chain(&self.system.net_ro) {
            base.check("the system path", &system_path("the system path", spec)?)?;
        }
        for s in &self.system.symlinks {
            base.check("the symlink", &system_path("the symlink", &s.link)?)?;
            if s.target.is_empty() || s.target.contains('\0') {
                return Err(bad(
                    "the symlink target",
                    &s.target,
                    "is empty or has a NUL",
                ));
            }
        }
        for spec in &self.system.path {
            base.check("the PATH entry", &path_entry(spec, home)?)?;
        }
        let mut roots: Vec<(String, PathBuf)> = Vec::new();
        for spec in &self.home.toolchains_ro {
            roots.push((
                "the toolchain".into(),
                home_path("the toolchain", spec, home)?,
            ));
        }
        for spec in &self.home.caches_rw {
            roots.push(("the cache".into(), home_path("the cache", spec, home)?));
        }
        roots.push((
            "the check target".into(),
            home_path("the check target", &self.home.check_target, home)?,
        ));
        roots.push((
            "the shim directory".into(),
            home_path("the shim directory", &self.home.shims, home)?,
        ));
        for (what, p) in &roots {
            base.check(what, p)?;
        }
        let parent = template_parent(&self.default.home, home)?;
        base.check("the default lane home", &parent.join("lane"))?;
        for (what, p) in &roots {
            if overlap(p, &parent) {
                return Err(SandboxError::Overlap {
                    a_what: what.clone(),
                    a: show(p),
                    b_what: "the lane homes".into(),
                    b: show(&parent),
                });
            }
        }
        for (key, lp) in &self.lanes {
            lane_key(key)?;
            if lp.home.contains('{') {
                return Err(SandboxError::Policy(format!(
                    "lane {key}: {{lane}} is for the default only"
                )));
            }
            let what = format!("the sign-in home of lane {key}");
            let p = home_path(&what, &lp.home, home)?;
            base.check(&what, &p)?;
            if p == parent || (p.starts_with(&parent) && p.parent() != Some(parent.as_path())) {
                return Err(bad(
                    &what,
                    &lp.home,
                    "must be a direct child of the default's lane-home directory, or outside it",
                ));
            }
            roots.push((what, p));
        }
        disjoint(&roots)
    }

    fn lane(&self, lane: &str) -> Result<LaneChoice<'_>> {
        if !valid_lane_name(lane) {
            return Err(SandboxError::Lane {
                lane: lane.to_owned(),
                why: "not a lane name ([a-z0-9][a-z0-9._-]*, at most 64)".to_owned(),
            });
        }
        if let Some(lp) = self.lanes.get(lane) {
            return Ok(LaneChoice {
                key: Some(lane.to_owned()),
                lane: lp,
            });
        }
        let mut best: Option<(&String, &LanePolicy, usize)> = None;
        for (key, lp) in &self.lanes {
            if let Ok(Some(prefix)) = lane_key(key) {
                if lane.len() > prefix.len()
                    && lane.starts_with(prefix)
                    && best.is_none_or(|(_, _, n)| prefix.len() > n)
                {
                    best = Some((key, lp, prefix.len()));
                }
            }
        }
        Ok(match best {
            Some((key, lp, _)) => LaneChoice {
                key: Some(key.clone()),
                lane: lp,
            },
            None => LaneChoice {
                key: None,
                lane: &self.default,
            },
        })
    }

    /// The key a lane name resolves to (`None`: the default) and its network.
    pub fn lane_entry(&self, lane: &str) -> Result<(Option<String>, Network)> {
        let c = self.lane(lane)?;
        Ok((c.key, c.lane.network))
    }
}

// ---------------------------------------------------------------------------------------------
// Environment.

/// Set by the generator; a caller may not pass them.
const OWNED_ENV: &[&str] = &[
    "HOME",
    "PATH",
    "TMPDIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_TERMINAL_PROMPT",
    "CARGO_TARGET_DIR",
    "LUCINA_SANDBOX",
    "LUCINA_LANE_HOME",
];

/// Point at agents, keyrings, buses or another git directory: refused outright.
const REFUSED_ENV: &[&str] = &[
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "GPG_AGENT_INFO",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_RUNTIME_DIR",
    "GNOME_KEYRING_CONTROL",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

fn check_env(env: &[(String, String)]) -> Result<()> {
    let err = |name: &str, why: &str| SandboxError::Env {
        name: name.to_owned(),
        why: why.to_owned(),
    };
    for (i, (name, value)) in env.iter().enumerate() {
        let mut chars = name.chars();
        let ok = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return Err(err(name, "not a variable name ([A-Za-z_][A-Za-z0-9_]*)"));
        }
        if value.contains('\0') {
            return Err(err(name, "its value contains a NUL byte"));
        }
        if OWNED_ENV.contains(&name.as_str()) {
            return Err(err(name, "is set by the sandbox, not by the caller"));
        }
        if REFUSED_ENV.contains(&name.as_str()) {
            return Err(err(
                name,
                "points at an agent, a keyring, a bus or another git directory",
            ));
        }
        if env[..i].iter().any(|(n, _)| n == name) {
            return Err(err(name, "passed twice"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The generator.

struct Args(Vec<String>);

impl Args {
    fn flag(&mut self, f: &str) {
        self.0.push(f.to_owned());
    }
    fn one(&mut self, f: &str, a: &str) {
        self.0.push(f.to_owned());
        self.0.push(a.to_owned());
    }
    fn bind(&mut self, f: &str, p: &Path) {
        let s = show(p);
        self.0.push(f.to_owned());
        self.0.push(s.clone());
        self.0.push(s);
    }
    fn setenv(&mut self, name: &str, value: &str) {
        self.0.push("--setenv".to_owned());
        self.0.push(name.to_owned());
        self.0.push(value.to_owned());
    }
}

/// The complete bubblewrap argument list for one run, ending with `--` (the program and its
/// arguments follow). Refuses, naming the path, any policy or input that would expose a hidden
/// path; see the module documentation and `docs/design/agents-sandbox.md` for the view.
pub fn to_bwrap_args(policy: &Policy, run: &RunInputs) -> Result<Vec<String>> {
    policy.validate()?;
    let home = run_home(&run.home)?;
    let choice = policy.lane(&run.lane)?;
    let lane_home = choice.home(&home, &run.lane)?;
    let network = choice.lane.network;
    let build = run.cost == CostClass::Build;
    let hidden = Hidden::for_run(policy, &home, &run.lane)?;

    // Run inputs: absolute, not hidden, not in a dot directory of the home.
    let dot_check = |what: &str, p: &Path| -> Result<()> {
        if let Ok(rest) = p.strip_prefix(&home) {
            if let Some(Component::Normal(first)) = rest.components().next() {
                if first.to_string_lossy().starts_with('.') {
                    return Err(SandboxError::Hidden {
                        what: what.to_owned(),
                        path: show(p),
                        hidden: format!(
                            "~/{} (a run input may not lie in a dot directory of the home)",
                            first.to_string_lossy()
                        ),
                    });
                }
            }
        }
        Ok(())
    };
    let input = |what: &str, p: &Path| -> Result<PathBuf> {
        let p = input_path(what, p)?;
        hidden.check(what, &p)?;
        dot_check(what, &p)?;
        Ok(p)
    };
    let workspace = input("the workspace", &run.workspace)?;
    let common = input("the common git directory", &run.common_git_dir)?;
    let tmpdir = input("the run's temp directory", &run.tmpdir)?;
    let mut extra = Vec::new();
    for p in &run.extra_ro {
        extra.push(input("the extra read-only path", p)?);
    }

    // Git layout.
    let git_err = |p: &Path, why: &str| SandboxError::Git {
        path: show(p),
        why: why.to_owned(),
    };
    let ends_in_git = common
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".git"));
    if !ends_in_git {
        return Err(git_err(
            &common,
            "a common git directory is named .git or <name>.git",
        ));
    }
    let own = match &run.worktree_git_dir {
        None => {
            if common != workspace.join(".git") {
                return Err(git_err(
                    &common,
                    "for a plain repository it must be <workspace>/.git",
                ));
            }
            None
        }
        Some(p) => {
            let own = input("the worktree's git directory", p)?;
            let name_ok = own.parent() == Some(common.join("worktrees").as_path())
                && matches!(own.file_name(), Some(n) if !n.is_empty());
            if !name_ok {
                return Err(git_err(
                    &own,
                    "a worktree's git directory is <common>/worktrees/<name>",
                ));
            }
            Some(own)
        }
    };

    // Policy paths for this home.
    let mut toolchains = Vec::new();
    for spec in &policy.home.toolchains_ro {
        toolchains.push(home_path("the toolchain", spec, &home)?);
    }
    let mut caches = Vec::new();
    for spec in &policy.home.caches_rw {
        caches.push(home_path("the cache", spec, &home)?);
    }
    let check_target = home_path("the check target", &policy.home.check_target, &home)?;
    let shims = home_path("the shim directory", &policy.home.shims, &home)?;
    hidden.check("the lane home", &lane_home)?;
    let mut system = Vec::new();
    for spec in &policy.system.ro {
        system.push(system_path("the system path", spec)?);
    }
    let mut net_ro = Vec::new();
    for spec in &policy.system.net_ro {
        net_ro.push(system_path("the system path", spec)?);
    }

    // No two bound roots overlap (a plain repository's .git lies in its workspace by design).
    let mut roots: Vec<(String, PathBuf)> = vec![
        ("the workspace".into(), workspace.clone()),
        ("the run's temp directory".into(), tmpdir.clone()),
        ("the lane home".into(), lane_home.clone()),
        ("the shim directory".into(), shims.clone()),
    ];
    if own.is_some() {
        roots.push(("the common git directory".into(), common.clone()));
    }
    if build {
        roots.push(("the check target".into(), check_target.clone()));
    }
    roots.extend(
        extra
            .iter()
            .map(|p| ("the extra read-only path".into(), p.clone())),
    );
    roots.extend(
        toolchains
            .iter()
            .map(|p| ("the toolchain".into(), p.clone())),
    );
    roots.extend(caches.iter().map(|p| ("the cache".into(), p.clone())));
    disjoint(&roots)?;
    for (what, p) in &roots {
        for s in system.iter().chain(&net_ro) {
            if overlap(p, s) {
                return Err(SandboxError::Overlap {
                    a_what: what.clone(),
                    a: show(p),
                    b_what: "the system path".into(),
                    b: show(s),
                });
            }
        }
    }

    check_env(&run.env)?;
    let mut path = vec![show(&shims)];
    for spec in &policy.system.path {
        let p = path_entry(spec, &home)?;
        hidden.check("the PATH entry", &p)?;
        path.push(show(&p));
    }

    // The list. Order matters: bubblewrap mounts in order, a later mount covers an earlier one.
    let mut a = Args(Vec::new());
    for f in [
        "--die-with-parent",
        "--new-session",
        "--unshare-user",
        "--disable-userns",
        "--unshare-ipc",
        "--unshare-pid",
        "--unshare-uts",
        "--unshare-cgroup",
    ] {
        a.flag(f);
    }
    if network == Network::None {
        a.flag("--unshare-net");
    }
    a.one("--cap-drop", "ALL");
    a.flag("--clearenv");
    for p in &system {
        a.bind("--ro-bind-try", p);
    }
    for s in &policy.symlinks_checked()? {
        a.0.extend(["--symlink".to_owned(), s.0.clone(), show(&s.1)]);
    }
    if network == Network::Host {
        for p in &net_ro {
            a.bind("--ro-bind-try", p);
        }
    }
    a.one("--proc", "/proc");
    a.one("--dev", "/dev");
    a.one("--tmpfs", "/tmp");
    a.one("--perms", "0700");
    a.one("--tmpfs", &show(&home));
    for p in &toolchains {
        a.bind("--ro-bind-try", p);
    }
    for p in &caches {
        a.bind("--bind-try", p);
    }
    if build {
        a.bind("--bind", &check_target);
    }
    a.bind("--bind-try", &lane_home);
    a.bind("--bind", &tmpdir);
    for p in &extra {
        a.bind("--ro-bind", p);
    }
    a.bind("--ro-bind", &shims);
    a.bind("--bind", &workspace);
    match &own {
        Some(own) => {
            // The common directory read-only; only what a commit writes is writable.
            a.bind("--ro-bind", &common);
            a.bind("--bind", &common.join("objects"));
            a.bind("--bind", &common.join("refs"));
            a.bind("--bind-try", &common.join("logs"));
            a.bind("--bind-try", &common.join("packed-refs"));
            a.bind("--bind-try", &common.join("reftable"));
            a.bind("--bind", own);
        }
        None => a.bind("--bind", &common),
    }
    // After the common directory: no hook, no remote URL, no attribute or submodule config.
    a.bind("--ro-bind", &common.join("hooks"));
    a.bind("--ro-bind", &common.join("config"));
    a.bind("--ro-bind", &common.join("info"));
    a.bind("--ro-bind-try", &common.join("modules"));
    if let Some(own) = &own {
        // No redirect of this worktree to another common directory, from inside or outside.
        a.bind("--ro-bind", &own.join("commondir"));
        a.bind("--ro-bind", &own.join("gitdir"));
        a.bind("--ro-bind-try", &own.join("config.worktree"));
        a.bind("--ro-bind", &workspace.join(".git"));
    }
    a.one("--chdir", &show(&workspace));
    a.setenv("HOME", &show(&home));
    a.setenv("PATH", &path.join(":"));
    a.setenv("TMPDIR", &show(&tmpdir));
    a.setenv("GIT_CONFIG_GLOBAL", "/dev/null");
    a.setenv("GIT_CONFIG_SYSTEM", "/dev/null");
    a.setenv("GIT_CONFIG_NOSYSTEM", "1");
    a.setenv("GIT_TERMINAL_PROMPT", "0");
    a.setenv("LUCINA_SANDBOX", "bwrap");
    a.setenv("LUCINA_LANE_HOME", &show(&lane_home));
    if build {
        a.setenv("CARGO_TARGET_DIR", &show(&check_target));
    }
    for (name, value) in &run.env {
        a.setenv(name, value);
    }
    a.flag("--");
    audit(&hidden, &a.0)?;
    Ok(a.0)
}

impl Policy {
    fn symlinks_checked(&self) -> Result<Vec<(String, PathBuf)>> {
        self.system
            .symlinks
            .iter()
            .map(|s| Ok((s.target.clone(), system_path("the symlink", &s.link)?)))
            .collect()
    }
}

/// The arity of each option [`to_bwrap_args`] emits; anything else is refused by [`audit`].
fn arity(flag: &str) -> Option<usize> {
    Some(match flag {
        "--die-with-parent" | "--new-session" | "--unshare-user" | "--disable-userns"
        | "--unshare-ipc" | "--unshare-pid" | "--unshare-uts" | "--unshare-cgroup"
        | "--unshare-net" | "--clearenv" => 0,
        "--cap-drop" | "--proc" | "--dev" | "--tmpfs" | "--chdir" | "--perms" => 1,
        "--bind" | "--bind-try" | "--ro-bind" | "--ro-bind-try" | "--symlink" | "--setenv" => 2,
        _ => return None,
    })
}

fn is_bind(flag: &str) -> bool {
    matches!(
        flag,
        "--bind" | "--bind-try" | "--ro-bind" | "--ro-bind-try"
    )
}

/// The last line of defence: walks an argument list, refuses any option this module does not emit
/// (a device bind, a bind from a file descriptor), any bind whose source or target is hidden, a
/// list without the namespaces, `--clearenv` after a `--setenv`, and a list that does not end with `--`.
pub fn audit(hidden: &Hidden, args: &[String]) -> Result<()> {
    let mut i = 0;
    let mut seen: Vec<&str> = Vec::new();
    let mut ended = false;
    while i < args.len() {
        let f = args[i].as_str();
        if f == "--" {
            if i + 1 != args.len() {
                return Err(SandboxError::Args("`--` must end the list".to_owned()));
            }
            ended = true;
            break;
        }
        let Some(n) = arity(f) else {
            return Err(SandboxError::Args(format!(
                "{f} is not an option this generator emits"
            )));
        };
        if i + n >= args.len() {
            return Err(SandboxError::Args(format!("{f} is missing its arguments")));
        }
        if is_bind(f) {
            hidden.check("the bind source", Path::new(&args[i + 1]))?;
            hidden.check("the bind target", Path::new(&args[i + 2]))?;
            if !args[i + 1].starts_with('/') || !args[i + 2].starts_with('/') {
                return Err(SandboxError::Args(format!(
                    "{f} {} {}: paths must be absolute",
                    args[i + 1],
                    args[i + 2]
                )));
            }
        }
        match f {
            "--cap-drop" if args[i + 1] != "ALL" => {
                return Err(SandboxError::Args("--cap-drop must be ALL".to_owned()));
            }
            "--proc" if args[i + 1] != "/proc" => {
                return Err(SandboxError::Args("--proc must be /proc".to_owned()));
            }
            "--dev" if args[i + 1] != "/dev" => {
                return Err(SandboxError::Args("--dev must be /dev".to_owned()));
            }
            "--clearenv" if seen.contains(&"--setenv") => {
                return Err(SandboxError::Args(
                    "--clearenv after a --setenv would drop it".to_owned(),
                ));
            }
            "--setenv" if !seen.contains(&"--clearenv") => {
                return Err(SandboxError::Args(
                    "--setenv before --clearenv: the environment would not be cleared".to_owned(),
                ));
            }
            _ => {}
        }
        seen.push(f);
        i += n + 1;
    }
    if !ended {
        return Err(SandboxError::Args("the list must end with `--`".to_owned()));
    }
    for required in [
        "--die-with-parent",
        "--new-session",
        "--unshare-user",
        "--disable-userns",
        "--unshare-ipc",
        "--unshare-pid",
        "--unshare-uts",
        "--cap-drop",
        "--clearenv",
        "--proc",
        "--dev",
    ] {
        if !seen.contains(&required) {
            return Err(SandboxError::Args(format!("{required} is missing")));
        }
    }
    Ok(())
}

/// The host paths a list binds (each bind's source), for a caller that resolves them on the host
/// and checks the real paths again with [`Hidden::check`].
pub fn bound_paths(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() && args[i] != "--" {
        let n = arity(&args[i]).unwrap_or(0);
        if is_bind(&args[i]) && i + 1 < args.len() {
            out.push(args[i + 1].as_str());
        }
        i += n + 1;
    }
    out
}

#[cfg(test)]
mod tests;
