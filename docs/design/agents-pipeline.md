# Lucina pipeline: task → workspace → merge → deploy → device

Status (2026-11-01): pieces 1–3 built in `crates/lucina-agentd` (`src/pipeline/`, `src/manager/pipeline.rs`,
`src/api/pipeline.rs`, `src/cli/pipe.rs`); device leases, lease-checked iPhone installs and the board's API
built (pieces 4 and 5); the board page in the app, installers for the other devices and the stale question to the
owner are designed below.
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

Built for the iPhone (rules `lucina_agents::device`, daemon `src/devices.rs`, routes `src/api/devices.rs`, CLI
`src/cli/device.rs`, test `tests/devices.rs`):
- `lagent device install iphone [--commit] [--test --approval dc-…] [--dry-run [--build]]`, `lagent device grant
  iphone --commit <sha> [--approval]`, `lagent device history iphone`; `POST /devices/{d}/install|grants`,
  `GET /devices/{d}/grants/{id}`, `POST /devices/{d}/grants/{id}/done`, `GET /devices/{d}/history`. `GET /devices`
  also shows what agentd last installed.
- **A main install** is `origin/main`'s tip containing the live build:
  1. `deploy-guard check --runtime ios` from agentd's own clean checkout (`<state>/devices/iphone-ledger`);
  2. a lease for the install when nobody holds the phone (another holder's lease refuses it);
  3. a one-use grant, then the Mac executor `ios-install executor` over SSH (`mac-home`, then `mac-via-cursor`). It
     builds in the owner's GUI session (a launchd job: codesign can't reach the keychain over SSH) and shows the
     grant back before building and inside `/tmp/iphone16e.lock`;
  4. `deploy-guard record --runtime ios`.
- **A test build** of a branch needs the caller's lease and the owner's own go (`decided_by: owner`, ≤ 24 h, naming the
  sha8, session or workspace). It is recorded in `<state>/devices/history.jsonl`, never in the deploy ledger.
- **A phone test run** (`xcodebuild test` replaces the app with its test host) takes a grant (the lease, plus an
  approval unless the commit is the tip). On the Mac, `ios-install exec --grant <id> -- xcodebuild test …` checks it,
  runs under the device lock and reports back.
- pipeline-guard denies raw iPhone installs (devicectl, ios-deploy, ideviceinstaller, pymobiledevice3,
  `xcodebuild test|install` on the phone, also inside a script the command runs) and points here.

Designed:
- `lagent device install` for the stick and the Kindle (the LucinaOS image tools) and `poco` (the POCO flasher).
- Owner tests run through the coordinator session (LucinaWORKparent): it holds the lease while the owner tests.

## 5. The board

Built: `GET /pipeline/board` and `lagent board`: every worktree workspace with its state (`open`, `merge_queued`,
`merging`, `merged`, `stale` = unmerged and idle over 3 days), changes ahead, dirtiness, idle days and session;
worktrees nobody registered (`unmanaged`); the queue; recent jobs; leases; recent deploys (deploy-guard's
history). secretary-server proxies `/v1/agents/*` to agentd; adding `pipeline` and `devices` to its top-level path
list (`crates/secretary-server/src/agents.rs` `TOP`) and deploying it lets the app read `/v1/agents/pipeline/board`
and `/v1/agents/devices` (patch ready; it waits for main's failing secretary-server Keep test to be fixed).

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
