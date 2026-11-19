//! The worker sandbox (LA-C013): a [`Policy`] read from `deploy/lucina-agentd/sandbox-policy.toml`,
//! [`plan`], which turns a policy and one run into a [`Plan`], and [`Plan::resolve`], the only way to
//! the bubblewrap argument list. Design, the view a worker gets and the limits:
//! `docs/design/agents-sandbox.md`.
//!
//! Pure, except [`host`]: no file system, no environment, no clock. Paths are compared after lexical
//! normalisation (`.` and `..` removed without touching the file system). Because a bind mount follows
//! a symlink at its source, the arguments are not handed out by [`plan`]: [`Plan::resolve`] takes a
//! resolver (the real one is [`host::resolve`]) and refuses any bind whose source does not resolve to
//! exactly the string that was checked, or resolves into a hidden path.
//!
//! What can never be bound, whatever the policy says, is the fixed list in [`Hidden`] (in this file,
//! not in the policy). It is applied to the policy when it is read ([`Policy::validate`]), to every
//! run input, to every resolved bind source, and to every bind, symlink and environment value of the
//! finished list ([`audit`]).

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

pub mod host;

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
    #[error("bind source {path}: {why}")]
    Resolve { path: String, why: String },
    #[error("argument list: {0}")]
    Args(String),
}

type Result<T> = std::result::Result<T, SandboxError>;

/// The policy file (see `deploy/lucina-agentd/sandbox-policy.toml`). Read it with
/// [`Policy::from_toml`]; [`plan`] validates it again, so a value built by hand gets the same
/// refusals.
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

/// The system half of the view.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemView {
    /// Read-only trees, each one of [`SYSTEM_RO_ROOTS`].
    pub ro: Vec<String>,
    /// Read-only files for a lane whose network is `host` (name resolution); each must be a file.
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
    /// Read-only, first on `PATH`; must exist. It also holds [`EMPTY_FILE`].
    pub shims: String,
}

/// An empty file in the shim directory, bound read-only over a git file that must stay read-only but
/// does not exist yet (`config.worktree`).
pub const EMPTY_FILE: &str = "empty";

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

/// A git worktree: the repository's common git directory and this worktree's own git directory
/// (`<common>/worktrees/<name>`). A plain repository is refused (see [`plan`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktree {
    pub common_dir: PathBuf,
    pub own_dir: PathBuf,
}

/// What one run needs, all real paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInputs {
    /// The real home: an empty tmpfs inside, and `HOME`.
    pub home: PathBuf,
    /// Read-write, the working directory: a worktree's top, or a directory without git.
    pub workspace: PathBuf,
    /// The worktree's git directories; `None` for a workspace without git (which must then have no
    /// `.git`, checked at [`Plan::resolve`]).
    pub git: Option<GitWorktree>,
    pub lane: String,
    pub cost: CostClass,
    /// The per-run temp directory: read-write, and `TMPDIR`.
    pub tmpdir: PathBuf,
    /// Further read-only paths the run named (a sibling checkout, say).
    pub extra_ro: Vec<PathBuf>,
    /// The environment the caller already filtered (names and values); refused names in
    /// [`check_env_pair`].
    pub env: Vec<(String, String)>,
    /// `LUCINA_LEAD`, `LUCINA_RUN_ID`, `LUCINA_CHANGE`: each `[A-Za-z0-9-]{1,32}`.
    pub lead: Option<String>,
    pub run_id: Option<String>,
    pub change: Option<String>,
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
    ".cargo/config.toml",
    ".cargo/config",
    ".git-credentials",
    ".gitconfig",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".docker",
    ".kube",
    ".aws",
    ".azure",
    ".terraform.d",
    ".terraformrc",
    ".vault-token",
    ".pgpass",
    ".my.cnf",
    ".yarnrc",
    ".yarnrc.yml",
    ".gem",
    ".bundle",
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
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".pam_environment",
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
    "/run/podman",
    "/var/run/podman",
    "/run/snapd.socket",
    "/run/snapd-snap.socket",
    "/var/run/snapd.socket",
    "/var/run/snapd-snap.socket",
    "/run/dbus",
    "/var/run/dbus",
];

/// The only trees `system.ro` may name. `/usr` holds the programs and libraries a toolchain runs
/// (git, sh, the C linker, libc, ld.so); `/etc` holds what they read at start (nss and `passwd` for
/// the user name, `ld.so.cache`, `ssl/certs`, `hosts`, `alternatives`). Nothing else on the host is
/// needed for git and cargo, so nothing else may be asked for. Assumption: the host keeps secrets in
/// `/etc` and `/usr` at modes the owner's user cannot read (`/etc/shadow`, host keys, netplan): a
/// world-readable file with a secret there IS readable inside.
pub const SYSTEM_RO_ROOTS: &[&str] = &["/usr", "/etc"];

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

fn fold(p: &Path) -> PathBuf {
    PathBuf::from(p.to_string_lossy().to_ascii_lowercase())
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

    /// The same test with both sides ASCII-lowercased (`~/.SSH` is `~/.ssh` on a case-insensitive
    /// file system).
    fn covers_folded(&self, p: &Path) -> bool {
        let kind = match &self.kind {
            Kind::ChildrenExcept(own) => {
                Kind::ChildrenExcept(own.as_ref().map(|o| o.to_ascii_lowercase()))
            }
            k => k.clone(),
        };
        Rule {
            kind,
            path: fold(&self.path),
            label: String::new(),
        }
        .covers(&fold(p))
    }
}

/// The paths one run may never see. Built from the fixed lists above, the home, and the policy's
/// lanes (every lane home but the run's own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hidden {
    home: PathBuf,
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
        Hidden {
            home: home.to_path_buf(),
            rules,
        }
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
        self.refuse(what, &p, self.rules.iter().find(|r| r.covers(&p)))
    }

    /// [`Hidden::check`], and the same again ignoring ASCII case: for paths the policy names.
    pub fn check_folded(&self, what: &str, path: &Path) -> Result<()> {
        let p = normalize(path);
        self.refuse(
            what,
            &p,
            self.rules
                .iter()
                .find(|r| r.covers(&p) || r.covers_folded(&p)),
        )
    }

    fn refuse(&self, what: &str, p: &Path, hit: Option<&Rule>) -> Result<()> {
        match hit {
            Some(r) => Err(SandboxError::Hidden {
                what: what.to_owned(),
                path: show(p),
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

/// Where a symlink created in the view points, as an absolute path.
fn symlink_target(link: &Path, target: &str) -> PathBuf {
    if target.starts_with('/') {
        normalize(Path::new(target))
    } else {
        normalize(&link.parent().unwrap_or(Path::new("/")).join(target))
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

    /// Everything that can be checked without a run: the version, the path forms, `system.ro`
    /// against [`SYSTEM_RO_ROOTS`], every path against the hidden list (exactly and ignoring ASCII
    /// case, with the stand-in home `~`: all home paths are written `~/`), symlink targets, the lane
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
        for spec in &self.system.ro {
            let p = system_path("the system path", spec)?;
            if !SYSTEM_RO_ROOTS.contains(&show(&p).as_str()) {
                return Err(bad(
                    "the system path",
                    spec,
                    &format!("is not one of {SYSTEM_RO_ROOTS:?}"),
                ));
            }
            base.check_folded("the system path", &p)?;
        }
        for spec in &self.system.net_ro {
            base.check_folded("the network file", &system_path("the network file", spec)?)?;
        }
        for s in &self.system.symlinks {
            let link = system_path("the symlink", &s.link)?;
            base.check_folded("the symlink", &link)?;
            if s.target.is_empty() || s.target.contains('\0') {
                return Err(bad(
                    "the symlink target",
                    &s.target,
                    "is empty or has a NUL",
                ));
            }
            let target = symlink_target(&link, &s.target);
            if target.starts_with("/home") || target.starts_with("/root") {
                return Err(bad(
                    "the symlink target",
                    &s.target,
                    "points under /home or /root",
                ));
            }
            base.check_folded("the symlink target", &target)?;
        }
        for spec in &self.system.path {
            base.check_folded("the PATH entry", &path_entry(spec, home)?)?;
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
        let shims = home_path("the shim directory", &self.home.shims, home)?;
        roots.push(("the shim directory".into(), shims.clone()));
        for (what, p) in &roots {
            base.check_folded(what, p)?;
        }
        base.check_folded("the empty file", &shims.join(EMPTY_FILE))?;
        let parent = template_parent(&self.default.home, home)?;
        base.check_folded("the default lane home", &parent.join("lane"))?;
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
            base.check_folded(&what, &p)?;
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

/// Names the fence sets itself (a caller may not pass them; [`audit`] checks their values).
const FENCE_ENV: &[&str] = &[
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
    "LUCINA_LEAD",
    "LUCINA_RUN_ID",
    "LUCINA_CHANGE",
];

/// Refused from a caller by exact name: agents, buses, another git directory, or a program that
/// runs code from a path or an option (`BASH_ENV`, `RUSTC_WRAPPER`, `NODE_OPTIONS`, ...).
const REFUSED_ENV: &[&str] = &[
    "HOME",
    "PATH",
    "TMPDIR",
    "GIT_TERMINAL_PROMPT",
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "GPG_AGENT_INFO",
    "DBUS_SESSION_BUS_ADDRESS",
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
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    "BASH_ENV",
    "ENV",
    "CDPATH",
    "PROMPT_COMMAND",
    "SHELLOPTS",
    "BASHOPTS",
    "GCONV_PATH",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTDOC",
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
    "NODE_OPTIONS",
    "NODE_PATH",
    "PERL5OPT",
    "PERL5LIB",
    "RUBYOPT",
    "RUBYLIB",
];

/// Refused from a caller by prefix: every git config variable (the fence sets the three it needs),
/// the loader, Python, cargo (the fence sets the one it owns), XDG directories and Lucina's own.
const REFUSED_ENV_PREFIXES: &[&str] = &[
    "GIT_CONFIG",
    "LD_",
    "PYTHON",
    "CARGO_",
    "XDG_",
    "LUCINA_",
    "BASH_FUNC_",
];

fn env_err(name: &str, why: &str) -> SandboxError {
    SandboxError::Env {
        name: name.to_owned(),
        why: why.to_owned(),
    }
}

/// Every absolute path in a value (the value itself, or each element of a `:` list) must not be
/// hidden.
fn check_env_value(hidden: &Hidden, name: &str, value: &str) -> Result<()> {
    if value.contains('\0') {
        return Err(env_err(name, "its value contains a NUL byte"));
    }
    for part in value.split(':') {
        if part.starts_with('/') {
            hidden
                .check(&format!("the value of {name}"), Path::new(part))
                .map_err(|e| env_err(name, &e.to_string()))?;
        }
    }
    Ok(())
}

/// The rule for one variable a caller passes: a plain name, not refused by name or prefix, and no
/// hidden path in its value.
pub fn check_env_pair(hidden: &Hidden, name: &str, value: &str) -> Result<()> {
    let mut chars = name.chars();
    let ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        return Err(env_err(
            name,
            "not a variable name ([A-Za-z_][A-Za-z0-9_]*)",
        ));
    }
    if REFUSED_ENV.contains(&name) {
        return Err(env_err(
            name,
            "is set by the fence, or points at an agent, a bus, another git directory or code to run",
        ));
    }
    if let Some(p) = REFUSED_ENV_PREFIXES.iter().find(|p| name.starts_with(*p)) {
        return Err(env_err(
            name,
            &format!("names starting with {p} are refused"),
        ));
    }
    check_env_value(hidden, name, value)
}

fn valid_id(s: &str) -> bool {
    (1..=32).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

// ---------------------------------------------------------------------------------------------
// The plan.

/// What a resolver reports about a host path: its real path (symlinks resolved), kind and length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub path: PathBuf,
    pub kind: FileKind,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    /// A socket, a device, a fifo.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
    Any,
    Dir,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Rw,
    Ro,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    /// Options with no host path.
    Args(Vec<String>),
    /// A bind of `path` at itself. `optional`: a missing source is left out.
    Bind {
        mode: Mode,
        path: PathBuf,
        optional: bool,
        need: Need,
    },
    /// `path` read-only whether or not it exists: itself when it is a file, else the empty file.
    Lock { path: PathBuf },
    /// `path` must not exist.
    Absent { path: PathBuf, why: &'static str },
}

/// A checked fence for one run. Its arguments come out only through [`Plan::resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    ops: Vec<Op>,
    hidden: Hidden,
    empty_file: PathBuf,
}

/// A resolver: the real path, kind and length of a host path, or `NotFound`.
pub type Resolver<'a> = &'a dyn Fn(&Path) -> io::Result<Resolved>;

impl Plan {
    fn args(&mut self, a: &[&str]) {
        self.ops
            .push(Op::Args(a.iter().map(|s| (*s).to_owned()).collect()));
    }

    fn bind(&mut self, mode: Mode, path: &Path, optional: bool, need: Need) {
        self.ops.push(Op::Bind {
            mode,
            path: path.to_path_buf(),
            optional,
            need,
        });
    }

    /// Resolves every bind source and returns the bubblewrap argument list, ending with `--` (the
    /// program and its arguments follow). A source must resolve to exactly the path that was
    /// checked (no symlink anywhere on it), must not resolve into a hidden path, must be of the
    /// kind the bind expects, and may be missing only where the bind is optional (then it is left
    /// out of the list, so it cannot appear unchecked later).
    pub fn resolve(&self, resolver: Resolver<'_>) -> Result<Vec<String>> {
        let mut a: Vec<String> = Vec::new();
        let rerr = |p: &Path, why: String| SandboxError::Resolve { path: show(p), why };
        let look = |p: &Path, need: Need| -> Result<Option<Resolved>> {
            match resolver(p) {
                Ok(r) => {
                    if r.path != p {
                        return Err(rerr(
                            p,
                            format!(
                                "resolves to {} (a symlink on the path; the fence binds real paths only)",
                                show(&r.path)
                            ),
                        ));
                    }
                    self.hidden.check("the resolved bind source", &r.path)?;
                    let ok = match need {
                        Need::Any => r.kind != FileKind::Other,
                        Need::Dir => r.kind == FileKind::Dir,
                        Need::File => r.kind == FileKind::File,
                    };
                    if !ok {
                        return Err(rerr(
                            p,
                            format!("is a {:?}, the bind needs a {need:?}", r.kind),
                        ));
                    }
                    Ok(Some(r))
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(rerr(p, e.to_string())),
            }
        };
        for op in &self.ops {
            match op {
                Op::Args(v) => a.extend(v.iter().cloned()),
                Op::Bind {
                    mode,
                    path,
                    optional,
                    need,
                } => match look(path, *need)? {
                    Some(_) => {
                        let flag = if *mode == Mode::Rw {
                            "--bind"
                        } else {
                            "--ro-bind"
                        };
                        a.extend([flag.to_owned(), show(path), show(path)]);
                    }
                    None if *optional => {}
                    None => return Err(rerr(path, "does not exist".to_owned())),
                },
                Op::Lock { path } => match look(path, Need::File)? {
                    Some(_) => a.extend(["--ro-bind".to_owned(), show(path), show(path)]),
                    None => {
                        let empty = look(&self.empty_file, Need::File)?.ok_or_else(|| {
                            rerr(
                                &self.empty_file,
                                "does not exist (an empty file in the shim directory)".to_owned(),
                            )
                        })?;
                        if empty.len != 0 {
                            return Err(rerr(&self.empty_file, "is not empty".to_owned()));
                        }
                        a.extend(["--ro-bind".to_owned(), show(&self.empty_file), show(path)]);
                    }
                },
                Op::Absent { path, why } => match resolver(path) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Ok(_) => return Err(rerr(path, (*why).to_owned())),
                    Err(e) => return Err(rerr(path, e.to_string())),
                },
            }
        }
        audit(&self.hidden, &a)?;
        Ok(a)
    }
}

/// Checks a policy and one run and returns the [`Plan`]. Refuses, naming the path, any policy or
/// input that would expose a hidden path, and a plain repository (a workspace whose `.git` is a
/// directory: its top must be writable for a commit, so a planted `commondir` there would be
/// followed by git run later outside the fence). See `docs/design/agents-sandbox.md` for the view.
pub fn plan(policy: &Policy, run: &RunInputs) -> Result<Plan> {
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
    let tmpdir = input("the run's temp directory", &run.tmpdir)?;
    let mut extra = Vec::new();
    for p in &run.extra_ro {
        extra.push(input("the extra read-only path", p)?);
    }

    // Git: a worktree, or none at all.
    let git_err = |p: &Path, why: &str| SandboxError::Git {
        path: show(p),
        why: why.to_owned(),
    };
    let git = match &run.git {
        None => None,
        Some(g) => {
            let common = input("the common git directory", &g.common_dir)?;
            let own = input("the worktree's git directory", &g.own_dir)?;
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
            let name_ok = own.parent() == Some(common.join("worktrees").as_path())
                && matches!(own.file_name(), Some(n) if !n.is_empty());
            if !name_ok {
                return Err(git_err(
                    &own,
                    "a worktree's git directory is <common>/worktrees/<name>",
                ));
            }
            Some((common, own))
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
    for (what, p) in toolchains
        .iter()
        .map(|p| ("the toolchain", p))
        .chain(caches.iter().map(|p| ("the cache", p)))
        .chain([
            ("the check target", &check_target),
            ("the shim directory", &shims),
            ("the lane home", &lane_home),
        ])
    {
        hidden.check_folded(what, p)?;
    }
    let mut system = Vec::new();
    for spec in &policy.system.ro {
        system.push(system_path("the system path", spec)?);
    }
    let mut net_ro = Vec::new();
    for spec in &policy.system.net_ro {
        net_ro.push(system_path("the network file", spec)?);
    }

    // No two bound roots overlap.
    let mut roots: Vec<(String, PathBuf)> = vec![
        ("the workspace".into(), workspace.clone()),
        ("the run's temp directory".into(), tmpdir.clone()),
        ("the lane home".into(), lane_home.clone()),
        ("the shim directory".into(), shims.clone()),
    ];
    if let Some((common, _)) = &git {
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

    for (i, (name, value)) in run.env.iter().enumerate() {
        check_env_pair(&hidden, name, value)?;
        if run.env[..i].iter().any(|(n, _)| n == name) {
            return Err(env_err(name, "passed twice"));
        }
    }
    let mut ids = Vec::new();
    for (name, v) in [
        ("LUCINA_LEAD", &run.lead),
        ("LUCINA_RUN_ID", &run.run_id),
        ("LUCINA_CHANGE", &run.change),
    ] {
        if let Some(v) = v {
            if !valid_id(v) {
                return Err(env_err(name, "must match [A-Za-z0-9-]{1,32}"));
            }
            ids.push((name, v.clone()));
        }
    }
    let mut path = vec![show(&shims)];
    for spec in &policy.system.path {
        let p = path_entry(spec, &home)?;
        hidden.check_folded("the PATH entry", &p)?;
        path.push(show(&p));
    }

    // The plan. Order matters: bubblewrap mounts in order, a later mount covers an earlier one.
    let mut pl = Plan {
        ops: Vec::new(),
        hidden,
        empty_file: shims.join(EMPTY_FILE),
    };
    pl.args(&[
        "--die-with-parent",
        "--new-session",
        "--unshare-user",
        "--disable-userns",
        "--unshare-ipc",
        "--unshare-pid",
        "--unshare-uts",
        "--unshare-cgroup",
    ]);
    if network == Network::None {
        pl.args(&["--unshare-net"]);
    }
    pl.args(&["--cap-drop", "ALL", "--clearenv"]);
    for p in &system {
        pl.bind(Mode::Ro, p, true, Need::Dir);
    }
    for s in &policy.system.symlinks {
        let link = system_path("the symlink", &s.link)?;
        pl.ops.push(Op::Args(vec![
            "--symlink".to_owned(),
            s.target.clone(),
            show(&link),
        ]));
    }
    if network == Network::Host {
        for p in &net_ro {
            pl.bind(Mode::Ro, p, true, Need::File);
        }
    }
    pl.args(&["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"]);
    // --perms applies to the next --tmpfs (bubblewrap 0.11.1; the host test checks the mode).
    pl.args(&["--perms", "0700", "--tmpfs", &show(&home)]);
    for p in &toolchains {
        pl.bind(Mode::Ro, p, true, Need::Dir);
    }
    for p in &caches {
        pl.bind(Mode::Rw, p, true, Need::Dir);
    }
    if build {
        pl.bind(Mode::Rw, &check_target, false, Need::Dir);
    }
    pl.bind(Mode::Rw, &lane_home, true, Need::Dir);
    pl.bind(Mode::Rw, &tmpdir, false, Need::Dir);
    for p in &extra {
        pl.bind(Mode::Ro, p, false, Need::Any);
    }
    pl.bind(Mode::Ro, &shims, false, Need::Dir);
    pl.bind(Mode::Rw, &workspace, false, Need::Dir);
    match &git {
        None => pl.ops.push(Op::Absent {
            path: workspace.join(".git"),
            why: "exists: a plain repository is refused (its .git top must be writable, so a planted \
                  commondir would be followed by git outside the fence); run in a worktree",
        }),
        Some((common, own)) => {
            // The common directory read-only; only what a commit writes is writable.
            pl.bind(Mode::Ro, common, false, Need::Dir);
            pl.bind(Mode::Rw, &common.join("objects"), false, Need::Dir);
            // objects/info holds alternates and commit-graph: read-only.
            pl.bind(Mode::Ro, &common.join("objects/info"), false, Need::Dir);
            pl.bind(Mode::Rw, &common.join("refs"), false, Need::Dir);
            pl.bind(Mode::Rw, &common.join("logs"), true, Need::Dir);
            pl.bind(Mode::Rw, &common.join("packed-refs"), true, Need::File);
            pl.bind(Mode::Rw, &common.join("reftable"), true, Need::Dir);
            pl.bind(Mode::Rw, own, false, Need::Dir);
            // After the common directory: no hook, no remote URL, no attribute or submodule config.
            pl.bind(Mode::Ro, &common.join("hooks"), false, Need::Dir);
            pl.bind(Mode::Ro, &common.join("config"), false, Need::File);
            pl.bind(Mode::Ro, &common.join("info"), false, Need::Dir);
            // The common directory is read-only: a missing modules cannot be created.
            pl.bind(Mode::Ro, &common.join("modules"), true, Need::Dir);
            // No redirect of this worktree, from inside or outside.
            pl.bind(Mode::Ro, &own.join("commondir"), false, Need::File);
            pl.bind(Mode::Ro, &own.join("gitdir"), false, Need::File);
            pl.ops.push(Op::Lock {
                path: own.join("config.worktree"),
            });
            pl.bind(Mode::Ro, &workspace.join(".git"), false, Need::File);
        }
    }
    let mut env = vec![
        ("HOME".to_owned(), show(&home)),
        ("PATH".to_owned(), path.join(":")),
        ("TMPDIR".to_owned(), show(&tmpdir)),
        ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
        ("GIT_CONFIG_SYSTEM".to_owned(), "/dev/null".to_owned()),
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
        ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
        ("LUCINA_SANDBOX".to_owned(), "bwrap".to_owned()),
        ("LUCINA_LANE_HOME".to_owned(), show(&lane_home)),
    ];
    if build {
        env.push(("CARGO_TARGET_DIR".to_owned(), show(&check_target)));
    }
    env.extend(ids.into_iter().map(|(n, v)| (n.to_owned(), v)));
    env.extend(run.env.iter().cloned());
    let mut tail = vec!["--chdir".to_owned(), show(&workspace)];
    for (n, v) in env {
        tail.extend(["--setenv".to_owned(), n, v]);
    }
    tail.push("--".to_owned());
    pl.ops.push(Op::Args(tail));
    Ok(pl)
}

/// The `-c` options agentd must pass whenever it runs git on a worker's tree outside the fence
/// (review, merge, status). A worker controls the work tree: `.gitmodules`, `.gitattributes`, a
/// nested `.git` or an embedded bare repository there must not make that git run or follow
/// anything. Each pair is `key=value` for `git -c`:
/// - `core.hooksPath=/dev/null`: no hook runs, wherever the repository says hooks are;
/// - `core.fsmonitor=false`: no fsmonitor command runs;
/// - `submodule.recurse=false`, `diff.ignoreSubmodules=all`, `status.submoduleSummary=false`: no git
///   runs inside a submodule (whose config and hooks the worker may have written);
/// - `safe.bareRepository=explicit`: a bare repository planted in the tree is not discovered.
///
/// `-c` options reach child git processes through `GIT_CONFIG_PARAMETERS`, so they hold for the
/// submodule and hook machinery too.
pub fn outside_git_options() -> &'static [&'static str] {
    &[
        "core.hooksPath=/dev/null",
        "core.fsmonitor=false",
        "submodule.recurse=false",
        "diff.ignoreSubmodules=all",
        "status.submoduleSummary=false",
        "safe.bareRepository=explicit",
    ]
}

// ---------------------------------------------------------------------------------------------
// The audit.

/// The arity of each option the generator emits; anything else is refused by [`audit`].
fn arity(flag: &str) -> Option<usize> {
    Some(match flag {
        "--die-with-parent" | "--new-session" | "--unshare-user" | "--disable-userns"
        | "--unshare-ipc" | "--unshare-pid" | "--unshare-uts" | "--unshare-cgroup"
        | "--unshare-net" | "--clearenv" => 0,
        "--cap-drop" | "--proc" | "--dev" | "--tmpfs" | "--chdir" | "--perms" => 1,
        "--bind" | "--ro-bind" | "--symlink" | "--setenv" => 2,
        _ => return None,
    })
}

fn is_rw_bind(flag: &str) -> bool {
    flag == "--bind"
}

fn is_ro_bind(flag: &str) -> bool {
    flag == "--ro-bind"
}

/// The fence's own environment values ([`FENCE_ENV`]): fixed, the home, a run id, or a path.
fn audit_fence_env(hidden: &Hidden, name: &str, value: &str) -> Result<()> {
    let fixed = match name {
        "GIT_CONFIG_GLOBAL" | "GIT_CONFIG_SYSTEM" => Some("/dev/null"),
        "GIT_CONFIG_NOSYSTEM" => Some("1"),
        "GIT_TERMINAL_PROMPT" => Some("0"),
        "LUCINA_SANDBOX" => Some("bwrap"),
        _ => None,
    };
    if let Some(f) = fixed {
        return if value == f {
            Ok(())
        } else {
            Err(env_err(name, &format!("must be {f}")))
        };
    }
    match name {
        "HOME" if Path::new(value) == hidden.home => Ok(()),
        "HOME" => Err(env_err(name, "must be the run's home")),
        "LUCINA_LEAD" | "LUCINA_RUN_ID" | "LUCINA_CHANGE" if valid_id(value) => Ok(()),
        "LUCINA_LEAD" | "LUCINA_RUN_ID" | "LUCINA_CHANGE" => {
            Err(env_err(name, "must match [A-Za-z0-9-]{1,32}"))
        }
        _ => check_env_value(hidden, name, value),
    }
}

/// The last line of defence: walks a resolved argument list and refuses
/// - an option the generator does not emit (a try bind, a device bind, a bind from a file descriptor),
///   naming the option and its position in the list;
/// - a bind whose source or target is hidden, or a read-write bind whose source is not its target;
/// - a read-write bind after a read-only bind of the same path or of a path inside it (it would
///   undo a lock: `<common>/hooks`, `config`, `info`, `objects/info`, `commondir`, ...);
/// - a `--tmpfs` that is not `/tmp` or the run home, or that comes after a bind at or under its path;
///   a `--chdir` that is not a read-write bind target;
/// - a symlink pointing into a hidden path; `--perms` other than `0700`;
/// - `--clearenv` after a `--setenv`, a `--setenv` before `--clearenv`, a variable set twice, a
///   variable a caller may not pass, a fence variable with a wrong value, a hidden path in a value;
/// - a list without the namespaces, or one that does not end with `--`.
pub fn audit(hidden: &Hidden, args: &[String]) -> Result<()> {
    let mut i = 0;
    let mut seen: Vec<&str> = Vec::new();
    let mut ro_targets: Vec<PathBuf> = Vec::new();
    let mut bind_targets: Vec<PathBuf> = Vec::new();
    let mut rw_targets: Vec<PathBuf> = Vec::new();
    let mut env_names: Vec<&str> = Vec::new();
    let mut ended = false;
    let refuse = |m: String| Err(SandboxError::Args(m));
    while i < args.len() {
        let f = args[i].as_str();
        if f == "--" {
            if i + 1 != args.len() {
                return refuse("`--` must end the list".to_owned());
            }
            ended = true;
            break;
        }
        let Some(n) = arity(f) else {
            return refuse(format!(
                "{f} at position {i} is not an option this generator emits"
            ));
        };
        if i + n >= args.len() {
            return refuse(format!("{f} is missing its arguments"));
        }
        if is_rw_bind(f) || is_ro_bind(f) {
            let (src, dst) = (&args[i + 1], &args[i + 2]);
            if !src.starts_with('/') || !dst.starts_with('/') {
                return refuse(format!("{f} {src} {dst}: paths must be absolute"));
            }
            hidden.check("the bind source", Path::new(src))?;
            hidden.check("the bind target", Path::new(dst))?;
            let dst = normalize(Path::new(dst));
            if is_rw_bind(f) {
                if normalize(Path::new(src)) != dst {
                    return refuse(format!(
                        "{f} {src} {}: a read-write bind is at its own path",
                        show(&dst)
                    ));
                }
                if let Some(locked) = ro_targets.iter().find(|ro| ro.starts_with(&dst)) {
                    return refuse(format!(
                        "{f} {} after the read-only bind of {}: it would undo the lock",
                        show(&dst),
                        show(locked)
                    ));
                }
            } else {
                ro_targets.push(dst.clone());
            }
            if is_rw_bind(f) {
                rw_targets.push(dst.clone());
            }
            bind_targets.push(dst);
        }
        match f {
            "--cap-drop" if args[i + 1] != "ALL" => {
                return refuse("--cap-drop must be ALL".to_owned())
            }
            "--proc" if args[i + 1] != "/proc" => return refuse("--proc must be /proc".to_owned()),
            "--dev" if args[i + 1] != "/dev" => return refuse("--dev must be /dev".to_owned()),
            "--perms" if args[i + 1] != "0700" => return refuse("--perms must be 0700".to_owned()),
            "--tmpfs" => {
                let p = normalize(Path::new(&args[i + 1]));
                if !args[i + 1].starts_with('/') || (p != Path::new("/tmp") && p != hidden.home) {
                    return refuse(format!(
                        "--tmpfs at position {i}: {} is not /tmp or the run's home",
                        args[i + 1]
                    ));
                }
                if let Some(b) = bind_targets.iter().find(|b| b.starts_with(&p)) {
                    return refuse(format!(
                        "--tmpfs at position {i}: {} would hide the earlier bind of {}",
                        show(&p),
                        show(b)
                    ));
                }
            }
            "--chdir" => {
                let p = normalize(Path::new(&args[i + 1]));
                if !args[i + 1].starts_with('/') || !rw_targets.contains(&p) {
                    return refuse(format!(
                        "--chdir at position {i}: {} is not a path the list binds read-write",
                        args[i + 1]
                    ));
                }
            }
            "--symlink" => {
                let link = normalize(Path::new(&args[i + 2]));
                hidden.check("the symlink", &link)?;
                hidden.check("the symlink target", &symlink_target(&link, &args[i + 1]))?;
            }
            "--clearenv" if seen.contains(&"--setenv") => {
                return refuse("--clearenv after a --setenv would drop it".to_owned());
            }
            "--setenv" => {
                if !seen.contains(&"--clearenv") {
                    return refuse(
                        "--setenv before --clearenv: the environment would not be cleared"
                            .to_owned(),
                    );
                }
                let (name, value) = (args[i + 1].as_str(), args[i + 2].as_str());
                if env_names.contains(&name) {
                    return Err(env_err(name, "set twice"));
                }
                env_names.push(name);
                if FENCE_ENV.contains(&name) {
                    audit_fence_env(hidden, name, value)?;
                } else {
                    check_env_pair(hidden, name, value)?;
                }
            }
            _ => {}
        }
        seen.push(f);
        i += n + 1;
    }
    if !ended {
        return refuse("the list must end with `--`".to_owned());
    }
    for required in [
        "--die-with-parent",
        "--new-session",
        "--unshare-user",
        "--disable-userns",
        "--unshare-ipc",
        "--unshare-pid",
        "--unshare-uts",
        "--unshare-cgroup",
        "--cap-drop",
        "--clearenv",
        "--proc",
        "--dev",
    ] {
        if !seen.contains(&required) {
            return refuse(format!("{required} is missing"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
