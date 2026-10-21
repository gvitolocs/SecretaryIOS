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
| `SECRETARY_WEB_DIR` | unset | static web app at `/` |
| `SECRETARY_TRUST_PROXY` | `0` | `1`: client IP from `X-Forwarded-For` |
| `SECRETARY_UPSTREAM_LEGACY` / `_SERVICES` / `_CATALOG` | unset | enable `/v1/upstream/*` |
| `RUST_LOG` | `secretary_server=info` | log filter |

Logs (`journalctl -u secretary-server`) carry method, route template,
status, latency, body length and device id. They never contain tokens,
cookies, `Authorization` headers, query strings or bodies.
