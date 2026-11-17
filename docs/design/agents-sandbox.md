# The worker sandbox (LA-C010, step 1: LA-C013)

A worker (a lane run, an agentd provider, an in-app agent) runs as the owner's Unix user. This fence is
what keeps it from his secrets and from agentd's state. Step 1 is the policy and the bubblewrap argument
generator; `lagent exec`, the shim binary, the bypass token and the Mac half are later changes.

Code: `crates/lucina-agents/src/sandbox.rs` (pure: no IO, no environment, no clock), the host resolver
`crates/lucina-agents/src/sandbox/host.rs` (the one part that reads the file system), the unit tests in
`crates/lucina-agents/src/sandbox/tests.rs` (a fake file system), the host test
`crates/lucina-agents/tests/sandbox_host.rs` (real `/usr/bin/bwrap`), and
`crates/lucina-delegate/tests/sandbox_parity.rs` (the new view hides at least what `lucina-delegate`'s
sandbox hides, and every canary). Policy: `deploy/lucina-agentd/sandbox-policy.toml`.

## How the policy file is read

`Policy::from_toml(text)` parses the file (unknown fields refused, `version = 1`) and validates it:
`system.ro` only `/usr` and `/etc` (`SYSTEM_RO_ROOTS`); other system paths absolute and never under
`/home` or `/root`; home paths written `~/...`; lane keys an exact name or a `prefix-*` pattern; the
default lane's home ends in `/{lane}`; no two policy paths overlap; every path, and every symlink target,
checked against the hidden list, exactly and ignoring ASCII case. `plan(&policy, &run)` validates the
policy again, so a value built by hand gets the same refusals.

A lane resolves to its exact key, else the longest matching `prefix-*`, else the default (no network,
`~/.local/share/lane-homes/<lane>`).

## How the arguments are made

`plan(&policy, &run)` checks the run (lexical normalisation: `.` and `..` removed without touching the
file system) and returns a `Plan`. A `Plan` does not hand out arguments: `Plan::resolve(resolver)` does,
and only after it has resolved every bind source with the resolver (`host::resolve`: `canonicalize` plus
`metadata`; `host::bwrap_args` does both steps). A source is refused when its real path is not exactly
the path that was checked (a symlink anywhere on it, including the git paths the planner derives:
`hooks`, `config`, `info`, `objects/info`, `modules`, `commondir`, `gitdir`, `config.worktree`, the
workspace's `.git`), when it resolves into a hidden path, when it is the wrong kind (a socket where a
file is expected), or when it is missing and the bind is not optional. A missing optional source is left
out of the list, so the list has no `-try` binds that could pick up a path created later. A dangling
symlink is an error, not "missing".

A symlink INSIDE a bound directory that points at a hidden path is harmless: bubblewrap binds the
directory, the link resolves inside the view, where that path does not exist (the host test reads
through one and fails). A hard link cannot be seen by any path check: a hard link to a secret placed
inside a bound directory is readable.

## What the fence refuses

The hidden list is code, not policy (`HOME_WHOLE`, `HOME_TREES`, `ABS_TREES` in `sandbox.rs`). A policy
path, run input or resolved bind source that is a hidden path, lies inside a "tree" entry, or is an
ancestor of one is refused, naming the path:

- the home as a whole, `~/.cargo`, `~/.cache`, `~/.local/bin`, `~/Projects`, `~/Projects/.worktrees`,
  `~/scratch`, `~/scratch/tmp` as a whole (inside them is allowed: a worktree, `~/.cargo/registry`);
- trees: `~/.ssh`, `~/.gnupg`, `~/.config`, `~/.claude`, `~/.claude.json`, cargo's `credentials`,
  `credentials.toml`, `config`, `config.toml`, `~/.git-credentials`, `~/.gitconfig`, `~/.netrc`,
  `~/.npmrc`, `~/.pypirc`, `~/.docker`, `~/.kube`, `~/.aws`, `~/.azure`, terraform and vault files,
  `~/.pgpass`, `~/.my.cnf`, yarn, gem and bundler credentials, agentd's state, the tool directories
  (`~/.qoder`, `~/.zcode`, `~/.codex`, `~/.qwen`, `~/.opencode`, `~/.local/share/opencode`, `~/.vibe`,
  `~/.cursor`, `~/.gemini`, `~/.local/share/zcode-lane-home`), keyrings, `~/.password-store`,
  `~/.infisical`, `~/secrets`, `~/mnt`, browser profiles, the shell start-up files (`.bashrc`,
  `.profile`, `.zshenv`, `.pam_environment`, ...) and shell histories;
- every lane's sign-in home but the run's own, and the directory that holds them;
- `/proc`, `/sys`, `/dev`, `/root`, the host's `/tmp` and `/var/tmp` (other programs' sockets),
  `/run/user`, `/run/dbus`, `/run/containerd`, `/run/podman`, the docker and snapd sockets (and their
  `/var/run` spellings); so `/`, `/home` and `/run` are refused as ancestors.

Besides: a run input may not lie in any dot directory of the home; bound roots may not overlap each other
or the system paths; a plain repository is refused (below).

Environment: a caller may not pass a name the fence sets (`HOME`, `PATH`, `TMPDIR`), nor any
`GIT_CONFIG*`, `CARGO_*`, `LUCINA_*`, `LD_*`, `PYTHON*`, `XDG_*`, `BASH_FUNC_*`, nor `GIT_EXEC_PATH`,
`GIT_DIR` and the other git location variables, `GIT_SSH*`, `GIT_ASKPASS`, `SSH_AUTH_SOCK`,
`DBUS_SESSION_BUS_ADDRESS`, `BASH_ENV`, `ENV`, `CDPATH`, `GCONV_PATH`, `RUSTC`, `RUSTC_WRAPPER`,
`RUSTFLAGS`, `NODE_OPTIONS`, `PERL5OPT`, `RUBYOPT` and their kin (`REFUSED_ENV`). An absolute path in a
value (or in an element of a `:` list) must not be hidden. Run identifiers are typed inputs (`lead`,
`run_id`, `change`, each `[A-Za-z0-9-]{1,32}`), emitted as `LUCINA_LEAD`, `LUCINA_RUN_ID`,
`LUCINA_CHANGE`.

Last, `audit()` walks the resolved list: an option the generator never emits (a device bind, a bind from
a file descriptor) is refused; every bind's source and target is checked; a read-write bind must be at
its own path and may not come after a read-only bind of the same path or of one inside it (that would
undo a lock); symlink targets are checked; `--perms` must be `0700`; every `--setenv` is checked again
(the fence's own names against their fixed values or as paths, the rest by the caller rule; no name
twice); the namespaces (cgroup included), `--clearenv` before any `--setenv` and the final `--` must be
there.

## The view a worker gets

- `--die-with-parent --new-session`, user (nested user namespaces disabled), IPC, PID, UTS and cgroup
  namespaces unshared, the network unshared for a lane marked `none`; `--cap-drop ALL`.
- `/usr` and `/etc` read-only, the merged-`/usr` symlinks; for `host` lanes the resolver's two
  `resolv.conf` files (not its directory, which holds a world-writable Varlink socket); a new `/proc`,
  a minimal `/dev` (no device nodes: not for Qwen either), a private `/tmp` (mode 0755).
- The home an empty tmpfs, mode 0700 (`--perms 0700` applies to the next `--tmpfs` on bubblewrap
  0.11.1; the host test reads the mode inside). Bound back in at their real paths: `~/.rustup` and
  `~/.cargo/bin` read-only; `~/.cargo/registry`, `~/.cargo/git`, `~/.cache/bq` read-write; the check
  target read-write for a build run only; the lane's own sign-in home; the run's temp directory
  (`TMPDIR`); each extra read-only path; the shim directory read-only and first on `PATH`; the workspace
  read-write.
- Git, a worktree: the common git directory read-only, with `objects`, `refs`, `logs`, `packed-refs`,
  `reftable` and `worktrees/<this one>` writable (a commit works); then read-only on top: `objects/info`
  (alternates, commit-graph), `hooks`, `config`, `info`, `modules`, this worktree's `commondir`,
  `gitdir`, `config.worktree` and the workspace's `.git` file. `config.worktree` is locked whether or not
  it exists: when it does not, the shim directory's empty file (`empty`) is bound read-only over it
  (bubblewrap leaves an empty file behind on the host as the mount point; an empty config is inert).
  `hooks`, `config`, `info` and `objects/info` must exist (fail closed: the caller creates empty ones).
- A plain repository (a workspace with its own `.git` directory or file and no worktree) is refused: its
  `.git` top must be writable for a commit, so a planted `commondir` there would be followed by git run
  later outside the fence. Workers run in worktrees.
- A workspace without git (readers): the workspace, nothing git-related; the plan checks that it has no
  `.git`.
- Environment: cleared; `HOME` (the tmpfs), `PATH`, `TMPDIR`, `GIT_CONFIG_GLOBAL=/dev/null`,
  `GIT_CONFIG_SYSTEM=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_TERMINAL_PROMPT=0`, `LUCINA_SANDBOX`,
  `LUCINA_LANE_HOME`, `CARGO_TARGET_DIR` for a build run, the run identifiers; then the names the
  caller passed (the commit identity comes from `GIT_AUTHOR_*`/`GIT_COMMITTER_*`). No credential helper
  can be reached: its files and sockets are not in the view.
- What runs is the argument list ending in `--`, then the program.

## Git outside the fence

A worker controls its work tree, and git run on it later outside the fence (review, merge) would follow
what is there: `.gitmodules` (a submodule `update` command), `.gitattributes` (drivers, when some config
names them), a nested `.git` or an embedded bare repository with hooks. `outside_git_options()` returns
the `-c` options agentd must pass whenever it runs git on a worker's tree (`core.hooksPath=/dev/null`,
`core.fsmonitor=false`, `submodule.recurse=false`, `diff.ignoreSubmodules=all`,
`status.submoduleSummary=false`, `safe.bareRepository=explicit`). Wiring it into agentd's own git calls
is a later change.

## Limits

bubblewrap is a building block, not a security boundary: the boundary is this policy plus the kernel's
namespaces, and a kernel namespace bug defeats it. A worker inside the fence can still do anything to its
own workspace. Network stays open for lanes marked `host`, so what a worker can read it can send. The
Mac's mount of this host is outside the fence.

Also not closed by step 1:

- Shared caches: `~/.cargo/registry`, `~/.cargo/git`, `~/.cache/bq` and the build check target are the
  same host directories in every run, read-write; one run can poison what another builds (FENCE-01; an
  overlay proposal is with the coordinator).
- A token passed through `--setenv` is on bwrap's command line (readable in `/proc/<pid>/cmdline`) until
  `lagent exec` hands the list to bwrap through `--args <fd>`.
- `/etc` and `/usr` are the host's trees: a world-readable file with a secret there (`/etc/fstab` with a
  password, an apt source URL with a token, `/etc/environment`) is readable inside.
- With `host` network the abstract Unix socket namespace is the host's: such sockets are listed in
  `/proc/net/unix` and connectable (Landlock's abstract-socket scoping can close it later).
- `refs` is writable as a whole: a worker can move any branch of the repository, `main` included. The
  merge gates and the review are what catch that.
- Workspace files git outside would follow (`.gitmodules`, `.gitattributes`, a nested `.git`): closed
  only when agentd passes `outside_git_options()`.
- A hard link to a secret inside a bound directory cannot be detected by a path check.
- No seccomp filter yet.

## Open, to measure at roll-out

What each real lane needs in its home is a guess until measured (reviews Q12, G12): where each tool reads
its sign-in (`~/.qoder`, `~/.local/share/opencode`, `~/.vibe`, `~/.zcode`, `~/.cursor`, `~/.claude` are
hidden; the lane home is at `LUCINA_LANE_HOME`, and `XDG_*` names are refused from callers), whether a
tool's binary lives under `~/.local/bin` or `~/.nvm` (neither is in the view), and whether a lane needs
more than `/usr/bin` on `PATH`. Nothing changes until those are measured.
