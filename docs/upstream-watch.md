# Upstream watch

Lucina must not fall behind the originals she was ported from. Every Monday at 07:30 (Europe/Copenhagen),
`lucina-rs upstream check` takes every original listed in `docs/upstreams.toml`, fetches what changed upstream since
our pin, has JEV judge how important it is, and for significant changes Lucina asks the owner on his phone:
**Port into ours / Later / Ignore**. Porting rules: `docs/porting-guidelines.md`.

## Pieces

| Piece | Where |
|---|---|
| Registry (one entry per original, our counterpart, pin, licences) | `docs/upstreams.toml`, read from `origin/main` at run time |
| Logic (fetch, summary, JEV questions, policy, state, decisions, briefs, report) | crate `lucina-upstream` |
| JEV call with Lucina's key | `POST /api/secretary/jev/upstream` in lucina-rs (purpose `upstream`) |
| CLI | `lucina-rs upstream check | reconcile | status | validate` |
| Units (nezopt, user) | `lucina-upstream-watch.timer` (weekly check), `lucina-upstream-reconcile.timer` (hourly: acts on answers); files in `crates/lucina-upstream/deploy/` |
| State and results | `~/Projects/Lucina/data/upstream-watch/`: `state.json`, `runs/<date>.{json,md}`, `briefs/`, `contrib/` |
| Ops record | one `kind=status` record per weekly run |

## What one run does

1. Reconciles open decisions (same as the hourly timer).
2. Per entry, by `source`:
   - `github`: releases or tags after `pin`, plus the commits from `pin` (or since `pin_date`) to `branch`. `paths`
     narrows it to the parts we ported. Uses `gh api`, logged in as gvitolocs on nezopt.
   - `git`: `git log pin..branch` in a `local` clone, or `git ls-remote` on `url`.
   - `debian`: the versions of `package` on sources.debian.org.
   - `frozen`, `manual`: listed in the report only.
3. A deterministic summary: releases first, then security, breaking, features and fixes.
4. JEV answers typed questions:
   - importance 0-4;
   - breaking? security fix? touches a feature in `rely_on`?
   - effort small/medium/large.

   When JEV is unavailable, a heuristic answers instead, and the report says so.
5. It asks the owner only when the change is significant (importance ≥ `notify_min_importance`, or security
   ≥ `security_min`) and newer than what he was last asked about. At most `max_decisions_per_run` decisions go out,
   security first. Each one is a `lucina_decide` request with impact `high`, so Lucina always asks him rather than
   deciding herself. It carries the change list, JEV's verdict and the compare link.
6. Answers (picked up hourly):
   - **Port into ours**: `lagent workspace create` makes a worktree off `origin/main` of our repo, branch
     `port-<id>-<date>`, and a brief goes to `data/upstream-watch/briefs/`. A session follows the brief and the
     guidelines, then moves the `pin`. With `auto_run = true` it also starts a Lucina Agents run.
   - **Later**: the entry stays on the backlog list of every weekly report.
   - **Ignore**: nothing happens until a newer upstream version arrives.
7. The reverse direction:
   - `[[upstream.contrib]]` items are offered one per week: "Draft a contribution (no push) / Later / Never".
     A draft is a write-up in `data/upstream-watch/contrib/`. Nothing goes upstream without the owner's OK in chat.
   - The report also lists our recent commits on each ported path as possible contributions.

## Add an upstream

1. Work in a Lucina Agents workspace off `origin/main`, as for any change.
2. Add an `[[upstream]]` block to `docs/upstreams.toml`. The keys are documented at the top of that file. At minimum:
   ```toml
   [[upstream]]
   id = "foo"
   name = "Foo"
   url = "https://github.com/owner/foo"
   source = "github"
   repo = "owner/foo"
   pin = "v1.2.3"            # or pin_date = "2026-11-01" when the exact base is unknown
   ours = [{ repo = "/home/nez/Projects/Lucina/SecretaryIOS", path = "crates/lucina-foo" }]
   licence_upstream = "MIT"  # from its LICENSE file
   licence_ours = "MIT"
   relation = "port"
   confidence = "confirmed"
   rely_on = ["the features we copy"]
   notes = "where the pin comes from"
   ```
3. Check it, then try the entry without notifying anyone:
   ```bash
   lucina-rs upstream validate --registry docs/upstreams.toml
   lucina-rs upstream check --registry docs/upstreams.toml --only foo --dry-run
   ```
4. Merge to `main` in the lock. The next weekly run reads it from `origin/main`; no redeploy is needed.

After a port, move `pin` to the version you ported, so the next run only reports what came after.

## Operate

```bash
systemctl --user list-timers 'lucina-upstream-*'
lucina-rs upstream status
lucina-rs upstream check --dry-run
lucina-rs upstream check --only asterinas
journalctl --user -u lucina-upstream-watch -n 50
```

- `check --dry-run` asks and stores nothing.
- `check --only asterinas` runs one entry for real.
