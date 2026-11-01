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

`lagent merge [<workspace id | path | branch>] [--keep] [--full] [--allow rule=reason] [--no-wait]`

- Refuses a dirty workspace (commit first). Queues the workspace's HEAD as a **job**; the same HEAD queued twice is
  the same job. One FIFO queue per repo, one job at a time in each.
- agentd writes `job.json` under `~/.local/share/lucina-agentd/pipeline/jobs/<id>/` and starts
  `flock <repo lock> lagent pipeline-exec <dir>` as a transient systemd user unit (`lagent-job-<id>`), so a job
  survives agentd restarts, including agentd's own deploy. The lock is `/tmp/lucina-merge.lock` for every repo:
  the one deploy-guard checks and sessions took by hand for all Lucina repos, so jobs and hand deploys never overlap.
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
- Recipes are `[deploy.<runtime>]` in `.lucina/pipeline.toml`: `lucina-agentd`, `lucina-rs`, `lucina-media`, `lucina-memo`, `secretary-server`,
  `secretary-mcp` and `web` (p1 scripts in `deploy/p1/`). A runtime without one gets `NO_RECIPE` and is deployed by hand
  (iOS goes through `lagent device install`). Adding one is a change to that file.
- `lucina-agentd` waits for its agents: while one runs, its recipe stops with `BUILD_FAILED` before anything changes,
  since a restart would interrupt it (`--force` goes ahead). Jobs themselves survive the restart.
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

## What the first real day changed (2026-11-01)

- **The sweep leaves used worktrees alone.** Most work runs in Claude sessions agentd can't see, so it goes by what the
  checkout shows: no sweep during a rebase, merge, cherry-pick, revert or bisect, on a detached HEAD, with
  uncommitted changes, with a process whose cwd is inside, or within 30 minutes of git last touching it
  (`LUCINA_AGENTD_SWEEP_IDLE_SECS`). The branch itself must be merged, not HEAD.
- **One queue and one lock per repo** (it was one for all until a 24-minute kernel merge held up an urgent iPhone fix):
  SecretaryIOS merges and all deploys hold `/tmp/lucina-merge.lock` (deploy-guard checks it), others
  `/tmp/lucina-merge-<repo>.lock`. A hand deploy inside SecretaryIOS's lock still stalls that repo's queue. `lagent job next
  <id>` moves a queued job up its repo's queue.
- **Fast gates, cheapest first**: diffgate, `cargo check`, then whatever else the repo declares. A merge took 1.5 to 8.5
  minutes in `cargo test` (24 once), so `cargo = "check"` trusts the session's own test run; the gate catches what the
  rebase broke at compile time, not behaviour. `lagent merge --full` tests, `cargo = "touched"` restores it per repo.
- **Local trunks** fast-forward where they are checked out; uncommitted files that don't overlap stay, and an overlap is
  `TRUNK_CHECKOUT_BLOCKED` naming the files. `lagent board` lists main checkouts with uncommitted files or running
  processes. git can't say who wrote a file; the processes named there are the lead.
- **Flaky tests**: a failed `cargo test` gate (`--full`, or `cargo = "touched"`) runs once more; a pass is reported as flaky, not hidden.
- **Resources**: `lagent top` and the board's RUNNING column show processes and memory per workspace.
- **Moves**: `lagent project move <prj> --path <new> [--name <n>]` after a checkout moved on disk (move it, and
  `<worktrees>/<old>` to `<worktrees>/<new>`, then `git worktree repair`). Workspaces follow; one whose new directory
  doesn't exist keeps its path and is reported. Running agents keep their cwd, so leave a symlink at the old path.
- **Sessions**: `lagent task adopt <path>` also takes a task back after a session lost its link. A session that calls
  `ssh nezopt lagent …` first shows as `nez@nez_opt`, until its hook reports the workspace (section 6).

Not built yet: the app's Pipeline section and the daily stale question.

## 6. Claude sessions

Most work runs in Claude desktop chats on the Mac, which agentd doesn't start, so the Agents page looked empty while a
dozen sessions were working (owner, 2026-11-01). A Claude session that works in a workspace now shows as an agent:
provider `claude-session`, `external: true`, `host` `mac` or `nezopt`, the session id and branch in its `labels`. It is
in `lagent ls`, `/v1/agents`, Lucina's `lucina_agents` list, and the board's SESSION column. `lagent session ls` lists
them with all their branches. Sessions in no workspace leave no trace.

- **Reporting**: `pipeline-guard session-hook` (`tools/pipeline-guard/src/session.rs`) runs on SessionStart,
  UserPromptSubmit, PostToolUse, Stop and SessionEnd, and calls `lagent session beat` (over SSH from the Mac, one
  ControlMaster connection, with PostToolUse throttled to one beat per 15 s). Failures are silent, logged in
  `~/.local/state/lagent-session/hook.log`, and never stop the session. An agent agentd runs itself
  (`LUCINA_AGENT_ID`) skips the hook.
- **Which workspace**: the session's tasks (`lagent task start|adopt` with its id); its cwd in a worktree; a file it
  edited or a commit it made there; a `lagent task` answer over a bare `ssh nezopt` (and the task, which named
  `user@host`, becomes the session's). Reading a worktree doesn't link it: the coordinator reads everybody's.
- **Status**: prompt and tool beats mean `running`, Stop means `idle`, SessionEnd means `closed`. Between turns the
  Stop hook keeps polling (every 30 s, after 2 h every 2 min, gives up after 24 h). Silence for 30 min
  (`LUCINA_AGENTD_SESSION_TTL_SECS`) ends the session; the next beat brings the same agent back, unarchiving it if
  need be. An ended session is archived after a day. After an agentd restart it shows `closed` until its next beat.
  An interrupted turn fires no Stop, so it shows `running` until the next beat or the silence limit. A session pausing
  pushes nothing, unlike a finished run.
- **Timeline**: the user's prompts, the messages delivered, and each turn's last message. No tool calls.
- **Not supervised**: `lagent stop` (and `POST /agents/{id}/stop`) is refused with 403 saying so; `archive` and `delete`
  are refused until the session ended; rewind is refused like every external agent's. `lagent ls --supervised` leaves
  sessions out, which is what the `lucina-agentd` deploy recipe counts: agentd's restart doesn't touch them.
- **Sweep**: a live session's workspaces count as busy, whatever its status, until it ends. A merge still archives
  them (the session asked for it, or someone did by hand): the session just loses the link.
- **Who reports**: `/sessions*` answers `lagent` on the host only (Origin::Local), and secretary-server doesn't forward
  it. A session's commands sit in the queue under owner `session:<id>`, which no device id can equal.

### Lucina reaching a session

**Decision (2026-11-01): the existing command queue, read by the hook; the answer comes back on a reach thread.**
`lucina_agents send` (or the app's send box, or `lagent send`) posts `/agents/{id}/send`, which queues a `send` command.

1. A working session gets it as context on its next tool result or prompt (`additionalContext`). A subagent's hook
   only reports, because the message would land in the subagent.
2. An idle session is woken: the Stop hook is `asyncRewake`, and exit code 2 starts a turn with the message.
3. The hook acknowledges after delivering, so a lost answer means a second delivery, never none. Unread messages
   expire after 24 h and wait through agentd restarts.
4. Lucina's message is a reach thread: its envelope tells the session to answer with `lucina_reply` (or `lucina-rs reach
   reply`), which returns it to the owner's conversation like any reply. Lucina doesn't watch for a run's end, since
   nothing ends, and an unanswered thread stays open in `lucina_inbox --to agents`.

Why not the mailbox (`lucina_inbox`) alone: a session would have to poll it, and it addresses "Claude", not one
session. Why not Claude Code's own session messaging: its socket and token are private to the desktop app.

Limits: `claude -p` runs don't wait for messages at their end; a session whose app quit its process hears at its next
prompt, if that comes within 24 h.

### Installing the hook

`cargo build --release --manifest-path tools/pipeline-guard/Cargo.toml` on the Mac and on nezopt, copy to
`~/.local/bin/pipeline-guard`, and in `~/.claude/settings.json` on both (Mac path shown; `/home/nez` on nezopt):

```json
{"hooks": {
  "SessionStart": [{"hooks": [{"type": "command", "command": "/Users/giuseppe/.local/bin/pipeline-guard session-hook", "timeout": 15}]}],
  "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "/Users/giuseppe/.local/bin/pipeline-guard session-hook", "timeout": 15}]}],
  "PostToolUse": [{"hooks": [{"type": "command", "command": "/Users/giuseppe/.local/bin/pipeline-guard session-hook", "timeout": 15}]}],
  "Stop": [{"hooks": [{"type": "command", "command": "/Users/giuseppe/.local/bin/pipeline-guard session-hook", "asyncRewake": true, "timeout": 90000}]}],
  "SessionEnd": [{"hooks": [{"type": "command", "command": "/Users/giuseppe/.local/bin/pipeline-guard session-hook", "timeout": 15}]}]
}}
```

## Files

| What | Where |
|---|---|
| Types, store tables (`pipeline_tasks`, `pipeline_jobs`, `device_leases`), config | `crates/lucina-agentd/src/pipeline/mod.rs` |
| `.lucina/pipeline.toml` and gate selection | `crates/lucina-agentd/src/pipeline/repo.rs` |
| The executor (`lagent pipeline-exec`) | `crates/lucina-agentd/src/pipeline/exec.rs` |
| Queue worker, tasks, after-merge, leases, board | `crates/lucina-agentd/src/manager/pipeline.rs` |
| HTTP routes | `crates/lucina-agentd/src/api/pipeline.rs`, `api/sessions.rs` |
| Claude sessions: rules, manager, CLI, test, hook | `crates/lucina-agents/src/external.rs`, `crates/lucina-agentd/src/manager/sessions.rs`, `cli/session.rs`, `tests/sessions.rs`, `tools/pipeline-guard/src/session.rs` |
| CLI | `crates/lucina-agentd/src/cli/pipe.rs` |
| End-to-end test (adopt → queue → executor → archive; leases) | `crates/lucina-agentd/tests/pipeline.rs` |
| Hook | `tools/pipeline-guard` |
| Recipes and gates of this repo | `.lucina/pipeline.toml` |
