# SecretaryIOS

Secretary is the owner's personal assistant and life partner: chat with
Lucina (with Honcho memory), reminders, a daily agenda, the grocery list,
gym training and body weight, with more modules on the way (see
[`docs/ROADMAP.md`](docs/ROADMAP.md)). It runs as an iPhone app and a web
app at `https://secratary.app`.

## Architecture

One design rule: **all logic in Rust, thin shells.** The shells send JSON
and render what comes back. Rules, units, statistics and labels all live in
Rust crates.

```
iPhone app (SwiftUI, ios/) ── links secretary-ffi (SecretaryCore.xcframework): gym + body engines on device
      │  HTTPS + device token (sec_…)
      ▼
secretary-server (Rust, axum, Oracle VM "p1")  ◀── web app (SolidJS, web/), same origin, sec_session cookie
      │  hosts the gym + body engines (gym.json, body.json in SECRETARY_DATA_DIR)
      │  owner login (email + password, Argon2id), device tokens, sessions
      │  /v1/assets (private avatar files), /v1/tts → Kokoro over Tailscale
      │  server-held Lucina token                    ▲
      ▼                                              │ GET /v1/insights/summary (svc_… service token)
Lucina (Hermes bot on nezopt :8788) via SSH relay 127.0.0.1:18788 on p1
/api/secretary/*  (chat w/ Honcho memory, reminders, agenda, shopping)
```

- **Feature modules** are Rust engines with one command protocol
  (`crates/secretary-gym` is the template): a JSON request
  `{op_id, expected_revision, timezone, command}` in, `{ok, replayed, code?,
  error?, view}` out. `op_id` receipts make every write idempotent, so a
  phone can replay its offline queue safely. `expected_revision` catches
  concurrent edits (`conflict`). A command is applied to a copy, validated,
  written atomically (0600, fsync, rename) and only then swapped in. State
  stores integer grams and milliseconds, never floats.
- **The same engines run in two places:** on the phone through the C ABI
  (`secretary-ffi`: `secretary_<module>_open / _request / _close`,
  `secretary_string_free`, header `crates/secretary-ffi/include/SecretaryCore.h`)
  and on the server behind `/v1/gym/*` and `/v1/body/*`.
- **The server** is thin: auth (one owner account, device tokens, cookie
  sessions, a Lucina-only service token), validation, forwarding to Lucina
  with its own token, hosting the engines, error mapping, and serving the
  built web app. The phone never holds the Lucina token.
- **Lucina** reads training and weight through
  `GET /v1/insights/summary` with the service token.

Contracts:

- App-facing: [`docs/server-api.md`](docs/server-api.md) (every `/v1`
  route, auth, sessions, the module protocol, every gym and body command,
  insights, errors).
- Lucina-side: `docs/secretary-api.md` in the Lucina repo (branch
  `secretary-api`).

## Layout

```
Cargo.toml                 workspace (resolver 2)
crates/secretary-core/     models (serde) for chat/jobs/reminders/agenda/shopping, input validation,
                           LucinaClient (reqwest + rustls), Error (thiserror);
                           module `shopping` = the former lucina-core (models, client, sync op-log, catalog)
crates/secretary-gym/      gym engine (from Lucina Gym): workouts, sets, routines, catalog, rest timer,
                           records/e1RM, charts, CSV/JSON backup, insights for Lucina
crates/secretary-body/     body-weight engine: entries, 7-day average, weekly change, 30/90-day trends, goal
crates/secretary-ffi/      C-ABI staticlib for iOS (gym + body), include/SecretaryCore.h
crates/secretary-server/   bin `secretary-server`: axum 0.8 gateway + CLI (token, user, service-token)
scripts/                   build-ios-core.sh (xcframework, macOS), verify-verus.sh
verification/verus/        Verus proofs: sync_kernel.rs (shopping sync), gym_kernel.rs, gym_lifecycle.rs,
                           body_kernel.rs (the rounded mean stays within min/max)
formal/                    TLC (PlusCal) + Quint models of original vs migrated sync, runners
assets/                    placeholder catalog (see docs/api-surface.md for provenance)
docs/                      server-api.md (this server), ROADMAP.md, api-surface.md (upstream shopping routes)
deploy/                    systemd unit, Caddyfile snippet, env example, install steps
ios/                       the SwiftUI shell (being ported from the Lucina Gym app)
web/                       SolidJS frontend
```

## secretary-server in one screen

- **Auth:** one owner account (`user set`, Argon2id, lockout after 10
  failures); `Authorization: Bearer sec_…` device tokens (32 random bytes,
  base64url; from `token issue` or `POST /v1/auth/device`). Browsers log in at
  `POST /v1/session` (email + password, or a device token) for an `HttpOnly;
  Secure; SameSite=Strict` `sec_session` cookie. Cookie writes need
  `X-Secretary-CSRF: 1`. Only hashes are stored (`devices.json`,
  `sessions.json`, `owner.json`, 0600, atomic writes), compared in constant
  time. A revoke takes effect on the next request. Lucina uses a separate
  `svc_…` service token that opens only the insights route.
- **Routes:** `GET /v1/health` (no auth; cached Lucina probe), `POST
  /v1/chat`, `GET /v1/jobs/{id}`, `GET|POST /v1/reminders`, `DELETE
  /v1/reminders/{id}`, `GET /v1/agenda`, `GET|POST /v1/shopping`, `PATCH
  /v1/shopping/{id}`, `GET /v1/gym/state|clock`, `POST /v1/gym/command`,
  `GET /v1/body/state`, `POST /v1/body/command`, `GET /v1/insights/summary`,
  `GET /v1/assets/{name}`, `POST /v1/tts`, `GET|POST|DELETE /v1/session`,
  `POST /v1/session/password`, `POST /v1/auth/device`, and, when
  configured, the old shopping upstream routes under `/v1/upstream/`.
- **Errors:** `{"error":{"code","message"}}`; the module routes answer with
  the engine body (`conflict` 409, `invalid_command`/`op_id_reused` 422).
  Lucina 4xx codes pass through. Lucina failures map to 502 `lucina_error` /
  `lucina_unreachable` and 504 `lucina_timeout` (chat 120 s, else 15 s).
- **Hardening:** 256 KiB body limit (16 MiB for gym backups), 60 req/min per device, 5 logins/min per
  IP, CSP / nosniff / no-referrer on every response, no CORS, no secrets or
  bodies in logs, graceful SIGTERM.

## Run

```bash
# token for a local Lucina, then a device token
printf '%s\n' "$LUCINA_TOKEN" > /tmp/lucina.token && chmod 600 /tmp/lucina.token
export SECRETARY_DATA_DIR=/tmp/secretary SECRETARY_LUCINA_TOKEN_FILE=/tmp/lucina.token
cargo run -p secretary-server -- token issue --name dev
cargo run -p secretary-server               # serve on 127.0.0.1:8800
curl -s localhost:8800/v1/health
```

## Gates

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check

formal/run-quint    # primary: Quint simulator, Rust backend (4 checks)
formal/run-checks   # reference: exhaustive bounded TLC, pinned toolchain (4 checks)
scripts/verify-verus.sh   # every verification/verus/*.rs (sync, gym kernel + lifecycle, body kernel)
bash -n scripts/build-ios-core.sh   # the iOS build itself runs on the Mac

# static release binary for p1 (see deploy/README.md)
CC_x86_64_unknown_linux_musl=gcc cargo build --release --target x86_64-unknown-linux-musl -p secretary-server
```

The formal models and Verus proofs cover the shopping op-log sync kernel
carried over from `lucina_shoppinglist` (idempotent apply, dedup gate,
deleted-stays-deleted; the original snapshot sync's lost update is shown by
a required-to-fail witness). They do not model the gateway.
The gym Verus files (from Lucina Gym) prove the volume, rest-timer and
record kernels and the workout lifecycle; `body_kernel.rs` proves the rounded
mean used by every body average lies within the min/max of its inputs (a
proptest checks the same for every 7-day average point).

## Provenance

`crates/secretary-core/src/shopping`, `formal/`, `verification/`, `assets/`
and `docs/api-surface.md` come from `lucina_shoppinglist`. Its notes on
provenance and rights still apply: the upstream shopping API paths were
inventoried from the reference client (functional facts), no decompiled
source was translated, and no proprietary assets are bundled.

`crates/secretary-gym`, `verification/verus/gym_*.rs` and the FFI pattern come
from the owner's Lucina Gym project (backup: `~/backups/lucina-gym-20261021/`).
