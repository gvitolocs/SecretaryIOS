//! Unit tests of the sandbox policy, plan and audit. No IO: the repository's policy file is compiled
//! in with `include_str!`, and bind sources are resolved by [`Fake`], a file system in a table.

use std::collections::{BTreeMap, BTreeSet};

use super::*;

const POLICY: &str = include_str!("../../../../deploy/lucina-agentd/sandbox-policy.toml");
const HOME: &str = "/home/nez";
const WS: &str = "/home/nez/Projects/.worktrees/SecretaryIOS/lagent-x";
const COMMON: &str = "/home/nez/Projects/Lucina/SecretaryIOS/.git";
const OWN: &str = "/home/nez/Projects/Lucina/SecretaryIOS/.git/worktrees/lagent-x";
const SHIMS: &str = "/home/nez/.local/lib/lucina-shims";
const READER_WS: &str = "/home/nez/Projects/Work/notes";
const QODER_HOME_LINE: &str = "home = \"~/.local/share/lane-homes/qoder\"";

/// A file system in a table: every path is a directory unless it is listed as a file or a socket,
/// is missing (or lies under a missing path), or goes through a symlink.
#[derive(Default, Clone)]
struct Fake {
    files: BTreeSet<PathBuf>,
    sockets: BTreeSet<PathBuf>,
    missing: BTreeSet<PathBuf>,
    links: BTreeMap<PathBuf, PathBuf>,
    dangling: BTreeSet<PathBuf>,
    nonempty: BTreeSet<PathBuf>,
}

impl Fake {
    /// The worktree fixture: a worktree whose `config.worktree` and `reftable` do not exist, a
    /// reader workspace without `.git`, the shim directory with its empty file.
    fn standard() -> Fake {
        let mut f = Fake::default();
        for p in [
            format!("{WS}/.git"),
            format!("{COMMON}/config"),
            format!("{COMMON}/packed-refs"),
            format!("{OWN}/commondir"),
            format!("{OWN}/gitdir"),
            format!("{SHIMS}/empty"),
            "/run/systemd/resolve/stub-resolv.conf".to_owned(),
            "/run/systemd/resolve/resolv.conf".to_owned(),
        ] {
            f.files.insert(p.into());
        }
        for p in [
            format!("{OWN}/config.worktree"),
            format!("{COMMON}/reftable"),
            format!("{READER_WS}/.git"),
        ] {
            f.missing.insert(p.into());
        }
        f
    }

    fn resolve(&self, p: &Path) -> io::Result<Resolved> {
        if self.dangling.contains(p) {
            return Err(io::Error::other("a dangling symlink"));
        }
        let mut real = p.to_path_buf();
        for (link, target) in &self.links {
            if let Ok(rest) = p.strip_prefix(link) {
                real = target.join(rest);
            }
        }
        if self.missing.iter().any(|m| real.starts_with(m)) {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        let kind = if self.files.contains(&real) {
            FileKind::File
        } else if self.sockets.contains(&real) {
            FileKind::Other
        } else {
            FileKind::Dir
        };
        Ok(Resolved {
            len: u64::from(self.nonempty.contains(&real)),
            path: real,
            kind,
        })
    }
}

fn policy() -> Policy {
    Policy::from_toml(POLICY).expect("the repository's policy file")
}

fn run(lane: &str) -> RunInputs {
    RunInputs {
        home: HOME.into(),
        workspace: WS.into(),
        git: Some(GitWorktree {
            common_dir: COMMON.into(),
            own_dir: OWN.into(),
        }),
        lane: lane.to_owned(),
        cost: CostClass::Light,
        tmpdir: "/home/nez/scratch/tmp/run-1".into(),
        extra_ro: Vec::new(),
        env: vec![
            ("GIT_AUTHOR_NAME".into(), "Worker".into()),
            ("GIT_AUTHOR_EMAIL".into(), "w@example.invalid".into()),
        ],
        lead: Some("LA".into()),
        run_id: Some("LA-R038".into()),
        change: Some("LA-C013".into()),
    }
}

fn reader_run(lane: &str) -> RunInputs {
    RunInputs {
        workspace: READER_WS.into(),
        git: None,
        ..run(lane)
    }
}

fn resolve_with(fs: &Fake, p: &Policy, r: &RunInputs) -> Result<Vec<String>> {
    let resolver = |path: &Path| fs.resolve(path);
    plan(p, r)?.resolve(&resolver)
}

fn args(r: &RunInputs) -> Vec<String> {
    resolve_with(&Fake::standard(), &policy(), r).expect("a valid run")
}

/// Index of the triple `flag src dst`.
fn bind_from(a: &[String], flag: &str, src: &str, dst: &str) -> Option<usize> {
    a.windows(3)
        .position(|w| w[0] == flag && w[1] == src && w[2] == dst)
}

/// Index of the triple `flag p p` (a bind of `p` at its own path).
fn bind_at(a: &[String], flag: &str, p: &str) -> Option<usize> {
    bind_from(a, flag, p, p)
}

fn any_bind_of(a: &[String], p: &str) -> bool {
    a.windows(3)
        .any(|w| w[0].contains("bind") && (w[1] == p || w[2] == p))
}

fn env_of<'a>(a: &'a [String], name: &str) -> Option<&'a str> {
    a.windows(3)
        .find(|w| w[0] == "--setenv" && w[1] == name)
        .map(|w| w[2].as_str())
}

fn joined(a: &[String]) -> String {
    a.join(" ")
}

// ------------------------------------------------------------------------------------------------
// The repository's policy file.

#[test]
fn the_repository_policy_parses_and_validates() {
    let p = policy();
    for key in [
        "qoder",
        "zen-*",
        "mistral-*",
        "zcode",
        "qwen",
        "cursor-*",
        "claude-*",
    ] {
        assert!(p.lanes.contains_key(key), "lane {key} missing");
    }
    assert_eq!(p.default.network, Network::None);
    assert!(p.default.home.ends_with("/{lane}"));
    assert_eq!(p.system.ro, ["/usr", "/etc"]);
    assert!(
        p.system.net_ro.iter().all(|f| f.ends_with(".conf")),
        "the resolver's files, not its directory"
    );
}

#[test]
fn lane_names_resolve_to_exact_then_longest_prefix_then_default() {
    let p = policy();
    assert_eq!(p.lane_entry("qoder").unwrap().0.as_deref(), Some("qoder"));
    assert_eq!(
        p.lane_entry("zen-glm-flash").unwrap().0.as_deref(),
        Some("zen-*")
    );
    assert_eq!(
        p.lane_entry("claude-sonnet").unwrap().0.as_deref(),
        Some("claude-*")
    );
    assert_eq!(
        p.lane_entry("zen-").unwrap().0,
        None,
        "a pattern needs a rest"
    );
    assert_eq!(p.lane_entry("brandnew").unwrap(), (None, Network::None));
    for bad in ["", "Qoder", "../x", ".x", "a/b", "a b", &"x".repeat(65)] {
        assert!(p.lane_entry(bad).is_err(), "{bad:?}");
    }
}

// ------------------------------------------------------------------------------------------------
// The argument list for a typical run of each lane.

#[test]
fn a_typical_run_of_each_lane() {
    let homes = |lane: &str| format!("{HOME}/.local/share/lane-homes/{lane}");
    for (lane, own_home, net) in [
        ("qoder", homes("qoder"), Network::Host),
        ("zen-deepseek-flash", homes("zen"), Network::Host),
        ("mistral-vibe", homes("mistral"), Network::Host),
        ("zcode", homes("zcode"), Network::Host),
        ("qwen", homes("qwen"), Network::Host),
        ("cursor-agent", homes("cursor"), Network::Host),
        ("claude-sonnet", homes("claude"), Network::Host),
        ("brandnew", homes("brandnew"), Network::None),
    ] {
        let a = args(&run(lane));
        let s = joined(&a);
        assert_eq!(a.last().map(String::as_str), Some("--"), "{lane}");
        assert!(bind_at(&a, "--bind", &own_home).is_some(), "{lane}: {s}");
        assert_eq!(env_of(&a, "LUCINA_LANE_HOME"), Some(own_home.as_str()));
        for other in [
            "qoder", "zen", "mistral", "zcode", "qwen", "cursor", "claude",
        ] {
            let o = homes(other);
            if o != own_home {
                assert!(!s.contains(&o), "{lane} sees {o}");
            }
        }
        assert_eq!(
            a.contains(&"--unshare-net".to_owned()),
            net == Network::None,
            "{lane}"
        );
        assert!(!s.contains("--dev-bind"), "{lane}: no device nodes");
        assert!(
            !s.contains("-try"),
            "{lane}: a resolved list has no -try bind"
        );
        for f in [
            "--die-with-parent",
            "--new-session",
            "--unshare-user",
            "--disable-userns",
            "--unshare-ipc",
            "--unshare-pid",
            "--unshare-uts",
            "--unshare-cgroup",
            "--clearenv",
        ] {
            assert!(a.contains(&f.to_owned()), "{lane}: {f}");
        }
        assert!(s.contains("--cap-drop ALL"));
        assert!(
            s.contains("--proc /proc") && s.contains("--dev /dev") && s.contains("--tmpfs /tmp")
        );
        assert!(s.contains(&format!("--perms 0700 --tmpfs {HOME}")));
        assert!(bind_at(&a, "--ro-bind", "/usr").is_some());
        assert!(bind_at(&a, "--ro-bind", "/etc").is_some());
        assert!(bind_at(&a, "--ro-bind", "/home/nez/.rustup").is_some());
        assert!(bind_at(&a, "--ro-bind", "/home/nez/.cargo/bin").is_some());
        assert!(bind_at(&a, "--bind", "/home/nez/.cargo/registry").is_some());
        assert!(bind_at(&a, "--bind", "/home/nez/.cargo/git").is_some());
        assert!(bind_at(&a, "--bind", "/home/nez/.cache/bq").is_some());
        assert!(bind_at(&a, "--ro-bind", SHIMS).is_some());
        assert!(bind_at(&a, "--bind", "/home/nez/scratch/tmp/run-1").is_some());
        assert!(bind_at(&a, "--bind", WS).is_some());
        assert_eq!(env_of(&a, "HOME"), Some(HOME));
        assert_eq!(env_of(&a, "TMPDIR"), Some("/home/nez/scratch/tmp/run-1"));
        assert!(env_of(&a, "PATH")
            .unwrap()
            .starts_with(&format!("{SHIMS}:")));
        assert_eq!(env_of(&a, "GIT_CONFIG_GLOBAL"), Some("/dev/null"));
        assert_eq!(env_of(&a, "GIT_CONFIG_SYSTEM"), Some("/dev/null"));
        assert_eq!(env_of(&a, "GIT_AUTHOR_NAME"), Some("Worker"));
        assert_eq!(env_of(&a, "LUCINA_LEAD"), Some("LA"));
        assert_eq!(env_of(&a, "LUCINA_RUN_ID"), Some("LA-R038"));
        assert_eq!(env_of(&a, "LUCINA_CHANGE"), Some("LA-C013"));
        assert!(s.contains(&format!("--chdir {WS}")));
        let tmpfs_home = a
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == HOME)
            .unwrap();
        assert!(tmpfs_home < bind_at(&a, "--ro-bind", "/home/nez/.rustup").unwrap());
        for never in [
            HOME,
            "/home/nez/.ssh",
            "/home/nez/.config",
            "/home/nez/.cargo",
            "/home/nez/.claude",
        ] {
            assert!(!any_bind_of(&a, never), "{lane}: {never}");
        }
        // Name resolution only with the host network, and only the two files.
        assert_eq!(
            bind_at(&a, "--ro-bind", "/run/systemd/resolve/stub-resolv.conf").is_some(),
            net == Network::Host
        );
        assert!(!any_bind_of(&a, "/run/systemd/resolve"));
    }
}

#[test]
fn network_none_versus_host() {
    let host = args(&run("qoder"));
    let none = args(&run("brandnew"));
    assert!(!host.contains(&"--unshare-net".to_owned()));
    assert!(none.contains(&"--unshare-net".to_owned()));
    let text = POLICY.replace(
        "[lanes.qoder]\nhome = \"~/.local/share/lane-homes/qoder\"\nnetwork = \"host\"",
        "[lanes.qoder]\nhome = \"~/.local/share/lane-homes/qoder\"\nnetwork = \"none\"",
    );
    assert_ne!(text, POLICY);
    let a = resolve_with(
        &Fake::standard(),
        &Policy::from_toml(&text).unwrap(),
        &run("qoder"),
    )
    .unwrap();
    assert!(a.contains(&"--unshare-net".to_owned()));
    assert!(!joined(&a).contains("/run/systemd"));
}

#[test]
fn light_versus_build() {
    let ct = "/home/nez/.cache/lucina-check-target";
    let light = args(&run("qoder"));
    assert!(
        !joined(&light).contains(ct),
        "a light run has no check target"
    );
    assert_eq!(env_of(&light, "CARGO_TARGET_DIR"), None);
    let build = args(&RunInputs {
        cost: CostClass::Build,
        ..run("qoder")
    });
    assert!(bind_at(&build, "--bind", ct).is_some());
    assert_eq!(env_of(&build, "CARGO_TARGET_DIR"), Some(ct));
}

#[test]
fn a_missing_optional_source_is_left_out_and_a_missing_required_one_refused() {
    let mut fs = Fake::standard();
    fs.missing
        .insert("/home/nez/.local/share/lane-homes/qoder".into());
    fs.missing.insert("/home/nez/.rustup".into());
    let a = resolve_with(&fs, &policy(), &run("qoder")).unwrap();
    assert!(!any_bind_of(&a, "/home/nez/.local/share/lane-homes/qoder"));
    assert!(!any_bind_of(&a, "/home/nez/.rustup"));
    for required in [
        WS.to_owned(),
        SHIMS.to_owned(),
        "/home/nez/scratch/tmp/run-1".to_owned(),
        format!("{COMMON}/hooks"),
        format!("{COMMON}/info"),
        format!("{COMMON}/config"),
        format!("{COMMON}/objects/info"),
        format!("{OWN}/commondir"),
        format!("{OWN}/gitdir"),
        format!("{WS}/.git"),
    ] {
        let mut fs = Fake::standard();
        fs.missing.insert(required.clone().into());
        let e = resolve_with(&fs, &policy(), &run("qoder")).unwrap_err();
        assert!(e.to_string().contains(&required), "{required}: {e}");
    }
}

// ------------------------------------------------------------------------------------------------
// Resolution: symlinks and kinds (Q1, G9).

#[test]
fn a_symlinked_bind_source_is_refused_wherever_it_is() {
    let p = policy();
    for (link, target) in [
        (format!("{COMMON}/hooks"), "/home/nez/.ssh".to_owned()),
        (
            format!("{COMMON}/config"),
            "/home/nez/.gitconfig".to_owned(),
        ),
        (
            format!("{COMMON}/info"),
            "/home/nez/Projects/Other/.git/info".to_owned(),
        ),
        (
            format!("{COMMON}/objects/info"),
            "/srv/elsewhere".to_owned(),
        ),
        (format!("{COMMON}/modules"), "/srv/elsewhere".to_owned()),
        (
            format!("{OWN}/commondir"),
            "/srv/elsewhere/commondir".to_owned(),
        ),
        (format!("{OWN}/gitdir"), "/srv/elsewhere/gitdir".to_owned()),
        (format!("{WS}/.git"), "/srv/elsewhere/gitfile".to_owned()),
        (
            "/home/nez/.cargo/bin".to_owned(),
            "/home/nez/.ssh".to_owned(),
        ),
        ("/home/nez/Projects".to_owned(), "/data/Projects".to_owned()),
        (SHIMS.to_owned(), "/srv/shims".to_owned()),
    ] {
        let mut fs = Fake::standard();
        fs.links.insert(link.clone().into(), target.clone().into());
        if link.ends_with("config")
            || link.ends_with("commondir")
            || link.ends_with("gitdir")
            || link.ends_with(".git")
        {
            fs.files.insert(target.clone().into());
        }
        let e = resolve_with(&fs, &p, &run("qoder")).unwrap_err();
        assert!(
            matches!(
                e,
                SandboxError::Resolve { .. } | SandboxError::Hidden { .. }
            ),
            "{link} -> {target}: {e}"
        );
        assert!(
            e.to_string().contains(&target) || e.to_string().contains(&link),
            "{e}"
        );
    }
    // A config.worktree that is a symlink.
    let mut fs = Fake::standard();
    fs.missing.clear();
    fs.links.insert(
        format!("{OWN}/config.worktree").into(),
        "/home/nez/.gitconfig".into(),
    );
    assert!(resolve_with(&fs, &p, &run("qoder")).is_err());
    // An extra read-only path that is a symlink into a hidden tree.
    let mut fs = Fake::standard();
    fs.links.insert(
        "/home/nez/Projects/Work/sibling".into(),
        "/home/nez/.ssh".into(),
    );
    let mut r = run("qoder");
    r.extra_ro = vec!["/home/nez/Projects/Work/sibling".into()];
    assert!(resolve_with(&fs, &p, &r).is_err());
    // A dangling symlink at an optional source is not "missing".
    let mut fs = Fake::standard();
    fs.dangling
        .insert("/home/nez/.local/share/lane-homes/qoder".into());
    assert!(resolve_with(&fs, &p, &run("qoder")).is_err());
}

#[test]
fn a_source_of_the_wrong_kind_is_refused() {
    let p = policy();
    let mut fs = Fake::standard();
    fs.files
        .remove(Path::new("/run/systemd/resolve/stub-resolv.conf"));
    fs.sockets
        .insert("/run/systemd/resolve/stub-resolv.conf".into());
    assert!(
        resolve_with(&fs, &p, &run("qoder")).is_err(),
        "a socket as a network file"
    );
    let mut fs = Fake::standard();
    fs.files.remove(Path::new(&format!("{COMMON}/config")));
    assert!(
        resolve_with(&fs, &p, &run("qoder")).is_err(),
        "a directory as config"
    );
    let mut fs = Fake::standard();
    fs.files.insert(format!("{COMMON}/hooks").into());
    assert!(
        resolve_with(&fs, &p, &run("qoder")).is_err(),
        "a file as hooks"
    );
}

// ------------------------------------------------------------------------------------------------
// Git.

#[test]
fn the_worktree_case() {
    let a = args(&run("qoder"));
    let common_ro = bind_at(&a, "--ro-bind", COMMON).expect("common dir read-only");
    let objects = bind_at(&a, "--bind", &format!("{COMMON}/objects")).expect("objects");
    assert!(objects > common_ro);
    let objects_info = bind_at(&a, "--ro-bind", &format!("{COMMON}/objects/info"))
        .expect("objects/info read-only");
    assert!(objects_info > objects);
    for rw in ["refs", "logs", "packed-refs"] {
        assert!(
            bind_at(&a, "--bind", &format!("{COMMON}/{rw}")).unwrap() > common_ro,
            "{rw}"
        );
    }
    assert!(
        !any_bind_of(&a, &format!("{COMMON}/reftable")),
        "missing: left out"
    );
    let own = bind_at(&a, "--bind", OWN).expect("own worktree dir writable");
    for ro in ["commondir", "gitdir"] {
        assert!(bind_at(&a, "--ro-bind", &format!("{OWN}/{ro}")).unwrap() > own);
    }
    let ws = bind_at(&a, "--bind", WS).unwrap();
    let gitfile = bind_at(&a, "--ro-bind", &format!("{WS}/.git")).expect("gitfile read-only");
    assert!(gitfile > ws);
    assert!(!joined(&a).contains("/home/nez/Projects/Lucina/SecretaryIOS "));
}

#[test]
fn config_worktree_is_read_only_whether_or_not_it_exists() {
    let cw = format!("{OWN}/config.worktree");
    let empty = format!("{SHIMS}/empty");
    // Missing: the empty file is bound over it.
    let a = args(&run("qoder"));
    let i = bind_from(&a, "--ro-bind", &empty, &cw).expect("empty file over config.worktree");
    assert!(i > bind_at(&a, "--bind", OWN).unwrap());
    // Present: bound read-only at itself.
    let mut fs = Fake::standard();
    fs.missing.remove(Path::new(&cw));
    fs.files.insert(cw.clone().into());
    let a = resolve_with(&fs, &policy(), &run("qoder")).unwrap();
    assert!(bind_at(&a, "--ro-bind", &cw).unwrap() > bind_at(&a, "--bind", OWN).unwrap());
    // The empty file must exist and be empty.
    let mut fs = Fake::standard();
    fs.missing.insert(empty.clone().into());
    assert!(resolve_with(&fs, &policy(), &run("qoder"))
        .unwrap_err()
        .to_string()
        .contains(&empty));
    let mut fs = Fake::standard();
    fs.nonempty.insert(empty.clone().into());
    assert!(resolve_with(&fs, &policy(), &run("qoder")).is_err());
}

#[test]
fn a_plain_repository_is_refused_and_a_workspace_without_git_gets_nothing_git() {
    // Without git: the workspace and nothing git-related.
    let a = args(&reader_run("qoder"));
    assert!(bind_at(&a, "--bind", READER_WS).is_some());
    let s = joined(&a);
    assert!(
        !s.contains(".git") && !s.contains("/hooks") && !s.contains("objects"),
        "{s}"
    );
    assert!(s.contains(&format!("--chdir {READER_WS}")));
    // A plain repository: the workspace has a .git directory.
    let mut fs = Fake::standard();
    fs.missing.remove(Path::new(&format!("{READER_WS}/.git")));
    let e = resolve_with(&fs, &policy(), &reader_run("qoder")).unwrap_err();
    assert!(e.to_string().contains("plain repository"), "{e}");
    // ... or a .git file pointing elsewhere.
    let mut fs = Fake::standard();
    fs.missing.remove(Path::new(&format!("{READER_WS}/.git")));
    fs.files.insert(format!("{READER_WS}/.git").into());
    assert!(resolve_with(&fs, &policy(), &reader_run("qoder")).is_err());
}

#[test]
fn every_git_lock_comes_after_any_read_write_bind_that_holds_it() {
    let a = args(&run("qoder"));
    let locks: Vec<String> = ["hooks", "config", "info", "objects/info"]
        .iter()
        .map(|p| format!("{COMMON}/{p}"))
        .chain(
            ["commondir", "gitdir", "config.worktree"]
                .iter()
                .map(|p| format!("{OWN}/{p}")),
        )
        .chain([format!("{WS}/.git")])
        .collect();
    for lock in &locks {
        let at = a
            .windows(3)
            .position(|w| w[0] == "--ro-bind" && &w[2] == lock)
            .unwrap_or_else(|| panic!("{lock} not locked"));
        for (j, w) in a.windows(3).enumerate() {
            if (w[0] == "--bind") && Path::new(lock).starts_with(&w[2]) {
                assert!(
                    j < at,
                    "{} rw at {j} after the lock of {lock} at {at}",
                    w[2]
                );
            }
        }
    }
}

#[test]
fn git_layout_mistakes_are_refused() {
    let p = policy();
    let with = |common: &str, own: &str| {
        let mut r = run("qoder");
        r.git = Some(GitWorktree {
            common_dir: common.into(),
            own_dir: own.into(),
        });
        plan(&p, &r)
    };
    assert!(matches!(
        with(
            COMMON,
            "/home/nez/Projects/Lucina/SecretaryIOS/.git/other/lagent-x"
        ),
        Err(SandboxError::Git { .. })
    ));
    assert!(matches!(
        with(COMMON, &format!("{COMMON}/worktrees/a/b")),
        Err(SandboxError::Git { .. })
    ));
    let e = with(
        "/home/nez/Projects/Lucina/SecretaryIOS",
        "/home/nez/Projects/Lucina/SecretaryIOS/worktrees/lagent-x",
    )
    .unwrap_err();
    assert!(
        e.to_string()
            .contains("/home/nez/Projects/Lucina/SecretaryIOS"),
        "{e}"
    );
    assert!(matches!(
        with(&format!("{WS}/.git"), &format!("{WS}/.git/worktrees/x")),
        Err(SandboxError::Overlap { .. })
    ));
}

#[test]
fn outside_git_options_turn_off_what_a_worker_tree_could_trigger() {
    let o = outside_git_options();
    for want in [
        "core.hooksPath=/dev/null",
        "core.fsmonitor=false",
        "submodule.recurse=false",
        "safe.bareRepository=explicit",
    ] {
        assert!(o.contains(&want), "{want}");
    }
    assert!(o
        .iter()
        .all(|kv| kv.split_once('=').is_some_and(|(k, _)| k.contains('.'))));
}

// ------------------------------------------------------------------------------------------------
// Refusals: every hidden entry, by three routes where they apply.

/// The three routes for one path: as the workspace, as an extra read-only path, and as the qoder
/// lane's home in a tampered policy text (`lane_spec`, `None` where a lane home cannot name it).
/// Each refusal names the path.
fn refused_by_all_routes(path: &str, lane_spec: Option<&str>) {
    let p = policy();
    let mut ws = reader_run("qoder");
    ws.workspace = path.into();
    let e = plan(&p, &ws).expect_err(path);
    assert!(e.to_string().contains(path), "workspace {path}: {e}");

    let mut ex = run("qoder");
    ex.extra_ro = vec![path.into()];
    let e = plan(&p, &ex).expect_err(path);
    assert!(e.to_string().contains(path), "extra {path}: {e}");

    if let Some(spec) = lane_spec {
        let text = POLICY.replace(QODER_HOME_LINE, &format!("home = \"{spec}\""));
        assert_ne!(text, POLICY);
        let e = Policy::from_toml(&text)
            .and_then(|tampered| plan(&tampered, &run("qoder")))
            .expect_err(spec);
        let shown = spec.replace('~', HOME);
        assert!(
            e.to_string().contains(spec) || e.to_string().contains(&shown),
            "lane home {spec}: {e}"
        );
    }
}

fn tree(rel: &str) {
    refused_by_all_routes(&format!("{HOME}/{rel}"), Some(&format!("~/{rel}")));
    refused_by_all_routes(
        &format!("{HOME}/{rel}/inner"),
        Some(&format!("~/{rel}/inner")),
    );
}

#[test]
fn refuses_the_home_as_a_whole() {
    refused_by_all_routes(HOME, Some("~"));
    refused_by_all_routes("/home", None);
    refused_by_all_routes("/", None);
}

#[test]
fn refuses_ssh() {
    tree(".ssh");
}

#[test]
fn refuses_gnupg() {
    tree(".gnupg");
}

#[test]
fn refuses_config() {
    tree(".config");
    for sub in [
        "gh",
        "infisical-mcp",
        "pokoin",
        "lane",
        "opencode",
        "rustfmt",
    ] {
        refused_by_all_routes(
            &format!("{HOME}/.config/{sub}"),
            Some(&format!("~/.config/{sub}")),
        );
    }
}

#[test]
fn refuses_claude() {
    tree(".claude");
}

#[test]
fn refuses_cargo_as_a_whole() {
    refused_by_all_routes(&format!("{HOME}/.cargo"), Some("~/.cargo"));
}

#[test]
fn refuses_cargo_credentials_and_config() {
    for f in ["credentials.toml", "credentials", "config.toml", "config"] {
        refused_by_all_routes(
            &format!("{HOME}/.cargo/{f}"),
            Some(&format!("~/.cargo/{f}")),
        );
    }
}

#[test]
fn refuses_git_credentials() {
    refused_by_all_routes(
        &format!("{HOME}/.git-credentials"),
        Some("~/.git-credentials"),
    );
}

#[test]
fn refuses_gitconfig() {
    refused_by_all_routes(&format!("{HOME}/.gitconfig"), Some("~/.gitconfig"));
}

#[test]
fn refuses_netrc() {
    refused_by_all_routes(&format!("{HOME}/.netrc"), Some("~/.netrc"));
}

#[test]
fn refuses_docker_config() {
    tree(".docker");
}

#[test]
fn refuses_kube() {
    tree(".kube");
}

#[test]
fn refuses_aws() {
    tree(".aws");
}

#[test]
fn refuses_agentd_share() {
    tree(".local/share/lucina-agentd");
    refused_by_all_routes(&format!("{HOME}/.local/share"), Some("~/.local/share"));
}

#[test]
fn refuses_agentd_state() {
    tree(".local/state/lucina-agentd");
    refused_by_all_routes(&format!("{HOME}/.local/state"), Some("~/.local/state"));
}

#[test]
fn refuses_another_lanes_home() {
    let zen = format!("{HOME}/.local/share/lane-homes/zen");
    refused_by_all_routes(&zen, Some("~/.local/share/lane-homes/zen"));
    refused_by_all_routes(
        &format!("{zen}/sub"),
        Some("~/.local/share/lane-homes/zen/sub"),
    );
    refused_by_all_routes(&format!("{HOME}/.local/share/lane-homes/brandnew"), None);
    refused_by_all_routes(
        &format!("{HOME}/.local/share/lane-homes"),
        Some("~/.local/share/lane-homes"),
    );
    // An unnamed lane whose name meets a named home (zen is not matched by zen-*).
    let e = plan(&policy(), &run("zen")).unwrap_err();
    assert!(e.to_string().contains(&zen), "{e}");
}

#[test]
fn refuses_qoder_dir() {
    tree(".qoder");
}

#[test]
fn refuses_zcode_dir() {
    tree(".zcode");
}

#[test]
fn refuses_codex_dir() {
    tree(".codex");
}

#[test]
fn refuses_qwen_dir() {
    tree(".qwen");
}

#[test]
fn refuses_opencode_dir() {
    tree(".opencode");
}

#[test]
fn refuses_projects_as_a_whole() {
    refused_by_all_routes(&format!("{HOME}/Projects"), Some("~/Projects"));
    refused_by_all_routes(
        &format!("{HOME}/Projects/.worktrees"),
        Some("~/Projects/.worktrees"),
    );
}

#[test]
fn refuses_scratch_as_a_whole() {
    refused_by_all_routes(&format!("{HOME}/scratch"), Some("~/scratch"));
    refused_by_all_routes(&format!("{HOME}/scratch/tmp"), Some("~/scratch/tmp"));
}

#[test]
fn refuses_run_user() {
    // A lane home is written ~/...: an absolute path is refused by its form, naming it.
    refused_by_all_routes("/run/user", Some("/run/user"));
    refused_by_all_routes("/run/user/1000", Some("/run/user/1000"));
    refused_by_all_routes("/run/user/1000/keyring", None);
    refused_by_all_routes("/run", None);
    refused_by_all_routes("/var/run/user/1000/bus", None);
}

#[test]
fn refuses_the_docker_socket() {
    refused_by_all_routes("/run/docker.sock", Some("/run/docker.sock"));
    refused_by_all_routes("/var/run/docker.sock", None);
}

#[test]
fn refuses_host_proc() {
    refused_by_all_routes("/proc", Some("/proc"));
    refused_by_all_routes("/proc/1", None);
    refused_by_all_routes("/proc/self/environ", None);
}

#[test]
fn refuses_host_tmp_and_devices() {
    refused_by_all_routes("/tmp", None);
    refused_by_all_routes("/tmp/tmux-1000", None);
    refused_by_all_routes("/dev/dri", None);
    refused_by_all_routes("/dev/kfd", None);
}

/// Every fixed entry, as the workspace and as an extra path (G11: the entries the per-entry tests
/// above do not name).
#[test]
fn refuses_every_fixed_hidden_entry() {
    for rel in HOME_TREES {
        refused_by_all_routes(&format!("{HOME}/{rel}"), None);
    }
    for rel in HOME_WHOLE {
        refused_by_all_routes(
            &normalize(&Path::new(HOME).join(rel)).to_string_lossy(),
            None,
        );
    }
    for abs in ABS_TREES {
        refused_by_all_routes(abs, None);
    }
}

#[test]
fn refuses_a_dot_directory_of_the_home_as_a_run_input() {
    refused_by_all_routes(&format!("{HOME}/.some-new-tool/state"), None);
}

#[test]
fn paths_are_normalised_before_the_check() {
    for sneaky in [
        format!("{HOME}/Projects/x/../../.ssh"),
        format!("{HOME}/./.ssh"),
        format!("{HOME}/Projects/.."),
        format!("{HOME}//.config/gh"),
    ] {
        let mut r = run("qoder");
        r.extra_ro = vec![sneaky.clone().into()];
        assert!(plan(&policy(), &r).is_err(), "{sneaky}");
    }
    let text = POLICY.replace(
        QODER_HOME_LINE,
        "home = \"~/.local/share/lane-homes/../../../.ssh\"",
    );
    assert!(Policy::from_toml(&text).is_err());
    let text = POLICY.replace(QODER_HOME_LINE, "home = \"~/../other\"");
    assert!(Policy::from_toml(&text).is_err());
}

#[test]
fn policy_paths_are_compared_ignoring_case() {
    for spec in [
        "~/.SSH",
        "~/.Config/gh",
        "~/.CLAUDE",
        "~/.local/share/Lucina-Agentd",
    ] {
        let text = POLICY.replace(QODER_HOME_LINE, &format!("home = \"{spec}\""));
        let e = Policy::from_toml(&text).unwrap_err();
        assert!(e.to_string().contains(spec), "{spec}: {e}");
    }
    let text = POLICY.replace(
        "caches_rw = [\"~/.cargo/registry\", \"~/.cargo/git\", \"~/.cache/bq\"]",
        "caches_rw = [\"~/.Cargo/Config.toml\"]",
    );
    assert!(Policy::from_toml(&text).is_err());
}

#[test]
fn a_tampered_policy_cannot_bind_a_hidden_path_by_any_field() {
    for (from, to) in [
        ("ro = [\"/usr\", \"/etc\"]", "ro = [\"/usr\", \"/etc\", \"/\"]"),
        ("ro = [\"/usr\", \"/etc\"]", "ro = [\"/usr\", \"/etc\", \"/run\"]"),
        ("ro = [\"/usr\", \"/etc\"]", "ro = [\"/usr\", \"/etc\", \"/home/nez\"]"),
        ("ro = [\"/usr\", \"/etc\"]", "ro = [\"/usr\", \"/etc\", \"/proc\"]"),
        // Not hidden, but not one of the system roots either.
        ("ro = [\"/usr\", \"/etc\"]", "ro = [\"/usr\", \"/etc\", \"/var/lib\"]"),
        ("ro = [\"/usr\", \"/etc\"]", "ro = [\"/usr\", \"/etc\", \"/opt\"]"),
        ("ro = [\"/usr\", \"/etc\"]", "ro = [\"/usr/share\"]"),
        (
            "net_ro = [\"/run/systemd/resolve/stub-resolv.conf\", \"/run/systemd/resolve/resolv.conf\"]",
            "net_ro = [\"/run/user/1000\"]",
        ),
        (
            "net_ro = [\"/run/systemd/resolve/stub-resolv.conf\", \"/run/systemd/resolve/resolv.conf\"]",
            "net_ro = [\"/run/podman/podman.sock\"]",
        ),
        (
            "{ target = \"usr/bin\", link = \"/bin\" }",
            "{ target = \"../run/user/1000\", link = \"/bin\" }",
        ),
        (
            "{ target = \"usr/bin\", link = \"/bin\" }",
            "{ target = \"/home/nez/.ssh\", link = \"/bin\" }",
        ),
        (
            "toolchains_ro = [\"~/.rustup\", \"~/.cargo/bin\"]",
            "toolchains_ro = [\"~/.rustup\", \"~/.cargo\"]",
        ),
        (
            "toolchains_ro = [\"~/.rustup\", \"~/.cargo/bin\"]",
            "toolchains_ro = [\"~/.bashrc\"]",
        ),
        (
            "caches_rw = [\"~/.cargo/registry\", \"~/.cargo/git\", \"~/.cache/bq\"]",
            "caches_rw = [\"~/.ssh\"]",
        ),
        (
            "caches_rw = [\"~/.cargo/registry\", \"~/.cargo/git\", \"~/.cache/bq\"]",
            "caches_rw = [\"~/.cargo/config.toml\"]",
        ),
        (
            "check_target = \"~/.cache/lucina-check-target\"",
            "check_target = \"~/.config\"",
        ),
        ("shims = \"~/.local/lib/lucina-shims\"", "shims = \"~/.local/bin\""),
        (
            "home = \"~/.local/share/lane-homes/{lane}\"",
            "home = \"~/.ssh/{lane}\"",
        ),
        (
            "home = \"~/.local/share/lane-homes/{lane}\"",
            "home = \"~/.local/share/lane-homes/shared\"",
        ),
    ] {
        let text = POLICY.replace(from, to);
        assert_ne!(text, POLICY, "{from}");
        assert!(Policy::from_toml(&text).is_err(), "{to}");
    }
    let text = POLICY.replace(
        "network = \"none\"",
        "network = \"none\"\nnetwrok = \"host\"",
    );
    assert!(Policy::from_toml(&text).is_err());
    assert!(Policy::from_toml(&POLICY.replace("version = 1", "version = 2")).is_err());
}

#[test]
fn a_policy_built_by_hand_is_validated_by_the_planner() {
    let mut p = policy();
    p.home.caches_rw.push("~/.ssh".into());
    let e = plan(&p, &run("qoder")).unwrap_err();
    assert!(e.to_string().contains(".ssh"), "{e}");
    let mut p = policy();
    p.system.ro.push("/home/nez/.config".into());
    assert!(plan(&p, &run("qoder")).is_err());
}

#[test]
fn overlapping_inputs_are_refused() {
    let p = policy();
    for extra in [
        "/home/nez/Projects/.worktrees/SecretaryIOS",
        WS,
        "/home/nez/Projects/.worktrees/SecretaryIOS/lagent-x/src",
        "/home/nez/Projects/Lucina/SecretaryIOS",
        "/usr/share/doc",
    ] {
        let mut r = run("qoder");
        r.extra_ro = vec![extra.into()];
        assert!(plan(&p, &r).is_err(), "{extra}");
    }
    let mut r = run("qoder");
    r.tmpdir = WS.into();
    assert!(plan(&p, &r).is_err());
    let mut r = run("qoder");
    r.extra_ro = vec!["/home/nez/Projects/Work/sibling".into()];
    let a = resolve_with(&Fake::standard(), &p, &r).unwrap();
    assert!(bind_at(&a, "--ro-bind", "/home/nez/Projects/Work/sibling").is_some());
}

#[test]
fn relative_and_odd_inputs_are_refused() {
    let p = policy();
    let mut r = run("qoder");
    r.workspace = "Projects/x".into();
    assert!(matches!(plan(&p, &r), Err(SandboxError::BadPath { .. })));
    let mut r = run("qoder");
    r.home = "/".into();
    assert!(plan(&p, &r).is_err());
    let mut r = run("qoder");
    r.extra_ro = vec!["/srv/a\0b".into()];
    assert!(plan(&p, &r).is_err());
}

// ------------------------------------------------------------------------------------------------
// Environment (Q4, G8).

#[test]
fn the_environment_is_cleared_and_filtered() {
    let a = args(&run("qoder"));
    let clear = a.iter().position(|x| x == "--clearenv").unwrap();
    let first_set = a.iter().position(|x| x == "--setenv").unwrap();
    assert!(clear < first_set);
    let p = policy();
    for name in [
        "HOME",
        "PATH",
        "TMPDIR",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "GIT_EXEC_PATH",
        "GIT_SSH_COMMAND",
        "GIT_ASKPASS",
        "GIT_DIR",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "LD_AUDIT",
        "BASH_ENV",
        "ENV",
        "CDPATH",
        "GCONV_PATH",
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTFLAGS",
        "CARGO_TARGET_DIR",
        "CARGO_HOME",
        "CARGO_REGISTRIES_X_TOKEN",
        "NODE_OPTIONS",
        "PYTHONSTARTUP",
        "PYTHONPATH",
        "PERL5OPT",
        "RUBYOPT",
        "XDG_RUNTIME_DIR",
        "XDG_CONFIG_HOME",
        "SSH_AUTH_SOCK",
        "DBUS_SESSION_BUS_ADDRESS",
        "LUCINA_RUN_ID",
        "LUCINA_ANYTHING",
        "1BAD",
        "A=B",
        "",
    ] {
        let mut r = run("qoder");
        r.env.push((name.into(), "x".into()));
        let e = plan(&p, &r).unwrap_err();
        assert!(matches!(e, SandboxError::Env { .. }), "{name}: {e}");
    }
    let mut r = run("qoder");
    r.env.push(("GIT_AUTHOR_NAME".into(), "again".into()));
    assert!(plan(&p, &r).is_err(), "a name passed twice");
    let mut r = run("qoder");
    r.env.push(("X".into(), "a\0b".into()));
    assert!(plan(&p, &r).is_err());
}

#[test]
fn environment_values_naming_hidden_paths_are_refused() {
    let p = policy();
    for value in [
        "/home/nez/.ssh/id_ed25519",
        "/usr/bin:/home/nez/.config/gh",
        "/run/user/1000/bus",
        "/home/nez",
        "/",
    ] {
        let mut r = run("qoder");
        r.env.push(("SOME_TOOL_PATH".into(), value.into()));
        let e = plan(&p, &r).unwrap_err();
        assert!(matches!(e, SandboxError::Env { .. }), "{value}: {e}");
    }
    let mut r = run("qoder");
    r.env
        .push(("SOME_TOOL_PATH".into(), format!("{WS}/bin:/usr/bin")));
    r.env
        .push(("A_URL".into(), "https://example.invalid/x".into()));
    assert!(plan(&p, &r).is_ok());
}

#[test]
fn run_identifiers_are_typed_inputs() {
    let p = policy();
    for bad in ["", "LA R038", "LA/R038", "x;y", &"a".repeat(33)] {
        let mut r = run("qoder");
        r.run_id = Some(bad.into());
        assert!(
            matches!(plan(&p, &r), Err(SandboxError::Env { .. })),
            "{bad:?}"
        );
    }
    let mut r = run("qoder");
    r.lead = None;
    r.change = None;
    let a = args(&r);
    assert_eq!(env_of(&a, "LUCINA_LEAD"), None);
    assert_eq!(env_of(&a, "LUCINA_RUN_ID"), Some("LA-R038"));
}

// ------------------------------------------------------------------------------------------------
// The audit (G10).

#[test]
fn the_audit_refuses_what_the_generator_never_emits() {
    let hidden = Hidden::for_run(&policy(), Path::new(HOME), "qoder").unwrap();
    let good = args(&run("qoder"));
    assert!(audit(&hidden, &good).is_ok());
    let with = |extra: &[&str]| {
        let mut a = good.clone();
        a.pop();
        a.extend(extra.iter().map(|s| s.to_string()));
        a.push("--".into());
        audit(&hidden, &a)
    };
    let hooks = format!("{COMMON}/hooks");
    let objects_info = format!("{COMMON}/objects/info");
    let cw = format!("{OWN}/config.worktree");
    let (hooks, objects_info, cw) = (hooks.as_str(), objects_info.as_str(), cw.as_str());
    for bad in [
        vec!["--ro-bind", "/home/nez/.ssh", "/home/nez/.ssh"],
        vec!["--bind", "/home/nez", "/home/nez"],
        vec!["--ro-bind", "/home/nez/.ssh/id", "/home/nez/Projects/x"],
        vec!["--ro-bind", "/srv/ok", "/home/nez/.ssh"],
        vec!["--bind", "/srv/a", "/srv/b"],
        vec!["--dev-bind", "/dev/dri", "/dev/dri"],
        vec!["--bind-fd", "3", "/x"],
        vec!["--ro-bind", "relative", "relative"],
        vec!["--clearenv"],
        // Undoing a lock: the path itself, or a path that holds it.
        vec!["--bind", hooks, hooks],
        vec!["--bind", COMMON, COMMON],
        vec!["--bind", objects_info, objects_info],
        vec!["--bind", cw, cw],
        vec!["--bind-try", hooks, hooks],
        // Symlinks into hidden paths.
        vec!["--symlink", "/home/nez/.ssh", "/alias"],
        vec!["--symlink", "../run/user/1000", "/x/y"],
        // Perms other than the home's.
        vec!["--perms", "0777", "--tmpfs", "/x"],
        // Environment, re-checked.
        vec!["--setenv", "SSH_AUTH_SOCK", "/x"],
        vec!["--setenv", "GIT_CONFIG_KEY_0", "core.hooksPath"],
        vec!["--setenv", "LD_PRELOAD", "/usr/lib/x.so"],
        vec!["--setenv", "SOME", "/home/nez/.ssh/id"],
        vec!["--setenv", "GIT_AUTHOR_NAME", "twice"],
    ] {
        assert!(with(&bad).is_err(), "{bad:?}");
    }
    // A fence variable with the wrong value.
    for (name, value) in [
        ("HOME", "/home/other"),
        ("GIT_CONFIG_GLOBAL", "/home/nez/.gitconfig"),
        ("GIT_CONFIG_NOSYSTEM", "0"),
        ("LUCINA_RUN_ID", "a b"),
        ("TMPDIR", "/tmp"),
    ] {
        let mut a: Vec<String> = Vec::new();
        let mut i = 0;
        while i < good.len() {
            if good[i] == "--setenv" && good[i + 1] == name {
                a.extend(["--setenv".into(), name.into(), value.into()]);
                i += 3;
            } else {
                a.push(good[i].clone());
                i += 1;
            }
        }
        if !a.windows(2).any(|w| w[0] == "--setenv" && w[1] == name) {
            a.pop();
            a.extend(["--setenv".into(), name.into(), value.into(), "--".into()]);
        }
        assert!(audit(&hidden, &a).is_err(), "{name}={value}");
    }
    let mut no_dash = good.clone();
    no_dash.pop();
    assert!(audit(&hidden, &no_dash).is_err());
    for flag in ["--unshare-pid", "--unshare-cgroup", "--disable-userns"] {
        let without: Vec<String> = good.iter().filter(|x| *x != flag).cloned().collect();
        assert!(audit(&hidden, &without).is_err(), "{flag}");
    }
    let mut env_first = vec!["--setenv".to_owned(), "A".into(), "b".into()];
    env_first.extend(good.iter().cloned());
    assert!(audit(&hidden, &env_first).is_err());
}

/// A list [`Plan::resolve`] would hand out, with `flag p p` inserted before the final `--`.
/// `p` is the run's temp directory: its own path, not hidden, and not under an earlier read-only bind.
fn audit_appended(flag: &str) -> Result<Vec<String>> {
    let hidden = Hidden::for_run(&policy(), Path::new(HOME), "qoder").unwrap();
    let mut a = args(&run("qoder"));
    let p = "/home/nez/scratch/tmp/run-1";
    assert_eq!(a.last().map(String::as_str), Some("--"));
    let at = a.len() - 1;
    a.insert(at, p.to_owned());
    a.insert(at, p.to_owned());
    a.insert(at, flag.to_owned());
    audit(&hidden, &a).map(|()| a)
}

#[test]
fn the_audit_refuses_an_appended_bind_try() {
    let e = audit_appended("--bind-try").expect_err("an appended --bind-try");
    let text = e.to_string();
    assert!(text.contains("--bind-try"), "{text}");
    assert!(text.contains("position"), "{text}");
}

#[test]
fn the_audit_refuses_an_appended_ro_bind_try() {
    let e = audit_appended("--ro-bind-try").expect_err("an appended --ro-bind-try");
    let text = e.to_string();
    assert!(text.contains("--ro-bind-try"), "{text}");
    assert!(text.contains("position"), "{text}");
}
