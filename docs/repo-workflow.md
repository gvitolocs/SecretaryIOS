# Repo workflow: one trunk

Since 2026-10-30 SecretaryIOS has **one long-lived branch, `main`**. Every deploy target is built from `main`, and what
is live is marked with tags, not with branches.

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

1. **Work on a topic branch from `origin/main`**: `wt add <topic> origin/main` on nezopt. Never branch from, or merge,
   another deploy target's tree. There is only `main`.
2. **Land on `main`** with a PR, or under the lock:
   - `flock /tmp/lucina-merge.lock`, then `git fetch`, `git merge origin/main` (or rebase), the gates, and
     `git push origin HEAD:main`.
   - Fast-forward only: never force-push `main`.
3. **Shared code changes once.** A change to `secretary-core`, `lucina-store` or `web/src/lib` lands once on `main`.
   `deploy-guard plan` then shows every runtime it touches.
4. **Deploy from `main` only.** `deploy-guard check` refuses:
   - a commit that is not on `origin/main`;
   - any `--target` other than `main`;
   - a commit that does not contain the live build.

   It is the gate for every deploy script. Run it inside the lock, then deploy, then `deploy-guard record`.
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
| `ios` | iPhone (built on the Mac) | `ios/`, `crates/secretary-{ffi,core,image}`, `design/` | Xcode → devicectl | ios-app |

The Mac app (SecretaryCode `bundle-lucina-web.ts`) bundles the newest `deploy/web/*` tag, which is what p1 serves. It
falls back to `origin/main`. `LUCINA_WEB_REF` still overrides it.

## Deploying

```sh
cd <worktree at origin/main>          # wt add deploy-<runtime> origin/main
flock /tmp/lucina-merge.lock bash -c '
  git fetch -q origin && git merge-base --is-ancestor HEAD origin/main   # on main
  ~/.local/bin/deploy-guard plan                        # which runtimes main changed
  <build>                                               # from this clean tree
  ~/.local/bin/deploy-guard check --runtime <r>         # target is main; stop on REFUSED
  <back up, install, restart, health check>
  ~/.local/bin/deploy-guard record --runtime <r> --note "<what and the backup name>"  # ledger + tag + ops record
'
```

iOS: build and install on the Mac from a checkout at a `main` commit. Then record it on nezopt with
`deploy-guard record --runtime ios --commit <sha>`.

## Standalone crates

The workspace is the root `Cargo.toml`. These crates stay outside it on purpose, each with its own
`[workspace]` and `Cargo.lock`, and the root lists them in `exclude`:
- `crates/secretary-instagram` and `crates/secretary-pixiv`: plugin services with their own release cadence. Keeping
  them out keeps their dependency bumps off the shared `Cargo.lock`, which was the busiest conflict file.
- `tools/*`: deploy-guard, catalog-check, design-tokens, icons, lan-cert and lucina-device.

Build and test them with `--manifest-path crates/<name>/Cargo.toml`. AU Find lives in its own repo (`aufind-rust`).
Bringing a plugin into the workspace later takes one PR: add it to `members` and drop its lock.

## Agents and lanes

Lucina work goes through `lane run --category deep-lucina|routine-lucina` (no Codex). Specs name `main` as the base,
and the worktree is `wt add <topic> origin/main`. An agent never merges a whole branch into another to "sync" a line:
there are no lines.
