# Lucina pipeline: task → workspace → merge → deploy → device

Status (2026-11-01): pieces 1–3 built in `crates/lucina-agentd` (`src/pipeline/`, `src/manager/pipeline.rs`,
`src/api/pipeline.rs`, `src/cli/pipe.rs`); device leases and the board's API built (pieces 4 and 5); the
board page in the app, lease-checked installs and the stale question to the owner are designed below.
The rules every session follows are in [`docs/repo-workflow.md`](../repo-workflow.md).

## Why

On 2026-11-01 SecretaryIOS had 28 worktrees, 3 of them Lucina Agents workspaces, 22 unmerged (some 10 days old and
dirty). Every Claude session merged by hand under `/tmp/lucina-merge.lock`, deployed by hand, and installed on the
iPhone by hand: two installs bypassed deploy-guard and one dropped the photo-upload extension; the Instagram work
sat unmerged on `ig-gallery-paging`, so main installs looked like a regression. Paseo (which Lucina Agents
replaced) owned the worktrees; nobody owned what came after them.

The fix is Paseo's model taken one step further: Lucina owns the whole path. A session asks for a task, works and
commits in the workspace it gets, and asks Lucina to merge and deploy. Lucina does those one at a time, with the
repo's gates, and shows every piece of work in one place. Practice elsewhere says the same: short-lived worktrees,
a coordinator that sequences merges (a merge queue), a FIFO queue for shared resources (deploys, devices).

## 1. Tasks

`lagent task start "<title>" [--repo <path>] [--branch <name>] [--session <id>] [--owner <who>]`

- Fetches `origin`, makes a worktree from `origin/<trunk>` with `wt add` (in `~/Projects/.worktrees/<Repo>/`),
  registers it as an owned workspace and records a **task**: title, repo, branch, trunk, owner, session.
- The session is `$LAGENT_SESSION`, else `$CLAUDE_CODE_HOST_SESSION_ID` (`local_…`), else `$CLAUDE_CODE_SESSION_ID`.
  The Mac wrapper `~/.local/bin/lagent` (source `deploy/lucina-agentd/lagent-mac`) forwards over SSH, maps
  `/Users/giuseppe/mnt/nezopt/…` paths and passes the session id.
- `lagent task adopt <path>` takes over a worktree made by hand (label `adopted`, start = merge base with the
  trunk). A workspace that merges without a task becomes one on its first merge.
- Any Lucina repo works: the trunk is `.lucina/pipeline.toml`'s `trunk`, else origin's HEAD, else `main`. Repos
  without a remote (lucinaos, LucinaAndroidOS) merge into the local trunk branch.
- **Hook**: `tools/pipeline-guard` is a Claude Code PreToolUse(Bash) hook (Mac and nezopt `~/.claude/settings.json`).
  In Lucina repos it denies `git worktree add` / `wt add` (→ `lagent task start`) and `git push … main`
  (→ `lagent merge`). `LAGENT_BYPASS=<reason>` in the command passes and is logged.

## 2. The merge queue

`lagent merge [<workspace id | path | branch>] [--keep] [--allow rule=reason] [--no-wait]`

- Refuses a dirty workspace (commit first). Queues the workspace's HEAD as a **job**; the same HEAD queued twice is
  the same job. One FIFO queue, one job at a time, across repos.
- agentd writes `job.json` under `~/.local/share/lucina-agentd/pipeline/jobs/<id>/` and starts
  `flock <repo lock> lagent pipeline-exec <dir>` as a transient systemd user unit (`lagent-job-<id>`), so a job
  survives agentd restarts, including agentd's own deploy. The lock is `/tmp/lucina-merge.lock` for SecretaryIOS
  (the one hand merges and deploy-guard use), `/tmp/lucina-merge-<repo>.lock` elsewhere.
- The executor, in the repo's **landing worktree** (`~/Projects/.worktrees/<Repo>/.landing`, detached; the session's
  worktree is never touched):
  1. fetch; if the commit is already on the trunk: `NOTHING_TO_MERGE`;
  2. rebase the commit onto the trunk; if that conflicts, merge it (`Merge <branch>: <title>`); if that conflicts
     too: `CONFLICT` with the files, and the session merges `origin/main` into its branch and tries again;
  3. the gates, from `.lucina/pipeline.toml` **on the trunk** (a branch can't loosen its own gates):
     `cargo test -p` for each touched workspace crate, `cargo check --workspace`, `cargo test --manifest-path` for each
     touched standalone crate (not in `[merge] skip`), declared `[[gate]]`s whose `paths` match, then
     `diffgate --range <trunk>..<candidate>` with the job's `--allow` waivers. Shared `CARGO_TARGET_DIR`
     `~/.cache/cargo-target/<Repo>-landing`, so builds stay warm;
  4. push the result to the trunk (a fast-forward). If the trunk moved meanwhile, start over (three attempts).
- After a merge agentd moves the workspace's branch to the merged commit (`reset --keep`, only if the workspace is
  clean and still at the queued commit) and archives it: worktree and branch removed. `--keep` keeps it, starting
  from the merged commit. Gitignored outputs go with an archived worktree: copy artifacts out first.
- The CLI follows the job's log until it ends and exits non-zero with the job's code (`GATE_FAILED`, `CONFLICT`,
  …). `lagent job ls|show|logs [-f]|wait|cancel <id>`. Ctrl-C stops following, not the job.

## 3. Deploys

`lagent deploy <runtime> [--note <text>] [--force] [--no-wait]`

- A job like a merge (same queue, same lock). In `~/Projects/.worktrees/<Repo>/.deploy` at the trunk's tip:
  `deploy-guard plan` (up to date: `NOTHING_TO_DEPLOY` unless `--force`), the recipe's `build`,
  `deploy-guard check` (refused: `REFUSED`; it already refuses anything but main's tip and anything that drops a
  live commit), `install`, `health`, then `deploy-guard record --note "<note> (lagent deploy <job>)"`: ledger,
  `deploy/<runtime>/*` tag and the ops record.
- Recipes are `[deploy.<runtime>]` in `.lucina/pipeline.toml`: `lucina-agentd` (`deploy/lucina-agentd/install.sh`)
  and `lucina-rs` (`deploy/lucina-rs/install.sh`) so far. A runtime without one gets `NO_RECIPE` and is deployed
  by hand as before (p1 runtimes, web, iOS). Adding one is a PR to that file.
- A failed install, health check or record pushes a notification to the owner.

## 4. Devices

Built: leases in agentd. `lagent device ls`, `lagent device lease <device> [--purpose] [--minutes N]`,
`lagent device release <device> [--force --reason]`; `GET /devices`, `POST /devices/{id}/lease|release`.
Devices: `iphone`, `stick` (Samsung laptop USB stick), `kindle`, `poco`. A lease is exclusive (another holder gets
409 with who holds it and until when), renewed by leasing again, at most 240 minutes; forcing someone else's lease
needs a reason and is logged.

Designed (with the "Enforce iPhone installs only from main" session, which owns `ios-install` and its hook):
- `ios-install` keeps its device flock, its main-tip check (deploy-guard on nezopt) and its test-build rule (the
  owner's go through `lucina_decide`), and takes its exclusive lease from agentd (`lagent device lease iphone`)
  instead of its local `lease.json`, so the board shows who holds the phone.
- `lagent device install <device> [--test-build --approval dc-…]` runs the device's installer from the trunk's tip
  and records it (`deploy-guard record --runtime ios`): `ios-install` on the Mac for the iPhone, the LucinaOS
  image tools for the stick and the Kindle, the POCO flasher for `poco`. Without a lease held by the caller, or with
  a non-main build and no approval, it refuses.
- Owner tests run through the coordinator session (LucinaWORKparent): it holds the lease while the owner tests.

## 5. The board

Built: `GET /pipeline/board` and `lagent board`: every worktree workspace with its state (`open`, `merge_queued`,
`merging`, `merged`, `stale` = unmerged and idle over 3 days), changes ahead, dirtiness, idle days and session;
worktrees nobody registered (`unmanaged`); the queue; recent jobs; leases; recent deploys (deploy-guard's
history). secretary-server proxies `/v1/agents/*` to agentd; `pipeline` and `devices` are in its top-level path
list (`crates/secretary-server/src/agents.rs` `TOP`), so once secretary-server is deployed from a main that has
this, the app reads `/v1/agents/pipeline/board` and `/v1/agents/devices`.

Designed:
- **App**: a "Pipeline" section on the Agents page (web and iOS) with the same rows, grouped by state, a job's log
  on tap, and the leases. Read-only for agents; the owner can cancel a job or release a lease.
- **Stale work**: once a day agentd asks `lucina_decide` about each stale workspace ("keep or archive
  `<branch>`: +N commits, idle D days, dirty?"), options keep / archive / merge, impact low for clean ones, medium
  for dirty ones (owner-only: dirty work is never thrown away without his answer). The decision id is kept on the
  task so it is asked once; "archive" archives (a dirty worktree is kept on disk and listed), "merge" queues it.
- **Hand-made worktrees**: clean ones whose commits are all on main are archived; clean unmerged ones are adopted;
  dirty ones wait for the owner's answer through `lucina_decide`.

## Files

| What | Where |
|---|---|
| Types, store tables (`pipeline_tasks`, `pipeline_jobs`, `device_leases`), config | `crates/lucina-agentd/src/pipeline/mod.rs` |
| `.lucina/pipeline.toml` and gate selection | `crates/lucina-agentd/src/pipeline/repo.rs` |
| The executor (`lagent pipeline-exec`) | `crates/lucina-agentd/src/pipeline/exec.rs` |
| Queue worker, tasks, after-merge, leases, board | `crates/lucina-agentd/src/manager/pipeline.rs` |
| HTTP routes | `crates/lucina-agentd/src/api/pipeline.rs` |
| CLI | `crates/lucina-agentd/src/cli/pipe.rs` |
| End-to-end test (adopt → queue → executor → archive; leases) | `crates/lucina-agentd/tests/pipeline.rs` |
| Hook | `tools/pipeline-guard` |
| Recipes and gates of this repo | `.lucina/pipeline.toml` |
