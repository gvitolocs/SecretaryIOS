# SecretaryIOS

A personal secretary for the owner: chat with Lucina (with Honcho memory),
reminders, a daily agenda and the grocery list, from an iPhone app and a web
app at `https://secratary.app`.

## Architecture

```
iPhone app (SwiftUI, later: ios/)  ──HTTPS + device token──▶  secretary-server (Rust, axum, Oracle VM "p1")
web app (SolidJS, web/) ──same origin, sec_session cookie──▶      │  server-held Lucina token
                                                                  ▼
                                   Lucina (Hermes bot on nezopt :8788) via SSH relay 127.0.0.1:18788 on p1
                                   /api/secretary/*  (chat w/ Honcho memory, reminders, agenda, shopping)
```

The phone never holds the Lucina token. The server is thin: auth,
validation, forwarding, error mapping (and serving the built web app). Product
logic lives in Rust crates (`secretary-core`) or in Lucina, never in the iOS
shell.

- App-facing contract: [`docs/server-api.md`](docs/server-api.md) (every
  `/v1` route, auth, sessions, errors).
- Lucina-side contract: `docs/secretary-api.md` in the Lucina repo (branch
  `secretary-api`).

## Layout

```
Cargo.toml                 workspace (resolver 2)
crates/secretary-core/     models (serde) for chat/jobs/reminders/agenda/shopping, input validation,
                           LucinaClient (reqwest + rustls), Error (thiserror);
                           module `shopping` = the former lucina-core (models, client, sync op-log, catalog)
crates/secretary-server/   bin `secretary-server`: axum 0.8 gateway + token CLI
verification/verus/        sync_kernel.rs: Verus proofs of the shopping sync kernel
formal/                    TLC (PlusCal) + Quint models of original vs migrated sync, runners
assets/                    placeholder catalog (see docs/api-surface.md for provenance)
docs/                      server-api.md (this server), api-surface.md (upstream shopping routes)
deploy/                    systemd unit, Caddyfile snippet, env example, install steps
ios/                       placeholder: the SwiftUI shell comes next
web/                       SolidJS frontend (built by a separate task)
```

## secretary-server in one screen

- **Auth:** `Authorization: Bearer sec_…` device tokens (32 random bytes,
  base64url). Browsers exchange one for an `HttpOnly; Secure;
  SameSite=Strict` `sec_session` cookie at `POST /v1/session`. Cookie writes
  need `X-Secretary-CSRF: 1`. Only SHA-256 hashes are stored
  (`devices.json`, `sessions.json`, 0600, atomic writes), compared in
  constant time. A revoke takes effect on the next request.
- **Routes:** `GET /v1/health` (no auth; cached Lucina probe), `POST
  /v1/chat`, `GET /v1/jobs/{id}`, `GET|POST /v1/reminders`, `DELETE
  /v1/reminders/{id}`, `GET /v1/agenda`, `GET|POST /v1/shopping`, `PATCH
  /v1/shopping/{id}`, `GET|POST|DELETE /v1/session`, and, when configured,
  the old shopping upstream routes under `/v1/upstream/`.
- **Errors:** `{"error":{"code","message"}}`. Lucina 4xx codes pass through.
  Lucina failures map to 502 `lucina_error` / `lucina_unreachable` and 504
  `lucina_timeout` (chat 120 s, else 15 s).
- **Hardening:** 256 KiB body limit, 60 req/min per device, 5 logins/min per
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
VSTD_PATH=~/tools/verus/verus-x86-linux ~/tools/verus/verus-x86-linux/verus \
    verification/verus/sync_kernel.rs

# static release binary for p1 (see deploy/README.md)
CC_x86_64_unknown_linux_musl=gcc cargo build --release --target x86_64-unknown-linux-musl -p secretary-server
```

The formal models and Verus proofs cover the shopping op-log sync kernel
carried over from `lucina_shoppinglist` (idempotent apply, dedup gate,
deleted-stays-deleted; the original snapshot sync's lost update is shown by
a required-to-fail witness). They do not model the gateway.

## Provenance

`crates/secretary-core/src/shopping`, `formal/`, `verification/`, `assets/`
and `docs/api-surface.md` come from `lucina_shoppinglist`. Its notes on
provenance and rights still apply: the upstream shopping API paths were
inventoried from the reference client (functional facts), no decompiled
source was translated, and no proprietary assets are bundled.
