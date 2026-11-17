# The worker sandbox (LA-C010, step 1: LA-C013)

A worker (a lane run, an agentd provider, an in-app agent) runs as the owner's Unix user. This fence is
what keeps it from his secrets, from agentd's state and from other runs. Step 1 is the policy and the
bubblewrap argument generator; `lagent exec`, the shim binary, the bypass token and the Mac half are
later changes.

Code: `crates/lucina-agents/src/sandbox.rs` (pure: no IO, no environment, no clock), its unit tests in
`crates/lucina-agents/src/sandbox/tests.rs`, the host test `crates/lucina-agents/tests/sandbox_host.rs`
(real `/usr/bin/bwrap`), and `crates/lucina-delegate/tests/sandbox_parity.rs` (the new view hides at
least what `lucina-delegate`'s sandbox hides). Policy: `deploy/lucina-agentd/sandbox-policy.toml`.

## How the policy file is read

`Policy::from_toml(text)` parses the file (unknown fields refused, `version = 1`) and validates it:
system paths absolute and never under `/home` or `/root`; home paths written `~/...`; lane keys an exact
name or a `prefix-*` pattern; the default lane's home ends in `/{lane}`; no two policy paths overlap;
every path checked against the hidden list. `to_bwrap_args(&policy, &run)` validates the policy again
(a value built by hand gets the same refusals), so a caller cannot skip it.

A lane resolves to its exact key, else the longest matching `prefix-*`, else the default (no network,
`~/.local/share/lane-homes/<lane>`).

The caller passes real paths (symlinks resolved on the host). The module compares them after lexical
normalisation (`.` and `..` removed without touching the file system) and binds exactly the normalised
strings it checked. A caller that resolves the bound sources can check them again: `bound_paths(&args)`
and `Hidden::check` (the host test does this).

## What the fence refuses

The hidden list is code, not policy (`HOME_WHOLE`, `HOME_TREES`, `ABS_TREES` in `sandbox.rs`). The
generator refuses, naming the path, a policy path or run input that is a hidden path, lies inside a
"tree" entry, or is an ancestor of one:

- the home as a whole, `~/.cargo`, `~/.cache`, `~/.local/bin`, `~/Projects`, `~/Projects/.worktrees`,
  `~/scratch`, `~/scratch/tmp` as a whole (inside them is allowed: a worktree, `~/.cargo/registry`);
- trees: `~/.ssh`, `~/.gnupg`, `~/.config`, `~/.claude`, `~/.claude.json`, `~/.cargo/credentials{,.toml}`,
  `~/.git-credentials`, `~/.gitconfig`, `~/.netrc`, `~/.npmrc`, `~/.pypirc`, `~/.docker`, `~/.kube`,
  `~/.aws`, `~/.local/share/lucina-agentd`, `~/.local/state/lucina-agentd`, the tool directories
  `~/.qoder`, `~/.zcode`, `~/.codex`, `~/.qwen`, `~/.opencode`, `~/.local/share/opencode`, `~/.vibe`,
  `~/.cursor`, `~/.gemini`, `~/.local/share/zcode-lane-home`, keyrings, `~/.password-store`,
  `~/.infisical`, `~/secrets`, `~/mnt`, browser profiles and shell histories;
- every lane's sign-in home but the run's own, and the directory that holds them;
- `/proc`, `/sys`, `/dev`, `/root`, the host's `/tmp` and `/var/tmp` (other programs' sockets),
  `/run/user`, `/run/dbus`, `/run/containerd`, the docker socket (and their `/var/run` spellings); so
  `/`, `/home` and `/run` are refused as ancestors.

Besides: a run input (workspace, git directories, temp directory, extra read-only path) may not lie in
any dot directory of the home (credential stores the list does not name); bound roots may not overlap
each other or the system paths; the environment may not set the names the fence owns (`HOME`, `PATH`,
`TMPDIR`, `GIT_CONFIG_*`, `CARGO_TARGET_DIR`, `LUCINA_*`) or names that point at agents, buses or
another git directory (`SSH_AUTH_SOCK`, `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, `GIT_DIR`, ...).

Last, `audit()` walks the finished list: an option the generator never emits (a device bind, a bind
from a file descriptor) is refused, every bind's source and target is checked against the hidden list,
and the namespaces, `--clearenv` before any `--setenv` and the final `--` must be there.

## The view a worker gets

- `--die-with-parent --new-session`, user (with nested user namespaces disabled), IPC, PID, UTS and
  cgroup namespaces unshared, the network unshared for a lane marked `none`; `--cap-drop ALL`.
- `/usr` and `/etc` read-only, the merged-`/usr` symlinks, `/run/systemd/resolve` for `host` lanes
  (name resolution); a new `/proc`, a minimal `/dev` (no device nodes: not for Qwen either), a private
  `/tmp`.
- The home an empty tmpfs (0700). Bound back in at their real paths: `~/.rustup` and `~/.cargo/bin`
  read-only; `~/.cargo/registry`, `~/.cargo/git`, `~/.cache/bq` read-write; the check target read-write
  for a build run only; the lane's own sign-in home; the run's temp directory (`TMPDIR`); each extra
  read-only path; the shim directory read-only and first on `PATH`; the workspace read-write.
- Git, worktree case: the common git directory read-only, with `objects`, `refs`, `logs`,
  `packed-refs`, `reftable` and `worktrees/<this one>` writable (a commit works); then `hooks`, `config`,
  `info`, `modules`, this worktree's `commondir`, `gitdir`, `config.worktree` and the workspace's `.git`
  file read-only on top. Plain repository: the workspace's `.git` read-write with `hooks`, `config`,
  `info`, `modules` read-only on top. `hooks`, `config` and `info` must exist (fail closed: the caller
  creates empty `hooks/` and `info/` if a repository lacks them).
- Environment: cleared; `HOME` (the tmpfs), `PATH`, `TMPDIR`, `GIT_CONFIG_GLOBAL=/dev/null`,
  `GIT_CONFIG_SYSTEM=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_TERMINAL_PROMPT=0`, `LUCINA_SANDBOX`,
  `LUCINA_LANE_HOME`, `CARGO_TARGET_DIR` for a build run; then the names the caller passed (the commit
  identity comes from `GIT_AUTHOR_*`/`GIT_COMMITTER_*`). No credential helper can be reached: its files
  and sockets are not in the view.
- Symlinks: bubblewrap binds a directory, not the targets of links inside it. A link in the workspace
  that points at a hidden path resolves inside the view, where that path does not exist: reading through
  it fails (the host test checks this). A bound source that is itself a symlink is followed by the mount,
  which is why the caller passes real paths and may re-check them.
- What runs is the argument list ending in `--`, then the program. Values of `--setenv` are on bwrap's
  command line; a caller passing a token should hand the list to bwrap through `--args <fd>`.

## Limits

bubblewrap is a building block, not a security boundary: the boundary is this policy plus the kernel's
namespaces, and a kernel namespace bug defeats it. A worker inside the fence can still do anything to its
own workspace. Network stays open for lanes marked `host`, so what a worker can read it can send. The
Mac's mount of this host is outside the fence.

Also not closed by step 1:

- `refs` is writable as a whole: a worker can move any branch of the repository, `main` included. The
  merge gates and the review are what catch that.
- In a plain repository (not a worktree) the top of `.git` must be writable (index, `HEAD`, lock files),
  so a worker can create `.git/commondir` or `.git/config.worktree`, which git run later OUTSIDE the
  fence would read. Work in worktrees; `lagent exec` should refuse a plain repository for agent runs.
- Git run outside the fence on a worker's tree (the lead's review, the merge) should not trust it:
  `-c core.fsmonitor=false -c core.hooksPath=/dev/null` and no submodule recursion.
- With `host` network the abstract Unix socket namespace is shared with the host (Landlock's
  abstract-socket scoping can close it later).
- No seccomp filter yet; `/etc` is visible as a whole (the host's root-only files stay unreadable).
