# Repo workflow: one trunk

Since 2026-10-30 SecretaryIOS has **one long-lived branch, `main`**. Every deploy target is built from `main`, and what
is live is marked with tags, not with branches.

## Every session, every Lucina repo (since 2026-11-01): Lucina is the pipeline

Lucina Agents (`lagent`, lucina-agentd on nezopt) owns the path from a task to main, production and the devices.
Sessions don't make worktrees, merge, deploy or install by hand any more. On the Mac, `~/.local/bin/lagent`
forwards to nezopt and passes your session id; on nezopt it is `~/.local/bin/lagent`.

```sh
lagent task start "Instagram gallery paging" [--repo ~/Projects/Lucina/lucinaos] [--branch ig-gallery-paging]
#   -> a fresh worktree from origin/main in ~/Projects/.worktrees/<Repo>/<branch>, registered with your session
cd <the printed path>; …work…; git commit …           # commit everything that belongs to the task
lagent merge <workspace id | path | branch>          # queued; Lucina rebases on origin/main, runs the repo's gates
                                                     # (.lucina/pipeline.toml), pushes main, archives the workspace
lagent deploy lucina-agentd --note "what it brings"  # from main's tip: deploy-guard plan/check, install, record
lagent board                                         # every workspace's state, the queue, leases, recent deploys
lagent device lease iphone --purpose "owner test"    # devices are leased (iphone, stick, kindle, poco)
```

- **One task, one workspace.** `lagent task start` instead of `wt add` / `git worktree add` (a hook denies those in
  Lucina repos). A worktree you already made by hand: `lagent task adopt <path>`.
- **Main moves only through the queue.** `lagent merge` instead of `git push origin HEAD:main` (the hook denies that
  too). One merge at a time, FIFO, under `/tmp/lucina-merge.lock`; the CLI shows the log and exits non-zero on
  `CONFLICT` (merge `origin/main` into your branch, commit, merge again) or `GATE_FAILED` (`lagent job logs <id>`).
  The session's worktree is never touched during the merge; afterwards it is archived (worktree and branch go,
  gitignored outputs too: copy artifacts out first) unless you pass `--keep`.
- **Deploys** go through `lagent deploy <runtime>` where `.lucina/pipeline.toml` has a recipe (`lucina-agentd`,
  `lucina-rs` so far). Other runtimes still deploy by hand as in "Deploying" below, always through deploy-guard.
- **Devices**: lease before you touch one; the iPhone installs only through `ios-install` (main's tip, or a test
  build the owner approved through `lucina_decide`), and owner tests go through the coordinator session.
- **Exceptions** need the owner's go (`lucina_decide`); then `LAGENT_BYPASS=<reason>` in the command passes the hook
  and is logged.
- Design and internals: [`docs/design/agents-pipeline.md`](design/agents-pipeline.md).

## Why

Until 2026-10-30 the repo had five deploy-line branches: `lucina-rs` (nezopt), `server-live` (p1 server and MCP),
`web-live` (p1 web, bundled by the Mac app), `ios-app` (iPhone) and `main-next` (media, local to nezopt). Fixes were
cherry-picked from line to line, so one patch had a different hash on each line. That broke three things:
- `--is-ancestor` checks failed;
- the deploy-guard records drifted from what was running;
- agents merged one line's whole tree into another. Codex once put +105k lines into a lucina-rs fix.

The inventory on 2026-10-29 (tool: `~/scratch/branch-inventory` on nezopt) compared each path across the lines:
- Of 212 top-level paths, 20 were identical and 173 were **the same code at different ages**: one line had an older copy of
  another's file.
- Only 19 paths really diverged. Inside them, 55 files had edits on two lines, mostly the same feature ported twice:
  - secretary-server and secretary-mcp on lucina-rs and on server-live;
  - Computer, Links and notifications in `web/`;
  - the memo models (`formal/memos`, `verification/*/memo*`);
  - `Cargo.toml` and `Cargo.lock`;
  - the design tokens on ios-app.

On 2026-10-30 all five lines were merged into `main` (real merges, so every live commit is an ancestor of `main`).

Protocol 7 in `docs/verification.md` (`formal/deploy/Deploy.tla`, the Verus kernel `deploy_guard.rs`) was always
specified with one `main`. The lines were a drift away from the verified design.

## Rules

1. **Work on a topic branch from `origin/main`**: `lagent task start "<title>"` on nezopt (it runs `wt add <topic>
   origin/main` for you and registers the task). Never branch from, or merge, another deploy target's tree. There is
   only `main`.
2. **Land on `main`** through the merge queue: `lagent merge <workspace>` (rebase or merge onto `origin/main`, the
   gates, a fast-forward push, all under `/tmp/lucina-merge.lock`). A GitHub PR merged on GitHub is fine too; the
   merge sweep then archives the workspace. Never force-push `main`.
3. **Shared code changes once.** A change to `secretary-core`, `lucina-store` or `web/src/lib` lands once on `main`.
   `deploy-guard plan` then shows every runtime it touches.
4. **Deploy from `main` only.** `deploy-guard check` takes the lock first (it refuses at once without it), then
   fetches `origin/main` and the `deploy/<runtime>/*` tags, then refuses:
   - a commit that is not `origin/main`'s tip: one on a branch, or a stale or older `main` (it lists the newer commits);
   - any `--target` other than `main`;
   - a commit that does not contain the live build: the newest `deploy/<runtime>/*` tag and the ledger entry (it lists
     the live commits the deploy would drop).

   It is the gate for every deploy script. Run it inside the lock, then deploy, then `deploy-guard record`. Two
   overrides exist, and each one is logged to the ops record. `--allow-not-main=<reason>` deploys a commit that is
   not `main`'s tip, but it must still contain the live build. `--rollback-to <deploy tag> --reason <text>` goes back
   to a build that was live before: check out the tag, and pass both flags to `check` and to `record`. `record`
   tags the rollback too, so the newest tag is always what is live.
5. **Deployed versions are tags.** `deploy-guard record` writes the ledger with the `main` sha and creates and pushes
   the annotated tag `deploy/<runtime>/<UTC yyyymmddThhmmZ>-<sha8>`. To find what is live, read the newest
   `deploy/<runtime>/*` tag or run `deploy-guard status`.
6. **The old branch names are gone.** `lucina-rs`, `server-live`, `web-live` and `ios-app` were read-only aliases (moved
   only by `deploy-guard record`) until all six active sessions had acknowledged the switch. They were deleted on
   2026-10-30, and `~/.local/state/deploy-guard/aliases-retired` stops deploy-guard from recreating them. The
   `deploy/<runtime>/*` tags record every deploy since then. To see an old line's last state, use the merge
   commits on `main`.

## Deploy targets

Each runtime is built from `main`'s tree. `deploy/targets.tsv` lists the paths each build reads: its crate's
path-dependency closure from `cargo tree -e normal,build`, plus `Cargo.toml`, `Cargo.lock` and its deploy files.
`deploy-guard plan` reads it to tell whether `main` changed anything for a runtime since its live commit. Regenerate a
row when a crate gains a path dependency.

| Runtime | Host | Built from | Install | Old line |
|---|---|---|---|---|
| `lucina-rs` | nezopt | `crates/lucina-server` and its path deps | `deploy/lucina-rs/install.sh` | lucina-rs |
| `lucina-agentd`, `lucina-memo`, `lucina-proactive` | nezopt | `crates/<name>` and its path deps | `~/.local/bin/<name>` + user unit (`deploy/lucina-memo/install.sh`) | lucina-rs |
| `secretary-nclient`, `secretary-maps` | nezopt | `crates/<name>` and its path deps | `~/.local/bin/<name>` + user unit | lucina-rs |
| `secretary-instagram`, `secretary-pixiv` | nezopt | `crates/<name>` (standalone, own `Cargo.lock`) | `~/.local/bin/<name>` + user unit | lucina-rs |
| `lucina-media` | nezopt | `crates/lucina-media` and its path deps | `~/.local/bin/lucina-media` + user unit | main-next |
| `lucina-otp-relay` | p1 | `crates/lucina-otp-relay` (musl) | `deploy/lucina-otp/deploy.sh` | lucina-rs |
| `secretary-server`, `secretary-mcp` | p1 | `crates/secretary-{server,mcp,core}` (musl) | `deploy/README.md` | server-live |
| `web` | p1 | `web/`, `packages/lucina-stage`, `assets/icons` | `deploy/README.md` §4 | web-live |
| `ios` | iPhone (built on the Mac) | `ios/`, `crates/secretary-{ffi,core,image}`, `design/` | `ios-install` on the Mac (see "iPhone installs") | ios-app |

The Mac app (SecretaryCode `bundle-lucina-web.ts`) bundles the newest `deploy/web/*` tag, which is what p1 serves. It
falls back to `origin/main`. `LUCINA_WEB_REF` still overrides it.

## Deploying

With a recipe in `.lucina/pipeline.toml`: `lagent deploy <runtime> --note "<what>"` does all of the below in
`~/Projects/.worktrees/SecretaryIOS/.deploy`. Without one, by hand, from a clean detached checkout of main's tip
(`git worktree add --detach` is fine here: a deploy tree is not a task; give the command `LAGENT_BYPASS=deploy-tree`):

```sh
cd <worktree at origin/main>          # detached at origin/main
flock /tmp/lucina-merge.lock bash -c '
  git fetch -q origin && git merge-base --is-ancestor HEAD origin/main   # on main
  ~/.local/bin/deploy-guard plan                        # which runtimes main changed
  <build>                                               # from this clean tree
  ~/.local/bin/deploy-guard check --runtime <r>         # target is main; stop on REFUSED
  <back up, install, restart, health check>
  ~/.local/bin/deploy-guard record --runtime <r> --note "<what and the backup name>"  # ledger + tag + ops record
'
```

## iPhone installs

Every build reaches the owner's iPhone through **`ios-install`** on the Mac (`~/.local/bin/ios-install`, since
2026-11-01). On 2026-11-01 sessions installed branch builds over each other with raw `devicectl`: a test build at 09:18
and an install at 10:38 that dropped the `SecretaryPhotoUpload` extension, and neither reached the deploy ledger.

- `ios-install` installs `origin/main`'s tip. It resolves the tip on nezopt, runs `deploy-guard check --runtime ios`,
  builds Release in its own clean worktree (`~/Library/Caches/ios-install`), takes the device lease and
  `/tmp/iphone16e.lock`, installs with `devicectl`, and runs `deploy-guard record --runtime ios` (ledger, `deploy/ios/*`
  tag, ops record). It refuses any other commit.
- A test build of a branch needs the owner's own go and a lease:
  1. `ios-install lease acquire --purpose "<what>"`;
  2. get the go: `lucina-rs decide --impact high …` with a question naming the sha8 or your session id. It must be
     decided by the owner, not by Lucina, and be at most 24 h old;
  3. `ios-install --test-build --approval dc-<id> --commit <sha>`.

  A test build is recorded in the ios-install history and `phone.json`, never in the deploy ledger. When you are done,
  run `ios-install` to put main back, then `ios-install lease release`.
- Phone test runs (`xcodebuild test` installs the test host over the app) go through
  `ios-install exec [--approval dc-<id>] -- xcodebuild test …`. They need your lease, and an approval unless the
  checkout is `main`'s clean tip.
- `ios-install status` shows who holds the phone, what is on it, and the last installs. `--dry-run` checks everything
  without touching the phone.
- A Claude Code PreToolUse hook (`~/.claude/hooks/ios-install-guard.sh`) blocks raw `devicectl device install|uninstall`,
  `ios-deploy`, `ideviceinstaller`, `pymobiledevice3 apps install` and `xcodebuild test|install` on the iPhone, also inside
  scripts a command runs. Don't work around it. If `ios-install` refuses, report why to the coordinator.
- Only the coordinator or the owner breaks another session's lease (`ios-install lease release --force --reason …`,
  logged).

## Standalone crates

The workspace is the root `Cargo.toml`. These crates stay outside it on purpose, each with its own
`[workspace]` and `Cargo.lock`, and the root lists them in `exclude`:
- `crates/secretary-instagram` and `crates/secretary-pixiv`: plugin services with their own release cadence. Keeping
  them out keeps their dependency bumps off the shared `Cargo.lock`, which was the busiest conflict file.
- `tools/*`: deploy-guard, catalog-check, design-tokens, icons, lan-cert and lucina-device.
- `firmware/lucinavoice` and `firmware/lucinavoice-core`: the ESP32 speaker firmware (`esp` toolchain, Xtensa
  target) and its host-tested logic. See `firmware/README.md`.

Build and test them with `--manifest-path crates/<name>/Cargo.toml`. AU Find lives in its own repo (`aufind-rust`).
Bringing a plugin into the workspace later takes one PR: add it to `members` and drop its lock.

## Agents and lanes

Lucina work goes through `lane run --category deep-lucina|routine-lucina` (no Codex). Specs name `main` as the base,
and the worktree is the task's (`lagent task start`), merged with `lagent merge`. An agent never merges a whole branch into another to "sync" a line:
there are no lines.
