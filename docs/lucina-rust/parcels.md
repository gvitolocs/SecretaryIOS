# Parcel tracking

The owner sends a tracking number; Lucina keeps an eye on the parcel and tells him when it moves.
Upstream: Universal Parcel Scraper (id `universal-parcel-scraper`, `docs/upstreams.toml`), run as-is.

## What it does

- He writes "track this 3UW…". Jev routes the turn to the intent `parcel_track`
  (`docs/lucina-rust/JEV-ROUTING.md`), which takes the number out of his message.
- The carrier is detected offline (no page visit for a number we already know), the parcel is saved, and
  Lucina answers with its current status in the same turn.
- Every active parcel is checked once a day, and he gets one morning update covering all of them.
- A parcel near its destination — out for delivery, ready for pickup, a failed attempt, delivery expected
  today, or in transit at a depot in his home city — is checked every hour instead, and he is told when its
  status changes (not on every poll: only on a change).
- A delivered or returned parcel gets one final message and is never checked again.
- A parcel with no change for 45 days is dropped, with a line in the update saying so.

## Pieces

- `crates/lucina-parcels`: `policy.rs` (the pure near-destination and cadence rules, the only place a
  number is judged), `store.rs` (one JSON file, `<data dir>/parcels/parcels.json`), `client.rs` (the HTTP
  client of the scraper), `schedule.rs` (the tick), `text.rs` (what he reads).
- The `parcel` tool: `crates/lucina-tools/src/tools/parcels.rs` — add, list, status, remove.
- The chat turn: `crates/lucina-server/src/chat/parcel_turn.rs`.
- Scheduler and CLI: `crates/lucina-server/src/parcels.rs`, and
  `lucina-rs parcels list|add|status|remove|tick [--send]` (`tick` without `--send` answers what it would
  say, so the schedule is testable without messaging him).

## Settings

| Variable | Default | |
|---|---|---|
| `LUCINA_PARCELS` | on | `off` disables the whole feature: no scheduler, no tool, the intent falls back to chat |
| `LUCINA_PARCEL_SCRAPER_URL` | `http://127.0.0.1:8796` | where the scraper answers |
| `LUCINA_PARCELS_DIGEST_HOUR` | 9 | local time; the hour of the morning update |
| `LUCINA_PARCEL_HOME_CITY` | unset | optional; without it the "depot in his city" rule is off |

## Notifications

The same path as the caller digest (`crates/lucina-server/src/callers.rs`): one timeline item
(`meta.kind: "parcel_update"`, so it lands in the web/iOS thread like any other Lucina message) and one app
notification through secretary-server `POST /v1/notify` with category `reminder`. That means quiet hours and
his switches apply to parcels as they do to reminders. No Telegram, no new channel, no new credentials.

## The scraper

Universal Parcel Scraper (plhery/universal-parcel-scraper, Apache-2.0), pinned `v0.637.1`, runs unmodified as
the Docker image `ghcr.io/plhery/universal-parcel-scraper:0.637.1` from the user unit
`lucina-parcel-scraper.service` (`deploy/parcel-scraper/`), bound to `127.0.0.1:8796` with 1 GB memory and
1 CPU. No account, no API key, nothing to keep secret. `SCRAPER_PROVIDERS=UPU`: only the UPU fallback is
enabled, so a tracking number goes to the carrier (and UPU) and nowhere else. It answers one lookup at a
time and stores nothing — storing and polling are ours, in `lucina-parcels`.

## Porting record (docs/porting-guidelines.md section 3)

| Piece | Verdict | Reason |
|---|---|---|
| The Universal Parcel Scraper service | adopt, run as-is | 788 carrier folders, and they change weekly whenever a carrier changes its site; a Rust port would freeze them at the day we wrote it. Apache-2.0, no code copied |
| Its stage names and error hint kinds | adapt | mirrored as Rust enums in `lucina-parcels`; this is the contract we test against, with fixtures captured from the service |
| Its `trawl/` browser service | reject | not needed here, and AGPL-3.0 |
| Commercial fallbacks (17TRACK, ParcelsApp, Ship24, Postal Ninja, Track718) | reject | each one would receive his tracking numbers |
| Peek (plhery/peek-delivery-tracker, the app built on it) | reference | only, for how it words updates |

## Checks

- `cargo test -p lucina-parcels` (the policy rules and the fixture tests against captured scraper responses)
- `cargo test -p lucina-server --test parcels`
- service: `systemctl --user status lucina-parcel-scraper`, `curl -s http://127.0.0.1:8796/health`
- routing: `POST /api/secretary/jev/route` with a "track this <number>" text

## Not done yet

- No Verus proofs of `policy.rs` (the cadence rules are the natural candidate).
- The `parcel` tool is not exposed through the Lucina MCP server, so MCP callers cannot add or read parcels.
- Replies are English only.
