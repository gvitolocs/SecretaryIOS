//! Unit tests of the sandbox policy and generator. No IO: the repository's policy file is
//! compiled in with `include_str!`.

use super::*;

const POLICY: &str = include_str!("../../../../deploy/lucina-agentd/sandbox-policy.toml");
const HOME: &str = "/home/nez";
const WS: &str = "/home/nez/Projects/.worktrees/SecretaryIOS/lagent-x";
const COMMON: &str = "/home/nez/Projects/Lucina/SecretaryIOS/.git";
const OWN: &str = "/home/nez/Projects/Lucina/SecretaryIOS/.git/worktrees/lagent-x";
const QODER_HOME_LINE: &str = "home = \"~/.local/share/lane-homes/qoder\"";

fn policy() -> Policy {
    Policy::from_toml(POLICY).expect("the repository's policy file")
}

fn run(lane: &str) -> RunInputs {
    RunInputs {
        home: HOME.into(),
        workspace: WS.into(),
        common_git_dir: COMMON.into(),
        worktree_git_dir: Some(OWN.into()),
        lane: lane.to_owned(),
        cost: CostClass::Light,
        tmpdir: "/home/nez/scratch/tmp/run-1".into(),
        extra_ro: Vec::new(),
        env: vec![
            ("GIT_AUTHOR_NAME".into(), "Worker".into()),
            ("GIT_AUTHOR_EMAIL".into(), "w@example.invalid".into()),
            ("LUCINA_RUN_ID".into(), "LA-R026".into()),
        ],
    }
}

fn plain_run(lane: &str) -> RunInputs {
    let ws = "/home/nez/Projects/Work/demo";
    RunInputs {
        workspace: ws.into(),
        common_git_dir: format!("{ws}/.git").into(),
        worktree_git_dir: None,
        ..run(lane)
    }
}

fn args(r: &RunInputs) -> Vec<String> {
    to_bwrap_args(&policy(), r).expect("a valid run")
}

/// Index of the triple `flag a a` (a bind of `a` at its own path).
fn bind_at(a: &[String], flag: &str, p: &str) -> Option<usize> {
    a.windows(3)
        .position(|w| w[0] == flag && w[1] == p && w[2] == p)
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
    let p = policy();
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
        assert!(
            bind_at(&a, "--bind-try", &own_home).is_some(),
            "{lane}: {s}"
        );
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
        for f in [
            "--die-with-parent",
            "--new-session",
            "--unshare-user",
            "--disable-userns",
            "--unshare-ipc",
            "--unshare-pid",
            "--unshare-uts",
            "--clearenv",
        ] {
            assert!(a.contains(&f.to_owned()), "{lane}: {f}");
        }
        assert!(s.contains("--cap-drop ALL"));
        assert!(
            s.contains("--proc /proc") && s.contains("--dev /dev") && s.contains("--tmpfs /tmp")
        );
        assert!(bind_at(&a, "--ro-bind-try", "/usr").is_some());
        assert!(bind_at(&a, "--ro-bind-try", "/etc").is_some());
        assert!(bind_at(&a, "--ro-bind-try", "/home/nez/.rustup").is_some());
        assert!(bind_at(&a, "--ro-bind-try", "/home/nez/.cargo/bin").is_some());
        assert!(bind_at(&a, "--bind-try", "/home/nez/.cargo/registry").is_some());
        assert!(bind_at(&a, "--bind-try", "/home/nez/.cargo/git").is_some());
        assert!(bind_at(&a, "--bind-try", "/home/nez/.cache/bq").is_some());
        assert!(bind_at(&a, "--ro-bind", "/home/nez/.local/lib/lucina-shims").is_some());
        assert!(bind_at(&a, "--bind", "/home/nez/scratch/tmp/run-1").is_some());
        assert!(bind_at(&a, "--bind", WS).is_some());
        assert_eq!(env_of(&a, "HOME"), Some(HOME));
        assert_eq!(env_of(&a, "TMPDIR"), Some("/home/nez/scratch/tmp/run-1"));
        assert!(env_of(&a, "PATH")
            .unwrap()
            .starts_with("/home/nez/.local/lib/lucina-shims:"));
        assert_eq!(env_of(&a, "GIT_CONFIG_GLOBAL"), Some("/dev/null"));
        assert_eq!(env_of(&a, "GIT_CONFIG_SYSTEM"), Some("/dev/null"));
        assert_eq!(env_of(&a, "GIT_AUTHOR_NAME"), Some("Worker"));
        assert!(s.contains(&format!("--chdir {WS}")));
        // The home is a tmpfs, mounted before anything is bound into it.
        let tmpfs_home = a
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == HOME)
            .unwrap();
        assert!(tmpfs_home < bind_at(&a, "--ro-bind-try", "/home/nez/.rustup").unwrap());
        // The whole home, ~/.ssh, ~/.config, ~/.cargo are never bound.
        for never in [
            HOME,
            "/home/nez/.ssh",
            "/home/nez/.config",
            "/home/nez/.cargo",
            "/home/nez/.claude",
        ] {
            for f in ["--bind", "--bind-try", "--ro-bind", "--ro-bind-try"] {
                assert!(bind_at(&a, f, never).is_none(), "{lane}: {f} {never}");
            }
        }
        // Name resolution only with the host network.
        assert_eq!(
            bind_at(&a, "--ro-bind-try", "/run/systemd/resolve").is_some(),
            net == Network::Host
        );
        let _ = &p;
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
    let a = to_bwrap_args(&Policy::from_toml(&text).unwrap(), &run("qoder")).unwrap();
    assert!(a.contains(&"--unshare-net".to_owned()));
    assert!(bind_at(&a, "--ro-bind-try", "/run/systemd/resolve").is_none());
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

// ------------------------------------------------------------------------------------------------
// Git.

#[test]
fn the_worktree_case() {
    let a = args(&run("qoder"));
    let common_ro = bind_at(&a, "--ro-bind", COMMON).expect("common dir read-only");
    for rw in ["objects", "refs"] {
        let i = bind_at(&a, "--bind", &format!("{COMMON}/{rw}")).expect(rw);
        assert!(i > common_ro);
    }
    for rw in ["logs", "packed-refs", "reftable"] {
        assert!(bind_at(&a, "--bind-try", &format!("{COMMON}/{rw}")).unwrap() > common_ro);
    }
    let own = bind_at(&a, "--bind", OWN).expect("own worktree dir writable");
    for ro in ["commondir", "gitdir"] {
        assert!(bind_at(&a, "--ro-bind", &format!("{OWN}/{ro}")).unwrap() > own);
    }
    assert!(bind_at(&a, "--ro-bind-try", &format!("{OWN}/config.worktree")).unwrap() > own);
    let ws = bind_at(&a, "--bind", WS).unwrap();
    let gitfile = bind_at(&a, "--ro-bind", &format!("{WS}/.git")).expect("gitfile read-only");
    assert!(gitfile > ws);
    // No bind of the main checkout's working tree.
    assert!(!joined(&a).contains("/home/nez/Projects/Lucina/SecretaryIOS "));
}

#[test]
fn the_plain_repository_case() {
    let ws = "/home/nez/Projects/Work/demo";
    let git = format!("{ws}/.git");
    let a = args(&plain_run("qoder"));
    let w = bind_at(&a, "--bind", ws).unwrap();
    let g = bind_at(&a, "--bind", &git).unwrap();
    assert!(g > w, "the git binds come after the workspace bind");
    assert!(!joined(&a).contains("/worktrees/"));
    assert!(bind_at(&a, "--ro-bind", &format!("{ws}/.git/.git")).is_none());
    // The common directory must be the workspace's .git.
    let mut r = plain_run("qoder");
    r.common_git_dir = "/home/nez/Projects/Work/other/.git".into();
    assert!(matches!(
        to_bwrap_args(&policy(), &r),
        Err(SandboxError::Git { .. })
    ));
}

#[test]
fn hooks_config_and_info_are_read_only_after_the_common_dir_bind() {
    for (r, common, flag) in [
        (run("qoder"), COMMON.to_owned(), "--ro-bind"),
        (
            plain_run("qoder"),
            "/home/nez/Projects/Work/demo/.git".to_owned(),
            "--bind",
        ),
    ] {
        let a = args(&r);
        let c = bind_at(&a, flag, &common).expect("common dir bind");
        for ro in ["hooks", "config", "info"] {
            let i = bind_at(&a, "--ro-bind", &format!("{common}/{ro}")).expect(ro);
            assert!(i > c, "{ro} must come after the common dir bind");
        }
        assert!(bind_at(&a, "--ro-bind-try", &format!("{common}/modules")).unwrap() > c);
        // ... and nothing later binds the common dir or its hooks read-write again.
        let last_rw = a
            .windows(3)
            .rposition(|w| (w[0] == "--bind" || w[0] == "--bind-try") && common.starts_with(&w[1]))
            .unwrap_or(0);
        assert!(last_rw < bind_at(&a, "--ro-bind", &format!("{common}/hooks")).unwrap());
    }
}

#[test]
fn git_layout_mistakes_are_refused() {
    let p = policy();
    let mut r = run("qoder");
    r.worktree_git_dir = Some("/home/nez/Projects/Lucina/SecretaryIOS/.git/other/lagent-x".into());
    assert!(matches!(
        to_bwrap_args(&p, &r),
        Err(SandboxError::Git { .. })
    ));
    let mut r = run("qoder");
    r.worktree_git_dir = Some(format!("{COMMON}/worktrees/a/b").into());
    assert!(matches!(
        to_bwrap_args(&p, &r),
        Err(SandboxError::Git { .. })
    ));
    let mut r = run("qoder");
    r.common_git_dir = "/home/nez/Projects/Lucina/SecretaryIOS".into();
    r.worktree_git_dir = Some("/home/nez/Projects/Lucina/SecretaryIOS/worktrees/lagent-x".into());
    let e = to_bwrap_args(&p, &r).unwrap_err();
    assert!(
        e.to_string()
            .contains("/home/nez/Projects/Lucina/SecretaryIOS"),
        "{e}"
    );
    // A worktree whose common directory lies inside it.
    let mut r = run("qoder");
    r.common_git_dir = format!("{WS}/.git").into();
    r.worktree_git_dir = Some(format!("{WS}/.git/worktrees/x").into());
    assert!(matches!(
        to_bwrap_args(&p, &r),
        Err(SandboxError::Overlap { .. })
    ));
}

// ------------------------------------------------------------------------------------------------
// Refusals: every hidden entry, by three routes where they apply.

/// The three routes for one path: as the workspace, as an extra read-only path, and as the qoder
/// lane's home in a tampered policy text (`lane_spec`, `None` where a lane home cannot name it).
/// Each refusal names the path.
fn refused_by_all_routes(path: &str, lane_spec: Option<&str>) {
    let p = policy();
    let mut ws = plain_run("qoder");
    ws.workspace = path.into();
    ws.common_git_dir = format!("{path}/.git").into();
    let e = to_bwrap_args(&p, &ws).expect_err(path);
    assert!(e.to_string().contains(path), "workspace {path}: {e}");

    let mut ex = run("qoder");
    ex.extra_ro = vec![path.into()];
    let e = to_bwrap_args(&p, &ex).expect_err(path);
    assert!(e.to_string().contains(path), "extra {path}: {e}");

    if let Some(spec) = lane_spec {
        let text = POLICY.replace(QODER_HOME_LINE, &format!("home = \"{spec}\""));
        assert_ne!(text, POLICY);
        let e = Policy::from_toml(&text)
            .and_then(|tampered| to_bwrap_args(&tampered, &run("qoder")))
            .expect_err(spec);
        let shown = spec.replace('~', HOME);
        assert!(
            e.to_string().contains(spec) || e.to_string().contains(&shown),
            "lane home {spec}: {e}"
        );
    }
}

/// A tree entry: the path, a path inside it, and (where it is a directory) its parent route.
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
fn refuses_cargo_credentials() {
    refused_by_all_routes(
        &format!("{HOME}/.cargo/credentials.toml"),
        Some("~/.cargo/credentials.toml"),
    );
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
    // As the workspace and as an extra path; as qoder's home in a tampered policy it overlaps zen's.
    refused_by_all_routes(&zen, Some("~/.local/share/lane-homes/zen"));
    refused_by_all_routes(
        &format!("{zen}/sub"),
        Some("~/.local/share/lane-homes/zen/sub"),
    );
    // An unnamed lane's home, seen from qoder.
    refused_by_all_routes(&format!("{HOME}/.local/share/lane-homes/brandnew"), None);
    // The directory of all lane homes.
    refused_by_all_routes(
        &format!("{HOME}/.local/share/lane-homes"),
        Some("~/.local/share/lane-homes"),
    );
    // An unnamed lane whose name meets a named home (zen is not matched by zen-*).
    let e = to_bwrap_args(&policy(), &run("zen")).unwrap_err();
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
        assert!(to_bwrap_args(&policy(), &r).is_err(), "{sneaky}");
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
fn a_tampered_policy_cannot_bind_a_hidden_path_by_any_field() {
    for (from, to) in [
        (
            "ro = [\"/usr\", \"/etc\"]",
            "ro = [\"/usr\", \"/etc\", \"/\"]",
        ),
        (
            "ro = [\"/usr\", \"/etc\"]",
            "ro = [\"/usr\", \"/etc\", \"/run\"]",
        ),
        (
            "ro = [\"/usr\", \"/etc\"]",
            "ro = [\"/usr\", \"/etc\", \"/home/nez\"]",
        ),
        (
            "ro = [\"/usr\", \"/etc\"]",
            "ro = [\"/usr\", \"/etc\", \"/proc\"]",
        ),
        (
            "net_ro = [\"/run/systemd/resolve\"]",
            "net_ro = [\"/run/user/1000\"]",
        ),
        (
            "toolchains_ro = [\"~/.rustup\", \"~/.cargo/bin\"]",
            "toolchains_ro = [\"~/.rustup\", \"~/.cargo\"]",
        ),
        (
            "caches_rw = [\"~/.cargo/registry\", \"~/.cargo/git\", \"~/.cache/bq\"]",
            "caches_rw = [\"~/.ssh\"]",
        ),
        (
            "check_target = \"~/.cache/lucina-check-target\"",
            "check_target = \"~/.config\"",
        ),
        (
            "shims = \"~/.local/lib/lucina-shims\"",
            "shims = \"~/.local/bin\"",
        ),
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
    // An unknown field is a typo, not a default.
    let text = POLICY.replace(
        "network = \"none\"",
        "network = \"none\"\nnetwrok = \"host\"",
    );
    assert!(Policy::from_toml(&text).is_err());
    assert!(Policy::from_toml(&POLICY.replace("version = 1", "version = 2")).is_err());
}

#[test]
fn a_policy_built_by_hand_is_validated_by_the_generator() {
    let mut p = policy();
    p.home.caches_rw.push("~/.ssh".into());
    let e = to_bwrap_args(&p, &run("qoder")).unwrap_err();
    assert!(e.to_string().contains(".ssh"), "{e}");
    let mut p = policy();
    p.system.ro.push("/home/nez/.config".into());
    assert!(to_bwrap_args(&p, &run("qoder")).is_err());
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
        assert!(to_bwrap_args(&p, &r).is_err(), "{extra}");
    }
    let mut r = run("qoder");
    r.tmpdir = WS.into();
    assert!(to_bwrap_args(&p, &r).is_err());
    // A sibling checkout read-only is what extra_ro is for.
    let mut r = run("qoder");
    r.extra_ro = vec!["/home/nez/Projects/Work/sibling".into()];
    let a = to_bwrap_args(&p, &r).unwrap();
    assert!(bind_at(&a, "--ro-bind", "/home/nez/Projects/Work/sibling").is_some());
}

#[test]
fn relative_and_odd_inputs_are_refused() {
    let p = policy();
    let mut r = run("qoder");
    r.workspace = "Projects/x".into();
    assert!(matches!(
        to_bwrap_args(&p, &r),
        Err(SandboxError::BadPath { .. })
    ));
    let mut r = run("qoder");
    r.home = "/".into();
    assert!(to_bwrap_args(&p, &r).is_err());
    let mut r = run("qoder");
    r.extra_ro = vec!["/srv/a\0b".into()];
    assert!(to_bwrap_args(&p, &r).is_err());
}

// ------------------------------------------------------------------------------------------------
// Environment.

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
        "CARGO_TARGET_DIR",
        "SSH_AUTH_SOCK",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
        "GIT_ASKPASS",
        "GIT_DIR",
        "1BAD",
        "A=B",
        "",
    ] {
        let mut r = run("qoder");
        r.env.push((name.into(), "x".into()));
        let e = to_bwrap_args(&p, &r).unwrap_err();
        assert!(matches!(e, SandboxError::Env { .. }), "{name}: {e}");
    }
    let mut r = run("qoder");
    r.env.push(("GIT_AUTHOR_NAME".into(), "again".into()));
    assert!(to_bwrap_args(&p, &r).is_err(), "a name passed twice");
    let mut r = run("qoder");
    r.env.push(("X".into(), "a\0b".into()));
    assert!(to_bwrap_args(&p, &r).is_err());
}

// ------------------------------------------------------------------------------------------------
// The audit.

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
    assert!(with(&["--ro-bind", "/home/nez/.ssh", "/home/nez/.ssh"]).is_err());
    assert!(with(&["--bind", "/home/nez", "/home/nez"]).is_err());
    assert!(with(&["--ro-bind", "/home/nez/.ssh/id", "/home/nez/Projects/x"]).is_err());
    assert!(with(&["--ro-bind", "/srv/ok", "/home/nez/.ssh"]).is_err());
    assert!(with(&["--dev-bind", "/dev/dri", "/dev/dri"]).is_err());
    assert!(with(&["--bind-fd", "3", "/x"]).is_err());
    assert!(with(&["--ro-bind", "relative", "relative"]).is_err());
    assert!(with(&["--clearenv"]).is_err(), "a late --clearenv");
    let mut no_dash = good.clone();
    no_dash.pop();
    assert!(audit(&hidden, &no_dash).is_err());
    let no_pid: Vec<String> = good
        .iter()
        .filter(|x| *x != "--unshare-pid")
        .cloned()
        .collect();
    assert!(audit(&hidden, &no_pid).is_err());
    let mut env_first = vec!["--setenv".to_owned(), "A".into(), "b".into()];
    env_first.extend(good.iter().cloned());
    assert!(audit(&hidden, &env_first).is_err());
}

#[test]
fn bound_paths_lists_every_bind_source() {
    let a = args(&run("qoder"));
    let b = bound_paths(&a);
    assert!(b.contains(&WS));
    assert!(b.contains(&"/usr"));
    assert!(b.contains(&OWN));
    assert!(!b.contains(&"/proc"));
    assert!(!b.contains(&"/tmp"));
    assert_eq!(
        b.len(),
        a.iter()
            .filter(|x| matches!(
                x.as_str(),
                "--bind" | "--bind-try" | "--ro-bind" | "--ro-bind-try"
            ))
            .count()
    );
}
