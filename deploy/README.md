# Deploying secretary-server on p1

Target: the Oracle VM `p1` (Ubuntu, x86_64, 1 GB RAM), behind Caddy, serving
`https://secratary.app`. Lucina is reached through the existing SSH relay
`127.0.0.1:18788` on p1 → nezopt `:8788`.

Nothing here is automated; run the steps by hand as root on p1.

## 1. Build (on nezopt)

```bash
cd ~/Projects/SecretaryIOS
rustup target add x86_64-unknown-linux-musl          # once
# ring needs a C compiler for the musl target; the host gcc works
CC_x86_64_unknown_linux_musl=gcc \
  cargo build --release --target x86_64-unknown-linux-musl -p secretary-server
ldd target/x86_64-unknown-linux-musl/release/secretary-server   # "statically linked"
scp target/x86_64-unknown-linux-musl/release/secretary-server p1:/tmp/
```

The binary is static-pie (musl), about 5.5 MB, and has no runtime dependencies,
so the VM's glibc version does not matter.

## 2. Install (on p1)

```bash
# user and directories
useradd --system --home-dir /var/lib/secretary --shell /usr/sbin/nologin secretary
install -m 0755 /tmp/secretary-server /usr/local/bin/secretary-server
install -d -m 0750 -o root -g secretary /etc/secretary

# environment (no secrets in it)
install -m 0640 -o root -g secretary deploy/secretary.env.example /etc/secretary/secretary.env
$EDITOR /etc/secretary/secretary.env

# Lucina's SECRETARY_API_TOKEN, readable only by the service user.
# Paste it without echoing it to the terminal or shell history:
install -m 0600 -o secretary -g secretary /dev/null /etc/secretary/lucina.token
( umask 077; read -rs TOKEN && printf '%s\n' "$TOKEN" > /etc/secretary/lucina.token; unset TOKEN )

# unit
install -m 0644 deploy/secretary-server.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now secretary-server
systemctl status secretary-server
curl -s http://127.0.0.1:8800/v1/health     # lucina.reachable should be true
```

`StateDirectory=secretary` creates `/var/lib/secretary` (0700, owned by
`secretary`). It holds `devices.json`, `sessions.json`, `owner.json` (0600, hashes only),
the module files `gym.json` and `body.json` (0600) with their `.lock` files
and, if used, `web/`.

## 3. Issue the first device token

Run the CLI as the service user, so the files keep the right owner:

```bash
sudo -u secretary env SECRETARY_DATA_DIR=/var/lib/secretary \
  /usr/local/bin/secretary-server token issue --name "Giuseppe's iPhone"
```

It prints the token **once**. Put it in the iOS app, or paste it into the web
app's login screen. Then:

```bash
sudo -u secretary env SECRETARY_DATA_DIR=/var/lib/secretary secretary-server token list
sudo -u secretary env SECRETARY_DATA_DIR=/var/lib/secretary secretary-server token revoke dev_…
```

A revoke works on the running server at once, and ends that device's browser
sessions too. No restart needed.

## 3b. Owner account (email + password login)

There is exactly one user. Set its email and password as the service user;
the password is read from stdin (one line, at least 12 characters) and never
appears in the shell history or the process list:

```bash
sudo -u secretary env SECRETARY_DATA_DIR=/var/lib/secretary \
  /usr/local/bin/secretary-server user set --email owner@example.com --password-stdin
# type the password, Enter (or: read -rs PW; printf "%s\n" "$PW" | sudo -u secretary … user set …; unset PW)
sudo -u secretary env SECRETARY_DATA_DIR=/var/lib/secretary secretary-server user show
```

Running `user set` again with the same email changes the password and clears
a lockout. A different email is refused unless `--replace` is given. The
file is `/var/lib/secretary/owner.json` (0600, Argon2id hash only). No
restart is needed. The web app then logs in with email + password, and the
iOS app gets its device token from `POST /v1/auth/device`.

## 3c. Service token for Lucina (optional)

```bash
sudo -u secretary /usr/local/bin/secretary-server service-token generate /etc/secretary/service.token
# then set SECRETARY_SERVICE_TOKEN_FILE=/etc/secretary/service.token and restart;
# give Lucina the same file content over a private channel
```

## 3d. Links: Google Calendar and the AI client status

The Links tab connects Google Calendar (and later Gmail and Drive) with
OAuth, reusing Lucina's existing Google OAuth client.

**Client type.** Lucina's `GMAIL_REDIRECT_URI` / `KEEP_REDIRECT_URI` are
`http://127.0.0.1:8788/oauth2callback` and `…/keep-oauth2callback`: a fixed
port and a custom path, the shape of a **Web application** client with
registered loopback URIs (a Desktop client would not need them registered).
Confirm it in the console (step 2): a Web client shows an "Authorized
redirect URIs" list; a Desktop client does not.

**Owner actions in the Google Cloud Console** (console.cloud.google.com,
signed in as the account that owns Lucina's project):

1. Project picker (top bar) → the project of Lucina's Gmail client (its
   client id is `GMAIL_CLIENT_ID` in Lucina's `.env.local`).
2. ☰ → **APIs & Services → Credentials** → under *OAuth 2.0 Client IDs*,
   click the client whose id matches `GMAIL_CLIENT_ID`.
   - **Type "Web application"**: under **Authorized redirect URIs** click
     **+ Add URI**, enter
     `https://getsecretary.tech/v1/links/google/callback`, **Save**. (Changes
     can take a few minutes.)
   - **Type "Desktop"**: it cannot use an https redirect. **+ Create
     credentials → OAuth client ID → Web application**, name "Secretary",
     add the same redirect URI, **Create**, and use that client's id and
     secret for the files below instead (put them in Lucina's env as
     `GOOGLE_CALENDAR_CLIENT_ID/SECRET` and run the script with
     `GOOGLE_ENV_PREFIX=GOOGLE_CALENDAR`).
3. ☰ → **APIs & Services → Library** → search **Google Calendar API** →
   **Enable**. (Later, for the Gmail and Drive cards: **Gmail API** and
   **Google Drive API**.)
4. ☰ → **APIs & Services → OAuth consent screen** (Google Auth Platform →
   **Data Access**) → **Add or remove scopes** → add
   `.../auth/calendar.events` and `.../auth/calendar.readonly` → **Update** →
   **Save**.
5. Same place → **Audience**: if the app is in **Testing**, make sure the
   owner's Google account is under **Test users**. In Testing, Google expires
   refresh tokens after 7 days (Links then shows **Reconnect**); **Publish
   app** to stop that (sensitive scopes on an unverified app only show the
   "unverified app" warning to the owner, which is fine for a single user).

**Install the secrets** (on nezopt; prints no values):

```bash
deploy/install-links-secrets.sh      # GMAIL_* by default; GOOGLE_ENV_PREFIX=KEEP for Keep's client
```

It writes `/etc/secretary/google-client-id`, `/etc/secretary/google-client-secret`
and `/etc/secretary/mcp-status.token` (secretary 0600), and
`/etc/secretary-mcp/status.token` (secretary-mcp 0600, same token), then
prints the env lines to add:

```
# /etc/secretary/secretary.env
SECRETARY_TIMEZONE=Europe/Copenhagen
SECRETARY_GOOGLE_CLIENT_ID_FILE=/etc/secretary/google-client-id
SECRETARY_GOOGLE_CLIENT_SECRET_FILE=/etc/secretary/google-client-secret
SECRETARY_MCP_STATUS_TOKEN_FILE=/etc/secretary/mcp-status.token
# /etc/secretary-mcp/secretary-mcp.env
SECRETARY_MCP_STATUS_TOKEN_FILE=/etc/secretary-mcp/status.token
```

`systemctl restart secretary-server secretary-mcp`, then open
https://getsecretary.tech/links → Google Calendar → **Connect**. The server
keeps `links.json`, `links.key` and `calendar.json` in `/var/lib/secretary`
(0600); back up `links.key` with the rest of the data dir (without it the
stored tokens and ICS addresses cannot be read and must be added again).

## 3e. Seed notes

Notes live in `$SECRETARY_DATA_DIR/notes/` (docs/server-api.md, "Notes"). Import the seed notes in
`deploy/seed/notes/` (e.g. the CRAFT-IT4SD briefing `craftit-meeting`, recovered from Lucina's
timeline) once; existing slugs are kept unless `--replace`. The running server picks them up on its
next request, no restart needed.

```bash
# from nezopt
scp -r deploy/seed/notes p1:/tmp/secretary-seed-notes
# on p1
sudo install -d -o secretary -g secretary -m 700 /tmp/secretary-seed
sudo install -o secretary -g secretary -m 600 /tmp/secretary-seed-notes/*.md /tmp/secretary-seed/
sudo -u secretary env SECRETARY_DATA_DIR=/var/lib/secretary \
  /usr/local/bin/secretary-server notes import /tmp/secretary-seed
sudo -u secretary env SECRETARY_DATA_DIR=/var/lib/secretary /usr/local/bin/secretary-server notes list
sudo rm -rf /tmp/secretary-seed /tmp/secretary-seed-notes
```

## 4. Web app

Copy the SolidJS build output (`web/dist/`) to `/var/lib/secretary/web` (it
must contain `index.html`; hashed files go in `assets/`):

```bash
rsync -a --delete dist/ p1:/tmp/secretary-web/
install -d -m 0750 -o secretary -g secretary /var/lib/secretary/web
rsync -a --delete --chown=secretary:secretary /tmp/secretary-web/ /var/lib/secretary/web/
```

No restart is needed for new web files.

## 5. Caddy

Append `deploy/Caddyfile.snippet` to `/etc/caddy/Caddyfile` (DNS for
`secratary.app` and `www.secratary.app` must point at p1, ports 80/443
open), then:

```bash
caddy validate --config /etc/caddy/Caddyfile
systemctl reload caddy
curl -sI https://secratary.app/v1/health
```

Keep `SECRETARY_TRUST_PROXY=1` only while the server listens on 127.0.0.1
behind Caddy. Exposed directly, it would let clients forge
`X-Forwarded-For` and dodge the login rate limit.

## Configuration reference

| Variable | Default | |
|---|---|---|
| `SECRETARY_BIND` | `127.0.0.1:8800` | listen address |
| `SECRETARY_LUCINA_URL` | `http://127.0.0.1:18788` | Lucina origin |
| `SECRETARY_LUCINA_TOKEN_FILE` | unset | Lucina Bearer token file. Unset: data routes answer `503 lucina_not_configured` |
| `SECRETARY_DATA_DIR` | `/var/lib/secretary` | devices, sessions, owner account, gym and body data |
| `SECRETARY_SERVICE_TOKEN_FILE` | unset | `svc_…` token file for Lucina. Unset: `/v1/insights/summary` answers `503 service_token_not_configured` |
| `SECRETARY_PRIVATE_ASSETS_DIR` | `/var/lib/secretary/private-assets` | files for `GET /v1/assets/{name}` (authenticated only; keep the avatar model here, never in `web/`) |
| `SECRETARY_TIMEZONE` | `Europe/Copenhagen` | the owner's zone (calendar days, all-day events) |
| `SECRETARY_GOOGLE_CLIENT_ID_FILE` / `SECRETARY_GOOGLE_CLIENT_SECRET_FILE` | unset | Google OAuth client (both or neither). Unset: the Google cards show "not set up" |
| `SECRETARY_GOOGLE_REDIRECT_URI` | `https://getsecretary.tech/v1/links/google/callback` | must be registered on the client |
| `SECRETARY_CALENDAR_SYNC_SECS` | `300` | Google + ICS background sync; `0` off |
| `SECRETARY_MCP_STATUS_URL` / `SECRETARY_MCP_STATUS_TOKEN_FILE` | `http://127.0.0.1:8810/internal/status` / unset | AI client cards in Links |
| `SECRETARY_TTS_URL` | `http://100.107.164.33:8880` | Kokoro for `POST /v1/tts` (p1 must reach it over Tailscale) |
| `SECRETARY_TTS_VOICES` | `if_sara,af_heart,af_bella,bf_emma` | voices clients may request |
| `SECRETARY_WEB_DIR` | unset | static web app at `/` |
| `SECRETARY_TRUST_PROXY` | `0` | `1`: client IP from `X-Forwarded-For` |
| `SECRETARY_UPSTREAM_LEGACY` / `_SERVICES` / `_CATALOG` | unset | enable `/v1/upstream/*` |
| `RUST_LOG` | `secretary_server=info` | log filter |

Logs (`journalctl -u secretary-server`) carry method, route template,
status, latency, body length and device id. They never contain tokens,
cookies, `Authorization` headers, query strings or bodies.
